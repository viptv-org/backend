//! Bounded provider-label hints for display and preference ranking, never decoder facts.
use super::streams::SourcePresentation;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

macro_rules! pattern {
    ($pattern:literal) => {{
        static RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("source metadata pattern"));
        &*RE
    }};
}

fn clean(value: &str, limit: usize) -> String {
    value
        .split_whitespace()
        .filter(|word| !word.contains("://") && !word.to_ascii_lowercase().contains("magnet:"))
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

fn hints(source: &Value) -> String {
    [
        source.get("name"),
        source.get("title"),
        source.get("description"),
        source.get("filename"),
        source.pointer("/behaviorHints/filename"),
        source.get("quality"),
        source.get("codec"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(|text| {
        text.chars()
            .take(4096)
            .collect::<String>()
            .lines()
            .take(32)
            .map(|line| clean(line, 1024))
            .collect::<Vec<_>>()
            .join("\n")
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn height(text: &str) -> Option<u32> {
    let text = text.replace('_', " ");
    let text = text.as_str();
    // Explicit height wins over broad HD/UHD aliases. Boundaries avoid 1080pixels/4kids.
    let explicit = pattern!(
        r"(?i)(?:^|[^a-z0-9])(?:([1-9][0-9]{2,3})[pi]|[1-9][0-9]{2,3}x([1-9][0-9]{2,3}))\b"
    )
    .captures_iter(text)
    .filter_map(|m| m.get(1).or_else(|| m.get(2)))
    .filter_map(|m| m.as_str().parse::<u32>().ok())
    .filter(|h| (144..=4320).contains(h))
    .max();
    explicit.or_else(|| {
        pattern!(r"(?i)(?:^|[^a-z0-9])(8k|4k|uhd|fhd|full[ ._-]?hd|hd|sd)\b")
            .captures_iter(text)
            .map(|m| match m[1].to_ascii_lowercase().as_str() {
                "8k" => 4320,
                "4k" | "uhd" => 2160,
                "fhd" | "full hd" | "full-hd" | "full.hd" | "full_hd" => 1080,
                "hd" => 720,
                _ => 480,
            })
            .max()
    })
}

/// Quality is reported/hinted metadata, not a measured playback property.
pub fn source_quality_height(source: &Value) -> Option<u32> {
    source["quality_height"]
        .as_u64()
        .and_then(|h| u32::try_from(h).ok())
        .filter(|h| (144..=4320).contains(h))
        .or_else(|| {
            source
                .pointer("/presentation/quality")
                .and_then(Value::as_str)
                .filter(|s| {
                    s.chars().count() <= 128
                        && !s.chars().any(char::is_control)
                        && !s.contains("://")
                })
                .and_then(height)
        })
        .or_else(|| {
            // A source's reported label can describe a delivered rendition while
            // its filename still names an original encode. Keep field priority;
            // merging every field and picking the highest invents a quality.
            [
                source.get("quality"),
                source.get("name"),
                source.get("filename"),
                source.pointer("/behaviorHints/filename"),
                source.get("title"),
                source.get("description"),
            ]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(|s| height(&clean(&s.chars().take(4096).collect::<String>(), 4096)))
        })
}

/// Keep every distinct release/detail line; the delivery URL is never display evidence.
pub fn source_detail(source: &Value) -> Option<String> {
    let mut lines = Vec::new();
    for value in [
        source.get("title"),
        source.get("description"),
        source.get("filename"),
        source.pointer("/behaviorHints/filename"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    {
        for line in value.lines().take(32) {
            let line = clean(&line.chars().take(4096).collect::<String>(), 1024);
            if !line.is_empty() && !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    let detail: String = lines.join("\n").chars().take(4096).collect();
    (!detail.is_empty()).then_some(detail)
}

impl SourcePresentation {
    /// Structured declarations take precedence; missing fields use bounded release hints.
    /// Reported codec/container/quality are informational and never authorize playback.
    pub fn from_stream(source: &Value) -> Option<Self> {
        let mut value = Self::read(&source["presentation"]).unwrap_or_default();
        let words = hints(source).replace('_', " ");
        if value.quality.is_none() {
            value.quality = source_quality_height(source).map(|h| {
                if h == 2160 {
                    "4K".into()
                } else {
                    format!("{h}p")
                }
            });
        }
        if value.video_codec.is_none() {
            value.video_codec = pattern!(
                r"(?i)(?:^|[^a-z0-9])(h[ ._-]?26[45]|x26[45]|avc|hevc|av1|vp9|mpeg[ ._-]?2)\b"
            )
            .captures(&words)
            .map(|m| {
                match m[1]
                    .to_ascii_lowercase()
                    .replace(['.', ' ', '-', '_'], "")
                    .as_str()
                {
                    "h264" | "x264" | "avc" => "AVC",
                    "h265" | "x265" | "hevc" => "HEVC",
                    "av1" => "AV1",
                    "vp9" => "VP9",
                    _ => "MPEG-2",
                }
                .into()
            });
        }
        if value.container.is_none() {
            value.container = pattern!(r"(?i)(?:^|[^a-z0-9])(mkv|mp4|webm|m4v|avi|mov|mpg)\b")
                .captures_iter(&words)
                .last()
                .map(|m| m[1].to_ascii_lowercase());
        }
        if value.container.is_none() {
            value.container = ["ts", "mpeg"]
                .into_iter()
                .find(|extension| {
                    words.lines().any(|line| {
                        line.to_ascii_lowercase()
                            .ends_with(&format!(".{extension}"))
                    })
                })
                .map(str::to_owned);
        }
        if value.audio_codecs.is_empty() {
            let audio_words = pattern!(r"(?i)\bdd\+").replace_all(&words, "DDP");
            for m in pattern!(r"(?i)(?:^|[^a-z0-9])(e[ ._-]?ac[ ._-]?3|ddp(?:[ ._-]?[257][.]1)?|dd[+]|ac[ ._-]?3|dts(?:[ ._-]?hd(?:[ ._-]?ma)?)?|true[ ._-]?hd|aac|flac|opus|mp3|atmos)\b").captures_iter(&audio_words) {
                let token = m[1].to_ascii_lowercase().replace(['.', ' ', '-', '_'], "");
                let codec = if token.starts_with("eac3") || token.starts_with("ddp") || token == "dd+" { "EAC3" }
                    else if token == "ac3" { "AC3" } else if token.starts_with("dtshd") { "DTS-HD" }
                    else if token == "truehd" { "TrueHD" } else if token == "atmos" { "Atmos" }
                    else { match token.as_str() { "aac" => "AAC", "flac" => "FLAC", "opus" => "Opus", "mp3" => "MP3", _ => "DTS" } };
                if !value.audio_codecs.iter().any(|v| v == codec) { value.audio_codecs.push(codec.into()); }
            }
        }
        if value.size_bytes.is_none() {
            value.size_bytes = source["size_bytes"]
                .as_u64()
                .or_else(|| {
                    source
                        .pointer("/behaviorHints/videoSize")
                        .and_then(Value::as_u64)
                })
                .filter(|n| *n > 0 && *n <= (1_u64 << 50));
        }
        if value.size_label.is_none() && value.size_bytes.is_none() {
            value.size_label = pattern!(r"(?i)(?:^|[^a-z0-9])([0-9]{1,5}(?:[.,][0-9]{1,3})?)\s*(kib|mib|gib|tib|kb|mb|gb|tb)\b")
                .captures(&words).map(|m| format!("{} {}", m[1].replace(',', "."), m[2].to_ascii_uppercase()));
        }
        // Provider identity comes only from a host/configured producer, never release text.
        let present = value.provider_name.is_some()
            || value.source_id.is_some()
            || value.delivery_format.is_some()
            || value.container.is_some()
            || value.quality.is_some()
            || value.video_codec.is_some()
            || !value.audio_codecs.is_empty()
            || value.size_bytes.is_some()
            || value.size_label.is_some();
        present.then_some(value)
    }
}

/// Provider language reports and release hints; never a probed or selected audio track.
pub fn source_audio_languages(source: &Value) -> Vec<String> {
    let mut explicit = Vec::new();
    for key in [
        "audio_languages",
        "reported_languages",
        "language",
        "languages",
    ] {
        match &source[key] {
            Value::String(s) if !s.trim().is_empty() => explicit.push(s.as_str()),
            Value::Array(values) => explicit.extend(
                values
                    .iter()
                    .take(16)
                    .filter_map(Value::as_str)
                    .filter(|s| !s.trim().is_empty()),
            ),
            _ => {}
        }
        if !explicit.is_empty() {
            break;
        }
    }
    if !explicit.is_empty() {
        let mut output = Vec::<String>::new();
        for value in explicit.into_iter().take(16) {
            // Human-readable reports are accepted only as exact known names.
            // Arbitrary prose is never split into potential language tags.
            let value = match value.to_ascii_lowercase().as_str() {
                "english" => "en",
                "spanish" => "es",
                "french" => "fr",
                "german" => "de",
                "italian" => "it",
                "portuguese" => "pt",
                "japanese" => "ja",
                "korean" => "ko",
                "chinese" => "zh",
                "hindi" => "hi",
                "russian" => "ru",
                "arabic" => "ar",
                "dutch" => "nl",
                "polish" => "pl",
                "turkish" => "tr",
                "swedish" => "sv",
                "danish" => "da",
                "norwegian" => "no",
                "finnish" => "fi",
                "ukrainian" => "uk",
                _ => value,
            };
            if !crate::preferences::language_tag(value) {
                continue;
            }
            // Preserve every validated declared tag, including regions/scripts and
            // languages outside our finite release-hint vocabulary. Normalize only
            // known ISO-639-2 aliases; never split a reported tag into loose words.
            let (primary, suffix) = value
                .split_once('-')
                .map_or((value, None), |(primary, suffix)| (primary, Some(suffix)));
            let primary = match primary.to_ascii_lowercase().as_str() {
                "eng" => "en",
                "spa" => "es",
                "fra" | "fre" => "fr",
                "deu" | "ger" => "de",
                "ita" => "it",
                "por" => "pt",
                "jpn" => "ja",
                "kor" => "ko",
                "zho" | "chi" => "zh",
                "hin" => "hi",
                "rus" => "ru",
                "ara" => "ar",
                "nld" | "dut" => "nl",
                "pol" => "pl",
                "tur" => "tr",
                "swe" => "sv",
                "dan" => "da",
                "nor" => "no",
                "fin" => "fi",
                "ukr" => "uk",
                _ => primary,
            };
            let tag = suffix.map_or_else(
                || primary.to_owned(),
                |suffix| format!("{primary}-{suffix}"),
            );
            if !output.iter().any(|known| known.eq_ignore_ascii_case(&tag)) {
                output.push(tag);
            }
        }
        return output;
    }
    let declared = false;
    let fallback = hints(source);
    let values = if declared {
        explicit
    } else {
        fallback.lines().collect()
    };
    let mut output = Vec::new();
    for value in values.into_iter().take(32) {
        let text = clean(&value.chars().take(4096).collect::<String>(), 4096).to_lowercase();
        // Torrentio's `Multi Subs` label is subtitle evidence inside an audio
        // language line (`Multi Subs / 🇬🇧`); drop the label, keep the line.
        let text = pattern!(r"\bmulti(?:ple)?[ ._-]*(?:subs?|subtitles?)\b")
            .replace_all(&text, " ")
            .into_owned();
        if !declared
            && pattern!(r"\b(?:subtitles?|subs?|subbed|captions?)\b").is_match(&text)
            && !pattern!(r"\b(?:audio|dub|dubbed)\b").is_match(&text)
        {
            continue;
        }
        let text =
            pattern!(r"\b[a-z]+[ ._:-]+(?:subtitles?|subs?|subbed|captions?)\b").replace_all(&text, "");
        for token in text.split(|c: char| !c.is_alphabetic()) {
            if !declared && token.len() <= 2 {
                continue;
            }
            let language = match token {
                "english" | "eng" | "en" => "en",
                "spanish" | "spa" | "es" => "es",
                "french" | "fra" | "fre" | "fr" => "fr",
                "german" | "deu" | "ger" | "de" => "de",
                "italian" | "ita" | "it" => "it",
                "portuguese" | "por" | "pt" => "pt",
                "japanese" | "jpn" | "ja" => "ja",
                "korean" | "kor" | "ko" => "ko",
                "chinese" | "zho" | "chi" | "zh" => "zh",
                "hindi" | "hin" | "hi" => "hi",
                "russian" | "rus" | "ru" => "ru",
                "arabic" | "ara" | "ar" => "ar",
                "dutch" | "nld" | "dut" | "nl" => "nl",
                "polish" | "pol" | "pl" => "pl",
                "turkish" | "tur" | "tr" => "tr",
                "swedish" | "swe" | "sv" => "sv",
                "danish" | "dan" | "da" => "da",
                "norwegian" | "nor" | "no" => "no",
                "finnish" | "fin" | "fi" => "fi",
                "ukrainian" | "ukr" | "uk" => "uk",
                _ => continue,
            };
            if !output.iter().any(|s| s == language) && output.len() < 16 {
                output.push(language.to_owned());
            }
        }
        if !declared {
            for (flag, language) in [
                ("🇬🇧", "en"),
                ("🇺🇸", "en"),
                ("🇮🇹", "it"),
                ("🇫🇷", "fr"),
                ("🇩🇪", "de"),
                ("🇪🇸", "es"),
                ("🇵🇹", "pt"),
                ("🇧🇷", "pt"),
                ("🇯🇵", "ja"),
                ("🇰🇷", "ko"),
                ("🇨🇳", "zh"),
                ("🇮🇳", "hi"),
                ("🇷🇺", "ru"),
                ("🇺🇦", "uk"),
            ] {
                if text.contains(flag) && !output.iter().any(|s| s == language) && output.len() < 16
                {
                    output.push(language.to_owned());
                }
            }
        }
    }
    // VOSTFR/SUBFRENCH is original audio with French subtitles. Indexers
    // (PTT, therefore Torrentio's 🇫🇷) still list French; that is not audio.
    if output.iter().any(|language| language == "fr")
        && pattern!(r"(?i)(?:^|[^a-z0-9])(?:vostfr|subfrench)(?:$|[^a-z0-9])").is_match(&fallback)
        && !pattern!(r"(?i)(?:^|[^a-z0-9])(?:vff?|vfq|vfi|vf2|truefrench|french)(?:$|[^a-z0-9])")
            .is_match(&fallback)
    {
        output.retain(|language| language != "fr");
    }
    output
}
