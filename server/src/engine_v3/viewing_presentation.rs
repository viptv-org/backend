//! Additive rendering facts for thin clients. Membership policy stays here;
//! renderers never reconstruct canonical aliases or connector scope.
use super::*;
use viptv_engine::{library::LibraryMembershipSnapshot, presentation::Card};

/// Ordered display facts shared by all remote renderers. Raw metadata remains
/// intact for older consumers; no client needs to normalize chips or truncate genres.
pub(crate) fn card_view(card: &Card, saved: bool, show_ratings: bool) -> Value {
    let mut metadata = vec![match card.identity.kind.as_str() {
        "movie" => "Movie".to_owned(),
        "series" => "Series".to_owned(),
        _ => "Live".to_owned(),
    }];
    let mut compact = Vec::new();
    if let Some(meta) = &card.metadata {
        if let Some(year) = meta.year {
            metadata.push(year.to_string());
            compact.push(year.to_string());
        }
        if !meta.genres.is_empty() {
            metadata.push(
                meta.genres
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(minutes) = meta.runtime_minutes.filter(|n| *n > 0) {
            let label = if minutes < 60 {
                format!("{minutes}m")
            } else if minutes % 60 == 0 {
                format!("{}h", minutes / 60)
            } else {
                format!("{}h {}m", minutes / 60, minutes % 60)
            };
            metadata.push(label.clone());
            compact.push(label);
        }
        if show_ratings {
            if let Some(rating) = meta.rating.filter(|r| r.is_finite() && *r > 0.0) {
                compact.push(format!("★ {rating}"));
            }
        }
    }
    let mut badges = Vec::<Value>::new();
    for raw in &card.badges {
        let (label, role, order) = match raw.to_ascii_lowercase().as_str() {
            "4k" | "uhd" | "2160p" => ("4K", "capability", 0),
            "1080p" | "fhd" => ("1080p", "capability", 0),
            "720p" | "hd" => ("720p", "capability", 0),
            "dv" | "dolby vision" => ("Dolby Vision", "capability", 1),
            "hdr10+" => ("HDR10+", "capability", 1),
            "hdr10" => ("HDR10", "capability", 1),
            "hdr" => ("HDR", "capability", 1),
            "5.1" => ("5.1", "capability", 2),
            "7.1" => ("7.1", "capability", 2),
            "atmos" | "dolby atmos" => ("Dolby Atmos", "capability", 2),
            "watched" => ("Watched", "watched", 4),
            "new episode" | "new episodes" => (raw.as_str(), "new_episodes", 5),
            _ => (raw.as_str(), "label", 6),
        };
        if !badges.iter().any(|b| b["label"] == label) {
            badges.push(json!({"label":label,"role":role,"order":order}));
        }
    }
    if let Some(label) = card
        .metadata
        .as_ref()
        .and_then(|m| m.certification.as_deref())
        .filter(|s| !s.is_empty())
    {
        badges.retain(|badge| badge["label"] != label);
        badges.push(json!({"label":label,"role":"certification","order":3}));
    }
    badges.sort_by_key(|b| b["order"].as_u64().unwrap_or(6));
    for badge in &mut badges {
        badge.as_object_mut().unwrap().remove("order");
    }
    let tile_badge = badges
        .iter()
        .find(|b| matches!(b["role"].as_str(), Some("new_episodes" | "watched")))
        .cloned();
    let hero_metadata = metadata.iter().skip(1).collect::<Vec<_>>();
    let hero_badges = badges
        .iter()
        .filter(|b| matches!(b["role"].as_str(), Some("capability" | "certification")))
        .collect::<Vec<_>>();
    json!({"saved":saved,"hero_metadata_parts":hero_metadata,"hero_badges":hero_badges,"metadata_parts":metadata,"compact_metadata_parts":compact,"badges":badges,"tile_badge":tile_badge,"progress":card.progress})
}
fn saved(snapshot: &LibraryMembershipSnapshot, card: &Card) -> bool {
    let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
    if let Some(origin) = &card.metadata_origin {
        if snapshot
            .scoped_identities
            .iter()
            .any(|entry| entry.metadata_origin == *origin && entry.identity == *identity)
        {
            return true;
        }
    }
    let shared_allowed = !identity.id.starts_with("simkl:")
        || card
            .metadata_origin
            .as_ref()
            .map(|origin| origin.provider_id.as_str())
            .unwrap_or(&card.provider_id)
            == "simkl";
    (shared_allowed
        && snapshot
            .identities
            .iter()
            .any(|entry| entry.kind == identity.kind && entry.id == identity.id))
        || (card.metadata_origin.is_none()
            && snapshot
                .legacy_identities
                .iter()
                .any(|entry| entry.kind == identity.kind && entry.id == identity.id))
}
pub(crate) fn decorate(
    value: &mut Value,
    membership: &Value,
    show_ratings: bool,
) -> Result<(), ApiError> {
    let snapshot: LibraryMembershipSnapshot = serde_json::from_value(membership.clone())
        .map_err(|_| ApiError::from("Library membership unavailable"))?;
    fn visit(value: &mut Value, snapshot: &LibraryMembershipSnapshot, show_ratings: bool) {
        match value {
            Value::Array(items) => {
                for item in items {
                    visit(item, snapshot, show_ratings);
                }
            }
            Value::Object(object) => {
                if object.contains_key("cards") && object.contains_key("state") {
                    let shape = if object.get("id").and_then(Value::as_str) == Some("continue") {
                        "landscape"
                    } else {
                        "poster"
                    };
                    object.insert("view".into(), json!({"card_shape":shape}));
                }
                if object.contains_key("identity")
                    && object.contains_key("provider_id")
                    && object.contains_key("badges")
                {
                    if let Ok(card) = serde_json::from_value::<Card>(Value::Object(object.clone()))
                    {
                        object.insert(
                            "view".into(),
                            card_view(&card, saved(snapshot, &card), show_ratings),
                        );
                    }
                } else {
                    for child in object.values_mut() {
                        visit(child, snapshot, show_ratings);
                    }
                }
            }
            _ => {}
        }
    }
    visit(value, &snapshot, show_ratings);
    Ok(())
}
pub(crate) fn for_profile(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    value: &mut Value,
) -> Result<(), ApiError> {
    let show_ratings = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        super::preferences::read_display(&db, lease.principal.account_id().ok_or_else(auth::forbidden)?, profile)?.show_ratings
    };
    let snapshot =
        library::membership::snapshot(&app.clone().with_lease(lease.clone()), lease, profile)?;
    decorate(value, &snapshot, show_ratings)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendering_facts_order_capabilities_and_separate_title_state() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/remote-viewing-v3.json")).unwrap();
        let card: Card = serde_json::from_value(fixture["card"].clone()).unwrap();
        let view = card_view(&card, false, true);
        assert_eq!(view, fixture["card_view"]);
        assert_eq!(
            view["metadata_parts"],
            json!(["Movie", "2026", "Action, Adventure", "1h 40m"])
        );
        assert_eq!(
            view["badges"],
            json!([{"label":"4K","role":"capability"},{"label":"HDR","role":"capability"},{"label":"5.1","role":"capability"},{"label":"PG-13","role":"certification"},{"label":"Watched","role":"watched"}])
        );
        assert_eq!(view["tile_badge"]["label"], "Watched");
        assert!(view["progress"].is_null());
    }
    #[test]
    fn scoped_and_reserved_identifiers_do_not_leak_membership() {
        let identity = json!({"kind":"movie","id":"simkl:movie:1","ids":[]});
        let membership = json!({"identities":[identity],"revision":0,"complete":true});
        let mut card = json!({"identity":identity,"provider_id":"stremio","title":"Title","badges":[],"view":{"saved":true}});
        decorate(&mut card, &membership, true).unwrap();
        assert_eq!(card["view"]["saved"], false);
        card["provider_id"] = json!("simkl");
        decorate(&mut card, &membership, true).unwrap();
        assert_eq!(card["view"]["saved"], true);
    }
}

