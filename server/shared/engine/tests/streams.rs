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
