//! Normalized release facts (SRC-BEST-MATCH-001): add-on declarations first,
//! then PTT-equivalent release-name rules. Ranking reads only these fields.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

macro_rules! pattern {
    ($pattern:literal) => {{
        static RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("release facts pattern"));
        &*RE
    }};
}

pub const RELEASE_TYPES: [&str; 8] = [
    "web_dl",
    "webrip",
    "bluray",
    "remux",
    "hdtv",
    "dvd",
    "other_rip",
    "cam",
];
pub const DYNAMIC_RANGES: [&str; 5] = ["dv", "hdr10_plus", "hdr10", "hdr", "hlg"];
pub const AUDIO_FLAGS: [&str; 3] = ["dual", "multi", "dubbed"];
pub const DELIVERIES: [&str; 2] = ["direct", "torrent"];

/// Release facts derived from one stream. Delivery is deliberately absent: it
/// comes from the stream transport (`stream_delivery`), never from text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub three_d: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_range: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u32>,
}

/// `torrent` for infoHash, magnet or `.torrent` transports, otherwise `direct`.
pub fn stream_delivery(source: &Value) -> &'static str {
    let torrent = source.get("infoHash").is_some_and(|v| !v.is_null())
        || source["url"].as_str().is_some_and(|url| {
            let lower = url.trim().to_ascii_lowercase();
            lower.starts_with("magnet:")
                || lower
                    .split(['?', '#'])
                    .next()
                    .is_some_and(|path| path.ends_with(".torrent"))
        });
    if torrent { "torrent" } else { "direct" }
}

fn declared_list(value: &Value, allowed: &[&str]) -> Option<Vec<String>> {
    let values = value.as_array()?;
    let mut out: Vec<String> = Vec::new();
    for value in values.iter().take(16) {
        let value = value.as_str()?;
        if !allowed.contains(&value) {
            return None;
        }
        if !out.iter().any(|known| known == value) {
            out.push(value.to_owned());
        }
    }
    // Canonical order keeps equal facts byte-identical across producers.
    out.sort_by_key(|value| allowed.iter().position(|known| known == value));
    Some(out)
}

const STATS: [&str; 3] = ["👤", "💾", "⚙️"];

