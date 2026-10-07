//! Shared native runner for raw-text, state/authority and privacy vectors.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use viptv_core::{CoreError, NativeTorrentBridge, normalize};

pub fn corpus() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/native-torrent-vectors.json"
    ))
    .unwrap()
}
fn outcome(result: Result<Value, CoreError>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"result":value}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    }
}
fn text(value: &Value) -> String {
    value.to_string()
}
fn run_step(bridge: &NativeTorrentBridge, root: &Value, step: &Value) -> Result<Value, CoreError> {
    let clock = step.get("clock").unwrap_or(&root["clock"]).to_string();
    match step["action"].as_str().unwrap() {
        "acceptBytes" => bridge
            .accept_bytes(
                200,
                step["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect(),
                root["observation"].to_string(),
            )
            .map(|out| serde_json::from_str(&out).unwrap()),
        "accept" => {
            let mut body = step["body"].as_str().unwrap().to_owned();
            if let Some(bytes) = step["metainfoBytes"].as_u64() {
                let target = bytes as usize;
                let suffix = [
                    b"12:piece lengthi16384e6:pieces20:".as_slice(),
                    &[0; 20],
                    b"e",
                ]
                .concat();
                let mut name = target;
                loop {
                    let overhead = 7
                        + b"d6:lengthi4096e4:name".len()
                        + name.to_string().len()
                        + 1
                        + suffix.len()
                        + 1;
                    let next = target - overhead;
                    if next == name {
                        break;
                    }
                    name = next;
                }
                let info = [
                    format!("d6:lengthi4096e4:name{name}:").into_bytes(),
                    vec![b'a'; name],
                    suffix,
                ]
                .concat();
                let data = [b"d4:info".as_slice(), info.as_slice(), b"e"].concat();
                assert_eq!(data.len(), target);
                body = body
                    .replace("__META_BYTES__", &STANDARD.encode(data))
                    .replace("__META_HASH__", &format!("{:x}", Sha1::digest(info)));
            }
            if let Some(bytes) = step["padBodyBytes"].as_u64() {
                body.push_str(&" ".repeat((bytes as usize).saturating_sub(body.len())));
            }
            let status = step["status"].as_u64().unwrap_or(200) as u16;
            bridge
                .accept(
                    status,
                    body,
                    text(step.get("observation").unwrap_or(&root["observation"])),
                )
                .map(|out| serde_json::from_str(&out).unwrap())
        }
        "authorize" => bridge
            .authorize(clock)
            .map(|out| serde_json::from_str(&out).unwrap()),
        "metadata" => bridge
            .metadata_matches(text(&step["facts"]), clock)
            .map(Value::Bool),
        "metadataTyped" => {
            let facts = &step["facts"];
            bridge
                .metadata_matches_native(
                    facts["infoHash"].as_str().unwrap().into(),
                    facts["fileIndex"].as_u64().unwrap() as u32,
                    facts["fileCount"].as_u64().unwrap() as u32,
                    facts["selectedFileSize"].as_u64().unwrap(),
                    facts["validatedV1Metadata"].as_bool().unwrap(),
                    clock,
                )
                .map(Value::Bool)
        }
        "private" => {
            let hash = bridge.private_info_hash(clock.clone())?;
            let kind = bridge.private_input_kind(clock.clone())?;
            let input = bridge.private_input_value(clock.clone())?;
            let index = bridge.private_file_index(clock.clone())?;
            let size = bridge.private_expected_file_size(clock)?;
            Ok(Value::Bool(
                hash == step
                    .get("infoHash")
                    .unwrap_or(&root["infoHash"])
                    .as_str()
                    .unwrap()
                    && kind
                        == step
                            .get("inputKind")
                            .unwrap_or(&root["inputKind"])
                            .as_str()
                            .unwrap()
                    && input
                        == step
                            .get("inputValue")
                            .unwrap_or(&root["inputValue"])
                            .as_str()
                            .unwrap()
                    && u64::from(index) == root["fileIndex"].as_u64().unwrap()
                    && size == root["expectedFileSize"].as_u64(),
            ))
        }
        "debug" => Ok(json!(format!("{bridge:?}"))),
        "invalidate" => bridge.invalidate().map(|_| Value::Null),
        "normalize" => normalize(
            step["kind"].as_str().unwrap().into(),
            step["input"].as_str().unwrap().into(),
            "https://fixture.invalid".into(),
        )
        .map(|out| serde_json::from_str(&out).unwrap()),
        _ => Err(CoreError::InvalidInput),
    }
}
pub fn verify(case: &Value, results: &[Value]) {
    let expected = case["expected"].as_array().unwrap();
    assert_eq!(results.len(), expected.len(), "{}", case["name"]);
    for (got, want) in results.iter().zip(expected) {
        assert_eq!(got["ok"], want["ok"], "{}: {}", case["name"], got);
        if let Some(error) = want.get("error") {
            assert_eq!(&got["error"], error, "{}", case["name"]);
        }
        if let Some(status) = want.get("status") {
            assert_eq!(&got["result"]["status"], status, "{}", case["name"]);
        }
        if let Some(deadline) = want.get("deadline") {
            assert_eq!(
                &got["result"]["deadlineMillis"], deadline,
                "{}",
                case["name"]
            );
        }
        if let Some(value) = want.get("value") {
            assert_eq!(&got["result"], value, "{}", case["name"]);
        }
        let printable = got.to_string();
        for secret in [
            root_secret(),
            "magnet:?",
            "metainfo_base64",
            "private.invalid",
            "private_value",
            "grant_fixture",
        ] {
            assert!(
                !printable.contains(secret),
                "private output in {}",
                case["name"]
            );
        }
    }
}
fn root_secret() -> &'static str {
    "0000000000000000000000000000000000000000"
}
pub fn run_vectors() -> Vec<Value> {
    let root = corpus();
    root["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let bridge =
                NativeTorrentBridge::new(text(case.get("context").unwrap_or(&root["context"])));
            let results = match bridge {
                Ok(bridge) => case["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|step| outcome(run_step(&bridge, &root, step)))
                    .collect::<Vec<_>>(),
                Err(error) => vec![outcome(Err(error))],
            };
            verify(case, &results);
            json!({"name":case["name"],"results":results})
        })
        .collect()
}
