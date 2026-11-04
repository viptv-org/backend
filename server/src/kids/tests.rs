use super::*;
use crate::test_support::request;
use axum::{
    extract::Path as AxumPath,
    routing::{any, get},
    Json, Router,
};
use serde_json::json;

async fn fixture() -> (App, tokio::task::JoinHandle<()>) {
    let mut a = crate::auth_integration_tests::fixture();
    a.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let meta = get(|AxumPath((kind, id)): AxumPath<(String, String)>| async move {
        let id = id.trim_end_matches(".json").to_owned();
        let mut meta = json!({"id":id,"type":kind,"name":format!("Title {id}")});
        match id.as_str() {
            "pg" => meta["certification"] = json!("PG"),
            "pg13" => meta["certification"] = json!("PG-13"),
            "r" => meta["certification"] = json!("R"),
            "adult" => meta["certification"] = json!("NC-17"),
            "conflict" => {
                meta["contentRating"] = json!("TV-Y");
                meta["certification"] = json!("R");
            }
            "anime-r18" => {
                meta["ageRating"] = json!("R18+");
                meta["genres"] = json!(["Animation"]);
            }
            "adult-plus" => meta["certification"] = json!("18+"),
            "mystery" => meta["certification"] = json!("Zz-Restricted"),
            "hentai" => meta["genres"] = json!(["Animation", " HENTAI "]),
            "blank" => meta["certification"] = json!("  "),
            "cartoon" => meta["contentRating"] = json!("TV-Y"),
            "teen-series" => meta["contentRating"] = json!("TV-14"),
            _ => {}
        }
        if id == "cartoon" {
            meta["videos"] = json!([{"id":"cartoon-1","season":1,"episode":1,"title":"One"}]);
        }
        if id == "teen-series" {
            meta["videos"] = json!([{"id":"teen-1","season":1,"episode":1,"title":"One"}]);
        }
        Json(json!({"meta":meta}))
    });
    let metas = get(|AxumPath((kind, _)): AxumPath<(String, String)>| async move {
        let ids: Vec<&str> = if kind == "series" {
            vec!["cartoon", "teen-series"]
        } else {
            vec![
                "pg",
                "pg13",
                "r",
                "adult",
                "unrated",
                "conflict",
                "anime-r18",
                "adult-plus",
                "mystery",
                "hentai",
                "blank",
            ]
        };
        let items = ids
            .into_iter()
            .map(|id| json!({"id":id,"type":kind,"name":format!("Title {id}")}))
            .collect::<Vec<_>>();
        Json(json!({"metas":items}))
    });
    let service = Router::new()
        .route("/", any(|| async { "ok" }))
        .route("/meta/:kind/:id", meta)
        .route("/catalog/:kind/:catalog/:extra", metas.clone())
        .route("/catalog/:kind/:catalog", metas);
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO addons(account_id,name,manifest_url,manifest) VALUES(1,'Fixture',?1,?2)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Fixture","resources":["meta","catalog"],"types":["movie","series"],"catalogs":[{"id":"top","type":"movie"},{"id":"top-series","type":"series"}]}).to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&a);
    crate::test_support::send_port_probe(address).await;
    (a, task)
}

async fn pageable_fixture() -> (App, tokio::task::JoinHandle<()>) {
    let mut a = crate::auth_integration_tests::fixture();
    a.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let catalog = get(
        |AxumPath((kind, _catalog, extra)): AxumPath<(String, String, String)>| async move {
            let skip = extra
                .trim_end_matches(".json")
                .strip_prefix("skip=")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let metas = (skip..(skip + 24).min(120))
                .map(|i| json!({ "id": format!("tt{i}"), "type": kind, "name": format!("Title {i}") }))
                .collect::<Vec<_>>();
            Json(json!({ "metas": metas }))
        },
    );
    let service = Router::new()
        .route("/", any(|| async { "ok" }))
        .route("/catalog/:kind/:catalog/:extra", catalog);
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    {
        let db = a.db.lock().unwrap();
        db.execute(
            "INSERT INTO addons(account_id,name,manifest_url,manifest) VALUES(1,'Fixture',?1,?2)",
            params![
                format!("http://{address}/manifest.json"),
                json!({"id":"fixture","name":"Fixture","resources":["catalog"],"types":["movie"],"catalogs":[{"id":"top","type":"movie","extra":[{"name":"skip"}]}]})
                    .to_string()
            ],
        )
        .unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&a);
    crate::test_support::send_port_probe(address).await;
    (a, task)
}

