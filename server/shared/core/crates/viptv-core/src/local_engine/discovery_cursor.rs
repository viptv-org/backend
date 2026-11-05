const PREFIX: &str = "d1;";
pub(super) const MAX_LENGTH: usize = 2048;
const MAX_ENTRIES: usize = 32;
const MAX_ID: usize = 128;
const MAX_OFFSET: u32 = 10_000;

pub(super) fn encode(entries: &[(String, Option<u32>)]) -> String {
    let body = entries
        .iter()
        .map(|(id, offset)| match offset {
            Some(offset) => format!("{id}={offset}"),
            None => format!("{id}=-"),
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{PREFIX}{body}")
}

pub(super) fn decode(raw: &str) -> Option<Vec<(String, Option<u32>)>> {
    if raw.len() > MAX_LENGTH {
        return None;
    }
    let body = raw.strip_prefix(PREFIX)?;
    let mut entries = Vec::new();
    for entry in body.split(';') {
        let (id, offset) = entry.split_once('=')?;
        if id.is_empty()
            || id.len() > MAX_ID
            || !id.bytes().all(|b| b.is_ascii_graphic() && b != b'=')
            || entries.iter().any(|(known, _)| known == id)
        {
            return None;
        }
        let offset = if offset == "-" {
            None
        } else if offset.bytes().all(|b| b.is_ascii_digit()) {
            Some(offset.parse::<u32>().ok().filter(|n| *n <= MAX_OFFSET)?)
        } else {
            return None;
        };
        entries.push((id.to_string(), offset));
        if entries.len() > MAX_ENTRIES {
            return None;
        }
    }
    (!entries.is_empty()).then_some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_offsets_and_exhausted_catalogs() {
        let entries = vec![
            ("discover:addon:0f8f:0".to_string(), Some(20)),
            ("discover:addon:1a2b:0".to_string(), None),
        ];
        assert_eq!(decode(&encode(&entries)), Some(entries));
    }

    #[test]
    fn cursor_rejects_malformed_or_foreign_values() {
        for raw in [
            "",
            "d1;",
            "d2;discover:addon:a:0=20",
            "d1;discover:addon:a:0",
            "d1;=20",
            "d1;discover:addon:a:0=20=20",
            "d1;discover:addon:a:0=10001",
            "d1;discover:addon:a:0=+20",
            "d1;discover:addon:a:0=20;discover:addon:a:0=40",
            &format!("d1;{}", "a".repeat(MAX_LENGTH)),
        ] {
            assert_eq!(decode(raw), None, "{raw}");
        }
        let too_many = (0..=MAX_ENTRIES)
            .map(|n| (format!("discover:addon:{n}:0"), Some(20)))
            .collect::<Vec<_>>();
        assert_eq!(decode(&encode(&too_many)), None);
    }

    #[test]
    fn cursor_fits_the_largest_plan() {
        let entries = (0..20)
            .map(|n| {
                (
                    format!("discover:addon:{:08x}-0000-0000-0000-000000000000:{n}", n),
                    Some(10_000),
                )
            })
            .collect::<Vec<_>>();
        let raw = encode(&entries);
        assert!(raw.len() <= MAX_LENGTH, "{}", raw.len());
        assert_eq!(decode(&raw), Some(entries));
    }
}