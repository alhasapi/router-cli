//! SMS inbox scraping (`sms_inbox.htm`).
//!
//! The router embeds the whole inbox in a JavaScript string literal:
//!
//! ```js
//! var smsListInfo = "15,16,17}-{0}-{CAMTEL}-{2026-11-26 16:16:41}-{Your balance is ...}-{|,|14}-{...}";
//! ```
//!
//! Records are separated by `|,|` and fields by `}-{`, in the order
//! `index}-{stat}-{number}-{time}-{content}`. See `docs/router-api.md`.

use std::fmt::Write as _;

use anyhow::{Result, anyhow};

use crate::html;

/// One SMS row as shown in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Message {
    /// Modem storage index(es); comma-separated for multi-part messages.
    pub index: String,
    /// `true` when the device reports the message unread (`stat == 0`).
    pub unread: bool,
    /// Sender: network name or MSISDN.
    pub number: String,
    /// Device-local timestamp, `YYYY-MM-DD HH:MM:SS`.
    pub time: String,
    /// Body text, HTML entities decoded.
    pub content: String,
}

/// Parse the inbox embedded in `sms_inbox.htm`.
pub fn parse_inbox(page: &str) -> Result<Vec<Message>> {
    let raw = extract_js_string(page).ok_or_else(|| {
        anyhow!("could not find the SMS list in sms_inbox.htm (session expired?)")
    })?;

    let mut messages = Vec::new();
    for record in raw.split("|,|") {
        // Each record ends with `}-{` right before the `|,|` separator.
        let record = record.trim().trim_end_matches("}-{").trim();
        if record.is_empty() {
            continue;
        }
        let parts: Vec<&str> = record.split("}-{").collect();
        if parts.len() < 5 {
            // Tolerate a stray/short record rather than failing the whole list.
            continue;
        }
        // A content field may itself contain the separator, so rejoin extras.
        let content = parts[4..].join("}-{");
        messages.push(Message {
            index: parts[0].trim().to_string(),
            unread: parts[1].trim() == "0",
            number: html::decode_entities(parts[2].trim()),
            time: parts[3].trim().to_string(),
            content: html::decode_entities(content.trim()),
        });
    }

    sort_newest_first(&mut messages);
    Ok(messages)
}

/// Return the newest `n` messages.
///
/// The device emits storage order, not chronological order, so ordering must be
/// applied here. Ties (same second) keep device order.
pub fn last_n(messages: &[Message], n: usize) -> Vec<Message> {
    let mut sorted = messages.to_vec();
    sort_newest_first(&mut sorted);
    sorted.into_iter().take(n).collect()
}

fn sort_newest_first(messages: &mut [Message]) {
    // `YYYY-MM-DD HH:MM:SS` sorts lexicographically; sort is stable.
    messages.sort_by(|a, b| b.time.cmp(&a.time));
}

