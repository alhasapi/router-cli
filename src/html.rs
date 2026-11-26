//! Minimal HTML helpers for scraping the router's server-rendered pages.

/// Strip HTML tags and common entities, then collapse whitespace.
pub fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = decode_entities(&out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode the HTML entities the router emits (`&amp;`, `&#39;`, `&#NN;`, …).
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            // Not an entity (or too far to be one): keep the ampersand.
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let translated = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix('#')
                .and_then(
                    |num| match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse::<u32>().ok(),
                    },
                )
                .and_then(char::from_u32),
        };
        match translated {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Remove `<script>...</script>` blocks so `document.write`'d markup inside JS
/// isn't mistaken for real form fields.
fn strip_scripts(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</script>") {
            Some(end) => {
                let end = start + end + "</script>".len();
                rest = &rest[end..];
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Extract the text content of a `<div id="...">` element.
///
/// The router's pages use flat divs (no nested divs), so the first `</div>`
/// after the opening tag is the matching close.
pub fn div_text(html: &str, id: &str) -> Option<String> {
    let html = strip_scripts(html);
    let marker = format!("id=\"{id}\"");
    let idx = html.find(&marker)?;
    let content_start = html[idx..].find('>')? + idx + 1;
    let content_end = html[content_start..].find("</div>")? + content_start;
    Some(strip_html(&html[content_start..content_end]))
}

/// Extract a `name="..."` or `name=value` attribute from a tag. Handles both
/// double-quoted and unquoted values (the router mixes the two styles).
fn attr(tag: &str, name: &str) -> Option<String> {
    // Quoted form: name="value"
    let pattern = format!("{name}=\"");
    if let Some(idx) = tag.find(&pattern) {
        let after = &tag[idx + pattern.len()..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    // Unquoted form: name=value (terminated by whitespace or '>')
    let pattern = format!("{name}=");
    if let Some(idx) = tag.find(&pattern) {
        let after = &tag[idx + pattern.len()..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(after.len());
        return Some(after[..end].to_string());
    }
    None
}

/// Extract all `<input>` fields (name, value). Skips button/checkbox/radio/file
/// inputs; text, hidden, and password inputs are kept.
pub fn input_fields(html: &str) -> Vec<(String, String)> {
    let html = strip_scripts(html);
    let mut out = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<input") {
        let end = rest[start..]
            .find('>')
            .map(|i| start + i + 1)
            .unwrap_or(rest.len());
        let tag = &rest[start..end];
        let ty = attr(tag, "type").unwrap_or_else(|| "text".to_string());
        let skip = matches!(
            ty.as_str(),
            "submit" | "reset" | "button" | "image" | "checkbox" | "radio" | "file"
        );
        if !skip {
            if let Some(name) = attr(tag, "name") {
                let value = attr(tag, "value").unwrap_or_default();
                out.push((name, value));
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Extract all `<select>` fields (name, selected option value).
pub fn select_fields(html: &str) -> Vec<(String, String)> {
    let html = strip_scripts(html);
    let mut out = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<select") {
        let end = rest[start..]
            .find("</select>")
            .map(|i| start + i + "</select>".len())
            .unwrap_or(rest.len());
        let sel = &rest[start..end];
        if let Some(name) = attr(sel, "name") {
            let value = selected_option_value(sel).unwrap_or_default();
            out.push((name, value));
        }
        rest = &rest[end..];
    }
    out
}

fn selected_option_value(sel: &str) -> Option<String> {
    let mut rest = sel;
    while let Some(start) = rest.find("<option") {
        let end = rest[start..]
            .find('>')
            .map(|i| start + i + 1)
            .unwrap_or(rest.len());
        let opt = &rest[start..end];
        if opt.contains("selected") {
            return attr(opt, "value");
        }
        rest = &rest[end..];
    }
    // Fall back to the first option.
    let start = sel.find("<option")?;
    let end = sel[start..]
        .find('>')
        .map(|i| start + i + 1)
        .unwrap_or(sel.len());
    attr(&sel[start..end], "value")
}

/// All form fields (inputs + selects).
pub fn form_fields(html: &str) -> Vec<(String, String)> {
    let mut fields = input_fields(html);
    fields.extend(select_fields(html));
    fields
}

/// Read the current value of a form field by name (input or select).
pub fn field_value(html: &str, name: &str) -> Option<String> {
    form_fields(html)
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
}

/// Read a value assigned in JavaScript, e.g. `document.tcpip.lan_macAddr.value = "..."`.
pub fn document_value(html: &str, name: &str) -> Option<String> {
    let pattern = format!(".{name}.value");
    let idx = html.find(&pattern)?;
    let after = &html[idx + pattern.len()..];
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_removes_tags_and_entities() {
        assert_eq!(strip_html("<td>Hello&nbsp;World</td>"), "Hello World");
    }

    #[test]
    fn div_text_extracts_content() {
        let html = r#"<div id="ussd_menu_id"><table><td>Foo Bar</td></table></div>"#;
        assert_eq!(div_text(html, "ussd_menu_id"), Some("Foo Bar".to_string()));
    }

    #[test]
    fn extracts_input_and_select_values() {
        let html = r#"<form>
            <input type="text" name="lan_ip" size="15" value="192.168.0.1">
            <select name="wanType"><option value="dhcp">DHCP</option><option value="static" selected>Static</option></select>
            <input type="checkbox" name="select1" value="ON">
            <input type="submit" name="save_apply">
        </form>"#;
        assert_eq!(field_value(html, "lan_ip"), Some("192.168.0.1".into()));
        assert_eq!(field_value(html, "wanType"), Some("static".into()));
        assert_eq!(field_value(html, "select1"), None); // checkbox skipped
        assert_eq!(field_value(html, "save_apply"), None); // button skipped
    }

    #[test]
    fn extracts_document_value() {
        let html = r#"<script>document.tcpip.lan_macAddr.value = "00aabbccddee";</script>"#;
        assert_eq!(
            document_value(html, "lan_macAddr"),
            Some("00aabbccddee".into())
        );
    }

    #[test]
    fn extracts_unquoted_attribute_value() {
        let html = r#"<input type="text" name="lan_ip" value=192.168.0.1 >"#;
        assert_eq!(field_value(html, "lan_ip"), Some("192.168.0.1".into()));
    }
}
