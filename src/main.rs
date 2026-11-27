mod client;
mod config;
mod html;
mod sms;

use anyhow::Context;
use clap::{Parser, Subcommand};

use client::Session;

#[derive(Parser)]
#[command(
    name = "router-cli",
    version,
    about = "Terminal control for the router web UI (reboot, USSD, SMS, WAN/LAN/WiFi config)."
)]
struct Cli {
    /// Router base URL.
    #[arg(long, global = true)]
    base_url: Option<String>,

    /// Admin username.
    #[arg(long, global = true)]
    username: Option<String>,

    /// Admin password. Prefer the ROUTER_ADMIN_PASSWORD env var or config file.
    #[arg(long, global = true)]
    password: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Reboot the router.
    Reboot,
    /// Send a USSD code and print the response (or cancel the active session).
    Ussd {
        /// USSD code to dial (e.g. *825#) or a menu option such as 1.
        #[arg(required_unless_present = "cancel", conflicts_with = "cancel")]
        code: Option<String>,
        /// Cancel the active USSD session instead of dialing.
        #[arg(long)]
        cancel: bool,
    },
    /// Get or set WAN/LAN/WiFi configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Read SMS messages from the inbox.
    Sms {
        #[command(subcommand)]
        action: SmsAction,
    },
}

#[derive(Subcommand)]
enum SmsAction {
    /// Print the last N received messages (newest first).
    List {
        /// Number of messages to show.
        #[arg(short = 'n', long = "number", default_value_t = 10)]
        count: usize,
        /// Emit JSON instead of a table.
        #[arg(long, conflicts_with = "full")]
        json: bool,
        /// Print each message as a block with its full stored body.
        #[arg(short = 'f', long, conflicts_with = "json")]
        full: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Read a value: router-cli config get <section> <key>
    Get { section: String, key: String },
    /// Write a value: router-cli config set <section> <key> <value>
    Set {
        section: String,
        key: String,
        value: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let creds = config::resolve(
        cli.base_url.as_deref(),
        cli.username.as_deref(),
        cli.password.as_deref(),
    )?;

    let session = Session::login(&creds.base_url, &creds.username, &creds.password)?;

    match cli.command {
        Command::Reboot => cmd_reboot(&session),
        Command::Ussd { code, cancel } => cmd_ussd(&session, code.as_deref(), cancel),
        Command::Config { action } => cmd_config(&session, action),
        Command::Sms { action } => cmd_sms(&session, action),
    }
}

fn cmd_reboot(session: &Session) -> anyhow::Result<()> {
    let host = session.host().to_string();
    let port = session.port();

    // The router only accepts the reboot POST after the session has loaded
    // this page (weak CSRF / session-state guard).
    session
        .get("/saveconf.htm")
        .context("loading System page")?;

    // Trigger the reboot. The device may respond with a "rebooting" page and
    // then drop the connection, so either Ok or Err means the command was sent.
    match session.post_form("/boafrm/formSaveConfig", &[("reboot", "Reboot")]) {
        Ok(_) => println!("Reboot command sent."),
        Err(_) => println!("Reboot command sent (connection dropped while restarting)."),
    }

    eprintln!("Waiting for the router to go offline (reboot starting)...");
    if !client::wait_offline(&host, port, std::time::Duration::from_secs(30)) {
        anyhow::bail!("router did not go offline within 30 seconds (reboot did not start)")
    }

    eprintln!("Router is offline. Waiting for it to come back...");
    if client::wait_online(&host, port, std::time::Duration::from_secs(120)) {
        println!("Router is back online.");
        Ok(())
    } else {
        anyhow::bail!("router did not come back online within 120 seconds")
    }
}

fn cmd_ussd(session: &Session, code: Option<&str>, cancel: bool) -> anyhow::Result<()> {
    // Load the USSD page first (session-state guard, same as reboot).
    session.get("/ussd.htm").context("loading USSD page")?;

    if cancel {
        let body = session
            .post_form(
                "/boafrm/formUSSDSetup",
                &[
                    ("ussdValue", ""),
                    ("selectMenuValue", ""),
                    ("ussdStatusInput", "menu"),
                    ("ussdCancelInput", "1"),
                    ("submit-url", "/ussd.htm"),
                ],
            )
            .context("cancelling USSD session")?;
        session.get("/ussd.htm").context("checking USSD session")?;
        if body.contains("var ussdStatus = '0'") {
            println!("USSD session cancelled.");
        } else {
            println!("USSD cancel request sent (no open session reported).");
        }
        return Ok(());
    }

    let code = code.expect("clap requires a code unless --cancel is used");
    let body = session
        .post_form(
            "/boafrm/formUSSDSetup",
            &[
                ("ussdValue", code),
                ("ussdStatusInput", "ussd"),
                ("ussdCancelInput", "0"),
                ("selectMenuValue", ""),
                ("submit-url", "/ussd.htm"),
            ],
        )
        .context("sending USSD code")?;

    if body.contains("parent.location='/login.htm'") {
        anyhow::bail!("session expired or rejected; try again");
    }

    // The carrier response is rendered in one of these two divs, with one
    // device line per menu option.
    let menu = html::div_lines(&body, "ussd_menu_id").unwrap_or_default();
    let result = html::div_lines(&body, "ussd_result_id").unwrap_or_default();
    let lines = if menu.is_empty() { result } else { menu };

    if lines.is_empty() {
        println!("(no USSD response)");
        return Ok(());
    }
    for line in &lines {
        println!("{line}");
    }
    if lines.len() == 1 && lines[0] == "Unexpected Data Value" {
        eprintln!(
            "hint: an earlier USSD session may still be open — run `router-cli ussd --cancel` and dial again."
        );
    }
    Ok(())
}

fn cmd_config(session: &Session, action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Get { section, key } => config_get(session, &section, &key),
        ConfigAction::Set {
            section,
            key,
            value,
        } => config_set(session, &section, &key, &value),
    }
}

fn cmd_sms(session: &Session, action: SmsAction) -> anyhow::Result<()> {
    match action {
        SmsAction::List { count, json, full } => sms_list(session, count, json, full),
    }
}

fn sms_list(session: &Session, count: usize, json: bool, full: bool) -> anyhow::Result<()> {
    let body = session.get("/sms_inbox.htm").context("loading SMS inbox")?;

    if body.contains("parent.location='/login.htm'") {
        anyhow::bail!("session expired or rejected; try again");
    }

    let messages = sms::last_n(&sms::parse_inbox(&body)?, count);

    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else if full {
        println!("{}", sms::render_full(&messages, terminal_width()));
    } else {
        println!("{}", sms::render_table(&messages));
    }
    Ok(())
}

/// Terminal width for wrapping `--full` blocks, with a sane non-TTY default.
fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
        .unwrap_or(100)
        .max(20)
}

