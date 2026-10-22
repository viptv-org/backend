use super::*;
fn millis(raw: &str) -> Option<u64> {
    let value = chrono::DateTime::parse_from_str(raw, "%Y%m%d%H%M%S %z")
        .ok()?
        .timestamp_millis();
    u64::try_from(value).ok()
}
fn program(
    channel: &str,
    id: Option<String>,
    title: String,
    description: Option<String>,
    start: u64,
    end: u64,
) -> Option<LiveProgram> {
    let title = title.trim().to_owned();
    if title.is_empty()
        || title.len() > 1024
        || end <= start
        || end - start > 7 * 24 * 60 * 60 * 1000
    {
        return None;
    }
    Some(LiveProgram {
        id: id.unwrap_or_else(|| channel_id(channel, &format!("{start}:{end}:{title}"))),
        title,
        description,
        start_millis: start,
        end_millis: end,
    })
}
pub fn parse_xtream_guide(value: &serde_json::Value, channel: &str) -> Vec<LiveProgram> {
    value["epg_listings"]
        .as_array()
        .into_iter()
        .flatten()
        .take(100)
        .filter_map(|v| {
            let start = u64::try_from(viptv_provider::normalize::timestamp(&v["start_timestamp"])?)
                .ok()?
                .checked_mul(1000)?;
            let end = u64::try_from(
                viptv_provider::normalize::timestamp(&v["stop_timestamp"])
                    .or_else(|| viptv_provider::normalize::timestamp(&v["end_timestamp"]))?,
            )
            .ok()?
            .checked_mul(1000)?;
            program(
                channel,
                viptv_provider::normalize::scalar(&v["id"]),
                viptv_provider::normalize::decode_epg(Some(&v["title"])),
                Some(viptv_provider::normalize::decode_epg(Some(
                    &v["description"],
                ))),
                start,
                end,
            )
        })
        .collect()
}
pub fn parse_xmltv(raw: &str, epg: &str, channel: &str) -> Result<Vec<LiveProgram>, Error> {
    use quick_xml::{Reader, events::Event};
    if raw.len() > 16 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(false);
    let mut depth = 0usize;
    let mut root = false;
    let mut output = Vec::new();
    let mut active = None;
    let mut field = String::new();
    loop {
        match reader.read_event().map_err(|_| Error::InvalidQuery)? {
            Event::DocType(_) => return Err(Error::InvalidQuery),
            Event::Start(e) => {
                depth += 1;
                if e.local_name().as_ref() == b"tv" {
                    root = true;
                }
                let name = e.local_name();
                if name.as_ref() == b"programme" {
                    let mut start = None;
                    let mut end = None;
                    let mut matching = false;
                    for attr in e.attributes() {
                        let attr = attr.map_err(|_| Error::InvalidQuery)?;
                        let value = attr
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .map_err(|_| Error::InvalidQuery)?;
                        match attr.key.as_ref() {
                            b"channel" => matching = value == epg,
                            b"start" => start = millis(&value),
                            b"stop" => end = millis(&value),
                            _ => {}
                        }
                    }
                    active = if matching {
                        start
                            .zip(end)
                            .map(|(start, end)| (start, end, String::new(), String::new()))
                    } else {
                        None
                    };
                } else if active.is_some()
                    && (name.as_ref() == b"title" || name.as_ref() == b"desc")
                {
                    field = String::from_utf8_lossy(name.as_ref()).into_owned();
                }
            }
            Event::Text(e) => {
                if let Some((_, _, title, description)) = &mut active {
                    let decoded = e.decode().map_err(|_| Error::InvalidQuery)?;
                    let text =
                        quick_xml::escape::unescape(&decoded).map_err(|_| Error::InvalidQuery)?;
                    let target = if field == "title" {
                        title
                    } else if field == "desc" {
                        description
                    } else {
                        continue;
                    };
                    if target.len() + text.len() > 4096 {
                        return Err(Error::InvalidQuery);
                    }
                    target.push_str(&text);
                }
            }
            Event::CData(e) => {
                if let Some((_, _, title, description)) = &mut active {
                    let text = e.decode().map_err(|_| Error::InvalidQuery)?;
                    let target = if field == "title" {
                        title
                    } else if field == "desc" {
                        description
                    } else {
                        continue;
                    };
                    if target.len() + text.len() > 4096 {
                        return Err(Error::InvalidQuery);
                    }
                    target.push_str(&text);
                }
            }
            Event::GeneralRef(e) => {
                if let Some((_, _, title, description)) = &mut active {
                    let text =
                        if let Some(c) = e.resolve_char_ref().map_err(|_| Error::InvalidQuery)? {
                            c.to_string()
                        } else {
                            let name = e.decode().map_err(|_| Error::InvalidQuery)?;
                            match name.as_ref() {
                                "amp" => "&",
                                "lt" => "<",
                                "gt" => ">",
                                "quot" => "\"",
                                "apos" => "'",
                                _ => return Err(Error::InvalidQuery),
                            }
                            .into()
                        };
                    let target = if field == "title" {
                        title
                    } else if field == "desc" {
                        description
                    } else {
                        continue;
                    };
                    if target.len() + text.len() > 4096 {
                        return Err(Error::InvalidQuery);
                    }
                    target.push_str(&text);
                }
            }
            Event::End(e) => {
                depth = depth.checked_sub(1).ok_or(Error::InvalidQuery)?;
                if e.local_name().as_ref() == b"programme" {
                    if let Some((start, end, title, description)) = active.take() {
                        if let Some(p) =
                            program(channel, None, title, Some(description), start, end)
                        {
                            output.push(p);
                            if output.len() >= 1000 {
                                break;
                            }
                        }
                    }
                }
                field.clear();
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"tv" {
                    root = true;
                }
            }
            Event::Eof => {
                if !root || depth != 0 {
                    return Err(Error::InvalidQuery);
                }
                break;
            }
            _ => {}
        }
    }
    output.sort_by_key(|p| p.start_millis);
    Ok(output)
}
