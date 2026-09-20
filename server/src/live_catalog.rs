//! Viewer-facing US guide organization. Provider inventory and IDs remain intact.
use crate::{lineup, util};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::LazyLock};

mod classify;
mod query;

pub(crate) use classify::{category_exclusion, display_name, exclusion};
pub use classify::{classify, GROUPS};
use classify::{foreign, key, prohibited, words};
#[cfg(test)]
pub use query::channels;
pub use query::{browse, categories};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_recognized_us_english_networks_survive() {
        assert_eq!(
            classify("US | CARTOON NETWORK (WEST) FHD", "USA Kids")
                .unwrap()
                .0,
            5
        );
        assert_eq!(classify("CN HD", "US Kids").unwrap().1, "Cartoon Network");
        for (name, group) in [
            ("UK | Cartoon Network", "Kids"),
            ("CNN", "CANADA"),
            ("HBO Latino", "USA"),
            ("ESPN Deportes", "Sports"),
            ("HBO", "XXX"),
            ("Playboy", "USA"),
            ("Unknown Station", "USA"),
            ("Cartoon Network XXX", "Kids"),
        ] {
            assert!(
                classify(name, group).is_none(),
                "must exclude {name} in {group}"
            );
        }
        assert!(classify("NBC WNBC New York", "USA Local").is_some());
        assert!(classify("Cinemax East", "US Movies").is_some());
    }
}
