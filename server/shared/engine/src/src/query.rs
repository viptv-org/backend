use crate::Error;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum ControlKind {
    Text,
    SingleChoice,
    MultiChoice,
    Toggle,
    Number,
    Date,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct QueryControl {
    pub id: String,
    pub label: String,
    pub kind: ControlKind,
    pub required: bool,
    pub choices: Vec<Choice>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    /// Maximum selected values, independent of numeric value bounds.
    #[serde(default)]
    pub maximum_selections: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(deny_unknown_fields)]
pub struct QueryValue {
    pub id: String,
    pub values: Vec<String>,
}

pub fn validate(controls: &[QueryControl], values: &[QueryValue]) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for input in values {
        if !seen.insert(&input.id) {
            return Err(Error::InvalidQuery);
        }
        let control = controls
            .iter()
            .find(|c| c.id == input.id)
            .ok_or(Error::InvalidQuery)?;
        if input.values.len() > 64
            || control
                .maximum_selections
                .is_some_and(|n| input.values.len() > n as usize)
            || input
                .values
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != input.values.len()
            || (!matches!(control.kind, ControlKind::MultiChoice) && input.values.len() > 1)
        {
            return Err(Error::InvalidQuery);
        }
        for value in &input.values {
            if value.len() > 1024 {
                return Err(Error::InvalidQuery);
            }
            let valid = match control.kind {
                ControlKind::SingleChoice | ControlKind::MultiChoice => {
                    control.choices.iter().any(|c| c.value == *value)
                }
                ControlKind::Toggle => matches!(value.as_str(), "true" | "false"),
                ControlKind::Number => value.parse::<f64>().is_ok_and(|v| {
                    v.is_finite()
                        && control.minimum.is_none_or(|m| v >= m)
                        && control.maximum.is_none_or(|m| v <= m)
                }),
                ControlKind::Text => !value.trim().is_empty(),
                ControlKind::Date => valid_date(value),
            };
            if !valid {
                return Err(Error::InvalidQuery);
            }
        }
    }
    if controls
        .iter()
        .any(|c| c.required && !values.iter().any(|v| v.id == c.id && !v.values.is_empty()))
    {
        return Err(Error::InvalidQuery);
    }
    Ok(())
}

fn valid_date(value: &str) -> bool {
    let fields: Vec<_> = value.split('-').collect();
    if fields.len() != 3 || fields[0].len() != 4 || fields[1].len() != 2 || fields[2].len() != 2 {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        fields[0].parse::<u32>(),
        fields[1].parse::<u32>(),
        fields[2].parse::<u32>(),
    ) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let max = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    y > 0 && d > 0 && d <= max
}
