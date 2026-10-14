use viptv_engine::stremio::read_streams;

#[test]
fn only_direct_sources_with_bounded_headers_are_offered() {
    let data = r#"{"streams":[{"name":"HLS","url":"https://media.example/stream.m3u8","behaviorHints":{"proxyHeaders":{"request":{"Referer":"https://addon.example/"}}}},{"name":"Torrent","infoHash":"abc"},{"name":"Unsafe","url":"https://user:password@example.com/file.mp4"}]}"#;
    let streams = read_streams(data).unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].label, "HLS");
    assert_eq!(streams[0].headers[0].0, "Referer");
    assert!(!streams[0].url.contains("password"));
}

#[test]
fn malformed_or_private_header_lines_cannot_enter_the_player() {
    let bad = r#"{"streams":[{"url":"https://media.example/v.mp4","behaviorHints":{"proxyHeaders":{"request":{"Authorization":"Bearer x\r\nInjected: yes"}}}}]}"#;
    assert!(read_streams(bad).unwrap().is_empty());
    assert!(read_streams("{}").is_err());
}

#[test]
fn structured_source_presentation_is_additive_and_never_inferred_from_a_release_name() {
    let raw=serde_json::json!({"streams":[
        {"name":"Movie 2160p HEVC AAC 5GB","url":"https://media.example/a.mkv"},
        {"name":"Actual release","url":"https://media.example/b","presentation":{"provider_name":"Declared provider","source_id":"declared","delivery_format":"progressive","container":"mkv","video_codec":"HEVC","audio_codecs":["AAC"],"size_bytes":5000}},
        {"url":"https://media.example/c","presentation":{"provider_name":"https://private.invalid/token","video_codec":"AVC\r\nprivate","audio_codecs":["AAC","https://private.invalid"],"delivery_format":"guessed","size_bytes":0}}
    ]});
    let streams=read_streams(&raw.to_string()).unwrap();
    assert!(streams[0].presentation.is_none());
    assert_eq!(streams[1].presentation.as_ref().unwrap().size_bytes,Some(5000));
    assert_eq!(streams[1].presentation.as_ref().unwrap().container.as_deref(),Some("mkv"));
    let third=streams[2].presentation.as_ref().unwrap();
    assert!(third.provider_name.is_none() && third.video_codec.is_none() && third.delivery_format.is_none() && third.size_bytes.is_none());
    assert_eq!(third.audio_codecs,vec!["AAC"]);
    let old: viptv_engine::stremio::SourceCandidate=serde_json::from_value(serde_json::json!({"id":"opaque","provider_id":"addon:1","label":"old","detail":null})).unwrap();
    assert!(old.presentation.is_none() && old.audio_languages.is_empty());
}
