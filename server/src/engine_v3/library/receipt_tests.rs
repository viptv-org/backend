use crate::{auth_integration_tests::fixture, test_support::request, *};
fn input(id: &str) -> Value {
    json!({"card":{"identity":{"id":id,"kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"stremio","title":"Title","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[],"parent_identity":null}})
}
#[tokio::test]
async fn mutation_membership_is_complete_profile_owned_and_kids_filtered() {
    let app = fixture();
    for id in ["ordinary", "approved"] {
        let (status, receipt) = request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/library",
            input(id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt["library_membership"]["complete"], true);
    }
    {
        let db = app.db.lock().unwrap();
        kids::observe(
            &db,
            1,
            "movie",
            &json!({"id":"approved","type":"movie","name":"Approved","certification":"G"}),
            false,
        )
        .unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    }
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        input("approved"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let ids = saved["library_membership"]["legacy_identities"]
        .as_array()
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0]["id"], "approved");
    assert_eq!(saved["library_membership"]["complete"], true);
    for method in ["PUT", "DELETE"] {
        let path = if method == "PUT" {
            "/api/profiles/1/library"
        } else {
            "/api/profiles/1/library/movie/approved"
        };
        let (status, denied) =
            request(&app, "member-token-2", method, path, input("approved")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(denied.get("library_membership").is_none());
    }
    let (status, removed) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/library/movie/approved",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        removed["library_membership"]["identities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(removed["library_membership"]["complete"], true);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM favorites WHERE profile_id=1 AND id='ordinary'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn membership_bound_failure_rolls_back_the_mutation_instead_of_acknowledging_partial_state() {
    let app = fixture();
    app.db.lock().unwrap().execute("WITH RECURSIVE n(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO favorites(profile_id,id,type,name) SELECT 1,'existing-'||i,'movie','Existing' FROM n", []).unwrap();
    let (status, receipt) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        input("overflow"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(receipt.get("library_membership").is_none());
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM favorites WHERE profile_id=1",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        10000
    );
}
