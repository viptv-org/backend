use std::collections::HashMap;

pub(super) struct CatalogRow<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub addon: &'a str,
}

pub(super) fn catalog_row_titles(rows: &[CatalogRow]) -> Vec<String> {
    let typed = rows.iter().map(typed_title).collect::<Vec<_>>();
    let mut counts = HashMap::<String, usize>::new();
    for title in &typed {
        *counts.entry(title.to_lowercase()).or_default() += 1;
    }
    typed
        .into_iter()
        .zip(rows)
        .map(|(title, row)| match counts[&title.to_lowercase()] {
            1 => title,
            _ => format!("{title} · {}", addon_name(row.addon)),
        })
        .collect()
}

fn addon_name(name: &str) -> String {
    let name = name
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect::<String>();
    match name.trim() {
        "" => "Stremio addon".to_owned(),
        name => name.to_owned(),
    }
}

fn type_copy(kind: &str) -> (String, &'static [&'static str]) {
    let (label, words): (&str, &'static [&'static str]) = match kind.to_lowercase().as_str() {
        "movie" => ("Movies", &["movie", "movies", "film", "films"]),
        "series" => ("Series", &["series", "show", "shows"]),
        "anime" => ("Anime", &["anime"]),
        "channel" => ("Channels", &["channel", "channels"]),
        "tv" => ("TV", &["tv"]),
        _ => return (title_case(kind), &[]),
    };
    (label.to_owned(), words)
}

fn typed_title(row: &CatalogRow) -> String {
    let (label, words) = type_copy(row.kind);
    let name_words = words_of(row.name).collect::<Vec<_>>();
    let says_type = name_words.iter().any(|word| words.contains(&word.as_str()))
        || words_of(&label).all(|word| name_words.contains(&word));
    if says_type {
        row.name.to_owned()
    } else {
        format!("{} {label}", row.name)
    }
}

fn words_of(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

fn title_case(kind: &str) -> String {
    kind.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(rows: &[(&str, &str, &str)]) -> Vec<String> {
        let rows = rows
            .iter()
            .map(|&(name, kind, addon)| CatalogRow { name, kind, addon })
            .collect::<Vec<_>>();
        catalog_row_titles(&rows)
    }

    #[test]
    fn a_single_row_says_its_type() {
        assert_eq!(titles(&[("Popular", "movie", "Cinemeta")]), ["Popular Movies"]);
    }

    #[test]
    fn rows_of_different_types_differ_by_type_alone() {
        assert_eq!(
            titles(&[("Popular", "movie", "Cinemeta"), ("Popular", "series", "Cinemeta")]),
            ["Popular Movies", "Popular Series"]
        );
    }

    #[test]
    fn a_clash_across_add_ons_names_every_clashing_add_on() {
        assert_eq!(
            titles(&[
                ("Popular", "movie", "Cinemeta"),
                ("Popular", "series", "Cinemeta"),
                ("Popular", "movie", "Torrentio"),
            ]),
            ["Popular Movies · Cinemeta", "Popular Series", "Popular Movies · Torrentio"]
        );
    }

    #[test]
    fn a_name_that_already_says_its_type_is_not_doubled() {
        assert_eq!(
            titles(&[
                ("Top Movies", "movie", "A"),
                ("New Series", "series", "A"),
                ("top movie picks", "movie", "A"),
                ("Anime Season", "anime", "A"),
            ]),
            ["Top Movies", "New Series", "top movie picks", "Anime Season"]
        );
    }

    #[test]
    fn a_doubled_name_still_counts_toward_a_clash() {
        assert_eq!(
            titles(&[("Popular", "movie", "Cinemeta"), ("Popular Movies", "movie", "Other")]),
            ["Popular Movies · Cinemeta", "Popular Movies · Other"]
        );
    }

    #[test]
    fn every_type_has_title_cased_copy() {
        assert_eq!(
            titles(&[
                ("Popular", "anime", "A"),
                ("Popular", "channel", "A"),
                ("Popular", "tv", "A"),
                ("Popular", "events", "A"),
                ("Popular", "live_sports", "A"),
            ]),
            [
                "Popular Anime",
                "Popular Channels",
                "Popular TV",
                "Popular Events",
                "Popular Live Sports",
            ]
        );
    }

    #[test]
    fn type_copy_ignores_the_declared_types_letter_case() {
        assert_eq!(titles(&[("Popular", "Movie", "A")]), ["Popular Movies"]);
    }

    #[test]
    fn a_multi_word_type_is_said_only_by_all_its_words() {
        assert_eq!(
            titles(&[("Live Events", "live_sports", "A"), ("Live Sports Now", "live_sports", "A")]),
            ["Live Events Live Sports", "Live Sports Now"]
        );
    }

    #[test]
    fn the_add_on_name_is_one_bounded_line_with_a_fallback() {
        let long = "N".repeat(300);
        let titles = titles(&[
            ("Popular", "movie", "  Bad\nName\u{7}  "),
            ("Popular", "movie", " "),
            ("Popular", "movie", &long),
        ]);
        assert_eq!(titles[0], "Popular Movies · BadName");
        assert_eq!(titles[1], "Popular Movies · Stremio addon");
        assert_eq!(titles[2], format!("Popular Movies · {}", "N".repeat(128)));
    }

    #[test]
    fn clashes_ignore_letter_case() {
        assert_eq!(
            titles(&[("popular", "movie", "A"), ("Popular", "movie", "B")]),
            ["popular Movies · A", "Popular Movies · B"]
        );
    }
}
