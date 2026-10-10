use super::*;
use regex::Regex;
use std::sync::LazyLock;

/// Preferred audio-language codes and the alias alternation matched in source
/// labels. The final entry (no codes) is the English default.
const AUDIO_LANGUAGES: [(&[&str], &str); 11] = [
    (&["es", "spa"], "spanish|spa|es"),
    (&["fr", "fre", "fra"], "french|fre|fra|fr"),
    (&["de", "deu", "ger"], "german|ger|deu|de"),
    (&["it"], "italian|ita|it"),
    (&["pt", "por"], "portuguese|por|pt"),
    (&["ja", "jpn"], "japanese|jpn|ja"),
    (&["ko"], "korean|kor|ko"),
    (&["zh"], "chinese|zho|chi|zh"),
    (&["hi"], "hindi|hin|hi"),
    (&["ar"], "arabic|ara|ar"),
    (&[], "english|eng|en"),
];

/// Audio-language patterns for one alias set, compiled once per process.
struct LanguagePatterns {
    english: bool,
    /// `<language> subtitles` labels, stripped before looking for audio evidence.
    subtitle_label: Regex,
    mentioned: Regex,
    explicit: Regex,
    reported: Regex,
}

static LANGUAGE_PATTERNS: LazyLock<Vec<LanguagePatterns>> = LazyLock::new(|| {
    let compile = |pattern: String| Regex::new(&pattern).expect("policy regex");
    AUDIO_LANGUAGES
        .iter()
        .map(|(codes, aliases)| LanguagePatterns {
            english: codes.is_empty(),
            subtitle_label: compile(format!(
                "(?:{aliases})[ ._:-]+(?:subtitles?|subs?|captions?)"
            )),
            mentioned: compile(format!("(^|[^a-z])(?:{aliases})([^a-z]|$)")),
            explicit: compile(format!(
                "(^|[^a-z])(?:(?:{aliases})[ ._:-]+(?:audio|dubbed|dub)|(?:audio|dubbed|dub)[ ._:-]+(?:{aliases}))([^a-z]|$)"
            )),
            reported: compile(format!("(?i)^(?:{aliases})(?:[-_].*)?$")),
        })
        .collect()
});

fn language_patterns(language: &str) -> &'static LanguagePatterns {
    let index = AUDIO_LANGUAGES
        .iter()
        .position(|(codes, _)| codes.contains(&language))
        .unwrap_or(AUDIO_LANGUAGES.len() - 1);
    &LANGUAGE_PATTERNS[index]
}

pub(super) fn auto_next(v: &Value) -> Value {
    json!(
        v["playing"] == true
            && v["seeking"] != true
            && v["nextAvailable"] == true
            && v["autoplay"] == true
            && matches!(text(v, "type"), "series" | "episode")
            && num(v, "duration") > 10.0
            && num(v, "position") > 0.0
            && num(v, "duration") - num(v, "position") <= 10.0
    )
}

pub(super) fn source_display(v: &Value) -> Value {
    {
        let opaque = |s: &str| policy_regex!(r"^[A-Za-z0-9._-]+:[0-9]+$").is_match(s);
        let name = if text(v, "sourceName").trim().is_empty() {
            text(v, "name")
        } else {
            text(v, "sourceName")
        };
        let mut parts = Vec::new();
        for key in ["title", "description", "filename"] {
            let value = text(v, key);
            if !value.trim().is_empty() && !parts.contains(&value) {
                parts.push(value);
            }
        }
        let description = parts.join("\n");
        let description = description.as_str();
        let provider = text(v, "provider");
        let title = if !name.trim().is_empty() && !opaque(name) {
            name.to_owned()
        } else if !description.trim().is_empty() {
            description
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(180)
                .collect()
        } else if !provider.trim().is_empty() && !opaque(provider) {
            provider.to_owned()
        } else {
            "Source".into()
        };
        let label = title.lines().next().unwrap_or("Source").trim();
        let addon = text(v, "sourceAddonId").trim();
        let key = if !addon.is_empty() {
            format!("addon:{addon}")
        } else if !provider.trim().is_empty() {
            format!("provider:{}", provider.trim())
        } else {
            format!("label:{label}")
        };
        json!({"title":title,"body":if !description.trim().is_empty(){description}else if !opaque(provider){provider}else{""},
            "providerKey":key,"providerLabel":label})
    }
}

pub(super) fn source_identity(v: &Value) -> Value {
    {
        let addon = text(v, "addonId");
        let fingerprint = text(v, "fingerprint");
        if addon.is_empty() || fingerprint.is_empty() {
            Value::Null
        } else {
            json!(format!("{addon}\u{0000}{fingerprint}"))
        }
    }
}

