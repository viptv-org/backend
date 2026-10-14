use super::*;
fn control(id: &str, label: &str, choices: Vec<(&str, &str)>) -> QueryControl {
    QueryControl {
        id: id.into(),
        label: label.into(),
        kind: ControlKind::SingleChoice,
        required: false,
        choices: choices
            .into_iter()
            .map(|(value, label)| Choice {
                value: value.into(),
                label: label.into(),
            })
            .collect(),
        minimum: None,
        maximum: None,
    }
}
pub fn controls(entries: &[Entry], sources: &[LibrarySource], _source: &str) -> Vec<QueryControl> {
    let mut result = vec![
        control(
            "source",
            "List",
            sources
                .iter()
                .filter(|s| s.availability == Availability::Available)
                .map(|s| (s.id.as_str(), s.label.as_str()))
                .collect(),
        ),
        control(
            "sort",
            "Sort by",
            vec![
                ("last_watched", "Last watched"),
                ("name_asc", "Name A–Z"),
                ("name_desc", "Name Z–A"),
                ("times_watched", "Times watched"),
                ("date_added", "Date added"),
            ],
        ),
        control(
            "type",
            "Type",
            vec![
                ("all", "All"),
                ("movie", "Movies"),
                ("series", "Series"),
                ("anime", "Anime"),
            ],
        ),
    ];
    if entries.iter().all(|e| e.statistics.times_watched.is_none()) {
        result[1].choices.retain(|choice| choice.value != "times_watched");
    }
    let mut shows = vec![
        ("all", "All titles"),
        ("watched", "Watched"),
        ("not_watched", "Not watched"),
    ];
    if entries.iter().any(|e|e.statistics.status.is_some()||e.statistics.total_episode_count.is_some()||!e.statistics.provenance.is_empty()) {
        shows.extend([
            ("has_new_episode", "Has a new episode"),
            ("not_started", "Not started"),
            ("caught_up", "Caught up"),
            ("episodes_to_air", "Episodes still to air"),
        ]);
        result.push(control(
            "status",
            "Status",
            vec![
                ("", "Any"),
                ("watching", "Watching"),
                ("plantowatch", "Plan to watch"),
                ("hold", "On hold"),
                ("completed", "Completed"),
                ("dropped", "Dropped"),
            ],
        ));
        result.push(control(
            "rated",
            "Rated",
            vec![
                ("", "Any"),
                ("rated_8_plus", "Rated 8+"),
                ("not_rated", "Not rated"),
            ],
        ));
        result.push(control(
            "episodes_left_max",
            "Left",
            vec![("", "Any"), ("9", "Under 10 episodes")],
        ));
    }
    result.push(control("show", "Show", shows));
    let mut genres = std::collections::BTreeSet::new();
    for entry in entries {
        if let Some(m) = &entry.card.metadata {
            for genre in &m.genres {
                if !genre.is_empty() && genre.len() <= 128 {
                    genres.insert(genre.clone());
                }
            }
        }
    }
    if !genres.is_empty() {
        let mut genre = control("genre", "Genre", vec![("", "Any")]);
        genre
            .choices
            .extend(genres.into_iter().take(128).map(|value| Choice {
                label: value.clone(),
                value,
            }));
        result.push(genre);
    }
    result.push(control(
        "added",
        "Added",
        vec![
            ("", "Any"),
            ("this_year", "This year"),
            ("this_month", "This month"),
            ("last_7_days", "Last 7 days"),
        ],
    ));
    if sources.len()<=1 {result.retain(|control|control.id!="source");}
    result
}
