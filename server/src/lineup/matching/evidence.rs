use super::*;

fn words(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn decorated(word: &str) -> bool {
    matches!(
        word,
        "hd" | "sd"
            | "fhd"
            | "uhd"
            | "4k"
            | "8k"
            | "1080p"
            | "720p"
            | "1080i"
            | "hevc"
            | "h264"
            | "h265"
            | "50fps"
            | "60fps"
    )
}
fn sibling_spelling(value: &str) -> String {
    value
        .replace("hbo 2", "hbo2")
        .replace("fs 1", "fs1")
        .replace("fs 2", "fs2")
}
// Country, language and feed words never identify a channel on their own.
fn generic_word(w: &str) -> bool {
    decorated(w) || matches!(w, "us" | "usa" | "en" | "eng" | "english" | "east" | "west")
}
pub(super) fn normalized(value: &str) -> String {
    sibling_spelling(
        &words(value)
            .into_iter()
            .filter(|w| !decorated(w))
            .collect::<Vec<_>>()
            .join(" "),
    )
}
pub(super) fn input_key(name: &str) -> String {
    sibling_spelling(
        &words(name)
            .into_iter()
            .filter(|w| !generic_word(w))
            .collect::<Vec<_>>()
            .join(" "),
    )
}
pub(super) struct Input {
    pub(super) id: String,
    pub(super) provider: i64,
    pub(super) name: String,
    pub(super) category: String,
    pub(super) epg: String,
    pub(super) source: String,
    pub(super) pool: i64,
    pub(super) group: String,
}
pub(super) struct Evidence {
    pub(super) score: u8,
    pub(super) reason: &'static str,
    pub(super) safe: bool,
}
pub(super) fn evidence(
    channel: &Value,
    input: &Input,
    aliases: &[String],
    verified_id: bool,
    pinned: bool,
) -> Option<Evidence> {
    let tokens = words(&input.name);
    let labels = words(&format!("{} {}", input.name, input.category));
    let has = |word: &str| labels.iter().any(|w| w == word);
    let east = has("east");
    let west = has("west");
    let market = words(channel["market"].as_str().unwrap_or(""));
    let stripped = tokens
        .iter()
        .filter(|w| !generic_word(w) && !market.contains(w))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let stripped = sibling_spelling(&stripped);
    let network = normalized(channel["network"].as_str().unwrap_or(""));
    let exact = stripped == network;
    let alias = aliases.iter().any(|a| a == &stripped);
    let sibling_group = |name: &str| match name {
        "hbo" | "hbo2" => 1,
        "fx" | "fxx" => 2,
        "fs1" | "fs2" => 3,
        _ => 0,
    };
    if stripped != network
        && sibling_group(&network) != 0
        && sibling_group(&network) == sibling_group(&stripped)
    {
        return Some(Evidence {
            score: 0,
            reason: "sibling_network_conflict",
            safe: false,
        });
    }
    // Short names and numbered siblings never use approximate matching.
    let fuzzy = !exact
        && !alias
        && network.len() >= 8
        && stripped.len() >= 8
        && network
            .chars()
            .filter(|c| c.is_numeric())
            .eq(stripped.chars().filter(|c| c.is_numeric()))
        && network.chars().take(4).eq(stripped.chars().take(4))
        && one_edit(&network, &stripped);
    if !pinned && !verified_id && !exact && !alias && !fuzzy {
        return None;
    }
    let foreign = [
        "ca",
        "canada",
        "uk",
        "gb",
        "au",
        "australia",
        "fr",
        "france",
        "de",
        "germany",
        "mx",
        "mexico",
    ]
    .iter()
    .any(|w| has(w));
    let other_language = [
        "es",
        "spa",
        "spanish",
        "espanol",
        "español",
        "français",
        "french",
        "deutsch",
        "german",
        "pt",
        "portuguese",
        "latino",
    ]
    .iter()
    .any(|w| has(w));
    let feed = channel["feed"].as_str().unwrap_or("");
    if foreign
        || other_language
        || (east && west)
        || (east && feed != "east")
        || (west && feed != "west")
    {
        return Some(Evidence {
            score: 0,
            reason: "region_language_or_feed_conflict",
            safe: false,
        });
    }
    if feed == "local" && !market.iter().all(|w| labels.contains(w)) && !pinned && !verified_id {
        return Some(Evidence {
            score: 0,
            reason: "local_market_unverified",
            safe: false,
        });
    }
    let identity_known = (has("us") || has("usa"))
        && (has("en") || has("eng") || has("english"))
        && match feed {
            "east" => east,
            "west" => west,
            "national" => true,
            "local" => market.iter().all(|w| labels.contains(w)),
            _ => false,
        };
    if !identity_known && !pinned && !verified_id {
        return Some(Evidence {
            score: 70,
            reason: "region_language_or_feed_unverified",
            safe: false,
        });
    }
    Some(if pinned {
        Evidence {
            score: 100,
            reason: "owner_pin",
            safe: true,
        }
    } else if verified_id {
        Evidence {
            score: 100,
            reason: "verified_source_id",
            safe: true,
        }
    } else if exact {
        Evidence {
            score: 98,
            reason: "exact_normalized_name",
            safe: true,
        }
    } else if alias {
        Evidence {
            score: 97,
            reason: "owner_alias",
            safe: true,
        }
    } else {
        Evidence {
            score: 90,
            reason: "constrained_name_similarity",
            safe: true,
        }
    })
}
fn one_edit(a: &str, b: &str) -> bool {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            if a.len() >= b.len() {
                i += 1;
            }
            if b.len() >= a.len() {
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

pub(crate) fn compatible_guide(channel: &Value, name: &str, verified: bool) -> bool {
    let input = Input {
        id: String::new(),
        provider: 0,
        name: name.to_owned(),
        category: String::new(),
        epg: String::new(),
        source: String::new(),
        pool: 0,
        group: String::new(),
    };
    evidence(channel, &input, &[], false, verified)
        .is_some_and(|e| e.safe && (verified || e.score >= 95))
}
