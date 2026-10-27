//! Home titles for add-on catalog rows. Catalog names repeat across types and
//! add-ons ("Popular" for Cinemeta movies and series, and again elsewhere), so
//! a row title always says its type and names the add-on only on a clash.
use std::collections::HashMap;

/// One add-on catalog row before its title is decided.
pub(super) struct CatalogRow<'a> {
    /// The catalog's own name from the add-on manifest.
    pub name: &'a str,
    /// The catalog's content type (`movie`, `series`, ...).
    pub kind: &'a str,
    /// The add-on's display name from its manifest.
    pub addon: &'a str,
}

/// Titles for the whole list of add-on rows, in the same order. Decided over
/// the complete list, so a title never depends on load order or failures.
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
            _ => format!("{title} · {}", row.addon),
        })
        .collect()
}

/// The single place for each type's title copy: the label added to a row
/// title, and the words that mean a catalog name already says the type.
fn type_copy(kind: &str) -> (String, &'static [&'static str]) {
    let (label, words): (&str, &'static [&'static str]) = match kind {
        "movie" => ("Movies", &["movie", "movies", "film", "films"]),
        "series" => ("Series", &["series", "show", "shows"]),
        "anime" => ("Anime", &["anime"]),
        "channel" => ("Channels", &["channel", "channels"]),
        "tv" => ("TV", &["tv"]),
        // Add-ons may declare any type: title-case its words.
        other => return (title_case(other), &[]),
    };
    (label.to_owned(), words)
}

fn typed_title(row: &CatalogRow) -> String {
    let (label, words) = type_copy(row.kind);
    let label_words = words_of(&label).collect::<Vec<_>>();
    let says_type = words_of(row.name)
        .any(|word| words.contains(&word.as_str()) || label_words.contains(&word));
    if says_type || label.is_empty() {
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
    fn clashes_ignore_letter_case() {
        assert_eq!(
            titles(&[("popular", "movie", "A"), ("Popular", "movie", "B")]),
            ["popular Movies · A", "Popular Movies · B"]
        );
    }
}
