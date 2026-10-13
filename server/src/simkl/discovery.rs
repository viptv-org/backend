use super::*;
use viptv_engine::presentation::{Card, CatalogPage};

pub(super) type PublicCache = tokio::sync::Mutex<Option<(Instant, Arc<Value>)>>;

impl Service {
    pub(super) async fn trending(
        &self,
        check: impl FnOnce() -> Result<(), ApiError>,
    ) -> Result<Arc<Value>, ApiError> {
        let mut cache = self.trending.lock().await;
        if let Some((_, value)) = cache
            .as_ref()
            .filter(|(created, _)| created.elapsed() < Duration::from_secs(3600))
        {
            return Ok(value.clone());
        }
        check()?;
        // One bounded public document for all Home rows; never send profile tokens to the data CDN.
        let url = url::Url::parse(&format!(
            "{}/discover/trending/today_100.json",
            self.data_base
        ))
        .map_err(|_| unavailable())?;
        let value = crate::source_http::json(
            url,
            4 * 1024 * 1024,
            Duration::from_secs(8),
            cfg!(test),
            false,
        )
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                "Simkl trending is unavailable".into(),
            )
        })?;
        if !value.is_object() {
            return Err("Simkl trending was invalid".into());
        }
        let value = Arc::new(value);
        *cache = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

pub(crate) async fn page(
    app: &App,
    profile: i64,
    category: &str,
    offset: usize,
) -> Result<CatalogPage, ApiError> {
    viptv_engine::simkl::category(category)
        .map_err(|_| ApiError::from("Invalid Simkl category"))?;
    if offset > 100 {
        return Err("Invalid Simkl page".into());
    }
    let access = store::credential(app, profile, Capability::Discovery)?;
    let value = app
        .simkl
        .trending(|| store::validate(app, profile, &access))
        .await?;
    store::validate(app, profile, &access)?;
    let key = if category == "movie" {
        "movies"
    } else {
        category
    };
    let values = value[key]
        .as_array()
        .ok_or_else(|| ApiError::from("Simkl trending category was invalid"))?;
    if values.len() > 100 {
        return Err("Simkl trending exceeds the limit".into());
    }
    let cards: Vec<Card> = values
        .iter()
        .skip(offset)
        .take(24)
        .filter_map(|v| viptv_engine::simkl::card(v, category).ok())
        .collect();
    let more = values.len() > offset + 24;
    Ok(CatalogPage {
        cards: metadata::allowed(app, cards)?,
        next_cursor: more.then(|| (offset + 24).to_string()),
        complete: !more,
    })
}

pub(crate) async fn home_cards(
    app: App,
    profile: i64,
    category: &'static str,
) -> Result<Vec<Card>, String> {
    page(&app, profile, category, 0)
        .await
        .map(|p| p.cards)
        .map_err(|_| "Simkl row unavailable".into())
}
