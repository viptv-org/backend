//! Fixture-only helper: import the exact runtime Vault, never reimplement crypto.
#[allow(dead_code)]
#[path = "../../../server/src/secret_store.rs"]
mod actual_vault;
use std::io::{self, Read};

fn main() {
    let mut text = String::new();
    io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut text)
        .unwrap();
    let input: serde_json::Value = serde_json::from_str(&text).unwrap();
    let vault = actual_vault::Vault::from_json(&input["keyring"].to_string()).unwrap();
    let account = input["account"].as_i64().unwrap();
    let id = input["id"].as_str().unwrap();
    let value = input["value"].as_str().unwrap();
    let envelope = vault
        .seal(account, "gateway_key", id, value.as_bytes())
        .unwrap();
    let opened = vault.open(account, "gateway_key", id, &envelope).unwrap();
    assert!(
        opened.expose() == value.as_bytes(),
        "Fixture Vault roundtrip failed"
    );
    println!("{envelope}"); // Captured privately by the offline seed, never browser/public output.
}
