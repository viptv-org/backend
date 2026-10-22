//! SRC-BEST-MATCH-001 normalized release facts from realistic add-on streams.
use serde_json::{Value, json};
use viptv_engine::stremio::{
    ReleaseFacts, read_streams, source_audio_languages, source_release_facts, stream_delivery,
};

fn facts(source: Value) -> ReleaseFacts {
    source_release_facts(&source)
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}
#[track_caller]
fn expect(
    source: Value,
    release_type: Option<&str>,
    three_d: bool,
    dynamic_range: &[&str],
    audio_flags: &[&str],
    languages: &[&str],
) {
    let actual = facts(source.clone());
    assert_eq!(actual.release_type.as_deref(), release_type, "{source}");
    assert_eq!(actual.three_d, three_d, "{source}");
    assert_eq!(actual.dynamic_range, strings(dynamic_range), "{source}");
    assert_eq!(actual.audio_flags, strings(audio_flags), "{source}");
    assert_eq!(
        source_audio_languages(&source),
        strings(languages),
        "{source}"
    );
}

#[test]
fn movie_and_series_release_types_follow_ptt_precedence() {
    expect(
        json!({"name":"Provider\n4K","behaviorHints":{"filename":"Dune.Part.Two.2024.2160p.MAX.WEB-DL.DDP5.1.Atmos.DV.HDR10.H.265-FLUX.mkv"}}),
        Some("web_dl"),
        false,
        &["dv", "hdr10"],
        &[],
        &[],
    );
    expect(
        json!({"title":"Oppenheimer.2023.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.TrueHD.Atmos.7.1-FraMeSToR"}),
        Some("remux"),
        false,
        &["dv", "hdr"],
        &[],
        &[],
    );
    expect(
        json!({"title":"Interstellar 2014 1080p BDRemux AVC DTS-HD MA 5.1"}),
        Some("remux"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Avatar.2009.1080p.3D.BluRay.Half-SBS.x264.DTS-HD.MA.5.1-FGT"}),
        Some("bluray"),
        true,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Avatar 2009 1080p BluRay H-OU"}),
        Some("bluray"),
        true,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Arrival.2016.1080p.BDRip.x265-GRP"}),
        Some("bluray"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Arrival.2016.720p.BRRip.x264-GRP"}),
        Some("bluray"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"The.Bear.S03.COMPLETE.1080p.HULU.WEBRip.x265.10bit-Silence"}),
        Some("webrip"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Severance.S02E01.1080p.WEB.h264-ETHEL"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Show.S01E01.1080p.AMZN.WEBDL.DDP5.1.H.264"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Jeopardy.2024.10.21.720p.HDTV.x264-NTb"}),
        Some("hdtv"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Match.of.the.Day.2024.PDTV.x264"}),
        Some("hdtv"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Friends.S01.DVDRip.XviD-SAiNTS"}),
        Some("dvd"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Old.Movie.1985.DVD5.PAL"}),
        Some("dvd"),
        false,
        &[],
        &[],
        &[],
    );
    // HDRip is a rip, not high dynamic range; DVD is not Dolby Vision.
    expect(
        json!({"title":"Movie.2023.HDRip.XviD.AC3-EVO"}),
        Some("other_rip"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Movie.2019.DVDRip.x264"}),
        Some("dvd"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Movie 2023 1080p"}),
        None,
        false,
        &[],
        &[],
        &[],
    );
}

#[test]
fn camera_releases_and_their_false_positives() {
    for title in [
        "Deadpool.and.Wolverine.2024.1080p.CAM.x264-Will1869",
        "Movie 2024 HDCAM x264",
        "Movie.2024.1080p.HDTS.x264",
        "Movie.2024.TS.x264-GRP",
        "Movie.2024.TELESYNC.x264",
        "Movie.2024.HDTC.x264",
        "Movie.2024.DVDScr.XviD",
        "Movie.2024.SCREENER.x264",
        "Movie.2024.R5.LiNE.XviD",
    ] {
        assert_eq!(
            facts(json!({"title":title})).release_type.as_deref(),
            Some("cam"),
            "{title}"
        );
    }
    // Torrentio puts the camera source in its quality label.
    let torrentio = json!({"name":"Torrentio\nTeleSync","title":"Movie.2024.1080p.x264-GRP\n👤 120 💾 1.4 GB ⚙️ 1337x"});
    assert_eq!(facts(torrentio).release_type.as_deref(), Some("cam"));
    for title in [
        "Cam.2018.1080p.WEB-DL.x264",
        "Movie.2024.1080p.WEB-DL.DTS.x264",
        "Recording.2024.1080p.WEB.ts",
        "Movie.2024.1080p.WEB.TS",
    ] {
        assert_ne!(
            facts(json!({"title":title})).release_type.as_deref(),
            Some("cam"),
            "{title}"
        );
    }
}

#[test]
fn torrentio_name_title_pairs() {
    let source = json!({
        "name":"Torrentio\n4k DV | HDR10+ | HDR",
        "title":"The.Last.of.Us.S02E01.2160p.MAX.WEB-DL.DDP5.1.Atmos.DV.HDR10Plus.H.265-FLUX\n👤 42 💾 7.1 GB ⚙️ ThePirateBay\nDual Audio / 🇬🇧",
        "infoHash":"0123456789abcdef0123456789abcdef01234567",
        "behaviorHints":{"bingeGroup":"torrentio|4k|DV|HDR10+|HDR|WEB-DL"}
    });
    let actual = facts(source.clone());
    assert_eq!(actual.release_type.as_deref(), Some("web_dl"));
    assert_eq!(actual.dynamic_range, strings(&["dv", "hdr10_plus", "hdr"]));
    assert_eq!(actual.audio_flags, strings(&["dual"]));
    assert_eq!(actual.seeders, Some(42));
    assert_eq!(source_audio_languages(&source), strings(&["en"]));
    assert_eq!(stream_delivery(&source), "torrent");

    // Torrentio hides English when it is the only language: unmarked.
    let unmarked = json!({"name":"Torrentio\n1080p","title":"Movie.2023.1080p.WEB-DL.x264-GRP\n👤 9 💾 2.1 GB ⚙️ YTS"});
    let actual = facts(unmarked.clone());
    assert_eq!((actual.audio_flags.len(), actual.seeders), (0, Some(9)));
    assert!(source_audio_languages(&unmarked).is_empty());

    expect(
        json!({"name":"Torrentio\n1080p","title":"Movie.2023.MULTi.1080p.WEB.H264-GRP\n👤 3 💾 2.8 GB ⚙️ Torrent9\nMulti Audio / 🇫🇷"}),
        Some("web_dl"),
        false,
        &[],
        &["multi"],
        &["fr"],
    );
    expect(
        json!({"name":"Torrentio\n1080p","title":"Movie.2023.FRENCH.1080p.WEB.H264-GRP\n👤 3 💾 2.8 GB ⚙️ Torrent9\n🇫🇷"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &["fr"],
    );
    // Anime: Japanese removed, `Dubbed` added for every dual-audio release.
    expect(
        json!({"name":"Torrentio\n1080p","title":"[Judas] Jujutsu Kaisen (Season 2) [1080p][HEVC x265 10bit][Dual-Audio][Multi-Subs]\n👤 88 💾 9 GB ⚙️ Nyaa\nDubbed / Multi Subs / Dual Audio"}),
        None,
        false,
        &[],
        &["dual", "dubbed"],
        &[],
    );
    // A Multi Subs label no longer hides the audio flags on the same line.
    expect(
        json!({"name":"Torrentio\n1080p","title":"Show.S01E01.1080p.WEB.h264-GRP\n👤 5 💾 1 GB ⚙️ EZTV\nMulti Subs / 🇬🇧"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &["en"],
    );
    expect(
        json!({"name":"Torrentio\n1080p","title":"Show.S01E01.1080p.WEB.h264-GRP\n👤 5 💾 1 GB ⚙️ EZTV\nMulti Subs"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &[],
    );
    // VOSTFR is French subtitles; PTT (and Torrentio's 🇫🇷) list it as French.
    let vostfr = json!({"name":"Torrentio\n1080p","title":"Movie.2023.VOSTFR.1080p.WEB.H264-GRP\n👤 7 💾 1 GB ⚙️ YggTorrent\n🇫🇷"});
    expect(vostfr.clone(), Some("web_dl"), false, &[], &[], &[]);
    assert_eq!(facts(vostfr).seeders, Some(7));
    expect(
        json!({"title":"Movie.2023.MULTi.VFF.VOSTFR.1080p.BluRay.x264\nMulti Audio / 🇫🇷"}),
        Some("bluray"),
        false,
        &[],
        &["multi"],
        &["fr"],
    );
    expect(
        json!({"name":"Torrentio\n1080p 3D","title":"Movie.2012.1080p.BluRay.x264"}),
        Some("bluray"),
        true,
        &[],
        &[],
        &[],
    );
}

#[test]
fn anime_audio_markers_are_flags_not_languages() {
    // SubsPlease/Erai-raws style subtitle-only releases carry no audio evidence.
    expect(
        json!({"title":"[SubsPlease] Sousou no Frieren - 28 (1080p) [ABCD1234].mkv"}),
        None,
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"[Erai-raws] Kusuriya no Hitorigoto 2nd Season - 01 [1080p CR WEB-DL AVC AAC][MultiSub][ABCDEF12]"}),
        Some("web_dl"),
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"[Erai-raws] Show - 01 [1080p][Multiple Subtitle]"}),
        None,
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Spy x Family S01 1080p BluRay 10-Bit [Dual Audio] FLAC2.0 x265-YURASUKA"}),
        Some("bluray"),
        false,
        &[],
        &["dual"],
        &[],
    );
    expect(
        json!({"title":"Cowboy Bebop (1998) (Dual Audio) [1080p]"}),
        None,
        false,
        &[],
        &["dual"],
        &[],
    );
    expect(
        json!({"title":"Show S01 1080p Dual-Subs"}),
        None,
        false,
        &[],
        &[],
        &[],
    );
    expect(
        json!({"title":"Show S01 1080p Tri Audio WEB-DL"}),
        Some("web_dl"),
        false,
        &[],
        &["multi"],
        &[],
    );
    expect(
        json!({"title":"Demon Slayer S04E01 1080p CR WEB-DL AAC2.0 H 264 English Dub"}),
        Some("web_dl"),
        false,
        &[],
        &["dubbed"],
        &["en"],
    );
    expect(
        json!({"title":"Naruto Shippuden 001 Dublado 720p"}),
        None,
        false,
        &[],
        &["dubbed"],
        &[],
    );
    expect(
        json!({"title":"Movie 2023 1080p WEBRip DUBBED x264"}),
        Some("webrip"),
        false,
        &[],
        &["dubbed"],
        &[],
    );
    // Subtitled/hardsub markers are never audio evidence.
    expect(
        json!({"title":"Movie 2023 1080p WEBRip HC Subbed"}),
        Some("webrip"),
        false,
        &[],
        &[],
        &[],
    );
}

#[test]
fn declared_fields_win_and_invalid_declarations_fall_back() {
    let declared = json!({
        "title":"Movie.2024.CAM.HDR.x264",
        "release_type":"remux","three_d":false,
        "dynamic_range":["hdr10","dv","hdr10"],"audio_flags":["dubbed"],"seeders":12
    });
    let actual = facts(declared);
    assert_eq!(actual.release_type.as_deref(), Some("remux"));
    assert_eq!(actual.dynamic_range, strings(&["dv", "hdr10"]));
    assert_eq!(actual.audio_flags, strings(&["dubbed"]));
    assert_eq!(actual.seeders, Some(12));
    let invalid = json!({"title":"Movie.2024.1080p.WEBRip.HDR.Dual.Audio","release_type":"vhs","dynamic_range":["sdr"],"audio_flags":"dual"});
    let actual = facts(invalid);
    assert_eq!(actual.release_type.as_deref(), Some("webrip"));
    assert_eq!(actual.dynamic_range, strings(&["hdr"]));
    assert_eq!(actual.audio_flags, strings(&["dual"]));
    assert_eq!(actual.seeders, None);
}

#[test]
fn delivery_comes_from_transport_and_reaches_native_streams() {
    assert_eq!(
        stream_delivery(&json!({"url":"https://media.example/a.mkv","title":"magnet torrent"})),
        "direct"
    );
    assert_eq!(
        stream_delivery(
            &json!({"url":"magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"})
        ),
        "torrent"
    );
    assert_eq!(
        stream_delivery(&json!({"url":"https://tracker.example/file.torrent?x=1"})),
        "torrent"
    );
    let raw = json!({"streams":[{"url":"https://media.example/a.mkv","name":"Debrid\n4K","title":"Movie.2024.2160p.WEB-DL.DV.Dual.Audio\n👤 4"}]});
    let stream = &read_streams(&raw.to_string()).unwrap()[0];
    assert_eq!(stream.delivery, "direct");
    assert_eq!(stream.release_facts.release_type.as_deref(), Some("web_dl"));
    assert_eq!(stream.release_facts.dynamic_range, strings(&["dv"]));
    assert_eq!(stream.release_facts.audio_flags, strings(&["dual"]));
    assert_eq!(stream.release_facts.seeders, Some(4));
}
