//! Bounded public CDN browsing with provider-supplied genre choices.
use super::*;
use viptv_engine::{
    presentation::CatalogPage,
    query::{Choice, ControlKind, QueryControl},
};
impl Service {
    pub(super) async fn public_document(
        &self,
        path: &str,
        ttl: Duration,
        check: impl FnOnce() -> Result<(), ApiError>,
    ) -> Result<Arc<Value>, ApiError> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains("..") {
            return Err("Invalid Simkl document".into());
        }
        let mut cache = self.public_documents.lock().await;
        check()?;
        if let Some((_, value)) = cache.get(path).filter(|(time, _)| time.elapsed() < ttl) {
            return Ok(value.clone());
        }
        let config = self.config.as_ref().ok_or_else(unavailable)?;
        let mut url =
            url::Url::parse(&format!("{}{path}", self.data_base)).map_err(|_| unavailable())?;
        url.query_pairs_mut()
            .append_pair("client_id", &config.client_id)
            .append_pair("app-name", "viptv")
            .append_pair("app-version", env!("CARGO_PKG_VERSION"));
        let value = crate::source_http::json(
            url,
            8 * 1024 * 1024,
            Duration::from_secs(8),
            cfg!(test),
            false,
        )
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                "Simkl public data is unavailable".into(),
            )
        })?;
        cache.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(5 * 3600));
        if cache.len() >= 64 {
            cache.clear();
        }
        let value = Arc::new(value);
        cache.insert(path.into(), (Instant::now(), value.clone()));
        Ok(value)
    }
}
fn file_category(category: &str) -> Result<&'static str, ApiError> {
    match category {
        "movie" => Ok("movies"),
        "tv" => Ok("tv"),
        "anime" => Ok("anime"),
        _ => Err("Invalid Simkl category".into()),
    }
}
pub(crate) fn choice(id: &str, label: &str, choices: Vec<Choice>) -> QueryControl {
    QueryControl {
        id: id.into(),
        label: label.into(),
        kind: ControlKind::SingleChoice,
        required: false,
        choices,
        minimum: None,
        maximum: None,
    }
}
pub(crate) async fn controls(
    app: &App,
    profile: i64,
    category: &str,
) -> Result<Vec<QueryControl>, ApiError> {
    let access = store::credential(app, profile, Capability::Discovery)?;
    let category = file_category(category)?;
    let doc = app
        .simkl
        .public_document(
            &format!("/discover/trending/{category}/genre/index.json"),
            Duration::from_secs(24 * 3600),
            || store::validate(app, profile, &access),
        )
        .await?;
    let genres = doc
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or("Simkl genres were invalid")?;
    let mut choices = vec![Choice {
        value: "all".into(),
        label: "All genres".into(),
    }];
    for genre in genres {
        let (Some(slug), Some(name)) = (genre["slug"].as_str(), genre["name"].as_str()) else {
            continue;
        };
        if !slug.is_empty()
            && slug.len() <= 64
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-' || b.is_ascii_digit())
            && name.len() <= 128
            && !name.chars().any(char::is_control)
        {
            choices.push(Choice {
                value: slug.into(),
                label: name.into(),
            });
        }
    }
    store::validate(app, profile, &access)?;
    Ok(vec![
        choice("genre", "Genre", choices),
        choice(
            "timeframe",
            "Simkl Trending",
            [
                ("today", "Today"),
                ("week", "This week"),
                ("month", "This month"),
            ]
            .into_iter()
            .map(|(value, label)| Choice {
                value: value.into(),
                label: label.into(),
            })
            .collect(),
        ),
    ])
}
pub(crate) async fn page(
    app: &App,
    profile: i64,
    category: &str,
    skip: usize,
    genre: Option<&str>,
    extras: Option<&str>,
) -> Result<CatalogPage, ApiError> {
    let category_file = file_category(category)?;
    if extras.is_some_and(|s| s.len() > 8192) {
        return Err("Simkl options exceed the limit".into());
    }
    let extras: std::collections::HashMap<String, String> = extras
        .map(|s| serde_json::from_str(s).map_err(|_| ApiError::from("Invalid Simkl options")))
        .transpose()?
        .unwrap_or_default();
    if extras.len() > 1 || extras.keys().any(|k| k != "timeframe") {
        return Err("Unsupported Simkl catalog options".into());
    }
    let timeframe = extras.get("timeframe").map(String::as_str).unwrap_or(
        if genre.is_some_and(|g| g != "all") {
            "month"
        } else {
            "today"
        },
    );
    if !matches!(timeframe, "today" | "week" | "month") || skip > 500 {
        return Err("Invalid Simkl page".into());
    }
    let access = store::credential(app, profile, Capability::Discovery)?;
    let path = if let Some(genre) = genre.filter(|g| *g != "all") {
        if timeframe != "month" {
            return Err("Simkl genre files cover this month only".into());
        }
        let advertised = controls(app, profile, category).await?;
        if !advertised[0].choices.iter().any(|c| c.value == genre) {
            return Err("Unsupported Simkl genre".into());
        }
        format!("/discover/trending/{category_file}/genre/{genre}_month_500.json")
    } else {
        format!("/discover/trending/{category_file}/{timeframe}_500.json")
    };
    let doc = app
        .simkl
        .public_document(
            &path,
            Duration::from_secs(if timeframe == "today" {
                3600
            } else {
                24 * 3600
            }),
            || store::validate(app, profile, &access),
        )
        .await?;
    let values = doc
        .as_array()
        .filter(|v| v.len() <= 500)
        .ok_or("Simkl catalog was invalid")?;
    let cards = metadata::allowed(
        app,
        values
            .iter()
            .skip(skip)
            .take(24)
            .filter_map(|v| viptv_engine::simkl::card(v, category).ok())
            .collect(),
    )?;
    store::validate(app, profile, &access)?;
    let more = values.len() > skip + 24;
    Ok(CatalogPage {
        cards,
        next_cursor: more.then(|| (skip + 24).to_string()),
        complete: !more,
    })
}
