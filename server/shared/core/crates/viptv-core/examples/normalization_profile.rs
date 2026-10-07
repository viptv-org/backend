use serde::Deserialize;
use serde_json::{Value, json};
use std::{hint::black_box, time::Instant};
use viptv_core::{domain, dto};

fn measure(name: &str, iterations: u32, mut operation: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    println!(
        "{name}: {:.3} us/call",
        start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
    );
}
fn main() {
    let input = Value::Array(
        (0..10)
            .map(|i| {
                json!({
                    "id": format!("catalog-{i}"), "type": "anime", "name": format!("Anime {i}"),
                    "addon_id": 4, "addon_name": "Fixture"
                })
            })
            .collect(),
    );
    let text = serde_json::to_string(&input).unwrap();
    let normalized = domain::normalize_value("catalogs", &input, "https://example.test").unwrap();
    let iterations = 5000;
    measure("parse", iterations, || {
        black_box(serde_json::from_str::<Value>(black_box(&text)).unwrap());
    });
    measure("normalize", iterations, || {
        black_box(
            domain::normalize_value("catalogs", black_box(&input), "https://example.test").unwrap(),
        );
    });
    measure("sanitize", iterations, || {
        black_box(domain::clean(black_box(&input)));
    });
    measure("validate", iterations, || {
        black_box(Vec::<dto::Catalog>::deserialize(black_box(&normalized)).unwrap());
    });
    measure("serialize", iterations, || {
        black_box(serde_json::to_string(black_box(&normalized)).unwrap());
    });
    measure("total", iterations, || {
        black_box(
            viptv_core::normalize(
                "catalogs".into(),
                text.clone(),
                "https://example.test".into(),
            )
            .unwrap(),
        );
    });
}
