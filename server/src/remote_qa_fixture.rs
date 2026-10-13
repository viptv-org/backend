//! Explicit, disposable browser-QA host. This module is absent from production builds.
use crate::*;

#[tokio::test]
#[ignore = "Starts a loopback QA server; run explicitly with --ignored --nocapture"]
async fn remote_addon_browser_fixture() {
    let directory = tempfile::Builder::new()
        .prefix("viptv-remote-addon-")
        .tempdir()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database = directory.path().join("qa.sqlite");
    let mut app = test_support::app_with_db(Connection::open(&database).unwrap());
    test_support::configure_vault(&mut app);
    app.addons.allow_test_loopback = true;
    let password = format!("qa-{}", Uuid::new_v4());
    let profile = {
        let mut db = app.db.lock().unwrap();
        db.execute("DELETE FROM addons", []).unwrap();
        auth::create_owner_offline(&mut db, "qa-parent", "QA Parent", &password).unwrap();
        let profile = auth::create_profile(&db, 1, &json!({"name":"QA Parent"})).unwrap();
        db.execute("INSERT INTO auth_onboarding(account_id,state) VALUES(1,'complete') ON CONFLICT(account_id) DO UPDATE SET state='complete'",[]).unwrap();
        profile["id"].as_str().unwrap().to_owned()
    };
    let addon = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let manifest = format!("http://{}/manifest.json", addon.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let mock = Router::new().fallback(|request: Request| async move {
            let path = request.uri().path();
            axum::Json(if path == "/manifest.json" {
                json!({"id":"viptv.qa.addon","version":"1.0.0","name":"QA Catalog","description":"Disposable local addon fixture","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"qa","type":"movie","name":"QA Movies","extra":[{"name":"search","isRequired":false}]}]})
            } else if path.starts_with("/meta/") {
                json!({"meta":{"id":"tt1234567","type":"movie","name":"Fixture Movie","description":"Deterministic local QA title"}})
            } else {
                json!({"metas":[{"id":"tt1234567","type":"movie","name":"Fixture Movie","description":"Deterministic local QA title"}]})
            })
        });
        axum::serve(addon, mock).await.unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18183")
        .await
        .unwrap();
    let credentials = directory.path().join("fixture.json");
    std::fs::write(&credentials,serde_json::to_vec_pretty(&json!({"username":"qa-parent","password":password,"profile_id":profile,"profile_name":"QA Parent","manifest_url":manifest,"api_upstream":"http://127.0.0.1:18183","origin":"https://viptv.local.test:18443","database":database})).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&credentials, &database] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    println!("Disposable QA fixture: {}", credentials.display());
    axum::serve(listener, router(app, None))
        .with_graceful_shutdown(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        })
        .await
        .unwrap();
    upstream.abort();
}
