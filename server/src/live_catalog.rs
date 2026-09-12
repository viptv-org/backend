//! Viewer-facing US guide organization. Provider inventory and IDs remain intact.
use crate::{lineup, util};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::LazyLock};

pub const GROUPS: &[&str] = &[
    "News",
    "Sports",
    "Entertainment",
    "Movies",
    "Premium",
    "Kids",
    "Documentaries",
    "Home & Food",
    "Music",
    "Local",
];
// An intentionally small US English network vocabulary, not a provider playlist.
const NETWORKS: &[(&str, &str)] = &[
 ("News","ABC News Live|CBS News|NBC News Now|CNN|CNBC|CNBC World|MSNBC|MS NOW|Fox News|Fox Business|Newsmax|NewsNation|C SPAN|C SPAN 2|C SPAN 3|Bloomberg|The Weather Channel|WeatherNation|AccuWeather|Fox Weather|HLN"),
 ("Sports","ESPN|ESPN 2|ESPNU|ESPN News|ESPN Deportes|SEC Network|ACC Network|Big Ten Network|CBS Sports Network|Fox Sports 1|Fox Sports 2|Golf Channel|Tennis Channel|NFL Network|NFL RedZone|NBA TV|MLB Network|NHL Network|Olympic Channel|SportsNet New York|YES Network|FanDuel Sports Network"),
 ("Entertainment","ABC|CBS|NBC|Fox|The CW|USA Network|TBS|TNT|FX|FXX|FXM|AMC|A E|Bravo|Comedy Central|E Entertainment|Hallmark Channel|Hallmark Mystery|Lifetime|LMN|Paramount Network|TV Land|Syfy|Freeform|BET|OWN|Oxygen|TruTV|ION|MeTV|Cozi TV|Antenna TV|We TV"),
 ("Movies","TCM|Turner Classic Movies|Sundance TV|IFC|HDNet Movies|Sony Movies|Reelz|Hallmark Family"),
 ("Premium","HBO|HBO 2|HBO Comedy|HBO Signature|HBO Zone|HBO Family|HBO Latino|HBO Hits|HBO Drama|HBO Movies|Cinemax|MoreMax|ActionMax|ThrillerMax|MovieMax|OuterMax|5StarMax|Cinemax Action|Cinemax Classics|Cinemax Hits|Showtime|Showtime 2|Showtime Showcase|Showtime Extreme|Showtime Next|Showtime Women|Showtime Family Zone|The Movie Channel|The Movie Channel Xtra|Flix|Starz|Starz Cinema|Starz Comedy|Starz Edge|Starz Kids Family|Starz In Black|Starz Encore|Starz Encore Action|Starz Encore Classic|Starz Encore Family|Starz Encore Suspense|Starz Encore Westerns|MGM Plus|MGM Plus Hits|MGM Plus Drive In|Epix"),
 ("Kids","Cartoon Network|Boomerang|Nickelodeon|Nick Jr|Nicktoons|TeenNick|Disney Channel|Disney Junior|Disney XD|PBS Kids|Discovery Family|Universal Kids|BabyFirst"),
 ("Documentaries","Discovery Channel|Animal Planet|National Geographic|Nat Geo Wild|History Channel|Science Channel|Smithsonian Channel|Investigation Discovery|American Heroes Channel|FYI|Vice TV|Destination America"),
 ("Home & Food","HGTV|Food Network|Cooking Channel|Travel Channel|TLC|Magnolia Network|MotorTrend"),
 ("Music","MTV|MTV 2|MTV Classic|VH1|CMT|Great American Family"),
];
fn words(s: &str) -> String {
    s.to_lowercase()
        .replace('&', " ")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn has(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(i, _)| {
        (i == 0 || text.as_bytes()[i - 1] == b' ')
            && (i + phrase.len() == text.len() || text.as_bytes()[i + phrase.len()] == b' ')
    })
}
fn prohibited(name: &str, category: &str) -> bool {
    let s = words(&format!("{name} {category}"));
    [
        "adult",
        "xxx",
        "porn",
        "porno",
        "playboy",
        "penthouse",
        "hustler",
        "brazzers",
        "sex",
        "erotic",
        "erotica",
        "18",
        "redlight",
        "vivid",
    ]
    .iter()
    .any(|w| has(&s, w))
}
fn foreign(s: &str) -> bool {
    [
        "uk",
        "united kingdom",
        "canada",
        "ca",
        "can",
        "australia",
        "au",
        "ireland",
        "india",
        "indian",
        "pakistan",
        "arabic",
        "arab",
        "latino",
        "espanol",
        "español",
        "spanish",
        "deportes",
        "mexico",
        "mex",
        "brazil",
        "brasil",
        "argentina",
        "ecuador",
        "colombia",
        "peru",
        "chile",
        "france",
        "french",
        "germany",
        "german",
        "italy",
        "italian",
        "portugal",
        "portuguese",
        "spain",
        "turkey",
        "turkish",
        "russia",
        "russian",
        "poland",
        "polish",
        "africa",
        "african",
        "caribbean",
        "philippines",
        "filipino",
    ]
    .iter()
    .any(|w| has(s, w))
}
/// Only the explicit US decoration is removed; feed markers remain distinct.
pub(crate) fn display_name(name: &str) -> String {
    let text = name.trim();
    if let Some((prefix, rest)) = text.split_once(':') {
        if matches!(prefix.trim().to_ascii_uppercase().as_str(), "USA" | "US") {
            return rest.trim().to_owned();
        }
    }
    text.to_owned()
}
fn foreign_prefix(name: &str) -> bool {
    let text = name.trim().trim_start_matches(['[', '|', '(', '-']);
    let prefix = text
        .split([':', '|', ']', ')', '-'])
        .next()
        .unwrap_or("")
        .trim();
    let upper = prefix.to_ascii_uppercase();
    // A bare network name such as FX or USA is not a country decoration.
    prefix.len() < text.len()
        && (2..=3).contains(&prefix.len())
        && prefix.chars().all(|c| c.is_ascii_alphabetic())
        && !matches!(upper.as_str(), "US" | "USA")
}
// ISO 3166 country names/codes from the pycountry database; common regional
// and language labels are handled separately. Parsed once, never per channel.
fn country_category(text: &str) -> bool {
    static COUNTRIES: LazyLock<(std::collections::HashSet<String>, Vec<String>)> =
        LazyLock::new(|| {
            let data: Value = serde_json::from_str(include_str!("country_names.json"))
                .expect("bundled country data");
            let mut codes = std::collections::HashSet::new();
            let mut names = Vec::new();
            for country in data.as_array().unwrap() {
                for code in country["codes"].as_array().unwrap() {
                    codes.insert(code.as_str().unwrap().to_owned());
                }
                for name in country["names"].as_array().unwrap() {
                    names.push(words(name.as_str().unwrap()));
                }
            }
            (codes, names)
        });
    COUNTRIES
        .0
        .contains(text.split_whitespace().next().unwrap_or(""))
        || COUNTRIES.1.iter().any(|name| has(text, name))
}
pub(crate) fn category_exclusion(category: &str) -> Option<&'static str> {
    if category.trim().is_empty() {
        return None;
    }
    if prohibited("", category) {
        return Some("adult");
    }
    let s = words(category);
    let first = s.split_whitespace().next().unwrap_or("");
    if foreign_prefix(category)
        || foreign(&s)
        || country_category(&s)
        || matches!(
            first,
            "lat"
                | "latam"
                | "latino"
                | "latinos"
                | "latina"
                | "latinoamerica"
                | "international"
                | "world"
        )
        || [
            "hindi",
            "urdu",
            "tamil",
            "telugu",
            "punjabi",
            "bengali",
            "malayalam",
            "mandarin",
            "cantonese",
            "chinese",
            "japanese",
            "korean",
        ]
        .iter()
        .any(|language| has(&s, language))
    {
        return Some("foreign");
    }
    None
}
pub(crate) fn exclusion(name: &str, category: &str) -> Option<&'static str> {
    if let Some(reason) = category_exclusion(category) {
        return Some(reason);
    }
    if prohibited(name, category) {
        return Some("adult");
    }
    if foreign_prefix(name) || foreign(&words(name)) {
        return Some("foreign");
    }
    let cleaned = display_name(name);
    if words(&cleaned).split_whitespace().next() == Some("sd") {
        return Some("sd");
    }
    None
}
fn key(s: &str) -> String {
    let s = words(s);
    if s == "usa" {
        return "usa network".to_owned();
    }
    let tokens = s.split_whitespace().filter(|w| {
        !matches!(
            *w,
            "us" | "usa"
                | "united"
                | "states"
                | "hd"
                | "sd"
                | "fhd"
                | "uhd"
                | "4k"
                | "1080p"
                | "720p"
                | "60fps"
                | "east"
                | "west"
                | "eastern"
                | "western"
                | "backup"
        )
    });
    let raw = tokens.collect::<Vec<_>>().join(" ");
    match raw.as_str() {
        "cn" => "cartoon network",
        "nick" => "nickelodeon",
        "disney" => "disney channel",
        "history" => "history channel",
        "discovery" => "discovery channel",
        "nat geo" => "national geographic",
        "id" => "investigation discovery",
        "usa" => "usa network",
        "fs1" | "fox sports 1" => "fox sports 1",
        "fs2" => "fox sports 2",
        "espn2" => "espn 2",
        "espnews" => "espn news",
        "hbo2" => "hbo 2",
        "shox" => "showtime extreme",
        "shos" => "showtime showcase",
        "tcm" => "turner classic movies",
        "mgm" | "mgm plus" => "mgm plus",
        _ => &raw,
    }
    .to_owned()
}
pub fn classify(name: &str, category: &str) -> Option<(usize, String)> {
    if exclusion(name, category).is_some() {
        return None;
    }
    let normalized = key(name);
    static INDEX: LazyLock<HashMap<String, (usize, &'static str)>> = LazyLock::new(|| {
        let mut index = HashMap::new();
        for (group, names) in NETWORKS {
            for network in names.split('|') {
                index
                    .entry(key(network))
                    .or_insert((GROUPS.iter().position(|v| v == group).unwrap(), network));
            }
        }
        index
    });
    if let Some((rank, network)) = INDEX.get(&normalized) {
        return Some((*rank, (*network).to_owned()));
    }
    let tokens = words(name);
    let local = has(&words(category), "local")
        || has(&words(category), "us")
        || has(&words(category), "usa");
    if local
        && ["abc", "cbs", "nbc", "fox", "pbs", "cw"]
            .iter()
            .any(|w| has(&tokens, w))
        && tokens.split_whitespace().any(|w| {
            w.len() >= 4
                && w.len() <= 6
                && (w.starts_with('w') || w.starts_with('k'))
                && w.chars().all(char::is_alphabetic)
        })
    {
        return Some((9, name.to_owned()));
    }
    None
}
fn value(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_owned()
}

