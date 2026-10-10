use serde_json::{Value, json};
use viptv_core::{CoreError, TorrentRuntimeBridge};

pub fn corpus() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/torrent-runtime-bridge-vectors.json"
    ))
    .unwrap()
}
pub fn run(root: &Value, case: &Value) -> Value {
    let context = case.get("context").unwrap_or(&root["context"]);
    let bridge = match TorrentRuntimeBridge::new(context.to_string()) {
        Ok(bridge) => bridge,
        Err(_) => return json!({"constructs":false,"steps":[]}),
    };
    let steps: Vec<Value> = case["steps"].as_array().unwrap().iter().map(|step| {
        let clock = step.get("clock").unwrap_or(&root["clock"]).to_string();
        let result: Result<Value, CoreError> = (|| match step["action"].as_str().unwrap() {
            "accept" | "measured" => {
                let body = step["body"].as_str().map(str::to_owned).unwrap_or_else(||root["body"].to_string());
                let observation = step.get("observation").unwrap_or(&root["observation"]).clone();
                let text = if step["action"] == "measured" {
                    let mut observation = observation;
                    observation["trustedWallUpperUnixMillis"] = Value::Null;
                    bridge.accept_measured_bytes(200,body.into_bytes(),observation.to_string())?
                } else { bridge.accept_bytes(200,body.into_bytes(),observation.to_string())? };
                Ok(serde_json::from_str(&text).unwrap())
            }
            "resolve" => bridge.bind_resolution(step["fileIndex"].as_u64().unwrap() as u32,step["archiveIndex"].as_u64().map(|n|n as u32),step["length"].as_u64().unwrap(),clock).map(Value::Bool),
            "authorize" => Ok(serde_json::from_str(&bridge.authorize(clock)?).unwrap()),
            "state" => Ok(serde_json::from_str(&bridge.state()?).unwrap()),
            "invalidate" => {bridge.invalidate()?;Ok(Value::Null)},
            "wall" => Ok(json!(bridge.trusted_wall_upper_unix_millis()?)),
            "debug" => Ok(json!(bridge.to_string())),
            "private" => Ok(json!({"fileIndex":bridge.private_file_index(clock.clone())?,"archiveIndex":bridge.private_archive_index(clock.clone())?,
                "trackers":bridge.private_trackers(clock.clone())?,"kind":bridge.private_input_kind(clock.clone())?,"value":bridge.private_input_value(clock)?})),
            _ => panic!("unknown fixture action"),
        })();
        match result {Ok(result) => json!({"ok":true,"result":result}),Err(_) =>json!({"ok":false})}
    }).collect();
    json!({"constructs":true,"steps":steps})
}
