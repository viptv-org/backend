use super::*;

pub(super) fn timestamp(s: &str) -> Result<i64, String> {
    DateTime::parse_from_str(s.trim(), "%Y%m%d%H%M%S %z")
        .map(|d| d.timestamp())
        .map_err(|_| "guide_timestamp_invalid".into())
}
fn xml_text(bytes: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(bytes).map_err(|_| "guide_encoding_invalid")?;
    quick_xml::escape::unescape(raw)
        .map(|s| s.into_owned())
        .map_err(|_| "guide_entity_invalid".into())
}
pub(super) fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("guide_size_limit".into());
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut channels = HashMap::new();
    let mut programs = Vec::new();
    let mut channel = None::<String>;
    let mut name = String::new();
    let mut program = None::<(String, i64, i64, Value)>;
    let mut root = false;
    let mut capture_name = false;
    loop {
        let event = reader.read_event().map_err(|_| "guide_xml_invalid")?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::DocType(e) => {
                if e.as_ref().contains('[') {
                    return Err("guide_doctype_unsupported".into());
                }
            }
            Event::Start(e) | Event::Empty(e) => {
                let tag = e.name().as_ref().to_owned();
                let attrs = e
                    .attributes()
                    .map(|v| {
                        let v = v.map_err(|_| "guide_xml_invalid")?;
                        Ok((v.key.as_ref().to_owned(), xml_text(v.value.as_bytes())?))
                    })
                    .collect::<Result<HashMap<_, _>, String>>()?;
                if stack.is_empty() {
                    if tag != "tv" || root {
                        return Err("guide_xml_invalid".into());
                    }
                    root = true;
                }
                if stack.len() > 32 {
                    return Err("guide_depth_limit".into());
                }
                if tag == "channel" {
                    channel = Some(
                        attrs
                            .get("id")
                            .filter(|s| !s.is_empty() && s.len() <= 512)
                            .ok_or("guide_channel_invalid")?
                            .clone(),
                    );
                    name.clear();
                }
                if tag == "display-name" {
                    capture_name = name.is_empty();
                }
                if tag == "programme" {
                    let id = attrs.get("channel").ok_or("guide_channel_invalid")?.clone();
                    let start = timestamp(attrs.get("start").ok_or("guide_timestamp_invalid")?)?;
                    let end = timestamp(attrs.get("stop").ok_or("guide_timestamp_invalid")?)?;
                    if end <= start || end - start > 86400 {
                        return Err("guide_interval_invalid".into());
                    }
                    program = Some((id, start, end, json!({})));
                }
                if tag == "icon" {
                    if let Some((_, _, _, data)) = program.as_mut() {
                        if let Some(url) = attrs
                            .get("src")
                            .filter(|s| s.len() <= 2048 && util::validate_url(s).is_ok())
                        {
                            data["icon"] = json!(url);
                        }
                    }
                }
                if !empty {
                    stack.push(tag);
                }
            }
            Event::Text(e) => {
                let text = xml_text(e.as_ref().as_bytes())?;
                append_text(&stack, &mut name, &mut program, &text, capture_name)?;
            }
            Event::CData(e) => {
                let text = e.as_ref();
                append_text(&stack, &mut name, &mut program, text, capture_name)?;
            }
            Event::GeneralRef(e) => {
                let text = xml_text(format!("&{};", e.as_ref()).as_bytes())?;
                append_text(&stack, &mut name, &mut program, &text, capture_name)?;
            }
            Event::End(e) => {
                let tag = e.name().as_ref().to_owned();
                if stack.pop().as_deref() != Some(tag.as_str()) {
                    return Err("guide_xml_invalid".into());
                }
                if tag == "display-name" {
                    capture_name = false;
                }
                if tag == "channel" {
                    let id = channel.take().ok_or("guide_channel_invalid")?;
                    if name.trim().is_empty()
                        || channels.insert(id, name.trim().to_owned()).is_some()
                    {
                        return Err("guide_channel_invalid".into());
                    }
                    if channels.len() > 50000 {
                        return Err("guide_channel_limit".into());
                    }
                }
                if tag == "programme" {
                    let row = program.take().ok_or("guide_programme_invalid")?;
                    if row.3["title"].as_str().is_none_or(|s| s.trim().is_empty()) {
                        return Err("guide_title_invalid".into());
                    }
                    if row.2 > util::now() - 86400 && row.1 < util::now() + 8 * 86400 {
                        programs.push(row);
                    }
                    if programs.len() > 250000 {
                        return Err("guide_programme_limit".into());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || !stack.is_empty() || channels.is_empty() || programs.is_empty() {
        return Err("guide_empty_or_incomplete".into());
    }
    if programs
        .iter()
        .any(|(id, _, _, _)| !channels.contains_key(id))
    {
        return Err("guide_channel_missing".into());
    }
    Ok(Parsed { channels, programs })
}
fn append_text(
    stack: &[String],
    name: &mut String,
    program: &mut Option<(String, i64, i64, Value)>,
    text: &str,
    capture_name: bool,
) -> Result<(), String> {
    if let Some((_, _, _, data)) = program {
        let tag = stack.last().map(String::as_str).unwrap_or("");
        if [
            "title",
            "sub-title",
            "desc",
            "category",
            "date",
            "episode-num",
            "value",
            "country",
            "language",
        ]
        .contains(&tag)
        {
            let key = match tag {
                "desc" => "description",
                "sub-title" => "subtitle",
                "episode-num" => "episode",
                "value" => "rating",
                v => v,
            };
            let mut value = data[key].as_str().unwrap_or("").to_owned();
            value.push_str(text);
            if value.len() > 8192 {
                return Err("guide_text_limit".into());
            }
            data[key] = json!(value);
        }
    } else if capture_name && stack.last().is_some_and(|s| s == "display-name") {
        if name.len() + text.len() > 512 {
            return Err("guide_text_limit".into());
        }
        name.push_str(text);
    }
    Ok(())
}
