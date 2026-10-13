use crate::Error;

/// Browser tracks require WebVTT. Preserve VTT and convert ordinary UTF-8 SRT
/// without guessing binary, archive, ASS or legacy text encodings.
pub fn subtitle_vtt(raw: &str) -> Result<String, Error> {
    if raw.len() > 2 * 1024 * 1024
        || raw
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(Error::InvalidQuery);
    }
    let text = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let first = text.lines().next().unwrap_or_default();
    if first == "WEBVTT" || first.starts_with("WEBVTT ") || first.starts_with("WEBVTT\t") {
        return Ok(text);
    }
    let mut result = String::from("WEBVTT\n\n");
    let mut cues = 0;
    for block in text.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let mut lines = block.trim_matches('\n').lines();
        let first = lines.next().ok_or(Error::InvalidQuery)?;
        let timing = if first.trim().bytes().all(|b| b.is_ascii_digit()) {
            lines.next().ok_or(Error::InvalidQuery)?
        } else {
            first
        };
        let (start, end) = timing.split_once("-->").ok_or(Error::InvalidQuery)?;
        let start = timestamp(start.trim()).ok_or(Error::InvalidQuery)?;
        let end = timestamp(end.trim()).ok_or(Error::InvalidQuery)?;
        if end.0 <= start.0 {
            return Err(Error::InvalidQuery);
        }
        let body: Vec<_> = lines.collect();
        if body.is_empty() {
            return Err(Error::InvalidQuery);
        }
        result.push_str(&format!(
            "{} --> {}\n{}\n\n",
            start.1,
            end.1,
            body.join("\n")
        ));
        cues += 1;
        if cues > 20_000 {
            return Err(Error::InvalidQuery);
        }
    }
    if cues == 0 {
        return Err(Error::InvalidQuery);
    }
    Ok(result)
}

fn timestamp(raw: &str) -> Option<(u64, String)> {
    let normalized = raw.replace(',', ".");
    let (time, fraction) = normalized.split_once('.')?;
    if fraction.len() != 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let values: Vec<_> = time.split(':').collect();
    if values.len() != 3
        || values
            .iter()
            .any(|v| v.len() != 2 || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let hour: u64 = values[0].parse().ok()?;
    let minute: u64 = values[1].parse().ok()?;
    let second: u64 = values[2].parse().ok()?;
    if minute > 59 || second > 59 {
        return None;
    }
    Some((
        ((hour * 60 + minute) * 60 + second) * 1000 + fraction.parse::<u64>().ok()?,
        normalized,
    ))
}