pub(super) fn continuation_source(v: &Value) -> Result {
    Ok({
        let sources = v["sources"].as_array().ok_or(CoreError::InvalidInput)?;
        let provider = text(&v["current"], "sourceAddonId");
        let provider = if provider.is_empty() {
            text(v, "outgoingAddonId")
        } else {
            provider
        };
        let found = if !v["current"].is_object() {
            let expected = if provider.is_empty() {
                text(v, "nextAddonId")
            } else {
                provider
            };
            sources
                .iter()
                .find(|s| !expected.is_empty() && text(s, "sourceAddonId") == expected)
        } else if provider.starts_with("iptv:") {
            sources
                .iter()
                .find(|s| text(s, "sourceAddonId") == provider)
        } else {
            let (caps, prefs) = (&v["capabilities"], &v["preferences"]);
            let addons = sources
                .iter()
                .filter(|s| text(s, "sourceAddonId").starts_with("addon:"))
                .map(|s| (s, source_fit(s, caps, prefs)))
                .collect::<Vec<_>>();
            let reference = reference_height(caps, || addons.iter().map(|(_, fit)| *fit));
            addons
                .iter()
                .min_by(|(_, a), (_, b)| a.rank(reference).total_cmp(&b.rank(reference)))
                .map(|(s, _)| *s)
        };
        found
            .map(|s| {
                if v["current"].is_object() {
                    s.clone()
                } else {
                    s["id"].clone()
                }
            })
            .unwrap_or(Value::Null)
    })
}

