//! One owner of the durable Simkl provider `:` key.
//!
//! Simkl addresses one title through several catalog paths: a movie lives under
//! `/movies/<id>`, a series under `/tv/<id>` and an anime title under
//! `/anime/<id>`. Only the media kind is durable, so a movie read from the
//! anime catalog is the same title as the one read from the movie catalog. The
//! rows written before that rule (and the endpoints that still carry it) keep
//! the old spelling, so every reader normalises it here instead of inventing
//! its own prefix comparison (backend#29).

use std::borrow::Cow;
use viptv_engine::{identity::MediaIdentity, presentation::Card};

/// A catalog path or endpoint name as the media-key namespace it belongs to:
/// `movies` is the movie endpoint, `shows`/`tv_shows` the series ones.
pub(crate) fn category(value: &str) -> Cow<'_, str> {
    match value {
        "movies" => "movie".into(),
        "shows" | "tv_shows" => "tv".into(),
        other => other.into(),
    }
}

/// The durable key of one Simkl media identity. A movie keeps the movie
/// namespace whichever catalog served it (`simkl:movies:` and an
/// `simkl:anime:` movie are `simkl:movie:`); a series keeps its `tv`/`anime`
/// namespace and any `:<season>:<episode>` suffix. Any other identity is
/// opaque and stays exactly as it is.
pub(crate) fn media<'a>(kind: &str, id: &'a str) -> Cow<'a, str> {
    let Some(rest) = id.strip_prefix("simkl:") else {
        return id.into();
    };
    let category = rest.split_once(':').map_or(rest, |(category, _)| category);
    let canonical = match kind {
        "movie" if matches!(category, "movie" | "movies" | "anime") => "movie",
        "series" if matches!(category, "shows" | "tv_shows") => "tv",
        _ => return id.into(),
    };
    // Everything after the namespace: nothing, or the coordinates suffix.
    let suffix = &rest[category.len()..];
    format!("simkl:{canonical}{suffix}").into()
}

/// Every spelling one title has been stored under, the durable key first.
/// Only Simkl movie keys had several catalog paths; anything else is one key.
pub(crate) fn aliases(kind: &str, id: &str) -> Vec<String> {
    let canonical = media(kind, id).into_owned();
    if kind != "movie" || !canonical.starts_with("simkl:movie") {
        return vec![canonical];
    }
    // The coordinates suffix after the namespace (none for a movie, which is
    // the only shape with several catalog paths).
    let suffix = canonical["simkl:movie".len()..].to_owned();
    ["movie", "movies", "anime"]
        .into_iter()
        .map(|category| format!("simkl:{category}{suffix}"))
        .collect()
}

/// Rewrite a media identity in place to its durable key.
pub(crate) fn identity(value: &mut MediaIdentity) {
    let id = media(&value.kind, &value.id).into_owned();
    value.id = id;
}

/// Rewrite a card's identity and parent identity in place to their durable
/// keys. Episodes of a series keep their parent's namespace, so this only ever
/// changes movie cards.
pub(crate) fn card(value: &mut Card) {
    identity(&mut value.identity);
    if let Some(parent) = value.parent_identity.as_mut() {
        identity(parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_movie_key_covers_both_catalogs_and_the_old_store_prefix() {
        for id in ["simkl:movie:1902343", "simkl:movies:1902343", "simkl:anime:1902343"] {
            assert_eq!(media("movie", id), "simkl:movie:1902343");
        }
        assert_eq!(media("movie", "simkl:movie:1902343"), "simkl:movie:1902343");
    }

    #[test]
    fn a_series_keeps_its_namespace_its_episode_and_any_other_identity() {
        assert_eq!(media("series", "simkl:anime:40:2:5"), "simkl:anime:40:2:5");
        assert_eq!(media("series", "simkl:tv:40"), "simkl:tv:40");
        assert_eq!(media("series", "simkl:shows:40"), "simkl:tv:40");
        // A movie row is only rewritten by its own kind: series stay series.
        assert_eq!(media("movie", "simkl:tv:40"), "simkl:tv:40");
        assert_eq!(media("movie", "simkl:anime:40:2:5"), "simkl:movie:40:2:5");
        assert_eq!(media("movie", "tt1902343"), "tt1902343");
        assert_eq!(media("movie", "addon:7:opaque"), "addon:7:opaque");
        assert_eq!(media("live", "simkl:anime:1902343"), "simkl:anime:1902343");
    }

    #[test]
    fn aliases_list_the_stored_spellings_of_one_movie_only() {
        assert_eq!(
            aliases("movie", "simkl:movie:1902343"),
            vec![
                "simkl:movie:1902343",
                "simkl:movies:1902343",
                "simkl:anime:1902343"
            ]
        );
        assert_eq!(aliases("movie", "simkl:anime:1902343").len(), 3);
        assert_eq!(aliases("series", "simkl:anime:40"), vec!["simkl:anime:40"]);
        assert_eq!(aliases("movie", "tt1902343"), vec!["tt1902343"]);
    }

    #[test]
    fn category_accepts_the_endpoint_names() {
        assert_eq!(category("movies"), "movie");
        assert_eq!(category("shows"), "tv");
        assert_eq!(category("tv_shows"), "tv");
        assert_eq!(category("anime"), "anime");
    }

    #[test]
    fn a_card_is_rewritten_with_its_parent() {
        let mut value: Card = serde_json::from_value(serde_json::json!({
            "identity":{"id":"simkl:anime:1902343","kind":"movie","ids":[]},
            "provider_id":"library",
            "title":"Fixture film",
            "badges":[]
        }))
        .unwrap();
        card(&mut value);
        assert_eq!(value.identity.id, "simkl:movie:1902343");
    }
}