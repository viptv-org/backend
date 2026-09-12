//! Persistent viewer exclusions, independent of replaceable provider observations.
use super::*;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS live_category_rules(category TEXT PRIMARY KEY,enabled INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS live_policy_settings(id INTEGER PRIMARY KEY CHECK(id=1),require_schedule INTEGER NOT NULL DEFAULT 0);
    INSERT OR IGNORE INTO live_policy_settings(id) VALUES(1);")
}
pub(crate) fn category_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}
pub(crate) fn category_enabled(db: &Connection, category: &str) -> bool {
    db.query_row(
        "SELECT enabled FROM live_category_rules WHERE category=?1",
        [category_key(category)],
        |r| r.get::<_, bool>(0),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(true)
}
pub(crate) fn candidate_allowed(db: &Connection, id: &str) -> bool {
    db.query_row("SELECT l.name,COALESCE(l.category,'') FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1 AND p.enabled=1 AND p.enable_live=1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().ok().flatten().is_some_and(|(name,category)| crate::live_catalog::exclusion(&name,&category).is_none() && category_enabled(db,&category))
}
pub(crate) fn visible(db: &Connection, id: &str) -> bool {
    let eligible = if id.starts_with("family:") {
        db.prepare("SELECT live_id FROM family_candidates WHERE channel_id=?1")
            .ok()
            .and_then(|mut s| {
                s.query_map([id], |r| r.get::<_, String>(0))
                    .ok()
                    .map(|rows| {
                        rows.filter_map(Result::ok)
                            .any(|id| candidate_allowed(db, &id))
                    })
            })
            .unwrap_or(false)
    } else {
        candidate_allowed(db, id)
    };
    if !eligible {
        return false;
    }
    let required: bool = db
        .query_row(
            "SELECT require_schedule FROM live_policy_settings WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if !required {
        return true;
    }
    // Last-good schedules remain usable even when a refresh/cache TTL has failed.
    let now = util::now();
    db.query_row("SELECT EXISTS(SELECT 1 FROM family_programmes WHERE channel_id=?1 AND end>?2) OR EXISTS(SELECT 1 FROM provider_live l JOIN provider_cache c ON c.provider_id=l.provider_id AND c.cache_key='get_short_epg:'||l.stream_id, json_each(c.payload,'$.epg_listings') e WHERE l.id=?1 AND CAST(COALESCE(json_extract(e.value,'$.stop_timestamp'),json_extract(e.value,'$.end_timestamp')) AS INTEGER)>?2)",params![id,now],|r|r.get(0)).unwrap_or(false)
}
pub(super) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db=a.db.lock().unwrap();lease.validate(&db)?;
        let mut groups=std::collections::BTreeMap::<String,(String,usize)>::new();
        let mut s=db.prepare("SELECT COALESCE(category,''),COUNT(*) FROM provider_live GROUP BY category LIMIT 10000").map_err(db_error)?;
        for r in s.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,usize>(1)?))).map_err(db_error)? {
            let (name,count)=r.map_err(db_error)?;let e=groups.entry(category_key(&name)).or_insert((name,0));e.1+=count;
        }
        let categories=groups.into_iter().map(|(key,(name,count))|{
            let reason=crate::live_catalog::category_exclusion(&name);
            json!({"key":key,"name":if name.is_empty(){"Uncategorized"}else{&name},"count":count,"enabled":category_enabled(&db,&name),"reason":reason})
        }).collect::<Vec<_>>();
        let required:bool=db.query_row("SELECT require_schedule FROM live_policy_settings WHERE id=1",[],|r|r.get(0)).map_err(db_error)?;
        Ok(axum::Json(json!({"categories":categories,"require_schedule":required})))
    }).await
}
pub(super) async fn update(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        let db=a.db.lock().unwrap();crate::provider::accounts::owner(&lease,&db)?;
        if let Some(required)=v["require_schedule"].as_bool(){db.execute("UPDATE live_policy_settings SET require_schedule=?1 WHERE id=1",[required]).map_err(db_error)?;}
        else {
            let category=v["category"].as_str().filter(|s|s.len()<=512).ok_or("Invalid category")?;
            let enabled=v["enabled"].as_bool().ok_or("Invalid category setting")?;
            db.execute("INSERT INTO live_category_rules(category,enabled) VALUES(?1,?2) ON CONFLICT(category) DO UPDATE SET enabled=excluded.enabled",params![category_key(category),enabled]).map_err(db_error)?;
        }
        Ok(axum::Json(json!({"ok":true})))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamic_exclusions_preserve_inventory_and_category_preferences() {
        let db = Connection::open_in_memory().unwrap();
        crate::provider::init(&db).unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Test','http://fixture.invalid','u','p')",[]).unwrap();
        for (i, name, category) in [
            (1, "USA: Cinemax HD", "USA PREMIUM"),
            (2, "SD CINEMAX", "USA PREMIUM"),
            (3, "IT: Cinemax", "Movies"),
            (4, "Cinemax", "LAT CINE/PELICULAS"),
            (5, "USA: Cartoon Network", "BRAZIL"),
            (6, "USA: HGTV", "Home"),
        ] {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES(?1,1,?2,?3,?4)",params![format!("iptv:1:{i}"),i.to_string(),name,category]).unwrap();
        }
        let rows = crate::live_catalog::channels(&db).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r["name"] == "Cinemax HD"));
        db.execute(
            "INSERT INTO live_category_rules VALUES('USA PREMIUM',0)",
            [],
        )
        .unwrap();
        assert_eq!(crate::live_catalog::channels(&db).unwrap().len(), 1);
        db.execute(
            "UPDATE provider_live SET category='  usa   premium ' WHERE stream_id='1'",
            [],
        )
        .unwrap();
        assert!(!candidate_allowed(&db, "iptv:1:1"));
        db.execute("UPDATE live_policy_settings SET require_schedule=1", [])
            .unwrap();
        assert!(crate::live_catalog::channels(&db).unwrap().is_empty());
        let payload=json!({"epg_listings":[{"title":"SG9tZQ==","start_timestamp":util::now()-100,"stop_timestamp":util::now()+3600}]}).to_string();
        db.execute(
            "INSERT INTO provider_cache VALUES(1,'get_short_epg:6',0,?1)",
            [payload],
        )
        .unwrap();
        assert_eq!(
            crate::live_catalog::channels(&db).unwrap().len(),
            1,
            "usable last-good schedule survives expired fetch TTL"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM provider_live", [], |r| r
                .get::<_, usize>(0))
                .unwrap(),
            6
        );
    }
    #[test]
    fn country_and_regional_categories_apply_to_new_sources() {
        for category in [
            "LAT ENTRETENIMIENTO",
            "LAT CINE/PELICULAS",
            "LAT CULTURA",
            "LAT INFANTILES",
            "LAT DEPORTES",
            "LAT NOTICIAS",
            "LAT RELIGIOSOS",
            "LATINO 24/7",
            "ARGENTINA",
            "BOLIVIA",
            "BRAZIL",
            "IT: Cinema",
            "DE SPORTS",
            "Japan",
            "ARG SPORTS",
            "Liechtenstein",
            "NPL Movies",
        ] {
            assert!(
                crate::live_catalog::category_exclusion(category).is_some(),
                "{category}"
            );
        }
        for category in [
            "USA: Kids",
            "USA PREMIUM",
            "US News",
            "Movies",
            "Home & Food",
        ] {
            assert!(
                crate::live_catalog::category_exclusion(category).is_none(),
                "{category}"
            );
        }
        assert_eq!(
            crate::live_catalog::display_name("USA: Cartoon Network (West)"),
            "Cartoon Network (West)"
        );
    }
}
