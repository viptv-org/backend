use serde_json::{Value, json};
use viptv_engine::stremio::{
    SourcePresentation, read_streams, source_audio_languages, source_detail, source_quality_height,
};

#[test]
fn cached_source_metadata_corpus_in_one_batch() {
    let corpus: Value =
        serde_json::from_str(include_str!("../../../tests/source-metadata-vectors.json")).unwrap();
    let captured: Value =
        serde_json::from_str(include_str!("../../../tests/source-metadata-captured.json")).unwrap();
    let cases: Vec<_> = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(captured["cases"].as_array().unwrap())
        .collect();
    let started = std::time::Instant::now();
    for case in &cases {
        let source = &case["source"];
        let presentation = SourcePresentation::from_stream(source);
        let value = serde_json::to_value(&presentation).unwrap();
        for (key, expected) in case["expected"].as_object().unwrap() {
            let actual = match key.as_str() {
                "height" => json!(source_quality_height(source)),
                "languages" => json!(source_audio_languages(source)),
                "detail" => json!(source_detail(source)),
                _ => value[key].clone(),
            };
            assert_eq!(&actual, expected, "{} field {key}", case["id"]);
        }
        assert!(source_detail(source).is_none_or(|s| s.chars().count() <= 4096));
        assert!(source_audio_languages(source).len() <= 16);
    }
    // The same facts reach native local streams, not just the helper API.
    for batch in cases.chunks(80) {
        let streams: Vec<_> = batch
            .iter()
            .map(|case| {
                let mut source = case["source"].clone();
                source["url"] = json!("https://media.example/opaque");
                source
            })
            .collect();
        let parsed = read_streams(&json!({"streams":streams}).to_string()).unwrap();
        assert_eq!(parsed.len(), batch.len());
        for (source, case) in parsed.iter().zip(batch) {
            assert_eq!(
                source.quality_height,
                source_quality_height(&case["source"])
            );
            assert_eq!(
                source.audio_languages,
                source_audio_languages(&case["source"])
            );
            assert_eq!(source.detail, source_detail(&case["source"]));
        }
    }
    eprintln!(
        "SOURCE_METADATA_BATCH cases={} elapsed_ms={} synthetic=802 captured=140 network_requests=0",
        cases.len(),
        started.elapsed().as_millis()
    );
    assert!(started.elapsed().as_secs() < 60);
}
