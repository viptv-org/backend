use super::*;

const MOVIE_RATINGS: [&str; 5] = ["G", "PG", "PG-13", "R", "NC-17"];
const SERIES_RATINGS: [&str; 6] = ["TV-Y", "TV-Y7", "TV-G", "TV-PG", "TV-14", "TV-MA"];

fn chosen(value: Option<&Value>, current: &Value) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(current.as_str().map(str::to_owned)),
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_uppercase())),
        Some(_) => Err("Rating limit must be a rating label".into()),
    }
}
fn rating(
    value: Option<&Value>,
    current: &Value,
    allowed: &[&str],
    label: &str,
) -> Result<Option<(String, i64)>, ApiError> {
    let Some(rating) = chosen(value, current)? else {
        return Ok(None);
    };
    let mut age = None;
    for candidate in allowed {
        if *candidate == rating.as_str() {
            age = metadata::known_age(candidate);
            break;
        }
    }
    let age = age.ok_or_else(|| {
        ApiError::from(format!(
            "{label} rating limit must be one of {}",
            allowed.join(", ")
        ))
    })?;
    Ok(Some((rating, age)))
}
pub(crate) fn movie(
    value: Option<&Value>,
    current: &Value,
) -> Result<Option<(String, i64)>, ApiError> {
    rating(value, current, &MOVIE_RATINGS, "Movie")
}
pub(crate) fn series(
    value: Option<&Value>,
    current: &Value,
) -> Result<Option<(String, i64)>, ApiError> {
    rating(value, current, &SERIES_RATINGS, "Series")
}
fn labels(allowed: &[&str]) -> Value {
    Value::Array(
        allowed
            .iter()
            .filter_map(|label| {
                metadata::known_age(label)
                    .map(|age| json!({"label":label,"age":age}))
            })
            .collect(),
    )
}
pub(crate) fn mapping() -> Value {
    json!({"movie":labels(&MOVIE_RATINGS),"series":labels(&SERIES_RATINGS)})
}
pub(crate) fn ceiling(kind: &str) -> String {
    format!("(CASE {kind} WHEN 'movie' THEN COALESCE(k.movie_age,k.max_age) ELSE COALESCE(k.series_age,k.max_age) END)")
}
pub(crate) fn unrated_sql() -> &'static str {
    "(CASE k.allow_unrated WHEN 1 THEN 1 ELSE (m.conflict=0 AND root.conflict=0) END)"
}
pub(crate) fn age_sql(ceiling: &str) -> String {
    format!("(CASE k.allow_unrated WHEN 1 THEN ((m.age IS NULL OR m.age<={ceiling}) AND (root.age IS NULL OR root.age<={ceiling})) ELSE (m.age<={ceiling} AND root.age<={ceiling}) END)")
}