use super::*;
use crate::auth_integration_tests::fixture;
use viptv_engine::identity::MediaIdentity;
use viptv_engine::library::LibraryStatistics;
use viptv_engine::presentation::Card;

type OldRow = (
    String,
    String,
    f64,
    f64,
    u64,
    String,
    Option<String>,
    Option<u64>,
);

fn reference_statistics(history: &[OldRow], card: &Card) -> LibraryStatistics {
    let mut statistics = LibraryStatistics::empty(card.identity.clone());
    let matched = history
        .iter()
        .filter(|(id, kind, _, _, _, _, parent, _)| {
            *kind == card.identity.kind
                && (*id == card.identity.id || parent.as_ref() == Some(&card.identity.id))
        })
        .collect::<Vec<_>>();
    statistics.last_watched_millis = matched
        .iter()
        .filter(|(_, _, position, duration, _, context, _, _)| {
            let context: Value = serde_json::from_str(context).unwrap_or(Value::Null);
            context["stremio_watch_date_unknown"] != true
                && (*position > 0.0
                    || crate::library::watched(
                        &json!({"position":position,"duration":duration}),
                        &context,
                    ))
        })
        .map(|(_, _, _, _, updated, _, _, _)| updated.saturating_mul(1000))
        .filter(|n| *n > 0)
        .max();
    statistics.watched = matched.iter().any(|(_, _, position, duration, _, context, _, _)| {
        crate::library::watched(
            &json!({"position":position,"duration":duration}),
            &serde_json::from_str::<Value>(context).unwrap_or(Value::Null),
        )
    });
    let episodes = matched
        .iter()
        .filter(|(_, _, _, _, _, context, _, _)| {
            serde_json::from_str::<Value>(context)
                .ok()
                .and_then(|c| serde_json::from_value::<MediaIdentity>(c["engine_identity"].clone()).ok())
                .is_some_and(|identity| identity.episode.is_some())
        })
        .collect::<Vec<_>>();
    statistics.times_watched = if episodes.is_empty() {
        matched
            .iter()
            .try_fold(0u64, |sum, (_, _, _, _, _, _, _, count)| {
                count.map(|count| sum.saturating_add(count))
            })
    } else {
        None
    };
    if !episodes.is_empty() {
        statistics.watched_episode_count = Some(
            episodes
                .iter()
                .filter(|(_, _, position, duration, _, context, _, _)| {
                    crate::library::watched(
                        &json!({"position":position,"duration":duration}),
                        &serde_json::from_str::<Value>(context).unwrap_or(Value::Null),
                    )
                })
                .count() as u64,
        );
    }
    statistics
}

#[tokio::test]
async fn library_stats_match_the_linear_progress_index() {
    let app = fixture();
    {
        let db = app.db.lock().unwrap();
        for i in 0..500 {
            let id = format!("mv-{i:04}");
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name,context,added_at_millis) VALUES(1,?1,'movie',?2,'{}',?3)",
                params![id, format!("Movie {i}"), i as u64],
            )
            .unwrap();
            if i != 0 {
                db.execute(
                    "INSERT INTO engine_watch_counts(profile_id,type,id,count) VALUES(1,'movie',?1,12)",
                    [&id],
                )
                .unwrap();
            }
            let position = if i % 2 == 0 { 0.4 } else { 1.0 };
            db.execute(
                "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'movie','Title',NULL,?2,1.0,?3,'{}','')",
                params![id, position, (1000 + i) as u64],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO favorites(profile_id,id,type,name,context,added_at_millis) VALUES(1,'srs-0001','series','Series','{}',1000000)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'srs-0001','series','Series',NULL,1.0,1.0,2000,'{}','')",
            [],
        )
        .unwrap();
        for episode in 1..=4499 {
            let id = format!("srs-0001:1:{episode}");
            let context = json!({
                "engine_identity": {
                    "id": id,
                    "kind": "series",
                    "ids": [],
                    "episode": {"order":"aired","season":1,"episode":episode},
                    "edition": null,
                }
            })
            .to_string();
            let position = if episode % 2 == 0 { 1.0 } else { 0.4 };
            db.execute(
                "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Episode',NULL,?2,1.0,?3,?4,'srs-0001')",
                params![id, position, 2000 + episode as u64, context],
            )
            .unwrap();
        }
    }

    let history: Vec<OldRow> = {
        let db = app.db.lock().unwrap();
        let mut q = db
            .prepare(
                "SELECT p.id,p.type,p.position,p.duration,p.updated_at,p.context,p.title_id,w.count FROM progress p LEFT JOIN engine_watch_counts w ON w.profile_id=p.profile_id AND w.type=p.type AND w.id=p.id WHERE p.profile_id=?1 AND p.type IN ('movie','series') LIMIT 50001",
            )
            .unwrap();
        q.query_map([1], |r| {
            let kind = r.get::<_, String>(1)?;
            let id = r.get::<_, String>(0)?;
            let parent = r.get::<_, Option<String>>(6)?;
            Ok((
                crate::simkl::key::media(&kind, &id).into_owned(),
                kind,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, String>(5)?,
                parent.map(|parent| crate::simkl::key::media("series", &parent).into_owned()),
                r.get::<_, Option<u64>>(7)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };

    let owned = app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    });
    let lease = owned.request_lease();
    let entries = entries(&owned, &lease, 1, 0).unwrap();
    assert_eq!(entries.len(), 501);
    for entry in &entries {
        let expected = reference_statistics(&history, &entry.card);
        assert_eq!(
            serde_json::to_string(&entry.statistics).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "statistics mismatch for {}:{}",
            entry.card.identity.kind,
            entry.card.identity.id,
        );
    }
}