//! Provider prose becomes plain text once, before any platform projection.
use serde_json::Value;
fn entity(value: &str) -> Option<char> {
    match value {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "hellip" => Some('…'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        _ => value
            .strip_prefix("#x")
            .or_else(|| value.strip_prefix("#X"))
            .and_then(|n| u32::from_str_radix(n, 16).ok())
            .or_else(|| value.strip_prefix('#').and_then(|n| n.parse().ok()))
            .and_then(char::from_u32)
            .filter(|c| !c.is_control()),
    }
}
pub(super) fn text(value: &Value) -> Option<String> {
    let source = value
        .as_str()?
        .chars()
        .take(65536)
        .collect::<String>()
        .replace("\r\n", "\n");
    let mut stripped = String::new();
    let mut at = 0;
    let mut hidden = None;
    while at < source.len() {
        let rest = &source[at..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                at += end + 3;
                continue;
            }
        }
        if rest.starts_with('<') {
            if let Some(end) = rest.find('>').filter(|end| *end <= 512) {
                let tag = rest[1..end].trim();
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .to_ascii_lowercase();
                if !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric())
                    && name.as_bytes()[0].is_ascii_alphabetic()
                {
                    if hidden.as_deref() == Some(name.as_str()) && tag.starts_with('/') {
                        hidden = None;
                    } else if matches!(name.as_str(), "script" | "style") {
                        hidden = Some(name.clone());
                    } else if hidden.is_none() {
                        match name.as_str() {
                            "br" | "li" => stripped.push('\n'),
                            "p" | "div" => stripped.push_str("\n\n"),
                            _ => {}
                        }
                    }
                    at += end + 1;
                    continue;
                }
            }
        }
        let c = rest.chars().next()?;
        if hidden.is_none() {
            stripped.push(c);
        }
        at += c.len_utf8();
    }
    let mut decoded = String::new();
    let mut at = 0;
    while at < stripped.len() {
        let rest = &stripped[at..];
        if rest.starts_with('&') {
            if let Some(end) = rest.find(';').filter(|end| *end <= 16) {
                if let Some(c) = entity(&rest[1..end]) {
                    decoded.push(c);
                    at += end + 1;
                    continue;
                }
            }
        }
        let c = rest.chars().next()?;
        if c == '\n' || c == '\r' {
            decoded.push('\n');
        } else if !c.is_control() {
            decoded.push(c);
        }
        at += c.len_utf8();
    }
    let mut lines = Vec::new();
    let mut blank = false;
    for line in decoded.split('\n') {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !lines.is_empty() {
                blank = true;
            }
        } else {
            if blank {
                lines.push(String::new());
            }
            lines.push(line);
            blank = false;
        }
    }
    let result = lines.join("\n").chars().take(16384).collect::<String>();
    (!result.is_empty()).then_some(result)
}