/// Map a section name to its settings page and form action.
fn section_spec(section: &str) -> Option<(&'static str, &'static str)> {
    match section {
        "wan" => Some(("/wanconfig.htm", "/boafrm/formWanConfigSetup")),
        "lan" => Some(("/tcpiplan.htm", "/boafrm/formTcpipSetup")),
        "wifi" => Some(("/wlbasic.htm", "/boafrm/formWlanSetup")),
        _ => None,
    }
}

fn config_get(session: &Session, section: &str, key: &str) -> anyhow::Result<()> {
    let (page, _) = section_spec(section)
        .ok_or_else(|| anyhow::anyhow!("unknown section '{section}' (expected wan, lan, wifi)"))?;

    let body = session
        .get(page)
        .with_context(|| format!("loading {page}"))?;
    let value = html::field_value(&body, key)
        .or_else(|| html::document_value(&body, key))
        .ok_or_else(|| anyhow::anyhow!("field '{key}' not found in section '{section}'"))?;

    println!("{value}");
    Ok(())
}

fn config_set(session: &Session, section: &str, key: &str, value: &str) -> anyhow::Result<()> {
    let (page, action) = section_spec(section)
        .ok_or_else(|| anyhow::anyhow!("unknown section '{section}' (expected wan, lan, wifi)"))?;

    // Load the page first (session-state guard) and capture every current field
    // so we can re-submit the full form with only the target field changed.
    let body = session
        .get(page)
        .with_context(|| format!("loading {page}"))?;

    let mut fields = html::form_fields(&body);
    let mut found = false;
    for (k, v) in fields.iter_mut() {
        if k == key {
            *v = value.to_string();
            found = true;
        }
    }
    if !found {
        anyhow::bail!(
            "field '{key}' not found (or not writable) in section '{section}'; use `router-cli config get {section} ...` to see available fields"
        );
    }

    let mut params: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // The UI's "Save & Apply" button renames save_apply_flag -> save_apply on
    // submit; include it so the change is actually committed.
    params.push(("save_apply", "1"));

    session
        .post_form(action, &params)
        .with_context(|| format!("applying {section}.{key}"))?;

    println!("set {section}.{key} = {value}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn username_flag_is_optional() {
        // Without --username the value must be None so the config-file/default
        // fallback in config::resolve can apply (regression test for the bug
        // where a clap default_value forced it to Some("admin")).
        let cli = Cli::try_parse_from(["router-cli", "reboot"]).unwrap();
        assert_eq!(cli.username, None);

        let cli = Cli::try_parse_from(["router-cli", "--username", "bob", "reboot"]).unwrap();
        assert_eq!(cli.username.as_deref(), Some("bob"));
    }

    #[test]
    fn ussd_takes_a_code() {
        let cli = Cli::try_parse_from(["router-cli", "ussd", "*825#"]).unwrap();
        let Command::Ussd { code, cancel } = cli.command else {
            panic!("expected ussd");
        };
        assert_eq!(code.as_deref(), Some("*825#"));
        assert!(!cancel);
    }

    #[test]
    fn ussd_menu_reply_is_just_a_code() {
        let cli = Cli::try_parse_from(["router-cli", "ussd", "1"]).unwrap();
        let Command::Ussd { code, cancel } = cli.command else {
            panic!("expected ussd");
        };
        assert_eq!(code.as_deref(), Some("1"));
        assert!(!cancel);
    }

    #[test]
    fn ussd_cancel_needs_no_code() {
        let cli = Cli::try_parse_from(["router-cli", "ussd", "--cancel"]).unwrap();
        let Command::Ussd { code, cancel } = cli.command else {
            panic!("expected ussd");
        };
        assert!(code.is_none());
        assert!(cancel);
    }

    #[test]
    fn ussd_requires_a_code_or_cancel() {
        let err = match Cli::try_parse_from(["router-cli", "ussd"]) {
            Ok(_) => panic!("a code or --cancel is required"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn ussd_cancel_conflicts_with_code() {
        let err = match Cli::try_parse_from(["router-cli", "ussd", "1", "--cancel"]) {
            Ok(_) => panic!("--cancel should conflict with a code"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn sms_list_defaults_to_ten_and_table() {
        let cli = Cli::try_parse_from(["router-cli", "sms", "list"]).unwrap();
        let Command::Sms {
            action: SmsAction::List { count, json, full },
        } = cli.command
        else {
            panic!("expected sms list");
        };
        assert_eq!(count, 10);
        assert!(!json);
        assert!(!full);
    }

    #[test]
    fn sms_list_accepts_short_n_and_json() {
        let cli = Cli::try_parse_from(["router-cli", "sms", "list", "-n", "3", "--json"]).unwrap();
        let Command::Sms {
            action: SmsAction::List { count, json, full },
        } = cli.command
        else {
            panic!("expected sms list");
        };
        assert_eq!(count, 3);
        assert!(json);
        assert!(!full);
    }

    #[test]
    fn sms_list_full_flag_is_short_or_long() {
        for args in ["-f", "--full"] {
            let cli = Cli::try_parse_from(["router-cli", "sms", "list", "-n", "2", args]).unwrap();
            let Command::Sms {
                action: SmsAction::List { count, full, .. },
            } = cli.command
            else {
                panic!("expected sms list");
            };
            assert_eq!(count, 2);
            assert!(full);
        }
    }

    #[test]
    fn sms_list_full_and_json_conflict() {
        let err = match Cli::try_parse_from(["router-cli", "sms", "list", "--full", "--json"]) {
            Ok(_) => panic!("--full and --json should conflict"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