pub fn channels(db: &Connection) -> Result<Vec<Value>, String> {
    let mut rows = if lineup::enabled(db)? {
        lineup::live(db, None, None, 0, 500)?["channels"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        let mut stmt=db.prepare("SELECT l.id,l.name,l.logo,l.category FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE p.enabled=1 AND p.enable_live=1 ORDER BY l.id").map_err(|_|"Live inventory unavailable")?;
        let rows = stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"logo":r.get::<_,Option<String>>(2)?,"category":r.get::<_,Option<String>>(3)?}))).map_err(|_|"Live inventory unavailable")?.collect::<Result<Vec<_>,_>>().map_err(|_|"Live inventory unavailable")?;
        rows
    };
    rows.retain_mut(|row| {
        let name = display_name(&value(row, "name"));
        row["name"] = json!(name);
        let category = value(row, "category");
        let recognized = classify(&name, &category).or_else(|| {
            if value(row, "id").starts_with("family:")
                && !prohibited(&name, &category)
                && !foreign(&words(&name))
            {
                classify(&value(row, "network"), &category)
            } else {
                None
            }
        });
        if let Some((rank, network)) = recognized {
            row["section"] = json!(GROUPS[rank]);
            row["section_rank"] = json!(rank);
            row["network"] = json!(network);
            row["type"] = json!("live");
            crate::live_policy::visible(db, &value(row, "id"))
        } else {
            false
        }
    });
    rows.sort_by_key(|v| {
        (
            v["section_rank"].as_u64().unwrap_or(99),
            words(&value(v, "network")),
            words(&value(v, "name")),
            value(v, "id"),
        )
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row["number"] = json!(index + 1);
    }
    Ok(rows)
}

