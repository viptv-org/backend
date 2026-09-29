//! Raw account-owned live snapshots: provider order, bounded pages, no lineup rules.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    let tx = db.unchecked_transaction()?;
    let columns = tx
        .prepare("PRAGMA table_info(provider_live)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|name| name == "ordinal") {
        tx.execute_batch(
            "ALTER TABLE provider_live ADD COLUMN ordinal INTEGER NOT NULL DEFAULT 0;
            UPDATE provider_live SET ordinal=rowid;",
        )?;
    }
    // Legacy row insertion order is the best available order until the next refresh.
    tx.execute_batch("CREATE INDEX IF NOT EXISTS provider_live_page_v2 ON provider_live(provider_id,ordinal,id);
        CREATE INDEX IF NOT EXISTS provider_live_category_page_v2 ON provider_live(provider_id,category_id,ordinal,id);
        CREATE TABLE IF NOT EXISTS provider_live_generations(provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,generation INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS provider_live_categories_v2(provider_id INTEGER NOT NULL REFERENCES providers(id) ON DELETE CASCADE,id TEXT NOT NULL,name TEXT NOT NULL,ordinal INTEGER NOT NULL,PRIMARY KEY(provider_id,id));
        CREATE INDEX IF NOT EXISTS provider_live_categories_page_v2 ON provider_live_categories_v2(provider_id,ordinal,id);")?;
    tx.execute_batch("INSERT INTO provider_live_categories_v2(provider_id,id,name,ordinal)
        SELECT provider_id,category_id,COALESCE(category,category_id),MIN(ordinal) FROM provider_live
        WHERE category_id IS NOT NULL AND provider_id NOT IN (SELECT provider_id FROM provider_live_generations)
        GROUP BY provider_id,category_id ON CONFLICT(provider_id,id) DO NOTHING;
        INSERT INTO provider_live_generations(provider_id,generation) SELECT id,0 FROM providers WHERE true ON CONFLICT(provider_id) DO NOTHING;")?;
    tx.commit()
}

pub(super) fn replace_categories(
    db: &Connection,
    provider: i64,
    categories: &[Value],
) -> rusqlite::Result<()> {
    db.execute(
        "DELETE FROM provider_live_categories_v2 WHERE provider_id=?1",
        [provider],
    )?;
    let mut insert = db.prepare("INSERT INTO provider_live_categories_v2(provider_id,id,name,ordinal) VALUES(?1,?2,?3,?4) ON CONFLICT(provider_id,id) DO NOTHING")?;
    for (ordinal, value) in categories.iter().enumerate() {
        if let Some(id) = value.get("category_id").and_then(super::scalar) {
            let name = super::text(value, "category_name").unwrap_or_else(|| id.clone());
            insert.execute(params![provider, id, name, ordinal as i64])?;
        }
    }
    // Some providers omit a category from get_live_categories; retain its ID.
    db.execute("INSERT INTO provider_live_categories_v2(provider_id,id,name,ordinal)
        SELECT provider_id,category_id,COALESCE(category,category_id),?2+MIN(ordinal) FROM provider_live
        WHERE provider_id=?1 AND category_id IS NOT NULL GROUP BY category_id
        ON CONFLICT(provider_id,id) DO NOTHING", params![provider,categories.len() as i64])?;
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Filter {
    pub catalog_id: Option<i64>,
    pub category_id: Option<String>,
    #[serde(default)]
    pub search: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Channels,
    Categories,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    account: i64,
    catalog: i64,
    generation: i64,
    kind: Kind,
    filter: Filter,
    ordinal: i64,
    id: String,
}
#[derive(Debug, Serialize)]
pub(crate) struct Page {
    pub catalog_id: Option<i64>,
    pub generation: Option<i64>,
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}
fn storage(_: rusqlite::Error) -> &'static str {
    "provider_storage_unavailable"
}

pub(crate) fn page(
    db: &Connection,
    account: i64,
    kind: Kind,
    filter: Filter,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Page, &'static str> {
    if !(1..=200).contains(&limit)
        || filter.search.len() > 128
        || filter.category_id.as_ref().is_some_and(|id| id.len() > 256)
        || (kind == Kind::Categories && filter.category_id.is_some())
    {
        return Err("invalid_catalog_query");
    }
    let previous = cursor
        .map(|value| {
            if value.len() > 4096 {
                return Err("invalid_cursor");
            }
            let decoded: Cursor = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| "invalid_cursor")?,
            )
            .map_err(|_| "invalid_cursor")?;
            if decoded.version != 1
                || decoded.account != account
                || decoded.filter != filter
                || decoded.kind != kind
                || decoded.ordinal < 0
            {
                return Err("invalid_cursor");
            }
            Ok(decoded)
        })
        .transpose()?;
    // A read transaction keeps ownership, generation and page rows on one snapshot,
    // including when another process commits a provider refresh.
    let tx = db.unchecked_transaction().map_err(storage)?;
    let catalog = super::v2::live_catalog(&tx, account, filter.catalog_id)?;
    let Some(catalog) = catalog else {
        if previous.is_some() {
            return Err("catalog_changed");
        }
        tx.commit().map_err(storage)?;
        return Ok(Page {
            catalog_id: None,
            generation: None,
            items: vec![],
            next_cursor: None,
        });
    };
    let generation = tx
        .query_row(
            "SELECT generation FROM provider_live_generations WHERE provider_id=?1",
            [catalog],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .unwrap_or(0);
    if previous
        .as_ref()
        .is_some_and(|c| c.catalog != catalog || c.generation != generation)
    {
        return Err("catalog_changed");
    }
    let ordinal = previous.as_ref().map_or(-1, |c| c.ordinal);
    let id = previous.as_ref().map_or("", |c| c.id.as_str());
    // instr treats user '%'/'_' literally. Filtering happens before the bound;
    // no full catalog collection or synchronous count is performed.
    let sql = match kind {
        Kind::Channels => {
            "SELECT ordinal,id,name,logo,category_id,category,epg_channel_id FROM provider_live
            WHERE provider_id=?1 AND (ordinal,id)>(?2,?3) AND (?4 IS NULL OR category_id=?4)
            AND (?5='' OR instr(lower(name),lower(?5))>0) ORDER BY ordinal,id LIMIT ?6"
        }
        Kind::Categories => {
            "SELECT ordinal,id,name,NULL,id,name,NULL FROM provider_live_categories_v2
            WHERE provider_id=?1 AND (ordinal,id)>(?2,?3) AND ?4 IS NULL
            AND (?5='' OR instr(lower(name),lower(?5))>0) ORDER BY ordinal,id LIMIT ?6"
        }
    };
    let mut rows = tx.prepare(sql).map_err(storage)?.query_map(params![catalog,ordinal,id,filter.category_id,filter.search,limit+1], |row| {
        let item = match kind {
            Kind::Channels => json!({"id":row.get::<_,String>(1)?,"name":row.get::<_,String>(2)?,"logo":row.get::<_,Option<String>>(3)?,"category_id":row.get::<_,Option<String>>(4)?,"category":row.get::<_,Option<String>>(5)?,"epg_channel_id":row.get::<_,Option<String>>(6)?}),
            Kind::Categories => json!({"id":row.get::<_,String>(1)?,"name":row.get::<_,String>(2)?}),
        };
        Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,item))
    }).map_err(storage)?.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more {
        let (ordinal, id, _) = rows.last().ok_or("invalid_cursor")?;
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    version: 1,
                    account,
                    catalog,
                    generation,
                    kind,
                    filter,
                    ordinal: *ordinal,
                    id: id.clone(),
                })
                .map_err(|_| "invalid_cursor")?,
            ),
        )
    } else {
        None
    };
    tx.commit().map_err(storage)?;
    Ok(Page {
        catalog_id: Some(catalog),
        generation: Some(generation),
        items: rows.into_iter().map(|(_, _, v)| v).collect(),
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn additive_schema_preserves_existing_rows_and_is_repeatable() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE providers(id INTEGER PRIMARY KEY);
            INSERT INTO providers VALUES(1);
            CREATE TABLE provider_live(id TEXT PRIMARY KEY,provider_id INTEGER,name TEXT,category TEXT,category_id TEXT);
            INSERT INTO provider_live VALUES('z',1,'Last alphabetically','Zulu','9'),('a',1,'First alphabetically','Alpha','2');").unwrap();
        init(&db).unwrap();
        init(&db).unwrap();
        let rows = db
            .prepare("SELECT id FROM provider_live ORDER BY ordinal,id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows, vec!["z", "a"]);
        let categories = db
            .prepare("SELECT id FROM provider_live_categories_v2 ORDER BY ordinal,id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(categories, vec!["9", "2"]);
        assert_eq!(
            db.query_row(
                "SELECT generation FROM provider_live_generations WHERE provider_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
