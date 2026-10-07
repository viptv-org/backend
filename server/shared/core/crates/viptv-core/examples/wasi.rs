//! Persistent JSON-lines shell for WASI and the wasm2brs Roku experiment.
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufWriter, Read, Write},
};
use viptv_core::{CoreBridge, CoreError};

// HTTP effects encode a <=2 MiB body as JSON bytes, which can need ~8 MiB.
const MAX_REQUEST_BYTES: u64 = 12 * 1024 * 1024;

fn dispatch(core: &mut CoreBridge, input: &[u8]) -> Result<String, CoreError> {
    // Borrow nested JSON rather than building, serializing and reparsing its tree.
    let request: BTreeMap<String, &serde_json::value::RawValue> =
        serde_json::from_slice(input).map_err(|_| CoreError::InvalidInput)?;
    let field = |key: &str| {
        request
            .get(key)
            .map(|value| value.get())
            .ok_or(CoreError::InvalidInput)
    };
    let text = |key: &str| {
        serde_json::from_str::<String>(field(key)?).map_err(|_| CoreError::InvalidInput)
    };
    match text("op")?.as_str() {
        "normalize" => viptv_core::normalize(
            text("kind")?,
            field("input")?.to_owned(),
            text("origin").unwrap_or_default(),
        ),
        "vizio_platform_support" => Ok(viptv_core::vizio_platform_support(text("platform")?)),
        "update" | "process_event" => core.update(field("event")?.to_owned()),
        "resolve" | "handle_response" => {
            let id = serde_json::from_str(field("id")?).map_err(|_| CoreError::InvalidInput)?;
            core.resolve(id, field("result")?.to_owned())
        }
        "view" => core.view(),
        "reset" => {
            *core = CoreBridge::new();
            core.view()
        }
        _ => Err(CoreError::InvalidInput),
    }
}

fn main() -> io::Result<()> {
    let mut core = CoreBridge::new();
    let mut input = io::stdin().lock();
    let mut output = BufWriter::new(io::stdout().lock());
    let mut line = Vec::new();
    loop {
        line.clear();
        if input
            .by_ref()
            .take(MAX_REQUEST_BYTES + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        let oversized = line.len() as u64 > MAX_REQUEST_BYTES;
        let result = if oversized {
            Err(CoreError::InvalidInput)
        } else {
            dispatch(&mut core, &line)
        };
        match result {
            // Core outputs already are JSON: do not parse and serialize them twice.
            Ok(result) => writeln!(output, "{{\"ok\":true,\"result\":{result}}}")?,
            Err(error) => writeln!(
                output,
                "{}",
                serde_json::json!({"ok": false, "error": error.to_string()})
            )?,
        }
        // Resolve effects before requesting the next input; never wait for EOF.
        output.flush()?;
        if oversized {
            break;
        }
    }
    Ok(())
}