async fn catalog(a: &App, kind: &str) -> Vec<Value> {
    let (status, page) = request(
        a,
        "member-token-1",
        "GET",
        &format!("/api/discover?type={kind}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["cards"].as_array().unwrap().clone()
}

fn card_of(cards: &[Value], id: &str) -> Value {
    cards
        .iter()
        .find(|card| card["identity"]["id"] == id)
        .unwrap_or_else(|| panic!("catalog serves {id}"))
        .clone()
}

async fn detail(a: &App, kind: &str, card: &Value) -> (StatusCode, Value) {
    let id = card["identity"]["id"].as_str().unwrap();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("metadata_origin", &card["metadata_origin"].to_string());
    query.append_pair("card", &card.to_string());
    request(
        a,
        "member-token-1",
        "GET",
        &format!("/api/meta/{kind}/{id}?{}", query.finish()),
        Value::Null,
    )
    .await
}

async fn observe(a: &App, kind: &str, cards: &[Value], id: &str) -> Value {
    let card = card_of(cards, id);
    let (status, body) = detail(a, kind, &card).await;
    assert_eq!(status, StatusCode::OK, "observe {kind}/{id}: {body}");
    body["item"].clone()
}

async fn unlock(a: &App) {
    assert_eq!(
        request(
            a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

async fn switch(a: &App, id: &str) {
    let (status, body) = request(
        a,
        "member-token-1",
        "POST",
        "/api/auth/profile",
        json!({"profile_id":id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "switch to {id}: {body}");
}

async fn leave(a: &App, pin: Option<&str>) -> (StatusCode, Value) {
    let body = match pin {
        Some(pin) => json!({"profile_id":"1","pin":pin}),
        None => json!({"profile_id":"1"}),
    };
    request(a, "member-token-1", "POST", "/api/auth/profile", body).await
}

async fn create_kids(a: &App, policy: Value) -> String {
    let (_, child) = request(
        a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Kids"}),
    )
    .await;
    let id = child["id"].as_str().unwrap().to_owned();
    assert_eq!(
        request(
            a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut body = json!({"enabled":true,"max_age":12});
    for (key, value) in policy.as_object().unwrap() {
        body[key] = value.clone();
    }
    let (status, saved) = request(
        a,
        "member-token-1",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable kids profile: {saved}");
    id
}

async fn policy(a: &App, id: &str, body: Value) -> Value {
    let (status, saved) = request(
        a,
        "member-token-1",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save kids policy: {saved}");
    saved
}

async fn catalog_ids(a: &App, kind: &str) -> Vec<String> {
    catalog(a, kind)
        .await
        .iter()
        .map(|card| card["identity"]["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn rating_ceilings_apply_per_kind_and_keep_the_numeric_fallback() {
    let (a, task) = fixture().await;
    let movies = catalog(&a, "movie").await;
    let series = catalog(&a, "series").await;
    for id in ["pg", "pg13", "r", "adult", "unrated", "conflict"] {
        observe(&a, "movie", &movies, id).await;
    }
    for id in ["cartoon", "teen-series"] {
        observe(&a, "series", &series, id).await;
    }
    let id = create_kids(&a, json!({"max_age":17,"movie_rating":"PG","series_rating":"TV-Y"})).await;
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg"]);
    assert_eq!(catalog_ids(&a, "series").await, vec!["cartoon"]);
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "pg13")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "pg")).await.0,
        StatusCode::OK
    );
    unlock(&a).await;
    switch(&a, "1").await;
    let saved = policy(&a, &id, json!({"enabled":true,"max_age":12})).await;
    assert_eq!(saved["movie_rating"], "PG");
    assert_eq!(saved["series_rating"], "TV-Y");
    let saved = policy(
        &a,
        &id,
        json!({"enabled":true,"max_age":17,"movie_rating":"PG-13","series_rating":null}),
    )
    .await;
    assert_eq!(saved["movie_rating"], "PG-13");
    assert_eq!(saved["series_rating"], Value::Null);
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "pg13"]);
    assert_eq!(
        catalog_ids(&a, "series").await,
        vec!["cartoon", "teen-series"]
    );
    unlock(&a).await;
    switch(&a, "1").await;
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":12,"movie_rating":"PG-15"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    task.abort();
}

#[tokio::test]
async fn unrated_titles_stay_hidden_until_the_profile_allows_them() {
    let (a, task) = fixture().await;
    let movies = catalog(&a, "movie").await;
    for title in ["pg", "pg13", "r", "adult", "unrated", "conflict"] {
        observe(&a, "movie", &movies, title).await;
    }
    let id = create_kids(&a, json!({"max_age":17,"movie_rating":"R"})).await;
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "pg13", "r"]);
    for title in ["unrated", "conflict"] {
        assert_eq!(
            detail(&a, "movie", &card_of(&movies, title)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    unlock(&a).await;
    switch(&a, "1").await;
    let saved = policy(&a, &id, json!({"enabled":true,"max_age":17,"allow_unrated":true})).await;
    assert_eq!(saved["allow_unrated"], true);
    switch(&a, &id).await;
    assert_eq!(
        catalog_ids(&a, "movie").await,
        vec!["pg", "pg13", "r", "unrated", "conflict"]
    );
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "unrated")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "adult")).await.0,
        StatusCode::FORBIDDEN
    );
    unlock(&a).await;
    switch(&a, "1").await;
    policy(&a, &id, json!({"enabled":true,"max_age":17,"movie_rating":"PG"})).await;
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "unrated"]);
    task.abort();
}

#[test]
fn rating_markers_map_to_minimum_ages_and_unknown_markers_do_not() {
    let cases: [(&str, i64); 40] = [
        ("G", 0),
        ("pg", 10),
        (" PG-13 ", 13),
        ("Rated R", 17),
        ("NC-17", 18),
        ("nc 17", 18),
        ("X", 18),
        ("TV-Y", 0),
        ("tv-y7-fv", 7),
        ("TV-PG", 10),
        ("TV-14", 14),
        ("TV-MA", 18),
        ("AO", 18),
        ("18", 18),
        ("18+", 18),
        ("16", 16),
        ("21+", 18),
        ("FSK 18", 18),
        ("FSK ab 12", 12),
        ("FSK16", 16),
        ("BBFC 18", 18),
        ("12A", 12),
        ("15", 15),
        ("R18", 18),
        ("PEGI 18", 18),
        ("PEGI 7", 7),
        ("KR 19", 18),
        ("KR-15", 15),
        ("JP R18", 18),
        ("R-18", 18),
        ("R15+", 15),
        ("PG12", 12),
        ("MA15+", 15),
        ("R18+", 18),
        ("US:PG-13", 13),
        ("G - All Ages", 0),
        ("PG-13 - Teens 13 or older", 13),
        ("R - 17+ (violence & profanity)", 17),
        ("R+ - Mild Nudity", 17),
        ("Rx - Hentai", 18),
    ];
    for (marker, age) in cases {
        assert_eq!(metadata::known_age(marker), Some(age), "{marker}");
    }
    for marker in ["Zz-Restricted", "Not Rated", "99", "US", "ABC"] {
        assert_eq!(metadata::known_age(marker), None, "{marker}");
    }
}

#[test]
fn present_unknown_ratings_and_adult_genres_are_restricted_but_absent_ones_are_unrated() {
    let restricted = Some(metadata::RESTRICTED_AGE);
    assert_eq!(
        metadata::assessment(&json!({"certification":"Zz-Restricted"})),
        (restricted, true)
    );
    assert_eq!(
        metadata::assessment(&json!({"ageRating":["R18+"]})),
        (restricted, true)
    );
    for genre in ["Hentai", "EROTICA", "adult", "Porn"] {
        assert_eq!(
            metadata::assessment(&json!({"certification":"PG","genres":[genre]})).0,
            restricted,
            "{genre}"
        );
    }
    for meta in [
        json!({}),
        json!({"certification":null}),
        json!({"certification":"  ","contentRating":""}),
    ] {
        assert_eq!(metadata::assessment(&meta), (None, false), "{meta}");
    }
    assert_eq!(
        metadata::assessment(&json!({"certification":"PG-13"})),
        (Some(13), false)
    );
}

#[tokio::test]
async fn allowing_unrated_titles_never_admits_adult_or_unrecognised_ratings() {
    let (a, task) = fixture().await;
    let movies = catalog(&a, "movie").await;
    let hidden = ["anime-r18", "adult-plus", "mystery", "hentai"];
    for title in ["pg", "unrated", "blank"].into_iter().chain(hidden) {
        observe(&a, "movie", &movies, title).await;
    }
    let id = create_kids(&a, json!({"max_age":17,"movie_rating":"R","allow_unrated":true})).await;
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "unrated", "blank"]);
    for title in hidden {
        assert_eq!(
            detail(&a, "movie", &card_of(&movies, title)).await.0,
            StatusCode::FORBIDDEN,
            "{title}"
        );
    }
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "unrated")).await.0,
        StatusCode::OK
    );
    task.abort();
}

