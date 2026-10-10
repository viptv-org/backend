//! Optional artwork only. SIMKL remains the authority for identity, descriptions and tracking.
use super::*;
use std::time::Duration;

impl Service {
    pub(crate) async fn artwork(&self, item: &mut Value) {
        if item["background"].is_string() && item["title_logo"].is_string() { return }
        let id = item["id"].as_str().unwrap_or("");
        let cache_key = format!("artwork:{id}");
        let cached = {
            let db = self.db.lock().unwrap();
            db.query_row("SELECT value FROM simkl_cache WHERE key=?1 AND expires>?2", params![cache_key, util::now()], |row| row.get::<_, String>(0)).optional().ok().flatten()
                .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        };
        let art = if let Some(art) = cached { art } else {
            let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(4)).connect_timeout(Duration::from_secs(2)).build() else { return };
            let ids = &item["simkl_ids"];
            let movie = item["type"] == "movie" || item["anime_type"] == "movie";
            let mut result = json!({});
            if let Ok(key) = std::env::var("FANART_TV_API_KEY") {
                let reference = if movie { ids["tmdb"].as_str().map(str::to_owned).or_else(|| ids["tmdb"].as_u64().map(|id| id.to_string())).or_else(|| ids["imdb"].as_str().map(str::to_owned)) } else { ids["tvdb"].as_str().map(str::to_owned).or_else(|| ids["tvdb"].as_u64().map(|id| id.to_string())) };
                if let Some(reference) = reference.filter(|id| id.chars().all(|ch| ch.is_ascii_alphanumeric())) {
                    let url = format!("https://webservice.fanart.tv/v3/{}/{reference}", if movie { "movies" } else { "tv" });
                    if let Ok(response) = client.get(url).query(&[("api_key",key)]).send().await {
                        if response.status().is_success() { if let Ok(body) = response.json::<Value>().await {
                            for (target, keys) in [("background", vec!["showbackground", "moviebackground"]), ("title_logo", vec!["hdtvlogo", "hdmovielogo", "clearlogo", "movielogo"])] {
                                let image = keys.iter().flat_map(|key| body[*key].as_array().into_iter().flatten())
                                    .filter(|image| image["lang"].as_str().is_none_or(|lang| ["en","00", ""].contains(&lang)))
                                    .max_by_key(|image| image["likes"].as_str().and_then(|value| value.parse::<u32>().ok()).unwrap_or(0));
                                if let Some(url) = image.and_then(|image| image["url"].as_str()).filter(|url| url.starts_with("https://assets.fanart.tv/")) { result[target] = json!(url); }
                            }
                            result["artwork_source"] = json!("Fanart.tv");
                        } }
                    }
                }
            }
            if result["background"].is_null() || result["title_logo"].is_null() {
                if let (Ok(key), Some(reference)) = (std::env::var("TMDB_API_KEY"), ids["tmdb"].as_str().map(str::to_owned).or_else(|| ids["tmdb"].as_u64().map(|id| id.to_string()))) {
                    if reference.chars().all(|ch| ch.is_ascii_digit()) {
                        let url = format!("https://api.themoviedb.org/3/{}/{reference}/images", if movie { "movie" } else { "tv" });
                        if let Ok(response) = client.get(url).query(&[("api_key",key.as_str()),("include_image_language","en,null")]).send().await {
                            if response.status().is_success() { if let Ok(body) = response.json::<Value>().await {
                                for (target, field) in [("background","backdrops"),("title_logo","logos")] {
                                    if result[target].is_null() {
                                        let image = body[field].as_array().into_iter().flatten().filter(|image| image["iso_639_1"].is_null() || image["iso_639_1"] == "en")
                                            .max_by_key(|image| image["vote_count"].as_u64().unwrap_or(0));
                                        if let Some(path) = image.and_then(|image| image["file_path"].as_str()).filter(|path| path.starts_with('/')) { result[target] = json!(format!("https://image.tmdb.org/t/p/original{path}")); }
                                    }
                                }
                                result["artwork_source"] = json!("Fanart.tv / TMDB");
                            } }
                        }
                    }
                }
            }
            let ttl = if result["background"].is_string() || result["title_logo"].is_string() { 604800 } else { 1800 };
            let _ = self.db.lock().unwrap().execute("INSERT INTO simkl_cache VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,expires=excluded.expires", params![cache_key,result.to_string(),util::now()+ttl]);
            result
        };
        for key in ["background", "title_logo"] { if item[key].is_null() && art[key].is_string() { item[key] = art[key].clone(); item["artwork_source"] = art["artwork_source"].clone(); } }
    }
}
