//! V2 account ownership and bounded catalog queries. No implicit legacy grants.
//! Wiring into public handlers is a coordinated cutover, not an automatic migration.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, &'static str>;
fn db_error(_: rusqlite::Error) -> &'static str {
    "provider_storage_unavailable"
}
const MATCH_CURSOR_BYTES: usize = 2048;

/// Never return a token larger than the matching decoder accepts. Original
/// provider identifiers remain stored unchanged; unsupported pages fail safely.
pub(super) fn encode_page_cursor<T: Serialize>(value: &T, bound: usize) -> Result<String> {
    let token = URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).map_err(|_| "invalid_cursor")?);
    if token.len() > bound {
        return Err("catalog_cursor_too_large");
    }
    Ok(token)
}

pub(crate) fn init(db: &Connection) -> Result<()> {
    let tx = db.unchecked_transaction().map_err(db_error)?;
    init_in_transaction(&tx)?;
    tx.commit().map_err(db_error)
}

pub(crate) fn init_in_transaction(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let indexed: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_vod_search_v2')",
            [],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS provider_ownership(provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE);
      CREATE INDEX IF NOT EXISTS provider_ownership_account ON provider_ownership(account_id,provider_id);
      CREATE TABLE IF NOT EXISTS account_media_settings(account_id INTEGER PRIMARY KEY REFERENCES auth_accounts(id) ON DELETE CASCADE,default_live_provider_id INTEGER REFERENCES providers(id) ON DELETE SET NULL);
      CREATE INDEX IF NOT EXISTS provider_vod_unmatched_page ON provider_vod(provider_id,id) WHERE imdb_id IS NULL AND tmdb_id IS NULL;
      CREATE VIRTUAL TABLE IF NOT EXISTS provider_vod_search_v2 USING fts5(name,content='provider_vod',content_rowid='rowid',tokenize='unicode61');
      CREATE TRIGGER IF NOT EXISTS provider_vod_search_v2_insert AFTER INSERT ON provider_vod BEGIN INSERT INTO provider_vod_search_v2(rowid,name) VALUES(new.rowid,new.name); END;
      CREATE TRIGGER IF NOT EXISTS provider_vod_search_v2_delete AFTER DELETE ON provider_vod BEGIN INSERT INTO provider_vod_search_v2(provider_vod_search_v2,rowid,name) VALUES('delete',old.rowid,old.name); END;
      CREATE TRIGGER IF NOT EXISTS provider_vod_search_v2_update AFTER UPDATE OF name ON provider_vod BEGIN INSERT INTO provider_vod_search_v2(provider_vod_search_v2,rowid,name) VALUES('delete',old.rowid,old.name); INSERT INTO provider_vod_search_v2(rowid,name) VALUES(new.rowid,new.name); END;") .map_err(db_error)?;
    if !indexed {
        tx.execute(
            "INSERT INTO provider_vod_search_v2(provider_vod_search_v2) VALUES('rebuild')",
            [],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct OwnershipReport {
    pub unassigned: Vec<i64>,
    pub assignments: Vec<(i64, i64)>,
}
pub(crate) fn inspect_ownership(db: &Connection) -> Result<OwnershipReport> {
    let initialized: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_ownership')", [], |row| row.get(0)).map_err(db_error)?;
    let sql = if initialized {
        "SELECT p.id,o.account_id FROM providers p LEFT JOIN provider_ownership o ON o.provider_id=p.id ORDER BY p.id"
    } else {
        "SELECT id,NULL FROM providers ORDER BY id"
    };
    let mut query = db.prepare(sql).map_err(db_error)?;
    let mut report = OwnershipReport {
        unassigned: vec![],
        assignments: vec![],
    };
    for row in query
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })
        .map_err(db_error)?
    {
        let (id, owner) = row.map_err(db_error)?;
        if let Some(owner) = owner {
            report.assignments.push((id, owner));
        } else {
            report.unassigned.push(id);
        }
    }
    Ok(report)
}

/// Requires a complete explicit map. Unknown/disabled accounts and partial maps
/// roll back atomically rather than assigning server-wide subscriptions publicly.
pub(crate) fn assign_legacy(
    tx: &rusqlite::Transaction<'_>,
    owners: &BTreeMap<i64, i64>,
) -> Result<()> {
    let report = inspect_ownership(tx)?;
    if report.unassigned.len() != owners.len()
        || report.unassigned.iter().any(|id| !owners.contains_key(id))
    {
        return Err("legacy_provider_owner_map_required");
    }
    for (&provider, &account) in owners {
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE id=?1 AND disabled=0)",
                [account],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !active {
            return Err("invalid_legacy_provider_owner");
        }
        tx.execute(
            "INSERT INTO provider_ownership(provider_id,account_id) VALUES(?1,?2)",
            params![provider, account],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

pub(crate) fn live_catalog(
    db: &Connection,
    account: i64,
    requested: Option<i64>,
) -> Result<Option<i64>> {
    let enabled = |id: i64| {
        db.query_row("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE o.account_id=?1 AND p.id=?2 AND p.enabled=1 AND p.enable_live=1)",params![account,id],|row|row.get::<_,bool>(0)).map_err(db_error)
    };
    if let Some(id) = requested {
        return if enabled(id)? {
            Ok(Some(id))
        } else {
            Err("catalog_unavailable")
        };
    }
    let prior: Option<i64> = db
        .query_row(
            "SELECT default_live_provider_id FROM account_media_settings WHERE account_id=?1",
            [account],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .flatten();
    if let Some(id) = prior {
        if enabled(id)? {
            return Ok(prior);
        }
    }
    let fallback:Option<i64>=db.query_row("SELECT p.id FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE o.account_id=?1 AND p.enabled=1 AND p.enable_live=1 ORDER BY p.id LIMIT 1",[account],|row|row.get(0)).optional().map_err(db_error)?;
    db.execute("INSERT INTO account_media_settings(account_id,default_live_provider_id) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET default_live_provider_id=excluded.default_live_provider_id",params![account,fallback]).map_err(db_error)?;
    Ok(fallback)
}
pub(crate) fn set_live_default(db: &Connection, account: i64, provider: i64) -> Result<()> {
    live_catalog(db, account, Some(provider))?;
    db.execute("INSERT INTO account_media_settings(account_id,default_live_provider_id) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET default_live_provider_id=excluded.default_live_provider_id",params![account,provider]).map_err(db_error)?;
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MatchFilter {
    pub provider_id: Option<i64>,
    pub kind: Option<String>,
    #[serde(default)]
    pub search: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MatchCursor {
    version: u8,
    account: i64,
    filter: MatchFilter,
    provider: i64,
    id: String,
}
#[derive(Debug, Serialize)]
pub(crate) struct MatchPage {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

pub(crate) fn matches_page(
    db: &Connection,
    account: i64,
    filter: MatchFilter,
    cursor: Option<&str>,
    limit: usize,
) -> Result<MatchPage> {
    if !(1..=200).contains(&limit)
        || filter.search.len() > 128
        || filter
            .kind
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "movie" | "series"))
    {
        return Err("invalid_matches_query");
    }
    let search = filter
        .search
        .split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let (after_provider, after_id) = if let Some(cursor) = cursor {
        if cursor.len() > MATCH_CURSOR_BYTES {
            return Err("invalid_cursor");
        }
        let decoded: MatchCursor = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| "invalid_cursor")?,
        )
        .map_err(|_| "invalid_cursor")?;
        if decoded.version != 1 || decoded.account != account || decoded.filter != filter {
            return Err("invalid_cursor");
        }
        (decoded.provider, decoded.id)
    } else {
        (0, String::new())
    };
    let mut query = db
        .prepare(include_str!("matches_page_v2.sql"))
        .map_err(db_error)?;
    let mut items=query.query_map(params![account,filter.provider_id,filter.kind,after_provider,after_id,search,limit+1],|row|Ok(json!({"vod_id":row.get::<_,String>(0)?,"provider_id":row.get::<_,i64>(1)?,"type":row.get::<_,String>(2)?,"name":row.get::<_,String>(3)?,"year":row.get::<_,Option<i64>>(4)?,"poster":row.get::<_,Option<String>>(5)?}))).map_err(db_error)?.collect::<std::result::Result<Vec<_>,_>>().map_err(db_error)?;
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        let last = items.last().ok_or("invalid_cursor")?;
        let value = MatchCursor {
            version: 1,
            account,
            filter,
            provider: last["provider_id"].as_i64().ok_or("invalid_cursor")?,
            id: last["vod_id"].as_str().ok_or("invalid_cursor")?.into(),
        };
        Some(encode_page_cursor(&value, MATCH_CURSOR_BYTES)?)
    } else {
        None
    };
    Ok(MatchPage { items, next_cursor })
}

pub(crate) fn override_match(
    db: &Connection,
    account: i64,
    vod_id: &str,
    metadata_id: &str,
    kind: &str,
) -> Result<()> {
    if vod_id.is_empty()
        || vod_id.len() > 256
        || metadata_id.is_empty()
        || metadata_id.len() > 256
        || vod_id.chars().any(char::is_control)
        || metadata_id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || !matches!(kind, "movie" | "series")
    {
        return Err("invalid_match_request");
    }
    // Authorization is part of the write, not an earlier lookup vulnerable to
    // a concurrent ownership change. Unknown/foreign IDs share one response.
    let changed = db.execute("INSERT INTO provider_matches(vod_id,metadata_id,kind)
        SELECT v.id,?3,v.kind FROM provider_vod v JOIN provider_ownership o ON o.provider_id=v.provider_id
        WHERE v.id=?2 AND v.kind=?4 AND o.account_id=?1
        ON CONFLICT(vod_id) DO UPDATE SET metadata_id=excluded.metadata_id,kind=excluded.kind",
        params![account, vod_id, super::normalize::canonical_id(metadata_id), kind]).map_err(db_error)?;
    if changed == 0 {
        return Err("stream_candidate_not_found");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_cursor_encoder_accepts_exact_decoder_bounds_and_refuses_larger_tokens() {
        for bound in [MATCH_CURSOR_BYTES, 4096] {
            // A JSON string adds exactly two quote bytes; these bounds are
            // multiples of four, so base64 without padding fills them exactly.
            let value = "x".repeat(bound / 4 * 3 - 2);
            let encoded = encode_page_cursor(&value, bound).unwrap();
            assert_eq!(encoded.len(), bound);
            assert_eq!(
                serde_json::from_slice::<String>(&URL_SAFE_NO_PAD.decode(encoded).unwrap())
                    .unwrap(),
                value
            );
            assert_eq!(
                encode_page_cursor(&(value + "x"), bound),
                Err("catalog_cursor_too_large")
            );
        }
    }
    fn assign_legacy(db: &Connection, owners: &BTreeMap<i64, i64>) -> Result<()> {
        let tx = db.unchecked_transaction().map_err(db_error)?;
        super::assign_legacy(&tx, owners)?;
        tx.commit().map_err(db_error)
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,disabled INTEGER NOT NULL); INSERT INTO auth_accounts VALUES(11,0),(22,0),(33,1);
        CREATE TABLE providers(id INTEGER PRIMARY KEY,enabled INTEGER NOT NULL,enable_live INTEGER NOT NULL,enable_movies INTEGER NOT NULL,enable_series INTEGER NOT NULL); INSERT INTO providers VALUES(1,1,1,1,1),(2,1,1,1,1),(3,1,1,1,1);
        CREATE TABLE provider_vod(id TEXT PRIMARY KEY,provider_id INTEGER,kind TEXT,name TEXT,year INTEGER,poster TEXT,imdb_id TEXT,tmdb_id TEXT);
        CREATE TABLE provider_matches(vod_id TEXT PRIMARY KEY,metadata_id TEXT,kind TEXT);").unwrap();
        init(&db).unwrap();
        db
    }
    #[test]
    fn legacy_ownership_requires_complete_explicit_valid_map() {
        let db = fixture();
        assert_eq!(
            assign_legacy(&db, &BTreeMap::from([(1, 11)])),
            Err("legacy_provider_owner_map_required")
        );
        assert!(assign_legacy(&db, &BTreeMap::from([(1, 11), (2, 33), (3, 22)])).is_err());
        assert_eq!(inspect_ownership(&db).unwrap().unassigned.len(), 3);
        assign_legacy(&db, &BTreeMap::from([(1, 11), (2, 11), (3, 22)])).unwrap();
        assert!(inspect_ownership(&db).unwrap().unassigned.is_empty());
    }
    #[test]
    fn persistent_account_default_and_request_override_are_isolated() {
        let db = fixture();
        assign_legacy(&db, &BTreeMap::from([(1, 11), (2, 11), (3, 22)])).unwrap();
        assert_eq!(live_catalog(&db, 11, None).unwrap(), Some(1));
        assert_eq!(live_catalog(&db, 11, Some(2)).unwrap(), Some(2));
        assert_eq!(live_catalog(&db, 11, None).unwrap(), Some(1));
        assert_eq!(live_catalog(&db, 11, Some(3)), Err("catalog_unavailable"));
        set_live_default(&db, 11, 2).unwrap();
        assert_eq!(live_catalog(&db, 11, None).unwrap(), Some(2));
        db.execute("UPDATE providers SET enabled=0 WHERE id=2", [])
            .unwrap();
        assert_eq!(live_catalog(&db, 11, None).unwrap(), Some(1));
        assert_eq!(live_catalog(&db, 22, None).unwrap(), Some(3));
    }
    #[test]
    fn matches_are_bounded_filtered_and_cannot_cross_account_cursors() {
        let db = fixture();
        assign_legacy(&db, &BTreeMap::from([(1, 11), (2, 11), (3, 22)])).unwrap();
        for i in 1..=210 {
            db.execute("INSERT INTO provider_vod(id,provider_id,kind,name,year) VALUES(?1,1,'movie',?2,2020)",params![format!("{i:04}"),format!("Fixture movie {i}")]).unwrap();
        }
        db.execute("INSERT INTO provider_vod(id,provider_id,kind,name) VALUES('private',3,'movie','Private movie')",[]).unwrap();
        let first = matches_page(&db, 11, MatchFilter::default(), None, 50).unwrap();
        assert_eq!(first.items.len(), 50);
        let second = matches_page(
            &db,
            11,
            MatchFilter::default(),
            first.next_cursor.as_deref(),
            50,
        )
        .unwrap();
        assert_ne!(first.items[0]["vod_id"], second.items[0]["vod_id"]);
        assert!(matches_page(
            &db,
            22,
            MatchFilter::default(),
            first.next_cursor.as_deref(),
            50
        )
        .is_err());
        let private = matches_page(
            &db,
            11,
            MatchFilter {
                search: "Private".into(),
                ..Default::default()
            },
            None,
            50,
        )
        .unwrap();
        assert!(private.items.is_empty());
        assert_eq!(
            matches_page(&db, 22, MatchFilter::default(), None, 50)
                .unwrap()
                .items
                .len(),
            1
        );
        db.execute(
            "INSERT INTO provider_matches VALUES('0001','tt1234567','movie')",
            [],
        )
        .unwrap();
        assert_eq!(
            matches_page(&db, 11, MatchFilter::default(), None, 1)
                .unwrap()
                .items[0]["vod_id"],
            "0002"
        );
    }
}