/// Release-name lines in evidence order: filename, then title, name and
/// description lines. Torrentio's seeders/size/site line is excluded.
fn release_lines(source: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for value in [
        source.get("filename"),
        source.pointer("/behaviorHints/filename"),
        source.get("title"),
        source.get("name"),
        source.get("description"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    {
        for line in value
            .chars()
            .take(4096)
            .collect::<String>()
            .lines()
            .take(32)
        {
            if STATS.iter().any(|marker| line.contains(marker)) {
                continue;
            }
            let line: String = line
                .split_whitespace()
                .filter(|word| {
                    !word.contains("://") && !word.to_ascii_lowercase().contains("magnet:")
                })
                .collect::<Vec<_>>()
                .join(" ")
                .replace('_', " ")
                .chars()
                .filter(|c| !c.is_control())
                .take(1024)
                .collect();
            if !line.is_empty() && !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    lines
}

fn seeders(source: &Value) -> Option<u32> {
    if let Some(n) = source["seeders"].as_u64() {
        return Some(u32::try_from(n).unwrap_or(u32::MAX));
    }
    [
        source.get("title"),
        source.get("description"),
        source.get("name"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find_map(|text| {
        pattern!(r"👤\s*([0-9]{1,9})\b")
            .captures(&text.chars().take(4096).collect::<String>())
            .and_then(|m| m[1].parse::<u32>().ok())
    })
}

/// Bare upper-case `TS`/`TC` are telesync/telecine unless they are a file extension.
fn bare_cam_token(line: &str, token: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(token).any(|(start, _)| {
        let end = start + token.len();
        let before = start.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(end).copied();
        let boundary = |b: Option<u8>| b.is_none_or(|b| !b.is_ascii_alphanumeric());
        boundary(before) && boundary(after) && !(before == Some(b'.') && after.is_none())
    })
}

fn cam(line: &str) -> bool {
    let camera = pattern!(r"(?i)\b(?:H[DQ][ .-]*)?CAM(?:H[DQ])?(?:[ .-]*Rip)?\b")
        .find_iter(line)
        .any(|m| {
            // PTT: not `Cam S01`, `Cam E01` or `Cam (2018)`/`Cam.2018` (a title word).
            !pattern!(r"(?i)^[ .-]*(?:S[0-9]|E[0-9]|\(?(?:19|20)[0-9]{2}\b)")
                .is_match(&line[m.end()..])
        });
    camera
        || pattern!(r"(?i)\b(?:HD[ .-]*)?TELE[ .-]?(?:SYNC|CINE)(?:[ .-]?Rip)?\b|\bHD[ .-]?T[SC]\b|\bT[SC][ .-]?Rip\b|\bP(?:re)?-HD(?:Rip)?\b|\b(?:DVD?|BD|BR|HD)?[ .-]*Scr(?:eener)?\b|\bP(?:re)?DVD(?:Rip)?\b|\bR5\b|\bTELESYNC\b|\bTELECINE\b").is_match(line)
        || bare_cam_token(line, "TS")
        || bare_cam_token(line, "TC")
}

/// PTT source handler order; remux beats BluRay and cameras beat everything.
fn release_type(line: &str) -> Option<&'static str> {
    if cam(line) {
        return Some("cam");
    }
    let rules: [(&Regex, &str); 12] = [
        (pattern!(r"(?i)\bREMUX\b|(?:BD|BR|UHD)[- ]?remux"), "remux"),
        (
            pattern!(
                r"(?i)\bBlu[ .-]*Ray[ .-]*Rip\b|\b(?:BR|BD)[ .-]*Rip\b|\bBDR\b|\bBD-RM\b|[\[(]BD[\]) .,-]"
            ),
            "bluray",
        ),
        (pattern!(r"(?i)\bBlu[ .-]*Ray\b"), "bluray"),
        (pattern!(r"(?i)\bUHD[ .-]*Rip\b"), "other_rip"),
        (
            pattern!(r"(?i)\bHD[ .-]*Rip\b|\bMicro[ .-]*HD\b"),
            "other_rip",
        ),
        (
            pattern!(r"(?i)\b(?:HD[ .-]*)?DVD[ .-]*Rip\b|\bVHS[ .-]*Rip\b"),
            "dvd",
        ),
        (
            pattern!(r"(?i)\bDVD(?:R[0-9]?|[0-9])?\b|\bDVD[ .-]?(?:5|9)\b"),
            "dvd",
        ),
        (
            pattern!(
                r"(?i)\bHD[ .-]*TV(?:Rip)?\b|\bPDTV\b|\bDVB(?:[ .-]*Rip)?\b|\bSAT[ .-]*Rips?\b|\bTV[ .-]?Rips?\b|\bPPV[ .-]?Rip\b"
            ),
            "hdtv",
        ),
        (
            pattern!(r"(?i)\bWEB[ .-]*DL(?:Rip)?\b|\bWEBDL\b|\b(?:WEB|DL)[ .-]?MUX\b"),
            "web_dl",
        ),
        (pattern!(r"(?i)\bWEB[ .-]*Rip\b"), "webrip"),
        (pattern!(r"(?i)\bWEB\b"), "web_dl"),
        (pattern!(r"(?i)\b(?:BD|BR)[ .-]?MUX\b"), "bluray"),
    ];
    // First match wins; WEB-DL/WEBRip precede a bare `WEB`.
    rules
        .iter()
        .find(|(re, _)| re.is_match(line))
        .map(|(_, kind)| *kind)
}

fn dynamic_range(line: &str, out: &mut Vec<&'static str>) {
    let mut push = |value: &'static str| {
        if !out.contains(&value) {
            out.push(value);
        }
    };
    if pattern!(r"(?i)\bDV\b|\bDoVi\b|\bdolby[ .-]?vision\b").is_match(line) {
        push("dv");
    }
    if pattern!(r"(?i)\bHDR10(?:\+|[ .-]?plus\b)").is_match(line) {
        push("hdr10_plus");
    }
    if pattern!(r"(?i)\bHDR10\b").find_iter(line).any(|m| {
        let rest = &line[m.end()..];
        !rest.starts_with('+') && !pattern!(r"(?i)^[ .-]?plus\b").is_match(rest)
    }) {
        push("hdr10");
    }
    if pattern!(r"(?i)\bHDR\b").is_match(line) {
        push("hdr");
    }
    if pattern!(r"(?i)\bHLG\b").is_match(line) {
        push("hlg");
    }
}

fn three_d(line: &str) -> bool {
    pattern!(r"(?i)\b(?:BD)?3D\b").find_iter(line).any(|m| {
        // `3D audio` is spatial audio, not a stereoscopic video release.
        !pattern!(r"(?i)^[ .-]*audio\b").is_match(&line[m.end()..])
    }) || pattern!(r"(?i)\bH(?:alf)?[ .-]?(?:SBS|OU)\b").is_match(line)
}

fn audio_flags(line: &str, out: &mut Vec<&'static str>) {
    let mut push = |value: &'static str| {
        if !out.contains(&value) {
            out.push(value);
        }
    };
    // `Multi Subs`/`Multiple Subtitle` are subtitle evidence only.
    let multi = pattern!(r"(?i)\bmulti(?:ple)?\b")
        .find_iter(line)
        .any(|m| !pattern!(r"(?i)^[ .-]*m?sub").is_match(&line[m.end()..]))
        || pattern!(r"(?i)\btri(?:ple)?[ .-]*(?:audio|dub\w*)\b").is_match(line);
    if multi {
        push("multi");
    }
    let dual = pattern!(r"(?i)\bdual[ .-]*(?:audio|áudio|line|lang\w*)\b").is_match(line)
        || pattern!(r"(?i)\bdual\b")
            .find_iter(line)
            .any(|m| !pattern!(r"(?i)^[ .-]*(?:sub|!)").is_match(&line[m.end()..]));
    if dual {
        push("dual");
    }
    if pattern!(r"(?i)\b(?:dubbed|dublado|dubbing|dubs?)\b").is_match(line) {
        push("dubbed");
    }
}

fn canonical(values: Vec<&'static str>, order: &[&str]) -> Vec<String> {
    let mut values = values;
    values.sort_by_key(|value| order.iter().position(|known| known == value));
    values.into_iter().map(str::to_owned).collect()
}

/// Declared fields (same names as the output) win per field; otherwise the
/// release name decides. Flags never become audio languages.
pub fn source_release_facts(source: &Value) -> ReleaseFacts {
    let lines = release_lines(source);
    let declared_type = source["release_type"]
        .as_str()
        .and_then(|value| RELEASE_TYPES.iter().find(|known| **known == value))
        .map(|value| (*value).to_owned());
    let release_type = declared_type.or_else(|| {
        // A camera marker anywhere wins, as Torrentio's quality label does.
        if lines.iter().any(|line| cam(line)) {
            return Some("cam".into());
        }
        lines
            .iter()
            .find_map(|line| release_type(line))
            .map(str::to_owned)
    });
    let three_d = source["three_d"]
        .as_bool()
        .unwrap_or_else(|| lines.iter().any(|line| three_d(line)));
    let dynamic_range =
        declared_list(&source["dynamic_range"], &DYNAMIC_RANGES).unwrap_or_else(|| {
            let mut out = Vec::new();
            for line in &lines {
                dynamic_range(line, &mut out);
            }
            canonical(out, &DYNAMIC_RANGES)
        });
    let audio_flags = declared_list(&source["audio_flags"], &AUDIO_FLAGS).unwrap_or_else(|| {
        let mut out = Vec::new();
        for line in &lines {
            audio_flags(line, &mut out);
        }
        canonical(out, &AUDIO_FLAGS)
    });
    ReleaseFacts {
        release_type,
        three_d,
        dynamic_range,
        audio_flags,
        seeders: seeders(source),
    }
}