fn current(db: &Connection, now: i64) -> Result<HashMap<String, Value>, String> {
    let mut found = HashMap::new();
    let mut stmt=db.prepare("SELECT channel_id,data,start,end FROM family_programmes WHERE start<=?1 AND end>?1 ORDER BY start DESC").map_err(|_|"Guide unavailable")?;
    let rows = stmt
        .query_map([now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| "Guide unavailable")?;
    for row in rows {
        let (id, data, start, end) = row.map_err(|_| "Guide unavailable")?;
        if let Ok(mut v) = serde_json::from_str::<Value>(&data) {
            v["start"] = json!(start);
            v["end"] = json!(end);
            found.entry(id).or_insert(v);
        }
    }
    let mut stmt=db.prepare("SELECT l.id,c.payload FROM provider_live l JOIN provider_cache c ON c.provider_id=l.provider_id AND c.cache_key='get_short_epg:'||l.stream_id WHERE c.expires_at>?1").map_err(|_|"Guide cache unavailable")?;
    for row in stmt
        .query_map([now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| "Guide cache unavailable")?
    {
        let (id, data) = row.map_err(|_| "Guide cache unavailable")?;
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        for p in v["epg_listings"].as_array().into_iter().flatten().take(100) {
            let time = |k: &str| p[k].as_i64().or_else(|| p[k].as_str()?.parse().ok());
            let start = time("start_timestamp").unwrap_or(0);
            let end = time("stop_timestamp")
                .or_else(|| time("end_timestamp"))
                .unwrap_or(0);
            if start <= now && end > now {
                use base64::Engine;
                let raw = value(p, "title");
                let title = base64::engine::general_purpose::STANDARD
                    .decode(&raw)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or(raw);
                found
                    .entry(id.clone())
                    .or_insert(json!({"title":title,"start":start,"end":end}));
                break;
            }
        }
    }
    Ok(found)
}

pub fn browse(
    db: &Connection,
    category: Option<&str>,
    search: Option<&str>,
    collection: Option<&str>,
    profile: Option<i64>,
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    let mut rows = channels(db)?;
    let current = current(db, util::now())?;
    if let Some(kind) = collection.filter(|v| matches!(*v, "recent" | "favorites")) {
        let profile = profile.ok_or("Choose a profile")?;
        let table = if kind == "recent" {
            "progress"
        } else {
            "favorites"
        };
        let mut stmt = db
            .prepare(&format!(
                "SELECT id FROM {table} WHERE profile_id=?1 AND type='live' ORDER BY {}",
                if kind == "recent" {
                    "updated_at DESC,id"
                } else {
                    "name,id"
                }
            ))
            .map_err(|_| "Saved channels unavailable")?;
        let ids = stmt
            .query_map([profile], |r| r.get::<_, String>(0))
            .map_err(|_| "Saved channels unavailable")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Saved channels unavailable")?;
        let mut ranks = HashMap::new();
        for (rank, id) in ids.iter().enumerate() {
            let canonical =
                lineup::canonical_id(db, id).map_err(|_| "Saved channels unavailable")?;
            ranks.entry(canonical).or_insert(rank);
        }
        rows.retain(|v| ranks.contains_key(&value(v, "id")));
        if kind == "recent" {
            rows.sort_by_key(|v| ranks[&value(v, "id")]);
        }
    }
    let raw_query = words(search.unwrap_or(""));
    let normalized_query = if matches!(
        raw_query.as_str(),
        "cn" | "nick" | "fs1" | "fs2" | "espn2" | "nat geo" | "tcm"
    ) {
        key(&raw_query)
    } else {
        raw_query
    };
    let query = normalized_query.chars().take(128).collect::<String>();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    rows.retain_mut(|v| {
        if let Some(p) = current.get(&value(v, "id")) {
            if prohibited(&value(p, "title"), "") {
                return false;
            }
            v["now"] = p.clone();
        }
        if category.is_some_and(|g| {
            !g.is_empty() && g != "all" && g.trim_start_matches("section:") != value(v, "section")
        }) {
            return false;
        }
        let channel = words(&format!("{} {}", value(v, "name"), value(v, "network")));
        let group = words(&value(v, "section"));
        let title = words(&value(&v["now"], "title"));
        terms
            .iter()
            .all(|term| channel.contains(term) || group.contains(term) || title.contains(term))
    });
    let total = rows.len();
    Ok(
        json!({"channels":rows.into_iter().skip(offset).take(limit.min(200)).collect::<Vec<_>>(),"total":total,"search_scope":"US channels, sections and currently airing programmes with available guide data"}),
    )
}
pub fn categories(db: &Connection) -> Result<Value, String> {
    let rows = channels(db)?;
    let categories = GROUPS
        .iter()
        .filter_map(|group| {
            let count = rows.iter().filter(|v| v["section"] == *group).count();
            (count > 0).then(|| json!({"id":format!("section:{group}"),"name":group,"count":count}))
        })
        .collect::<Vec<_>>();
    Ok(json!({"total":categories.len(),"categories":categories}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_recognized_us_english_networks_survive() {
        assert_eq!(
            classify("US | CARTOON NETWORK (WEST) FHD", "USA Kids")
                .unwrap()
                .0,
            5
        );
        assert_eq!(classify("CN HD", "US Kids").unwrap().1, "Cartoon Network");
        for (name, group) in [
            ("UK | Cartoon Network", "Kids"),
            ("CNN", "CANADA"),
            ("HBO Latino", "USA"),
            ("ESPN Deportes", "Sports"),
            ("HBO", "XXX"),
            ("Playboy", "USA"),
            ("Unknown Station", "USA"),
            ("Cartoon Network XXX", "Kids"),
        ] {
            assert!(
                classify(name, group).is_none(),
                "must exclude {name} in {group}"
            );
        }
        assert!(classify("NBC WNBC New York", "USA Local").is_some());
        assert!(classify("Cinemax East", "US Movies").is_some());
    }
}