#[tokio::test]
async fn blocked_titles_hide_allowed_titles_and_outrank_approvals() {
    let (a, task) = fixture().await;
    let movies = catalog(&a, "movie").await;
    let series = catalog(&a, "series").await;
    for id in ["pg", "pg13", "r", "adult"] {
        observe(&a, "movie", &movies, id).await;
    }
    for id in ["cartoon", "teen-series"] {
        observe(&a, "series", &series, id).await;
    }
    let id = create_kids(
        &a,
        json!({"max_age":12,"movie_rating":"PG-13","series_rating":"TV-14"}),
    )
    .await;
    let approved = observe(&a, "movie", &movies, "r").await;
    let (status, approval) = request(
        &a,
        "member-token-1",
        "POST",
        &format!("/api/profiles/{id}/approvals"),
        json!({"type":"movie","id":"r","approved":true,"card":approved}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approval}");
    switch(&a, &id).await;
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "pg13", "r"]);
    let blocked_path = format!("/api/profiles/{id}/blocked");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            &blocked_path,
            json!({"type":"movie","id":"pg13","blocked":true,"name":"Title pg13"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    unlock(&a).await;
    for (kind, blocked) in [
        ("movie", json!({"type":"movie","id":"pg13","blocked":true,"name":"Title pg13"})),
        ("series", json!({"type":"series","id":"cartoon","blocked":true})),
        ("movie", json!({"type":"movie","id":"r","blocked":true})),
    ] {
        assert_eq!(
            request(&a, "member-token-1", "POST", &blocked_path, blocked)
                .await
                .0,
            StatusCode::OK,
            "block {kind}"
        );
    }
    let (status, list) = request(&a, "member-token-1", "GET", &blocked_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg"]);
    assert_eq!(catalog_ids(&a, "series").await, vec!["teen-series"]);
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "pg13")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        detail(&a, "movie", &card_of(&movies, "r")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        detail(&a, "series", &card_of(&series, "cartoon")).await.0,
        StatusCode::FORBIDDEN
    );
    for body in [
        json!({"type":"movie","id":"pg13","name":"Title pg13"}),
        json!({"type":"series","id":"cartoon-1"}),
    ] {
        assert_eq!(
            request(&a, "member-token-1", "POST", "/api/streams", body)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            &blocked_path,
            json!({"type":"movie","id":"pg13","blocked":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg", "pg13"]);
    task.abort();
}

#[tokio::test]
async fn repeated_wrong_leave_pins_lock_the_profile_briefly() {
    let (a, task) = fixture().await;
    let id = create_kids(&a, json!({"max_age":12})).await;
    let pin_path = format!("/api/profiles/{id}/pin");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &pin_path,
            json!({"pin":"4821"})
        )
        .await
        .0,
        StatusCode::OK
    );
    switch(&a, &id).await;
    unlock(&a).await;
    for _ in 0..5 {
        let (status, body) = leave(&a, Some("1111")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error_code"], "leave_pin_invalid");
    }
    let (status, body) = leave(&a, Some("4821")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error_code"], "rate_limited");
    task.abort();
}

#[tokio::test]
async fn a_profile_leave_pin_gates_switching_away_from_it() {
    let (a, task) = fixture().await;
    let id = create_kids(&a, json!({"max_age":12})).await;
    let pin_path = format!("/api/profiles/{id}/pin");
    let (status, body) = request(&a, "member-token-1", "GET", &pin_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["availability"], "Available");
    assert_eq!(body["configured"], false);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &pin_path,
            json!({"pin":"4821"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, status_body) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/parent/status",
        Value::Null,
    )
    .await;
    assert_eq!(status_body["leave_pin_configured"], false);
    switch(&a, &id).await;
    let (_, status_body) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/parent/status",
        Value::Null,
    )
    .await;
    assert_eq!(status_body["leave_pin_configured"], true);
    let (status, body) = leave(&a, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error_code"], "parent_required");
    unlock(&a).await;
    let (status, body) = leave(&a, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error_code"], "leave_pin_required");
    let (status, body) = leave(&a, Some("1111")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error_code"], "leave_pin_invalid");
    let (status, body) = leave(&a, Some("4821")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &pin_path,
            json!({"pin":"9999","current_pin":"0000"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &pin_path,
            json!({"pin":"9999","current_pin":"4821"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &pin_path,
            json!({"pin":"","current_pin":"9999"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, body) = request(&a, "member-token-1", "GET", &pin_path, Value::Null).await;
    assert_eq!(body["configured"], false);
    switch(&a, &id).await;
    unlock(&a).await;
    assert_eq!(leave(&a, None).await.0, StatusCode::OK);
    task.abort();
}

#[tokio::test]
async fn restricted_catalogs_are_empty_and_discover_stays_filtered() {
    let (a, task) = fixture().await;
    let movies = catalog(&a, "movie").await;
    let series = catalog(&a, "series").await;
    for id in ["pg", "pg13", "r", "adult", "unrated", "conflict"] {
        observe(&a, "movie", &movies, id).await;
    }
    for id in ["cartoon", "teen-series"] {
        observe(&a, "series", &series, id).await;
    }
    let (status, body) = request(&a, "member-token-1", "GET", "/api/catalogs", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["top", "top-series"]);
    let id = create_kids(
        &a,
        json!({"max_age":17,"movie_rating":"PG","series_rating":"TV-Y"}),
    )
    .await;
    switch(&a, &id).await;
    let (status, body) = request(&a, "member-token-1", "GET", "/api/catalogs", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([]));
    assert_eq!(catalog_ids(&a, "movie").await, vec!["pg"]);
    assert_eq!(catalog_ids(&a, "series").await, vec!["cartoon"]);
    task.abort();
}

#[tokio::test]
async fn a_kids_profile_pages_over_a_mixed_catalog_to_full_filtered_pages() {
    let (a, task) = pageable_fixture().await;
    {
        let db = a.db.lock().unwrap();
        for i in 0..120usize {
            let age = if i % 24 < 6 { Some(0) } else { Some(18) };
            db.execute(
                "INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(1,'movie',?1,?1,?2,?3,0,1)",
                params![format!("tt{i}"), json!({ "name": format!("Title {i}") }).to_string(), age],
            )
            .unwrap();
        }
    }
    let id = create_kids(&a, json!({ "max_age": 17 })).await;
    switch(&a, &id).await;

    let (status, first) =
        request(&a, "member-token-1", "GET", "/api/discover?type=movie", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_cards = first["cards"].as_array().unwrap();
    assert_eq!(first_cards.len(), 24);
    for card in first_cards {
        let index: usize = card["identity"]["id"]
            .as_str()
            .unwrap()
            .strip_prefix("tt")
            .unwrap()
            .parse()
            .unwrap();
        assert!(index % 24 < 6, "adult title {index} leaked into a kids page");
    }
    assert_eq!(first["next_cursor"], "96");
    assert_eq!(first["complete"], false);

    let (status, rest) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&skip=96",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(rest["cards"].as_array().unwrap().len(), 6);
    assert_eq!(rest["next_cursor"], Value::Null);
    assert_eq!(rest["complete"], true);
    task.abort();
}
