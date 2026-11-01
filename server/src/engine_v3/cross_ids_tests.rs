use super::cross_ids::{CrossIds, Service};
use axum::{http::StatusCode, routing::get, Router};
use serde_json::json;
use viptv_engine::identity::ExternalId;

async fn source(body: &'static str) -> String {
    let routes = Router::new()
        .route(
            "/",
            get(move || async move { (StatusCode::OK, body) }),
        )
        .fallback(|method: axum::http::Method| async move {
            if method.is_safe() {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let base = format!("http://{address}");
    std::mem::forget(task);
    base
}

fn ids(pairs: &[(&str, &str)]) -> Vec<ExternalId> {
    pairs
        .iter()
        .map(|(namespace, value)| ExternalId {
            namespace: (*namespace).into(),
            value: (*value).into(),
        })
        .collect()
}

#[tokio::test]
async fn a_mal_id_resolves_to_anidb_tvdb_tmdb_and_imdb_through_the_fribb_file() {
    let base = source(
        r#"[{"type":"TV","anidb_id":5459,"anilist_id":3269,"kitsu_id":2895,"mal_id":3269,
            "imdb_id":["tt1164545"],"themoviedb_id":{"tv":8864},"tvdb_id":79099,
            "season":{"tvdb":0,"tmdb":0},"episode_offset":{"tvdb":11,"tmdb":11}}]"#,
    )
    .await;
    let service = Service::fixture(&base);
    assert_eq!(
        service.resolve(&ids(&[("mal", "3269")])).await,
        CrossIds {
            mal: Some(3269),
            anilist: Some(3269),
            anidb: Some(5459),
            tvdb: Some(79099),
            tmdb: Some(8864),
            imdb: Some("tt1164545".into()),
            season: None,
            episode_offset: Some(11),
        }
    );
}

#[tokio::test]
async fn an_anilist_id_resolves_to_mal_through_the_fribb_file() {
    let base = source(r#"[{"type":"TV","anidb_id":5459,"anilist_id":3269,"mal_id":3269,"tvdb_id":79099}]"#).await;
    let service = Service::fixture(&base);
    assert_eq!(
        service.resolve(&ids(&[("anilist", "3269")])).await,
        CrossIds {
            mal: Some(3269),
            anilist: Some(3269),
            anidb: Some(5459),
            tvdb: Some(79099),
            tmdb: None,
            imdb: None,
            season: None,
            episode_offset: None,
        }
    );
}

#[tokio::test]
async fn an_anidb_id_resolves_back_to_mal_through_the_fribb_file() {
    let base = source(
        r#"[{"type":"TV","anidb_id":5459,"mal_id":3269,"tvdb_id":79099}]"#,
    )
    .await;
    let service = Service::fixture(&base);
    assert_eq!(
        service.resolve(&ids(&[("anidb", "5459")])).await,
        CrossIds {
            mal: Some(3269),
            anilist: None,
            anidb: Some(5459),
            tvdb: Some(79099),
            tmdb: None,
            imdb: None,
            season: None,
            episode_offset: None,
        }
    );
}

#[tokio::test]
async fn ids_that_are_missing_from_the_file_survive_alongside_the_direct_ids() {
    let base = source(r#"[{"type":"TV","anidb_id":5459,"mal_id":3269,"tvdb_id":79099}]"#).await;
    let service = Service::fixture(&base);
    assert_eq!(
        service
            .resolve(&ids(&[("tvdb", "91021"), ("tmdb", "44298")]))
            .await,
        CrossIds {
            mal: None,
            anilist: None,
            anidb: None,
            tvdb: Some(91021),
            tmdb: Some(44298),
            imdb: None,
            season: None,
            episode_offset: None,
        }
    );
}

#[tokio::test]
async fn an_unreachable_mapping_keeps_the_direct_ids() {
    let service = Service::fixture("http://127.0.0.1:1");
    assert_eq!(
        service.resolve(&ids(&[("mal", "3269")])).await,
        CrossIds {
            mal: Some(3269),
            anilist: None,
            anidb: None,
            tvdb: None,
            tmdb: None,
            imdb: None,
            season: None,
            episode_offset: None,
        }
    );
}