/// Pull the raw string assigned to `var smsListInfo`, unescaping JS literals.
fn extract_js_string(page: &str) -> Option<String> {
    let marker = page.find("var smsListInfo")?;
    let rest = &page[marker..];
    let quote = rest.find('"')? + 1;
    let mut out = String::new();
    let mut chars = rest[quote..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    // Unterminated literal: treat what we read as the list.
    Some(out)
}

/// Render messages as an aligned table.
pub fn render_table(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "No SMS messages on the device.".to_string();
    }

    let rows: Vec<[String; 4]> = messages
        .iter()
        .map(|m| {
            [
                m.time.clone(),
                m.number.clone(),
                if m.unread { "unread" } else { "read" }.to_string(),
                one_line(&m.content),
            ]
        })
        .collect();

    let mut widths = [4usize, 4, 6, 7]; // TIME, FROM, STATUS, MESSAGE
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let headers = ["TIME", "FROM", "STATUS", "MESSAGE"];
    for (i, header) in headers.iter().enumerate() {
        if i + 1 == headers.len() {
            let _ = write!(out, "{header}");
        } else {
            let _ = write!(out, "{header:width$}  ", width = widths[i]);
        }
    }
    out.push('\n');
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                let _ = write!(out, "{cell}");
            } else {
                let _ = write!(out, "{cell:width$}  ", width = widths[i]);
            }
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Collapse newlines/tabs and runs of spaces so each message fits on one row.
fn one_line(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render messages as one block per message, with the body wrapped to `width`.
///
/// The text is the full body the device exposes (see the truncation note in
/// `docs/router-api.md`); only whitespace runs are normalised for wrapping.
pub fn render_full(messages: &[Message], width: usize) -> String {
    if messages.is_empty() {
        return "No SMS messages on the device.".to_string();
    }

    let mut out = String::new();
    for (i, m) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let header = format!(
            "[{}] {} {} {}",
            m.index,
            m.time,
            m.number,
            if m.unread { "unread" } else { "read" }
        );
        for line in wrap(&header, width, "") {
            out.push_str(&line);
            out.push('\n');
        }

        let body = one_line(&m.content);
        if body.is_empty() {
            out.push_str("  (empty)\n");
        } else {
            for line in wrap(&body, width, "  ") {
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    out.trim_end().to_string()
}

/// Greedy word wrap with a fixed indent; over-long words are hard-split.
fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let avail = width.max(indent.chars().count() + 10) - indent.chars().count();
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_len = 0usize;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if line_len > 0 && line_len + 1 + word_len > avail {
            lines.push(format!("{indent}{line}"));
            line.clear();
            line_len = 0;
        }
        if word_len > avail {
            // Flush the current line first, then hard-split the long word.
            if line_len > 0 {
                lines.push(format!("{indent}{line}"));
                line.clear();
                line_len = 0;
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(avail) {
                lines.push(format!("{indent}{}", chunk.iter().collect::<String>()));
            }
            continue;
        }
        if line_len > 0 {
            line.push(' ');
            line_len += 1;
        }
        line.push_str(word);
        line_len += word_len;
    }

    if line_len > 0 {
        lines.push(format!("{indent}{line}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic inbox using the same shape as the device (fake numbers/texts).
    const PAGE: &str = r#"<html><script>
var smsListInfo = "0}-{1}-{1065510198}-{2018-09-01 07:46:37}-{older read msg}-{|,|10}-{0}-{CAMTEL}-{2026-11-26 16:16:41}-{Newest &amp; best &#39;quote&#39;}-{|,|7}-{0}-{12345}-{2026-11-25 06:23:46}-{middle unread}-{|,|4}-{1}-{CAMTEL}-{2026-11-24 18:36:06}-{oldest read}-{|,|";
</script></html>"#;

    fn sample() -> Vec<Message> {
        parse_inbox(PAGE).unwrap()
    }

    #[test]
    fn parses_all_records_and_fields() {
        let messages = sample();
        assert_eq!(messages.len(), 4);

        // parse_inbox returns newest first.
        let newest = &messages[0];
        assert_eq!(newest.index, "10");
        assert_eq!(newest.number, "CAMTEL");
        assert_eq!(newest.time, "2026-11-26 16:16:41");
        assert!(newest.unread); // stat 0 means unread
        assert_eq!(newest.content, "Newest & best 'quote'");

        assert!(messages.iter().any(|m| m.unread && m.index == "7"));
    }

    #[test]
    fn sorts_newest_first() {
        let messages = sample();
        let times: Vec<&str> = messages.iter().map(|m| m.time.as_str()).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted);
    }

    #[test]
    fn last_n_truncates_to_newest() {
        let messages = sample();

        let three = last_n(&messages, 3);
        assert_eq!(three.len(), 3);
        assert_eq!(three[0].index, "10");
        assert_eq!(three[2].index, "4");

        let one = last_n(&messages, 1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].index, "10");

        // More than available returns everything; zero returns nothing.
        assert_eq!(last_n(&messages, 99).len(), 4);
        assert!(last_n(&messages, 0).is_empty());
    }

    #[test]
    fn empty_inbox_parses_to_empty_list() {
        let page = r#"<script>var smsListInfo = "";</script>"#;
        assert!(parse_inbox(page).unwrap().is_empty());
        assert_eq!(render_table(&[]), "No SMS messages on the device.");
    }

    #[test]
    fn multi_part_index_and_blank_lines_are_preserved() {
        let page = r#"<script>var smsListInfo = "15,16,17}-{0}-{CAMTEL}-{2026-11-26 16:16:41}-{line one\nline two}-{|,|";</script>"#;
        let messages = parse_inbox(page).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].index, "15,16,17");
        assert_eq!(messages[0].content, "line one\nline two");
        // Table rendering keeps it to one row.
        assert!(render_table(&messages).contains("line one line two"));
    }

    #[test]
    fn missing_list_is_an_error() {
        assert!(parse_inbox("<html>login page</html>").is_err());
    }

    #[test]
    fn full_blocks_show_header_and_entire_body() {
        let messages = last_n(&sample(), 2);
        let text = render_full(&messages, 80);
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines[0].starts_with("[10] 2026-11-26 16:16:41 CAMTEL unread"));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("[7] 2026-11-25 06:23:46 12345 unread"))
        );
        // Every line stays within the requested width.
        assert!(lines.iter().all(|l| l.chars().count() <= 80), "{text}");
        // Body is indented and text is not dropped, just re-wrapped.
        assert!(text.contains("\n  Newest & best 'quote'\n"), "{text}");
    }

    #[test]
    fn full_blocks_wrap_long_bodies_and_separate_messages() {
        let messages = vec![Message {
            index: "15,16,17".into(),
            unread: false,
            number: "CAMTEL".into(),
            time: "2026-11-26 16:16:41".into(),
            content:
                "C'est une phrase suffisamment longue pour devoir etre coupee sur plusieurs lignes."
                    .into(),
        }];
        let text = render_full(&messages, 40);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().all(|l| l.chars().count() <= 40), "{text}");
        assert!(text.contains("  C'est une phrase"));

        let two = render_full(&last_n(&sample(), 2), 80);
        assert!(
            two.contains("\n\n"),
            "messages are separated by a blank line"
        );
    }

    #[test]
    fn full_blocks_hard_split_overlong_words_and_handle_empty_body() {
        let messages = vec![Message {
            index: "1".into(),
            unread: false,
            number: "X".into(),
            time: "2026-01-01 00:00:00".into(),
            content: "A".repeat(95),
        }];
        let text = render_full(&messages, 30);
        assert!(text.lines().all(|l| l.chars().count() <= 30), "{text}");
        assert!(text.lines().filter(|l| l.starts_with("  A")).count() >= 4);

        let empty = vec![Message {
            index: "2".into(),
            unread: false,
            number: "X".into(),
            time: "2026-01-01 00:00:00".into(),
            content: "   ".into(),
        }];
        assert!(render_full(&empty, 80).contains("  (empty)"));
        assert_eq!(render_full(&[], 80), "No SMS messages on the device.");
    }
}
