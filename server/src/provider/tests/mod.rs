use super::*;

mod matching;
mod service;
mod sync;

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

async fn mock_xtream() -> (
    String,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let actions = Arc::new(Mutex::new(Vec::new()));
    let fail_task = fail.clone();
    let actions_task = actions.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.windows(4).any(|b| b == b"\r\n\r\n") || bytes.len() > 16384 {
                    break;
                }
            }
            // Clients can abandon an unused/speculative connection without
            // sending a request. EOF is not a malformed HTTP request.
            if bytes.is_empty() {
                continue;
            }
            let request = String::from_utf8_lossy(&bytes);
            let path = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let url = Url::parse(&format!("http://localhost{path}")).unwrap();
            let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
            assert_eq!(query.get("username").map(String::as_str), Some("u/+"));
            assert_eq!(query.get("password").map(String::as_str), Some("SECRET&?"));
            let action = query.get("action").unwrap().clone();
            actions_task.lock().unwrap().push(action.clone());
            let response=match action.as_str() {
                    "get_live_categories"=>json!([{"category_id":"7","category_name":"News"}]),
                    "get_live_streams"=>json!([{"stream_id":11,"name":"World News","category_id":"7","epg_channel_id":"world"}]),
                    "get_vod_streams" if fail_task.load(std::sync::atomic::Ordering::SeqCst)=>json!({"error":"SECRET&? invalid credentials"}),
                    "get_vod_streams"=>json!([{"stream_id":22,"name":"Amélie (2001)","container_extension":"mkv"}]),
                    "get_series"=>json!([{"series_id":33,"name":"Example (2020)","imdb_id":"tt1234567"}]),
                    "get_vod_info"=> {
                        assert!(!query.contains_key("stream_id"));
                        match query["vod_id"].as_str() {
                            "2318" => json!({"info":{"name":"Inception","releasedate":"2010-07-15","tmdb_id":"27205"},"movie_data":{"name":"Inception","container_extension":"mp4"}}),
                            "4000" => json!({"info":{"name":"Other Film","releasedate":"2010-07-15"}}),
                            "4001" => json!({"info":{"name":"Inception","releasedate":"2010-bogus"}}),
                            "4002" => json!({"info":{"name":"Inception","releasedate":"2010-07-15","imdb_id":"tt9999999"}}),
                            "4003" => json!({"error":"failed"}),
                            _ => json!({"info":{"name":"Inception"}}),
                        }
                    },
                    "get_series_info" if query["series_id"] == "9562" => json!({"info":{"name":"Breaking Bad","releaseDate":"2008-01-20"},"episodes":{"1":[{"id":"942671","title":"Pilot","season":1,"episode_num":1,"container_extension":"mp4"}]}}),
                    "get_series_info"=> { assert_eq!(query["series_id"],"33"); json!({"episodes":{"1":[{"id":44,"episode_num":2,"container_extension":"mp4"}]}}) },
                    "get_short_epg"=> { assert_eq!(query["stream_id"],"11"); json!({"epg_listings":[{"id":"epg1","title":"TmV3cw==","description":"SGVsbG8=","start_timestamp":"1700000000","stop_timestamp":"1700003600"},{"id":"bad","start_timestamp":5,"stop_timestamp":3}]}) },
                    _=>panic!("Unexpected Xtream action"),
                }.to_string();
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response);
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}"), fail, actions, task)
}

fn sparse_row(s: &ProviderService, p: i64, id: &str, kind: &str, name: &str) -> String {
    let id = insert_candidate(s, p, id, kind);
    s.lock()
        .unwrap()
        .execute(
            "UPDATE provider_vod SET name=?1,normalized=?2,year=NULL,extension='mp4' WHERE id=?3",
            params![name, normalize(name), id],
        )
        .unwrap();
    id
}
