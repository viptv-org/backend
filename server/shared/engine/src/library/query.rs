use super::*;
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryQuery {
    pub source: String,
    pub offset: u32,
    pub limit: u32,
    pub sort: String,
    pub show: String,
    pub r#type: String,
    #[serde(default, deserialize_with = "optional_filter")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "optional_filter")]
    pub genre: Option<String>,
    #[serde(default, deserialize_with = "optional_filter")]
    pub rated: Option<String>,
    #[serde(default, deserialize_with = "optional_filter")]
    pub added: Option<String>,
    pub episodes_left_max: Option<u64>,
}
fn optional_filter<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.filter(|value| !value.is_empty()))
}
impl Default for LibraryQuery {
    fn default() -> Self {
        Self {
            source: "favorites".into(),
            offset: 0,
            limit: 100,
            sort: "date_added".into(),
            show: "all".into(),
            r#type: "all".into(),
            status: None,
            genre: None,
            rated: None,
            added: None,
            episodes_left_max: None,
        }
    }
}
impl LibraryQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if !matches!(self.source.as_str(), "favorites" | "simkl")
            || self.offset > 10000
            || self.limit == 0
            || self.limit > 100
            || !matches!(
                self.sort.as_str(),
                "last_watched" | "name_asc" | "name_desc" | "times_watched" | "date_added"
            )
            || !matches!(
                self.show.as_str(),
                "all"
                    | "watched"
                    | "not_watched"
                    | "has_new_episode"
                    | "not_started"
                    | "caught_up"
                    | "episodes_to_air"
            )
            || !matches!(self.r#type.as_str(), "all" | "movie" | "series" | "anime")
            || self.status.as_deref().is_some_and(|s| {
                !matches!(
                    s,
                    "watching" | "plantowatch" | "hold" | "completed" | "dropped"
                )
            })
            || self
                .genre
                .as_ref()
                .is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control))
            || self
                .rated
                .as_deref()
                .is_some_and(|s| !matches!(s, "rated_8_plus" | "not_rated"))
            || self
                .added
                .as_deref()
                .is_some_and(|s| !matches!(s, "this_year" | "this_month" | "last_7_days"))
            || self.episodes_left_max.is_some_and(|n| n > 10000)
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}
pub fn added_lower_bound(query: &LibraryQuery, now: u64) -> Option<u64> {
    use chrono::Datelike;
    let date = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(now).ok()?)?
        .date_naive();
    match query.added.as_deref() {
        Some("last_7_days") => Some(now.saturating_sub(7 * 86400000)),
        Some("this_year") => chrono::NaiveDate::from_ymd_opt(date.year(), 1, 1)?
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp_millis() as u64),
        Some("this_month") => chrono::NaiveDate::from_ymd_opt(date.year(), date.month(), 1)?
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp_millis() as u64),
        _ => None,
    }
}
