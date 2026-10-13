use super::*;
fn attributes(raw: &str) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let begin = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'=' {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        let key = &raw[begin..i];
        i += 1;
        let quote = bytes.get(i).copied().filter(|b| *b == b'"' || *b == b'\'');
        if quote.is_some() {
            i += 1;
        }
        let begin = i;
        while i < bytes.len()
            && quote.map_or(!bytes[i].is_ascii_whitespace(), |quote| bytes[i] != quote)
        {
            i += 1;
        }
        if i > begin && key.len() <= 64 && i - begin <= 4096 {
            output.insert(key.to_string(), raw[begin..i].to_string());
        }
        if quote.is_some() && i < bytes.len() {
            i += 1;
        }
    }
    output
}
fn label(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|word| !word.contains("://"))
        .flat_map(|word| {
            word.chars()
                .filter(|c| !c.is_control())
                .chain(std::iter::once(' '))
        })
        .take(512)
        .collect::<String>()
        .trim()
        .to_owned()
}
pub fn parse_m3u(raw: &str, base: &Url, connection: &str) -> Result<Vec<PrivateChannel>, Error> {
    if raw.len() > 3 * 1024 * 1024
        || !raw
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with("#EXTM3U")
        || raw.contains("#EXT-X-")
    {
        return Err(Error::InvalidQuery);
    }
    let mut output: Vec<PrivateChannel> = Vec::new();
    let mut positions = std::collections::HashMap::new();
    let mut info = None;
    let mut headers = BTreeMap::new();
    for line in raw.lines().take(50000) {
        let line = line.trim();
        if let Some(raw) = line.strip_prefix("#EXTINF:") {
            let mut quote = None;
            let comma = raw.char_indices().find_map(|(index, c)| {
                if c == '"' || c == '\'' {
                    if quote == Some(c) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(c);
                    }
                }
                (c == ',' && quote.is_none()).then_some(index)
            });
            let (attrs, name) = comma
                .map(|index| (&raw[..index], &raw[index + 1..]))
                .unwrap_or((raw, ""));
            info = Some((attributes(attrs), label(name)));
            headers.clear();
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXTVLCOPT:http-user-agent=") {
            if value.len() > 2048 || value.chars().any(char::is_control) {
                return Err(Error::InvalidQuery);
            }
            headers.insert("User-Agent".into(), value.into());
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXTVLCOPT:http-referrer=") {
            if value.len() > 2048 || value.chars().any(char::is_control) {
                return Err(Error::InvalidQuery);
            }
            headers.insert("Referer".into(), value.into());
            continue;
        }
        if let Some((name, value)) = line
            .strip_prefix("#EXTVLCOPT:")
            .and_then(|line| line.split_once('='))
        {
            let header = match name {
                "http-cookie" => Some("Cookie"),
                "http-origin" => Some("Origin"),
                _ => None,
            };
            if let Some(header) = header {
                if value.len() > 2048 || value.chars().any(char::is_control) {
                    return Err(Error::InvalidQuery);
                }
                headers.insert(header.into(), value.into());
                continue;
            }
            if name.starts_with("http-") {
                return Err(Error::InvalidQuery);
            }
        }
        if let Some(value) = line.strip_prefix("#KODIPROP:inputstream.adaptive.stream_headers=") {
            for (name, value) in url::form_urlencoded::parse(value.as_bytes()) {
                if !["referer", "origin", "user-agent", "cookie", "authorization"]
                    .iter()
                    .any(|header| name.eq_ignore_ascii_case(header))
                    || value.len() > 2048
                    || value.chars().any(char::is_control)
                {
                    return Err(Error::InvalidQuery);
                }
                headers.insert(name.into_owned(), value.into_owned());
            }
            continue;
        }
        if line.starts_with("#KODIPROP:inputstream.adaptive.license_") {
            return Err(Error::Unavailable);
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((attrs, name)) = info.take() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let url = base.join(line).map_err(|_| Error::InvalidQuery)?;
        viptv_provider::validate_url(url.as_str()).map_err(|_| Error::InvalidQuery)?;
        let epg = attrs.get("tvg-id").filter(|s| !s.is_empty()).cloned();
        let group = attrs
            .get("group-title")
            .map(|s| label(s))
            .filter(|s| !s.is_empty());
        let identity = format!(
            "{}\0{name}\0{}",
            epg.as_deref().unwrap_or(""),
            group.as_deref().unwrap_or("")
        );
        let source = LiveSource {
            url: url.to_string(),
            headers: headers.clone(),
        };
        let id = channel_id(connection, &identity);
        if let Some(position) = positions.get(&id).copied() {
            let existing: &mut PrivateChannel = &mut output[position];
            if existing.sources.len() < 16
                && !existing
                    .sources
                    .iter()
                    .any(|s| s.url == source.url && s.headers == source.headers)
            {
                existing.sources.push(source);
            }
            continue;
        }
        if output.len() >= 10000 {
            break;
        }
        positions.insert(id.clone(), output.len());
        output.push(PrivateChannel {
            channel: LiveChannel {
                id: id.clone(),
                connection_id: connection.into(),
                card: live_card(
                    id,
                    name,
                    artwork(attrs.get("tvg-logo").map(String::as_str)),
                    connection.into(),
                ),
                group_id: group
                    .as_ref()
                    .map(|name| channel_id(connection, &format!("group:{name}"))),
                group_name: group,
            },
            epg_id: epg,
            stream_id: None,
            sources: vec![source],
        });
    }
    Ok(output)
}
