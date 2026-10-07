//! Native raw-text corpus runner used by actual-WASM parity checks.
use serde_json::{Value, json};

fn main() {
    let vectors: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/playback-protocol-vectors.json"
    ))
    .unwrap();
    let results: Vec<Value> = vectors
        .iter()
        .map(|v| {
            match viptv_core::normalize(
                v["kind"].as_str().unwrap().into(),
                v["input"].as_str().unwrap().into(),
                "https://fixture.invalid".into(),
            ) {
                Ok(text) => {
                    json!({"ok":true,"result":serde_json::from_str::<Value>(&text).unwrap()})
                }
                Err(error) => json!({"ok":false,"error":error.to_string()}),
            }
        })
        .collect();
    println!("{}", serde_json::to_string(&results).unwrap());
}
