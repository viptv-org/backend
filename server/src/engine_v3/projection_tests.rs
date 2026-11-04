use super::*;
use crate::auth_integration_tests::fixture;
use std::sync::atomic::{AtomicUsize, Ordering};

fn kids_app() -> App {
    fixture().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    })
}

fn media(kind: &str, id: &str) -> String {
    json!({"id": id, "type": kind, "name": format!("Enriched {id}"), "poster": format!("poster/{id}")})
        .to_string()
}

fn kids_media_queries(app: &App, f: impl FnOnce()) -> usize {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_uint, c_void};
    struct Counter {
        total: usize,
    }
    unsafe extern "C" fn trace(
        mask: c_uint,
        ctx: *mut c_void,
        _stmt: *mut c_void,
        sql: *mut c_void,
    ) -> c_int {
        if mask != rusqlite::ffi::SQLITE_TRACE_STMT as c_uint {
            return 0;
        }
        let counter = &mut *(ctx as *mut Counter);
        let text = CStr::from_ptr(sql as *const c_char).to_string_lossy();
        if text.contains("kids_media") {
            counter.total += 1;
        }
        0
    }
    let mut counter = Counter { total: 0 };
    {
        let db = app.db.lock().unwrap();
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                db.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT as c_uint,
                Some(trace),
                &mut counter as *mut Counter as *mut c_void,
            );
        }
    }
    f();
    {
        let db = app.db.lock().unwrap();
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(db.handle(), 0, None, std::ptr::null_mut());
        }
    }
    counter.total
}

#[tokio::test]
async fn cards_filter_the_kids_policy_once_and_batch_the_media_read() {
    let app = kids_app();
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,17,0)",
            [],
        )
        .unwrap();
        for (kind, id, age) in [
            ("movie", "tt-a1", Some(0)),
            ("movie", "tt-a2", Some(5)),
            ("movie", "tt-adult", Some(18)),
        ] {
            db.execute(
                "INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(1,?1,?2,?2,?3,?4,0,1)",
                params![kind, id, media(kind, id), age],
            )
            .unwrap();
        }
    }
    let items = vec![
        json!({"id":"tt-a1","type":"movie","name":"A1"}),
        json!({"id":"tt-a2","type":"movie","name":"A2"}),
        json!({"id":"tt-adult","type":"movie","name":"Adult"}),
        json!({"id":"tt-missing","type":"movie","name":"Missing"}),
    ];
    let queries = kids_media_queries(&app, || {
        super::projection::cards(&app, items.clone(), "stremio").unwrap();
    });
    let cards = super::projection::cards(&app, items.clone(), "stremio").unwrap();
    let ids: Vec<&str> = cards.iter().map(|card| card.identity.id.as_str()).collect();
    assert_eq!(ids, vec!["tt-a1", "tt-a2"]);
    assert_eq!(cards[0].poster.as_deref(), Some("poster/tt-a1"));
    assert!(
        queries <= 2,
        "cards issue {queries} kids_media queries for one page; the policy and media read must be batched"
    );
}