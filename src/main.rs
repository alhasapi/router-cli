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
    /// Send a USSD code and print the response.
    Ussd {
        /// USSD code to dial, e.g. *100#
        code: String,
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
        #[arg(long)]
        json: bool,
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
        Command::Ussd { code } => cmd_ussd(&session, &code),
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

fn cmd_ussd(session: &Session, code: &str) -> anyhow::Result<()> {
    // Load the USSD page first (session-state guard, same as reboot).
    session.get("/ussd.htm").context("loading USSD page")?;

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

    // The carrier response is rendered in one of these two divs.
    let result = html::div_text(&body, "ussd_result_id").filter(|s| !s.is_empty());
    let menu = html::div_text(&body, "ussd_menu_id").filter(|s| !s.is_empty());

    match (result, menu) {
        (Some(text), _) => println!("{text}"),
        (None, Some(text)) => println!("{text}"),
        (None, None) => println!("(no USSD response)"),
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
        SmsAction::List { count, json } => sms_list(session, count, json),
    }
}

fn sms_list(session: &Session, count: usize, json: bool) -> anyhow::Result<()> {
    let body = session.get("/sms_inbox.htm").context("loading SMS inbox")?;

    if body.contains("parent.location='/login.htm'") {
        anyhow::bail!("session expired or rejected; try again");
    }

    let messages = sms::last_n(&sms::parse_inbox(&body)?, count);

    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else {
        println!("{}", sms::render_table(&messages));
    }
    Ok(())
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
    fn sms_list_defaults_to_ten_and_table() {
        let cli = Cli::try_parse_from(["router-cli", "sms", "list"]).unwrap();
        let Command::Sms {
            action: SmsAction::List { count, json },
        } = cli.command
        else {
            panic!("expected sms list");
        };
        assert_eq!(count, 10);
        assert!(!json);
    }

    #[test]
    fn sms_list_accepts_short_n_and_json() {
        let cli = Cli::try_parse_from(["router-cli", "sms", "list", "-n", "3", "--json"]).unwrap();
        let Command::Sms {
            action: SmsAction::List { count, json },
        } = cli.command
        else {
            panic!("expected sms list");
        };
        assert_eq!(count, 3);
        assert!(json);
    }
}
