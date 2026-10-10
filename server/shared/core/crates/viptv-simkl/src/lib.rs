//! SIMKL wire normalization and stream identity mapping. No networking unless `http` is enabled.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Movie,
    Tv,
    Anime,
}
impl Category {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::Movie => "movies",
            Self::Tv => "tv",
            Self::Anime => "anime",
        }
    }
    pub fn kind(&self) -> &'static str {
        if *self == Self::Movie {
            "movie"
        } else {
            "series"
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub category: Category,
    pub simkl_id: u64,
    pub ids: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveryCapabilities {
    pub full_search: bool,
    pub coverage: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybackEvent {
    pub event_id: String,
    pub session_id: String,
    pub action: String,
    pub item: Value,
    pub position: f64,
    pub duration: f64,
}
pub const STATUSES: &[&str] = &["watching", "plantowatch", "completed", "hold", "dropped"];
pub fn parse_id(id: &str) -> Option<(Category, u64)> {
    let mut parts = id.split(':');
    if parts.next()? != "simkl" {
        return None;
    }
    let category = match parts.next()? {
        "movies" | "movie" => Category::Movie,
        "tv" => Category::Tv,
        "anime" => Category::Anime,
        _ => return None,
    };
    let number = parts.next()?.parse().ok()?;
    (number > 0).then_some((category, number))
}
pub fn identifier(v: &Value) -> Option<u64> {
    let x = v.get("simkl").or_else(|| v.get("simkl_id"))?;
    x.as_u64()
        .or_else(|| x.as_str()?.parse().ok())
        .filter(|n| *n > 0)
}
pub fn image(fragment: &Value, kind: &str) -> Value {
    let Some(s) = fragment.as_str().filter(|s| !s.is_empty()) else {
        return Value::Null;
    };
    if s.starts_with("https://") {
        return json!(s);
    }
    let (folder, suffix) = match kind {
        "fanart" => ("fanart", "_medium.webp"),
        "episode" => ("episodes", "_w.webp"),
        _ => ("posters", "_m.webp"),
    };
    json!(format!("https://simkl.in/{folder}/{s}{suffix}"))
}
pub fn normalize(v: &Value, category: Category) -> Option<Value> {
    let id = identifier(&v["ids"])?;
    let canonical = format!("simkl:{}:{id}", category.endpoint());
    let mut ids = v["ids"].as_object().cloned().unwrap_or_default();
    ids.remove("simkl_id");
    ids.remove("slug");
    ids.insert("simkl".into(), json!(id));
    let minutes = runtime_minutes(&v["runtime"]);
    let runtime = minutes.map(|n| n * 60.0);
    let released = v
        .get("first_aired")
        .or_else(|| v.get("released"))
        .or_else(|| v.get("release_date"))
        .or_else(|| v.get("date"))
        .cloned()
        .unwrap_or(Value::Null);
    let year = v["year"].as_u64().or_else(|| {
        released.as_str().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| chrono::Datelike::year(&d) as u64)
                .or_else(|| {
                    chrono::NaiveDate::parse_from_str(s, "%m/%d/%Y")
                        .ok()
                        .map(|d| chrono::Datelike::year(&d) as u64)
                })
        })
    });
    let link = format!(
        "https://simkl.com/{}/{}/{}",
        category.endpoint(),
        id,
        v["ids"]["slug"].as_str().unwrap_or("")
    );
    Some(
        json!({"id":canonical,"type":category.kind(),"name":v["title"],"year":year,
        "poster":image(&v["poster"],"poster"),"background":image(&v["fanart"],"fanart"),
        "description":v.get("overview").unwrap_or(&Value::Null),"genres":v["genres"],
        "runtime":minutes,"duration":runtime,"imdbRating":v["ratings"]["imdb"]["rating"],
        "simkl_category":category,"simkl_ids":ids,"simkl_url":link,
        "ratings":v["ratings"],"rank":v["rank"],"status":v["status"],
        "last_aired":v["last_aired"],"released":released,
        "contentRating":v["certification"],"trailers":v["trailers"],"network":v["network"],
        "metadata_source":"simkl"}),
    )
}
pub fn episode(parent: &Value, v: &Value) -> Option<Value> {
    let mut out = parent.clone();
    let season = v["season"].as_u64().unwrap_or(1);
    let number = v["episode"].as_u64()?;
    out["series_id"] = parent["id"].clone();
    out["id"] = json!(format!("{}:{season}:{number}", parent["id"].as_str()?));
    out["season"] = json!(season);
    out["episode"] = json!(number);
    out["episode_title"] = v["title"].clone();
    out["title"] = v["title"].clone();
    out["overview"] = v["description"].clone();
    out["released"] = v["date"].clone();
    out["thumbnail"] = image(&v["img"], "episode");
    out["simkl_episode_ids"] = v["ids"].clone();
    out["tvdb"] = v["tvdb"].clone();
    if let Some(minutes) = runtime_minutes(&v["runtime"]) {
        out["duration"] = json!(minutes * 60.0);
        out["runtime"] = json!(minutes);
    }
    Some(out)
}
fn runtime_minutes(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64().filter(|n| *n > 0.0 && n.is_finite()) {
        return Some(n);
    }
    let text = v.as_str()?;
    let mut minutes = 0.0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
        } else if c == 'h' || c == 'm' {
            let n = number.parse::<f64>().ok()?;
            minutes += n * if c == 'h' { 60.0 } else { 1.0 };
            number.clear();
        }
    }
    if !number.is_empty() {
        minutes += number.parse::<f64>().ok()?;
    }
    (minutes > 0.0 && minutes.is_finite()).then_some(minutes)
}
/// Stream IDs are IMDb plus exact S/E coordinates. Never guess anime offsets.
pub fn stream_id(item: &Value) -> Result<String, &'static str> {
    let ids = &item["simkl_ids"];
    let imdb = ids["imdb"]
        .as_str()
        .filter(|s| s.starts_with("tt") && s[2..].chars().all(|c| c.is_ascii_digit()))
        .ok_or("SIMKL stream mapping unavailable")?;
    if item["episode"].is_null() {
        return Ok(imdb.into());
    }
    let (season, episode) = if item["simkl_category"] == "anime" {
        (
            item["tvdb"]["season"].as_u64(),
            item["tvdb"]["episode"].as_u64(),
        )
    } else {
        (item["season"].as_u64(), item["episode"].as_u64())
    };
    let (s, e) = season
        .zip(episode)
        .ok_or("SIMKL episode mapping unavailable")?;
    Ok(format!("{imdb}:{s}:{e}"))
}
pub fn write_item(item: &Value) -> Result<Value, &'static str> {
    let mut ids = item["simkl_ids"].as_object().cloned().unwrap_or_default();
    if let Some((_, id)) = parse_id(item["id"].as_str().unwrap_or("")) {
        ids.insert("simkl".into(), json!(id));
    }
    let legacy = item["id"].as_str().unwrap_or("");
    if ids.is_empty() && legacy.starts_with("tt") {
        ids.insert(
            "imdb".into(),
            json!(legacy.split(':').next().unwrap_or(legacy)),
        );
    }
    if ids.is_empty() {
        return Err("SIMKL identity unavailable");
    }
    let obj = json!({"ids":ids,"title":item["name"],"year":item["year"]});
    if item["type"] == "movie" {
        return Ok(json!({"movie":obj}));
    }
    if item["simkl_category"] == "anime" {
        let mut out = json!({"anime":obj});
        if !item["episode"].is_null() {
            out["episode"] = json!({"number":item["episode"]});
        }
        return Ok(out);
    }
    let mut out = json!({"show":obj});
    if !item["episode"].is_null() {
        out["episode"] = json!({"season":item["season"],"number":item["episode"]});
    }
    Ok(out)
}

#[cfg(feature = "http")]
pub mod http;
