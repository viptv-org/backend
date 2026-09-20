use super::*;

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
pub(super) fn words(s: &str) -> String {
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
pub(super) fn prohibited(name: &str, category: &str) -> bool {
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
pub(super) fn foreign(s: &str) -> bool {
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
            let data: Value = serde_json::from_str(include_str!("../country_names.json"))
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
pub(super) fn key(s: &str) -> String {
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