/// Server-advertised choices for preferences implemented by this server.
/// Unknown preferences never turn into placeholder TV controls.
pub(crate) fn preference_controls(value: &mut Value) {
    let choices = |items: Vec<(&str, Value)>| {
        items
            .into_iter()
            .map(|(label, value)| json!({"label":label,"value":value}))
            .collect::<Vec<_>>()
    };
    let languages = || {
        choices(vec![
            ("Original / system default", Value::Null),
            ("English", json!("en")),
            ("Spanish", json!("es")),
            ("French", json!("fr")),
            ("German", json!("de")),
            ("Italian", json!("it")),
            ("Portuguese", json!("pt")),
            ("Japanese", json!("ja")),
            ("Korean", json!("ko")),
            ("Chinese", json!("zh")),
            ("Hindi", json!("hi")),
            ("Arabic", json!("ar")),
            ("Dutch", json!("nl")),
            ("Polish", json!("pl")),
            ("Russian", json!("ru")),
            ("Turkish", json!("tr")),
        ])
    };
    value["controls"] = json!([
        {"id":"source_selection","group":"playback","label":"Default source","description":"How Play chooses between sources","choices":choices(vec![("Best match",json!("best")),("Ask every time",json!("ask"))])},
        {"id":"max_quality_height","group":"playback","label":"Preferred quality","description":"Best match aims for this, then falls back","choices":choices(vec![("Auto",Value::Null),("4K",json!(2160)),("1080p",json!(1080)),("720p",json!(720)),("480p",json!(480))])},
        {"id":"hdr_preference","group":"playback","label":"HDR preference","choices":choices(vec![("Any",json!("any")),("Prefer HDR",json!("prefer")),("Avoid HDR",json!("avoid"))])},
        {"id":"preferred_audio_language","group":"subtitles","label":"Audio language","choices":languages()},
        {"id":"subtitle_mode","group":"subtitles","label":"Subtitles","choices":choices(vec![("Always",json!("always")),("Never",json!("never"))])},
        {"id":"preferred_subtitle_language","group":"subtitles","label":"Subtitle language","choices":languages()},
        {"id":"subtitle_font_scale","group":"subtitles","label":"Subtitle size","choices":choices(vec![("Small",json!(0.8)),("Medium",json!(1.0)),("Large",json!(1.25)),("Extra large",json!(1.5))])},
        {"id":"subtitle_background_color","group":"subtitles","label":"Subtitle background","choices":choices(vec![("None",json!("#00000000")),("Dark",json!("#000000CC")),("Black",json!("#000000"))])},
        {"id":"subtitle_text_color","group":"subtitles","label":"Subtitle colour","choices":choices(vec![("White",json!("#FFFFFF")),("Yellow",json!("#F5C542"))])},
        {"id":"accent_color","group":"display","label":"Accent colour","description":"Progress bars, marks and toggles","kind":"swatches","choices":choices(vec![("Amber",json!("#F5C542")),("Coral",json!("#FF8B5C")),("Mint",json!("#62D9BC")),("Sky",json!("#A3BCFF"))])},
        {"id":"show_ratings","group":"display","label":"Show ratings on cards","description":"★ 7.8 under focused titles","choices":choices(vec![("On",json!(true)),("Off",json!(false))])},
        {"id":"text_size","group":"display","label":"Text size","choices":choices(vec![("Small",json!("small")),("Default",json!("default")),("Large",json!("large"))])},
        {"id":"reduce_motion","group":"display","label":"Reduce motion","description":"Slides become cuts. Follows the TV’s setting unless you change it","choices":choices(vec![("Follow TV",json!("follow")),("On",json!("on")),("Off",json!("off"))])},
        {"id":"screen_saver_minutes","group":"display","label":"Screen saver","description":"Dims to slow-moving art when nothing is playing","choices":choices(vec![("Off",json!(0)),("After 2 minutes",json!(2)),("After 5 minutes",json!(5)),("After 10 minutes",json!(10))])},
        {"id":"screen_inset","group":"display","label":"Fit to screen","description":"Use if the edges of the picture are cut off","kind":"calibration","min":0,"max":20,"step":1}
    ]);
}
