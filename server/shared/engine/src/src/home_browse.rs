//! Host-authored shelf navigation; clients never interpret opaque row identifiers.
use crate::{library::LibraryQuery, query::QueryValue};
use facet::Facet;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum HomeBrowseTarget {
    Discover,
    Library,
    Continue,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct HomeBrowseAction {
    pub target: HomeBrowseTarget,
    #[serde(default)]
    pub query: Vec<QueryValue>,
}
impl HomeBrowseAction {
    pub fn discover(kind: &str, catalog: &str, selector: Option<(&str, String)>) -> Self {
        let mut query = vec![value("type", kind), value("catalog", catalog)];
        if let Some((name, id)) = selector {
            query.push(value(name, &id));
        }
        Self {
            target: HomeBrowseTarget::Discover,
            query,
        }
    }
    pub fn library() -> Self {
        let defaults = LibraryQuery::default();
        Self {
            target: HomeBrowseTarget::Library,
            query: vec![
                value("source", &defaults.source),
                value("sort", &defaults.sort),
                value("type", &defaults.r#type),
                value("show", &defaults.show),
            ],
        }
    }
    pub fn continuation() -> Self {
        Self {
            target: HomeBrowseTarget::Continue,
            query: vec![],
        }
    }
}
fn value(id: &str, text: &str) -> QueryValue {
    QueryValue {
        id: id.into(),
        values: vec![text.into()],
    }
}
