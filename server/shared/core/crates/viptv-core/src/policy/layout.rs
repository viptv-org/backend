//! Which catalogs feed Home shelves and Search sections, in what order and
//! under which titles. Shells fetch the planned rows and render them.
use super::*;
use crate::dto::{HomeLayout, HomeShelfPlan, HomeShelfRole, SearchPlan, SearchSectionPlan};

/// Distinct catalogs searched for one query; the backend budget for a search fan-out.
const SEARCH_CATALOG_LIMIT: usize = 128;
/// Titles kept per search section after removing repeats.
const SEARCH_SECTION_LIMIT: u32 = 24;
/// Live channels requested for a search, and how many the Live TV section shows.
const SEARCH_LIVE_REQUEST_LIMIT: u32 = 80;
/// The live channel search accepts at most this many characters.
const SEARCH_QUERY_LIMIT: usize = 128;
const CONTINUE_WATCHING_LIMIT: u32 = 40;
const RECENT_LIVE_LIMIT: u32 = 24;

fn catalogs(v: &Value) -> &[Value] {
    v["catalogs"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// A Home catalog row loads without any viewer choice: it is not a live
/// namespace and every required filter has a declared default or option.
fn home_eligible(catalog: &Value) -> bool {
    if text(catalog, "type") == "live" {
        return false;
    }
    let defaults = catalog::catalog_filters("catalogDefaults", catalog);
    defaults.as_object().is_none_or(|values| {
        values
            .values()
            .all(|value| value.as_str().is_some_and(|s| !s.trim().is_empty()))
    })
}

/// "Addon · Catalog" names the source of a Home shelf or Search section; a
/// blank addon name is omitted so same-named catalogs stay distinguishable.
fn catalog_title(catalog: &Value) -> String {
    [text(catalog, "addonName"), text(catalog, "name")]
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `liveShelves: false` (the Android TV Home trial, TV-044) omits both live
/// shelves so their requests never start; Live TV stays its own destination.
pub(super) fn home_layout(v: &Value) -> Result {
    let live = v["liveShelves"].as_bool().unwrap_or(true);
    let shelf = |role, title: &str, limit, catalog_index| HomeShelfPlan {
        role,
        title: title.to_owned(),
        limit,
        catalog_index,
    };
    let mut shelves = vec![
        shelf(
            HomeShelfRole::ContinueWatching,
            "Continue watching",
            Some(CONTINUE_WATCHING_LIMIT),
            None,
        ),
        shelf(
            HomeShelfRole::RecentLive,
            "Recently watched live TV",
            Some(RECENT_LIVE_LIMIT),
            None,
        ),
    ];
    shelves.extend(
        catalogs(v)
            .iter()
            .enumerate()
            .filter(|(_, c)| home_eligible(c))
            .map(|(i, c)| HomeShelfPlan {
                role: HomeShelfRole::Catalog,
                title: catalog_title(c),
                limit: None,
                catalog_index: Some(index(i)),
            }),
    );
    shelves.push(shelf(HomeShelfRole::MyList, "My List", None, None));
    shelves.push(shelf(HomeShelfRole::LiveNow, "Live now", None, None));
    if !live {
        shelves.retain(|s| !matches!(s.role, HomeShelfRole::RecentLive | HomeShelfRole::LiveNow));
    }
    serde_json::to_value(HomeLayout { shelves }).map_err(|_| CoreError::InvalidInput)
}

pub(super) fn search_plan(v: &Value) -> Result {
    let query: String = text(v, "query")
        .trim()
        .chars()
        .take(SEARCH_QUERY_LIMIT)
        .collect();
    let query = query.trim_end().to_owned();
    let scope = match text(v, "scope") {
        "" => "all",
        scope => scope,
    };
    let searching = !query.is_empty() && !query.chars().any(char::is_control);
    let sections = if searching {
        catalogs(v)
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c["supportsSearch"] == true && (scope == "all" || text(c, "type") == scope)
            })
            .take(SEARCH_CATALOG_LIMIT)
            .map(|(i, c)| SearchSectionPlan {
                catalog_index: index(i),
                title: catalog_title(c),
            })
            .collect()
    } else {
        Vec::new()
    };
    let plan = SearchPlan {
        live: searching && matches!(scope, "all" | "live"),
        query: if searching { query } else { String::new() },
        sections,
        section_limit: SEARCH_SECTION_LIMIT,
        live_request_limit: SEARCH_LIVE_REQUEST_LIMIT,
        live_title: "Live TV".to_owned(),
    };
    serde_json::to_value(plan).map_err(|_| CoreError::InvalidInput)
}
