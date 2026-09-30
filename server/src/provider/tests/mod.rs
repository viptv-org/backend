use super::*;

mod matching;

fn service() -> ProviderService {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    init(&db).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();
    ProviderService::new(Arc::new(Mutex::new(db)), client)
}
fn add_provider(s: &ProviderService) -> i64 {
    s.add(json!({"name":"Test IPTV","url":"https://example.com/base/","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap()
}
fn insert_candidate(s: &ProviderService, provider: i64, stream: &str, kind: &str) -> String {
    let id = format!("iptv:{provider}:{kind}:{stream}");
    s.lock().unwrap().execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES(?1,?2,?3,?4,'Amélie (2001)','amelie',2001,'mkv')",params![id,provider,stream,kind]).unwrap();
    id
}
fn request(v: Value) -> MatchRequest {
    MatchRequest::parse(&v, v["type"].as_str().unwrap_or("movie")).unwrap()
}
fn candidate(v: Value) -> Candidate {
    candidate_from_json(1, "movie", &v).unwrap()
}
