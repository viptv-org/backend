use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const URL: &str = "https://v3-cinemeta.strem.io/meta/movie/tt123.json";

fn fixture() -> (Arc<Cache>, Value) {
    let db = Connection::open_in_memory().unwrap();
    init(&db).unwrap();
    (Arc::new(Cache::new(Arc::new(Mutex::new(db)))), json!({"meta":{"id":"tt123","type":"movie","name":"Fixture","description":"Public description"}}))
}

#[test]
fn only_exact_anonymous_cinemeta_metadata_is_shared() {
    assert!(identity(URL).is_some());
    assert!(identity("https://v3-cinemeta.strem.io/meta/series/tt123.json").is_some());
    for url in [
        "https://v3-cinemeta.strem.io/meta/movie/tt123.json?token=private",
        "https://user@v3-cinemeta.strem.io/meta/movie/tt123.json",
        "https://v3-cinemeta.strem.io/private/meta/movie/tt123.json",
        "https://v3-cinemeta.strem.io/catalog/movie/top.json",
        "https://v3-cinemeta.strem.io/meta/movie/tt123:1:2.json",
        "https://v3-cinemeta.strem.io.evil.test/meta/movie/tt123.json",
        "http://v3-cinemeta.strem.io/meta/movie/tt123.json",
        "https://v3-cinemeta.strem.io/meta/movie/tt123.json#private",
        "https://fixture.invalid/meta/movie/tt123.json",
    ] { assert!(identity(url).is_none(), "{url}"); }
}

#[tokio::test]
async fn cold_read_coalesces_and_survives_new_account_and_cache_instance() {
    let (cache, value) = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let results = futures::future::join_all((0..24).map(|_| {
        let calls = calls.clone(); let value = value.clone();
        cache.load(URL.into(), move || async move {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(value)
        })
    })).await;
    assert!(results.iter().all(|r| r.as_ref().unwrap() == &value));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let reconstructed = Arc::new(Cache::new(cache.db.clone()));
    assert_eq!(reconstructed.load(URL.into(), || async { panic!("persistent cache missed") }).await.unwrap(), value);
}

#[tokio::test]
async fn stale_read_returns_immediately_while_one_refresh_runs() {
    let (cache, value) = fixture(); cache.store(URL, &value).unwrap();
    cache.db.lock().unwrap().execute("UPDATE public_metadata_cache_v1 SET refresh_at=0", []).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let mut fresh = value.clone(); fresh["meta"]["description"] = json!("Updated public description");
    for _ in 0..12 {
        let calls = calls.clone(); let release = release.clone();let started = started.clone(); let fresh = fresh.clone();
        let hit = tokio::time::timeout(Duration::from_millis(100), cache.load(URL.into(), move || async move {
            calls.fetch_add(1, Ordering::SeqCst); started.notify_one();release.notified().await;Ok(fresh)
        })).await.unwrap().unwrap();
        assert_eq!(hit, value);
    }
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while cache.read(URL).unwrap().0 != fresh { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_refresh_retains_good_metadata_and_backs_off() {
    let (cache, value) = fixture(); cache.store(URL, &value).unwrap();
    cache.db.lock().unwrap().execute("UPDATE public_metadata_cache_v1 SET refresh_at=0", []).unwrap();
    assert_eq!(cache.load(URL.into(), || async { Err("upstream unavailable".into()) }).await.unwrap(), value);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !cache.retries.lock().unwrap().contains_key(URL) { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(cache.load(URL.into(), || async { panic!("retry backoff ignored") }).await.unwrap(), value);
}

#[tokio::test]
async fn wrong_identity_and_missing_metadata_do_not_poison_cache() {
    let (cache, mut value) = fixture(); value["meta"]["id"] = json!("tt999");
    assert!(cache.load(URL.into(), move || async move { Ok(value) }).await.is_err());
    assert!(cache.read(URL).is_none());
    let (_, value) = fixture();
    assert_eq!(cache.load(URL.into(), move || async move { Ok(value.clone()) }).await.unwrap()["meta"]["id"], "tt123");
}

#[tokio::test]
async fn cancelled_cold_fetch_does_not_strand_later_requests() {
    let (cache, value) = fixture();
    let started = Arc::new(tokio::sync::Notify::new()); let ready = started.clone();let pending = cache.clone();
    let task = tokio::spawn(async move { pending.load(URL.into(), move || async move {
        ready.notify_one(); std::future::pending::<Result<Value,String>>().await
    }).await });
    started.notified().await; task.abort();let _ = task.await;
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), cache.load(URL.into(), move || async move { Ok(value) })).await.unwrap().unwrap()["meta"]["id"], "tt123");
}

#[test]
fn changing_series_and_incomplete_movies_refresh_earlier_and_expired_rows_miss() {
    let (cache, mut value) = fixture();
    assert_eq!(lifetime(&value), (86400, 30 * 86400));
    value["meta"]["description"] = Value::Null;
    assert_eq!(lifetime(&value).0, 900);
    value["meta"]["type"] = json!("series");
    assert_eq!(lifetime(&value), (900, 7 * 86400));
    value["meta"]["type"] = json!("movie");cache.store(URL,&value).unwrap();
    cache.db.lock().unwrap().execute("UPDATE public_metadata_cache_v1 SET expires_at=0", []).unwrap();
    assert!(cache.read(URL).is_none());
}
