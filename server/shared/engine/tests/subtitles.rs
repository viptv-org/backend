use serde_json::json;
use viptv_engine::stremio::*;

#[test]
fn subtitle_requests_negotiate_resources_and_encode_file_evidence_once() {
    let manifest =
        json!({"resources":[{"name":"subtitles","types":["series"],"idPrefixes":["tt"]}]});
    let extras = SubtitleExtras {
        video_hash: Some("0123456789abcdef".into()),
        video_size: Some(1200),
        filename: Some("Film & / title.mkv".into()),
    };
    let url = subtitle_url(
        "https://addon.example/secret/manifest.json",
        &manifest,
        "series",
        "tt1:2:3",
        &extras,
    )
    .unwrap()
    .unwrap();
    assert!(url.contains("/subtitles/series/tt1:2:3/"));
    assert!(url.ends_with(
        "videoHash=0123456789abcdef&videoSize=1200&filename=Film+%26+%2F+title.mkv.json"
    ));
    assert!(!url.contains("%252F"));
    assert!(
        subtitle_url(
            "https://addon.example/manifest.json",
            &manifest,
            "movie",
            "tt1",
            &extras
        )
        .unwrap()
        .is_none()
    );
    assert!(
        subtitle_url(
            "https://addon.example/manifest.json",
            &manifest,
            "series",
            "tmdb:1",
            &extras
        )
        .unwrap()
        .is_none()
    );
    let invalid = SubtitleExtras {
        video_hash: Some("invented".into()),
        ..Default::default()
    };
    assert!(
        subtitle_url(
            "https://addon.example/manifest.json",
            &manifest,
            "series",
            "tt1",
            &invalid
        )
        .is_err()
    );
}

#[test]
fn subtitles_accept_optional_provider_ids_and_reject_unsafe_or_duplicate_entries() {
    let files = read_subtitles(
        &json!({"subtitles":[
            {"lang":"ENG","url":"https://cdn.example/sub.srt"},
            {"id":"duplicate","lang":"eng","url":"https://cdn.example/sub.srt"},
            {"lang":"pt-BR","url":"https://cdn.example/pt.vtt"},
            {"lang":"eng","url":"https://user:secret@cdn.example/sub.srt"},
            {"lang":"eng","url":"file:///etc/passwd"},
            {"lang":"English","url":"https://cdn.example/bad.srt"}
        ]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].language, "eng");
    assert_eq!(files[1].language, "pt-br");
    assert!(read_subtitles("{}").is_err());
}

#[test]
fn subtitle_text_converts_srt_and_rejects_non_text_or_invalid_timing() {
    let vtt = subtitle_vtt("\u{feff}1\r\n00:01:02,003 --> 00:01:04,005\r\nHello\r\nworld\r\n\r\n2\r\n00:01:05,000 --> 00:01:06,000\r\nNext").unwrap();
    assert_eq!(
        vtt,
        "WEBVTT\n\n00:01:02.003 --> 00:01:04.005\nHello\nworld\n\n00:01:05.000 --> 00:01:06.000\nNext\n\n"
    );
    assert_eq!(subtitle_vtt(&vtt).unwrap(), vtt);
    for invalid in [
        "",
        "<html>upstream error</html>",
        "1\n00:01:99,000 --> 00:02:01,000\nWrong",
        "1\n00:02:00,000 --> 00:01:00,000\nWrong",
        "WEBVTT\0",
    ] {
        assert!(subtitle_vtt(invalid).is_err());
    }
}
