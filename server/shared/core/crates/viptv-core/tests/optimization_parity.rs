use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use viptv_core::{domain::clean, dto::JsonValue};

// The pre-optimization implementations are kept only as independent test oracles.
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum LegacyJsonValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<LegacyJsonValue>),
    Object(BTreeMap<String, LegacyJsonValue>),
}

const FORBIDDEN: [&str; 15] = [
    "url",
    "uri",
    "link",
    "header",
    "authorization",
    "accesstoken",
    "refreshtoken",
    "devicecode",
    "devicetoken",
    "cookie",
    "password",
    "credential",
    "proxy",
    "referer",
    "origin",
];

fn legacy_clean(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(legacy_clean).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| {
                    let key = key.to_lowercase().replace(['_', '-'], "");
                    !FORBIDDEN.iter().any(|word| key.contains(word))
                })
                .map(|(key, value)| (key.clone(), legacy_clean(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

fn generated(seed: &mut u64, depth: usize) -> Value {
    let choice = next(seed) % if depth == 0 { 6 } else { 9 };
    match choice {
        0 => Value::Null,
        1 => json!(next(seed).is_multiple_of(2)),
        2 => json!(next(seed)),
        3 => json!(next(seed) as i64),
        4 => json!(f64::from_bits(next(seed))),
        5 => json!(
            ["映画 Café 🎬", "\"quoted\"\\\n\t", "İ Σ ß", "", "safe"][(next(seed) % 5) as usize]
        ),
        6 => Value::Array(
            (0..next(seed) % 5)
                .map(|_| generated(seed, depth - 1))
                .collect(),
        ),
        _ => {
            let mut fields = serde_json::Map::new();
            for _ in 0..next(seed) % 6 {
                let index = (next(seed) % 20) as usize;
                let word = if index < FORBIDDEN.len() {
                    FORBIDDEN[index]
                } else {
                    ["id", "name", "映画", "addon_id", "Café"][index - FORBIDDEN.len()]
                };
                let spelling = match next(seed) % 5 {
                    0 => word.to_owned(),
                    1 => word.to_uppercase(),
                    2 => word.chars().map(|ch| format!("{ch}_-")).collect(),
                    3 => format!("İ{word}映画"),
                    _ => format!("prefix_{word}-suffix"),
                };
                fields.insert(spelling, generated(seed, depth - 1));
            }
            Value::Object(fields)
        }
    }
}

#[test]
fn optimized_json_and_redaction_match_legacy_on_deterministic_corpus() {
    let mut corpus = vec![
        "null".to_owned(),
        "-0".into(),
        "-0.0".into(),
        "1e-999".into(),
        "-9223372036854775808".into(),
        "18446744073709551615".into(),
        "18446744073709551616".into(),
        "1.7976931348623157e308".into(),
        "2.2250738585072014e-308".into(),
        "5e-324".into(),
        r#"{"x":"\uD83C\uDFAC","nested":[null,true,{},[]]}"#.into(),
    ];
    let mut seed = 0x1357_9bdf_2468_ace0;
    for _ in 0..1024 {
        corpus.push(serde_json::to_string(&generated(&mut seed, 4)).unwrap());
    }
    for wire in corpus {
        let old: LegacyJsonValue = serde_json::from_str(&wire).unwrap();
        let new: JsonValue = serde_json::from_str(&wire).unwrap();
        assert_eq!(
            serde_json::to_string(&new).unwrap(),
            serde_json::to_string(&old).unwrap(),
            "{wire}"
        );
        let value: Value = serde_json::from_str(&wire).unwrap();
        let borrowed = JsonValue::deserialize(&value).unwrap();
        let old_borrowed = LegacyJsonValue::deserialize(&value).unwrap();
        assert_eq!(
            serde_json::to_string(&borrowed).unwrap(),
            serde_json::to_string(&old_borrowed).unwrap(),
            "{wire}"
        );
        let expected = serde_json::to_string(&legacy_clean(&value)).unwrap();
        assert_eq!(
            serde_json::to_string(&clean(&value)).unwrap(),
            expected,
            "{wire}"
        );
        assert_eq!(
            viptv_core::normalize("clean".into(), wire.clone(), String::new()).unwrap(),
            expected,
            "{wire}"
        );
    }
    for invalid in [
        "",
        "[",
        "{",
        "{1:2}",
        r#"{"x":"\uD800"}"#,
        "NaN",
        "1e9999",
        "[1,]",
        "true false",
        "{\"a\":01}",
    ] {
        let old = serde_json::from_str::<LegacyJsonValue>(invalid)
            .err()
            .unwrap();
        let new = serde_json::from_str::<JsonValue>(invalid).err().unwrap();
        assert_eq!(new.to_string(), old.to_string(), "{invalid}");
    }
}