/// Ranks one source. Optional `candidates` are the sources it is compared
/// with; they only set the quality reference when the device reports no
/// `maxHeight` (see [`reference_height`]).
pub(super) fn source_match(v: &Value) -> Value {
    let (caps, prefs) = (&v["capabilities"], &v["preferences"]);
    let fit = source_fit(&v["source"], caps, prefs);
    let reference = reference_height(caps, || {
        v["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| source_fit(s, caps, prefs))
            .chain([fit])
    });
    json!({
        "rank": fit.rank(reference),
        "likely": fit.likely,
        "best": fit.likely && fit.score >= 4.0 && fit.resolution == reference,
    })
}

/// Calculate each label fit once and compare against one shared quality reference.
pub(super) fn source_ranks(v: &Value) -> Value {
    let (caps, prefs) = (&v["capabilities"], &v["preferences"]);
    let fits: Vec<_> = v["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|source| source_fit(source, caps, prefs))
        .collect();
    let reference = reference_height(caps, || fits.iter().copied());
    let ranks: Vec<_> = fits
        .iter()
        .map(|fit| {
            json!({"rank":fit.rank(reference),"likely":fit.likely,
        "best":fit.likely && fit.score>=4.0 && fit.resolution==reference})
        })
        .collect();
    let mut order: Vec<_> = (0..fits.len()).collect();
    order.sort_by(|a, b| {
        fits[*a]
            .rank(reference)
            .total_cmp(&fits[*b].rank(reference))
    });
    json!({"ranks":ranks,"orderedIndices":order})
}

/// Join names only to observed producers; configured catalog-only addons add no choice.
pub(super) fn source_producer_labels(v: &Value) -> Value {
    let addons = v["addons"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let sources = v["sources"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let displays: Vec<_> = sources.iter().map(source_display).collect();
    let outcomes: Vec<_> = v["observed"].as_array().into_iter().flatten().filter_map(|producer| {
        let id = text(producer,"sourceId");
        let (kind, number) = id.split_once(':')?;
        if id.len()>128 || !matches!(kind,"addon"|"iptv") || number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let key = source_display(&json!({"sourceAddonId":id}))["providerKey"].clone();
        let configured = addons.iter().rev().find(|addon| format!("addon:{}",text(addon,"id"))==id)
            .and_then(|addon| addon["name"].as_str());
        let row = displays.iter().find(|display| display["providerKey"]==key)
            .and_then(|display| display["providerLabel"].as_str());
        let label: String = configured.or(row).or_else(|| producer["label"].as_str()).unwrap_or(id).chars().take(180).collect();
        let failed = producer["errorCode"].is_string() || producer["errorMessage"].is_string();
        let error = crate::domain::api_error(&json!({"status":502,"error":producer["errorMessage"],"error_code":producer["errorCode"]}));
        Some(json!({"sourceId":id,"label":label,"errorCode":if failed {error["code"].clone()} else {Value::Null},
            "errorMessage":if failed {error["message"].clone()} else {Value::Null}}))
    }).take(256).collect();
    json!(outcomes)
}

/// What one source label says about its audio language and playability.
#[derive(Clone, Copy)]
struct SourceFit {
    /// Preferred-audio-language evidence, 0 to 9.
    score: f64,
    /// Labelled vertical resolution, or 0 when unknown.
    resolution: f64,
    /// The label suggests this device can play it directly.
    likely: bool,
}

impl SourceFit {
    fn rank(self, reference: f64) -> f64 {
        let quality = if self.likely && self.resolution == reference {
            0.0
        } else if self.likely {
            1.0
        } else {
            5.0
        };
        (if self.likely { 0.0 } else { 100.0 }) + (9.0 - self.score) * 10.0 + quality
    }
}

/// The measured device height limit. Profile quality caps were retired
/// (design BACKEND_V2.md); only actual decoder/display limits remain.
fn device_max_height(caps: &Value) -> Option<f64> {
    caps["maxHeight"].as_f64().filter(|n| *n > 0.0)
}

/// The resolution a likely source must have to rank as the best quality: the
/// device `maxHeight` when reported, otherwise the highest resolution among
/// the compared sources that the device is likely to play. Without a limit
/// there is no fixed target, so the best quality actually on offer wins.
fn reference_height<I: IntoIterator<Item = SourceFit>>(
    caps: &Value,
    compared: impl FnOnce() -> I,
) -> f64 {
    device_max_height(caps).unwrap_or_else(|| {
        compared()
            .into_iter()
            .filter(|fit| fit.likely)
            .fold(0.0, |high, fit| f64::max(high, fit.resolution))
    })
}

fn source_fit(s: &Value, caps: &Value, prefs: &Value) -> SourceFit {
    let height = device_max_height(caps).unwrap_or(f64::INFINITY);
    let words = [
        text(s, "name"),
        text(s, "title"),
        text(s, "filename"),
        text(s, "audio"),
        text(&s["raw"], "description"),
    ]
    .join("\n")
    .to_lowercase();
    let language = language_patterns(&text(prefs, "audioLanguage").to_lowercase());
    let english = language.english;
    let mut mentioned = false;
    let mut explicit = false;
    let mut dubbed = false;
    let mut multi = false;
    for line in words.replace(['|', ';'], "\n").lines() {
        let subtitles =
            policy_regex!(r"(^|[^a-z])(?:subtitles?|subs?|captions?)([^a-z]|$)").is_match(line);
        let audio = policy_regex!(r"(^|[^a-z])(?:audio|dubbed|dub)([^a-z]|$)").is_match(line);
        if subtitles && !audio {
            continue;
        }
        let line = language.subtitle_label.replace_all(line, "");
        mentioned |= language.mentioned.is_match(&line);
        explicit |= language.explicit.is_match(&line);
        if english {
            dubbed |= policy_regex!(r"(^|[^a-z])(?:dubbed|dub)([^a-z]|$)").is_match(&line);
            multi |= policy_regex!(r"(^|[^a-z])(?:dual[ ._-]?audio|multi[ ._-]?audio)([^a-z]|$)")
                .is_match(&line);
        }
    }
    let reported = s["raw"]["reported_languages"].as_array().is_some_and(|a| {
        a.iter()
            .any(|x| language.reported.is_match(x.as_str().unwrap_or("")))
    });
    let evidence = if english {
        s["raw"]["audioEvidenceScore"].as_f64()
    } else {
        None
    };
    let score = evidence
        .unwrap_or(
            (i32::from(reported)
                + i32::from(mentioned)
                + 2 * i32::from(multi)
                + 3 * i32::from(dubbed)
                + 4 * i32::from(explicit)) as f64,
        )
        .clamp(0.0, 9.0);
    let mut resolution = 0.0;
    for n in [480, 720, 1080, 2160] {
        if words.contains(&format!("{n}p")) {
            resolution = n as f64;
        }
    }
    if words.contains("4k") {
        resolution = 2160.0;
    }
    let h264 = policy_regex!(r"(^|[^a-z0-9])(?:h\.?264|x264|avc)([^a-z0-9]|$)").is_match(&words);
    let h265 = policy_regex!(r"(^|[^a-z0-9])(?:h\.?265|x265|hevc)([^a-z0-9]|$)").is_match(&words);
    let heavy = policy_regex!(r"(^|[^a-z0-9])(?:hdr|hdr10|dv|10bit|10-bit|hi10p|av1)([^a-z0-9]|$)")
        .is_match(&words);
    let likely = resolution > 0.0
        && resolution <= height
        && ((h264 && caps["h264"] != false) || (h265 && caps["hevcSdr"] == true))
        && !heavy;
    SourceFit {
        score,
        resolution,
        likely,
    }
}
