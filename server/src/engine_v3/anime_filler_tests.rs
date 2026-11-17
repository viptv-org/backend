use super::anime_filler::{normalize, parse_index, parse_show, titles, Episode, Kind, Service};
use axum::{extract::Path, http::StatusCode, routing::get, Router};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const INDEX: &str = r#"<html><body>
<ul class="nav"><li><a href="/shows/latest-thing">Latest Thing</a></li></ul>
<div class="Group"><h2>B</h2><ul>
<li><a href="/shows/bleach-thousand-year-blood-war">Bleach: Thousand-Year Blood War (Bleach: Sennen Kessen-hen)</a></li>
</ul></div>
<div class="Group"><h2>N</h2><ul>
<li><a href="/shows/naruto">Naruto</a></li>
<li><a href="/shows/mobile-suit-gundam-zz">Naruto OVAs</a></li>
<li><a href="/shows/naruto-shippuden">Naruto Shippuden</a></li>
</ul></div>
<div class="Group"><h2>O</h2><ul>
<li><a href="/shows/one-piece">One Piece</a></li>
<li><a href="/shows/one-piece-0">The One Piece</a></li>
<li><a href="/shows/pok%C3%A9mon">Pokémon &amp; Friends (Twin)</a></li>
<li><a href="/shows/other-twin">Other (Twin)</a></li>
<li><a href="/shows/../admin">Escape</a></li>
</ul></div>
</body></html>"#;

const SHIPPUDEN: &str = r#"<h1>Naruto Shippuden Filler List</h1>
<table class="EpisodeList"><tbody>
<tr class="manga_canon odd" id="eps-1"><td class="Number">1</td><td class="Title">Homecoming</td><td class="Type"><span>Manga Canon</span></td></tr>
<tr class="filler even" id="eps-57"><td class="Number">57</td><td class="Title">Tenten's Troubles</td><td class="Type"><span>Filler</span></td></tr>
<tr class="mixed_canon/filler odd" id="eps-28"><td class="Number">28</td><td class="Title">Homecoming</td><td class="Type"><span>Mixed Canon/Filler</span></td></tr>
<tr class="anime_canon even" id="eps-90"><td class="Number">90</td><td class="Title">Assault</td><td class="Type"><span>Anime Canon</span></td></tr>
<tr class="filler odd" id="eps-x"><td class="Number">x</td><td class="Title">Broken</td></tr>
</tbody></table>
<table class="Other"><tr class="filler"><td class="Number">999</td></tr></table>"#;

struct Fake {
    base: String,
    requests: Arc<Mutex<Vec<(String, std::time::Instant)>>>,
}

impl Fake {
    fn count(&self, path: &str) -> usize {
        self.requests.lock().unwrap().iter().filter(|(seen, _)| seen == path).count()
    }

    fn times(&self) -> Vec<std::time::Instant> {
        self.requests.lock().unwrap().iter().map(|(_, at)| *at).collect()
    }
}

