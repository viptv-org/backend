use serde_json::{Map, Value};

const REDACTED: &str = "<redacted>";
const MIN_TOKEN_LEN: usize = 32;
const SECRET_NAMES: &[&str] = &[
    "token", "secret", "password", "passwd", "pwd", "pass", "apikey", "key", "auth",
    "authorization", "signature", "sig", "session", "sid", "credential", "credentials",
];

pub(super) fn value(input: Value) -> Value {
    match input {
        Value::String(text) => Value::String(text_of(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (text_of(&key), value(item)))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

pub(super) fn text_of(input: &str) -> String {
    tokens(&bearer(&pairs(&urls(input))))
}

fn is_url_end(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'<' | b'>' | b'(' | b')')
}

fn is_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
}

fn urls(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(found) = input[cursor..].find("://") {
        let marker = cursor + found;
        let mut start = marker;
        while start > cursor && is_scheme_byte(bytes[start - 1]) {
            start -= 1;
        }
        while start < marker && !bytes[start].is_ascii_alphabetic() {
            start += 1;
        }
        let authority_start = marker + 3;
        if start == marker {
            out.push_str(&input[cursor..authority_start]);
            cursor = authority_start;
            continue;
        }
        let end = bytes[authority_start..]
            .iter()
            .position(|byte| is_url_end(*byte))
            .map_or(input.len(), |at| authority_start + at);
        let authority_end = bytes[authority_start..end]
            .iter()
            .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
            .map_or(end, |at| authority_start + at);
        let host = input[authority_start..authority_end]
            .rsplit('@')
            .next()
            .unwrap_or("");
        out.push_str(&input[cursor..start]);
        out.push_str(&input[start..authority_start]);
        out.push_str(host);
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAMES.contains(&lower.replace(['_', '-'], "").as_str())
        || lower
            .split(['_', '-'])
            .any(|segment| SECRET_NAMES.contains(&segment))
}

fn pairs(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut search = 0;
    while let Some(found) = input[search..].find('=') {
        let equals = search + found;
        let mut name_start = equals;
        while name_start > cursor && is_name_byte(bytes[name_start - 1]) {
            name_start -= 1;
        }
        search = equals + 1;
        if name_start == equals || !is_secret_name(&input[name_start..equals]) {
            continue;
        }
        let value_start = equals + 1;
        let value_end = bytes[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b'&' | b';' | b',' | b'"' | b'\'' | b')' | b'<' | b'>'))
            .map_or(input.len(), |at| value_start + at);
        if value_end == value_start {
            continue;
        }
        out.push_str(&input[cursor..value_start]);
        out.push_str(REDACTED);
        cursor = value_end;
        search = value_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn bearer(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut search = 0;
    while search < input.len() {
        let hit = ["bearer ", "basic "]
            .iter()
            .filter_map(|word| lower[search..].find(word).map(|at| (search + at, word.len())))
            .min();
        let Some((at, word_len)) = hit else { break };
        search = at + word_len;
        if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
            continue;
        }
        let value_start = at + word_len;
        let value_end = bytes[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b',' | b';'))
            .map_or(input.len(), |at| value_start + at);
        if value_end == value_start {
            continue;
        }
        out.push_str(&input[cursor..value_start]);
        out.push_str(REDACTED);
        cursor = value_end;
        search = value_end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut index = 0;
    while index < bytes.len() {
        if !is_token_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_token_byte(bytes[index]) {
            index += 1;
        }
        if index - start >= MIN_TOKEN_LEN {
            out.push_str(&input[cursor..start]);
            out.push_str(REDACTED);
            cursor = index;
        }
    }
    out.push_str(&input[cursor..]);
    out
}
