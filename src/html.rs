//! Minimal HTML helpers for scraping the router's server-rendered pages.

/// Decode the HTML entities the router emits (`&amp;`, `&#39;`, `&#NN;`, …).
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        // Entities normally end with ';', but the router also emits `&nbsp`
        // without one, so fall back to the known names in that case.
        let end = tail
            .find(';')
            .filter(|end| *end <= 10)
            .filter(|end| {
                let name = &tail[1..*end];
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
            })
            .or_else(|| named_entity_prefix(tail));
        let Some(end) = end else {
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
                // Without ';' only the name itself was consumed.
                rest = if tail.as_bytes().get(end) == Some(&b';') {
                    &tail[end + 1..]
                } else {
                    &tail[end..]
                };
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

/// Length of a semicolon-less named entity at the start of `tail` (including
/// the leading `&`), if it is followed by a non-alphanumeric character.
fn named_entity_prefix(tail: &str) -> Option<usize> {
    for name in ["nbsp", "quot", "apos", "amp", "lt", "gt"] {
        let len = 1 + name.len();
        if tail.len() >= len
            && tail[1..].starts_with(name)
            && !tail[len..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return Some(len);
        }
    }
    None
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

/// Extract the inner HTML of a `<div id="...">`, matching nested divs.
fn div_inner<'a>(html: &'a str, id: &str) -> Option<&'a str> {
    let marker = format!("id=\"{id}\"");
    let idx = html.find(&marker)?;
    let content_start = html[idx..].find('>')? + idx + 1;

    let mut depth = 1usize;
    let mut pos = content_start;
    while pos < html.len() {
        let open = html[pos..].find("<div").map(|i| pos + i);
        let close = html[pos..].find("</div>").map(|i| pos + i);
        match (open, close) {
            (Some(open), Some(close)) if open < close => {
                depth += 1;
                pos = open + 4;
            }
            (_, Some(close)) => {
                depth -= 1;
                if depth == 0 {
                    return Some(&html[content_start..close]);
                }
                pos = close + 6;
            }
            _ => return None,
        }
    }
    None
}

/// Extract `<div id="...">` text with its original line structure.
///
/// Block-level boundaries (`<br>`, table rows/cells, paragraphs, list items)
/// become newlines; form controls are dropped; entities are decoded; runs of
/// spaces/tabs are collapsed and blank lines removed. Used for USSD menus,
/// where each option arrives on its own device line.
pub fn div_lines(html: &str, id: &str) -> Option<Vec<String>> {
    let html = strip_scripts(html);
    let inner = div_inner(&html, id)?;
    Some(text_lines(inner))
}

/// Convert an HTML fragment to display lines (see [`div_lines`]).
pub fn text_lines(fragment: &str) -> Vec<String> {
    let mut out = String::new();
    let mut rest = fragment;

    while let Some(idx) = rest.find('<') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        let Some(end) = after.find('>') else {
            // Unterminated tag: stop, keeping what came before it.
            rest = "";
            break;
        };
        let tag = after[..end].trim_start_matches('/').trim();
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = after.starts_with('/');
        let block = matches!(
            (name.as_str(), closing),
            ("br", _)
                | ("p", _)
                | ("div", _)
                | ("table", _)
                | ("tr", _)
                | ("td", _)
                | ("th", _)
                | ("li", _)
                | ("ul", _)
                | ("ol", _)
                | ("h1", _)
                | ("h2", _)
                | ("h3", _)
                | ("h4", _)
                | ("hr", _)
        );
        if block {
            out.push('\n');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);

    let decoded = decode_entities(&out);
    decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
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

    // Shape of the live /ussd.htm page after dialing *825# on the MC60.
    const USSD_PAGE: &str = r#"<html><body onload="Load_Setting();">
<script>var ussdStatus = '1';</script>
<div id="ussd_result_id">
<table class="block" border="0">
<tr><th>&nbsp;</th><td width="50%">&nbsp</td></tr>
</table>
<div id="ussd_menu_id">
<table width="100%">
<tr>
<td width="38%">&nbsp</td>
<td>
1) Forfaits Blue
<br>
2) Autres services
<br>3) Mon compte&nbsp;
<br><br>
<input type="text" name="selectMenuValue" size="10" maxlength="30">
</td>
</tr>
</table>
</div>
<BR>
</div>
</body></html>"#;

    #[test]
    fn div_lines_keeps_ussd_menu_lines() {
        let lines = div_lines(USSD_PAGE, "ussd_menu_id").unwrap();
        assert_eq!(
            lines,
            vec!["1) Forfaits Blue", "2) Autres services", "3) Mon compte",]
        );
    }

    #[test]
    fn div_lines_matches_nested_result_div() {
        let lines = div_lines(USSD_PAGE, "ussd_result_id").unwrap();
        assert_eq!(
            lines,
            vec!["1) Forfaits Blue", "2) Autres services", "3) Mon compte"]
        );
    }

    #[test]
    fn text_lines_decodes_entities_without_semicolons() {
        assert_eq!(
            text_lines("Menu&nbsp;principal<br>Solde:&nbsp;1&#39;000&nbsp;FCFA"),
            vec!["Menu principal", "Solde: 1'000 FCFA"]
        );
        assert_eq!(text_lines("A &amp B &lt; C"), vec!["A & B < C"]);
        // Unknown ampersand text is left alone.
        assert_eq!(text_lines("R&D and &bogus;"), vec!["R&D and &bogus;"]);
    }

    #[test]
    fn text_lines_drops_controls_and_blank_lines() {
        let lines = text_lines(
            "<table><tr><td>&nbsp</td><td>   </td></tr></table>\n<br>\n\n<p>Ok</p><input type=\"submit\" value=\"Send\">",
        );
        assert_eq!(lines, vec!["Ok"]);
    }

    #[test]
    fn div_lines_missing_id_is_none() {
        assert!(div_lines(USSD_PAGE, "nope_id").is_none());
    }
}