async fn fake(failing: bool, delay: Duration) -> Fake {
    let requests: Arc<Mutex<Vec<(String, std::time::Instant)>>> = Arc::default();
    let index_requests = requests.clone();
    let show_requests = requests.clone();
    let routes = Router::new()
        .route(
            "/shows",
            get(move || {
                let requests = index_requests.clone();
                async move {
                    requests.lock().unwrap().push(("/shows".into(), std::time::Instant::now()));
                    (StatusCode::OK, INDEX)
                }
            }),
        )
        .route(
            "/shows/:slug",
            get(move |Path(slug): Path<String>| {
                let requests = show_requests.clone();
                async move {
                    requests.lock().unwrap().push((format!("/shows/{slug}"), std::time::Instant::now()));
                    tokio::time::sleep(delay).await;
                    if failing {
                        (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                    } else if slug == "naruto-shippuden" {
                        (StatusCode::OK, SHIPPUDEN.to_owned())
                    } else {
                        (StatusCode::OK, "<table class=\"EpisodeList\"></table>".to_owned())
                    }
                }
            }),
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
    std::mem::forget(task);
    Fake {
        base: format!("http://{address}"),
        requests,
    }
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn titles_normalize_case_accents_punctuation_and_spacing() {
    assert_eq!(normalize("Naruto: Shippûden"), "narutoshippuden");
    assert_eq!(normalize("  ONE   PIECE! "), "onepiece");
    assert_ne!(normalize("The One Piece"), normalize("One Piece"));
}

#[test]
fn the_index_reads_only_grouped_show_links_with_names_and_their_parts() {
    let index = parse_index(INDEX);
    let slug = |title: &str| index.get(&normalize(title)).map(|show| show.slug.as_str());
    assert_eq!(slug("Naruto Shippuden"), Some("naruto-shippuden"));
    assert_eq!(slug("Naruto OVAs"), Some("mobile-suit-gundam-zz"));
    assert_eq!(slug("One Piece"), Some("one-piece"));
    assert_eq!(slug("The One Piece"), Some("one-piece-0"));
    assert_eq!(slug("Bleach: Thousand-Year Blood War"), Some("bleach-thousand-year-blood-war"));
    assert_eq!(slug("Bleach: Sennen Kessen-hen"), Some("bleach-thousand-year-blood-war"));
    assert_eq!(slug("Pokémon & Friends"), Some("pok%C3%A9mon"));
    assert_eq!(slug("Twin"), None);
    assert_eq!(slug("Latest Thing"), None);
    assert_eq!(slug("Escape"), None);
}

#[test]
fn a_show_page_lists_its_filler_and_mixed_episodes_in_order() {
    assert_eq!(
        parse_show(SHIPPUDEN),
        Some(vec![
            Episode { number: 28, kind: Kind::Mixed },
            Episode { number: 57, kind: Kind::Filler },
        ])
    );
    assert_eq!(parse_show("<html>moved</html>"), None);
}

#[test]
fn the_query_keeps_distinct_nonempty_titles() {
    assert_eq!(
        titles("title=Naruto%3A+Shipp%C3%BBden&title=&title=Naruto+Shippuden&title=Naruto%3A+Shipp%C3%BBden&other=x"),
        names(&["Naruto: Shippûden", "Naruto Shippuden"])
    );
}

#[tokio::test]
async fn a_matching_title_returns_the_shows_episodes_and_caches_them() {
    let fake = fake(false, Duration::ZERO).await;
    let service = Arc::new(Service::fixture(&fake.base, Duration::ZERO));
    let (show, episodes) = service
        .lookup(&names(&["Unknown", "Naruto: Shippûden"]), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(show.slug, "naruto-shippuden");
    assert_eq!(episodes, vec![Episode { number: 28, kind: Kind::Mixed }, Episode { number: 57, kind: Kind::Filler }]);
    service.lookup(&names(&["Naruto Shippuden"]), Duration::from_secs(5)).await.unwrap();
    assert_eq!(fake.count("/shows"), 1);
    assert_eq!(fake.count("/shows/naruto-shippuden"), 1);
}

#[tokio::test]
async fn an_unmatched_title_reads_only_the_index() {
    let fake = fake(false, Duration::ZERO).await;
    let service = Arc::new(Service::fixture(&fake.base, Duration::ZERO));
    assert!(service.lookup(&names(&["Breaking Bad"]), Duration::from_secs(5)).await.is_none());
    assert!(service.lookup(&[], Duration::from_secs(5)).await.is_none());
    assert_eq!(fake.count("/shows"), 1);
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn requests_are_spaced_by_the_crawl_delay() {
    let fake = fake(false, Duration::ZERO).await;
    let service = Arc::new(Service::fixture(&fake.base, Duration::from_millis(300)));
    let (one_piece, naruto) = (names(&["One Piece"]), names(&["Naruto"]));
    let (first, second) = tokio::join!(
        service.lookup(&one_piece, Duration::from_secs(5)),
        service.lookup(&naruto, Duration::from_secs(5)),
    );
    assert!(first.is_some() && second.is_some());
    let times = fake.times();
    assert_eq!(times.len(), 3);
    for pair in times.windows(2) {
        assert!(pair[1].duration_since(pair[0]) >= Duration::from_millis(290));
    }
}

#[tokio::test]
async fn a_failed_show_page_returns_nothing_and_is_not_retried_at_once() {
    let fake = fake(true, Duration::ZERO).await;
    let service = Arc::new(Service::fixture(&fake.base, Duration::ZERO));
    assert!(service.lookup(&names(&["Naruto"]), Duration::from_secs(5)).await.is_none());
    assert!(service.lookup(&names(&["Naruto"]), Duration::from_secs(5)).await.is_none());
    assert_eq!(fake.count("/shows/naruto"), 1);
}

#[tokio::test]
async fn a_slow_page_misses_the_budget_but_lands_in_the_cache() {
    let fake = fake(false, Duration::from_millis(400)).await;
    let service = Arc::new(Service::fixture(&fake.base, Duration::ZERO));
    assert!(service.lookup(&names(&["Naruto Shippuden"]), Duration::from_millis(100)).await.is_none());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(service.lookup(&names(&["Naruto Shippuden"]), Duration::from_millis(100)).await.is_some());
    assert_eq!(fake.count("/shows/naruto-shippuden"), 1);
}
