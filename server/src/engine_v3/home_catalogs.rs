use viptv_provider::discovery_values::ExtraValues;

pub(super) fn row_selections(extras: &serde_json::Value) -> Option<Vec<ExtraValues>> {
    let mut selections = Vec::new();
    for extra in extras.as_array().into_iter().flatten() {
        let Some(name) = extra["name"].as_str() else {
            continue;
        };
        if name == "skip" || extra["is_required"].as_bool() != Some(true) {
            continue;
        }
        if extra["options_limit"].as_u64() == Some(0) {
            return None;
        }
        let options = extra["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| option.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        if name == "search" {
            if extra["default"].as_str().is_some_and(|value| !value.trim().is_empty()) {
                continue;
            }
            return None;
        }
        let selection = extra["default"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .filter(|value| options.is_empty() || options.iter().any(|option| option.as_str() == *value))
            .map(str::to_owned)
            .or_else(|| options.first().cloned())?;
        selections.push(ExtraValues {
            id: name.to_owned(),
            values: vec![selection],
        });
    }
    Some(selections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn selections(extras: serde_json::Value) -> Option<Vec<(String, Vec<String>)>> {
        row_selections(&extras).map(|selections| {
            selections
                .into_iter()
                .map(|selection| (selection.id, selection.values))
                .collect()
        })
    }

    #[test]
    fn an_optional_catalog_needs_no_selection() {
        assert_eq!(selections(json!([])), Some(vec![]));
        assert_eq!(
            selections(json!([{"name":"genre","options":["2020"],"is_required":false}])),
            Some(vec![])
        );
        assert_eq!(selections(json!([{"name":"skip"}])), Some(vec![]));
    }

    #[test]
    fn a_required_extra_takes_its_first_advertised_option() {
        assert_eq!(
            selections(json!([{"name":"genre","is_required":true,"options":["2020","2019"]}])),
            Some(vec![("genre".into(), vec!["2020".into()])])
        );
    }

    #[test]
    fn a_required_extra_prefers_its_default() {
        assert_eq!(
            selections(json!([{"name":"genre","is_required":true,"options":["2020","2019"],"default":"2019"}])),
            Some(vec![("genre".into(), vec!["2019".into()])])
        );
        assert_eq!(
            selections(json!([{"name":"genre","is_required":true,"options":["2020"],"default":"1999"}])),
            Some(vec![("genre".into(), vec!["2020".into()])])
        );
    }

    #[test]
    fn a_required_extra_without_an_option_is_never_a_row() {
        assert_eq!(selections(json!([{"name":"lastVideosIds","is_required":true,"options":[]}])), None);
        assert_eq!(
            selections(json!([{"name":"calendarVideosIds","is_required":true,"options_limit":0}])),
            None
        );
        assert_eq!(
            selections(json!([{"name":"genre","is_required":true,"options":["2020"],"options_limit":0}])),
            None
        );
    }

    #[test]
    fn a_required_search_needs_the_catalogs_own_default() {
        assert_eq!(selections(json!([{"name":"search","is_required":true}])), None);
        assert_eq!(selections(json!([{"name":"search","is_required":true,"options_limit":0}])), None);
        assert_eq!(
            selections(json!([{"name":"search","is_required":true,"default":"star"}])),
            Some(vec![])
        );
    }

    #[test]
    fn every_required_extra_gets_its_own_selection() {
        assert_eq!(
            selections(json!([
                {"name":"genre","is_required":true,"options":["2020"]},
                {"name":"year","is_required":true,"options":["2024"]},
                {"name":"skip"}
            ])),
            Some(vec![
                ("genre".into(), vec!["2020".into()]),
                ("year".into(), vec!["2024".into()])
            ])
        );
    }
}