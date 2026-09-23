//! The SQLite cache: countries and per-country station lists with their fetch time, plus the
//! favourites and recents tables the store (`store.rs`) uses. One connection, owned by the
//! service's DB thread; never touched from anywhere else.
//!
//! What the cache decides: whether a stored list is **fresh** (within its TTL) or **expired**.
//! What it never does: drop an expired list — an expired list is served while a refresh runs
//! (stale-while-revalidate, plan F5) and with no age ceiling when the network is down (G3).
//! The service orchestrates the fetches; this module reads and writes.
//!
//! TTLs, from the census: radio-browser rechecks every station roughly daily (`lastchecktime`
//! age p50 13–18 h), so 24 h for a list tracks the data's own cadence; the countries list
//! changes by units, so 7 days.

use std::path::Path;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Country, Station};

pub const TTL_COUNTRIES: i64 = 7 * 24 * 3600;
pub const TTL_STATIONS: i64 = 24 * 3600;

/// Unix seconds. Injected so the TTL tests use a fake clock, never `sleep`.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored station is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A stored list with its age. `expired` is the cache's one judgement: `age_secs >= ttl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored<T> {
    pub items: T,
    pub fetched_at: i64,
    pub age_secs: u64,
    pub expired: bool,
}

impl<T> Stored<T> {
    /// What a list looks like the moment it was written: age 0, not expired. The service
    /// answers a fetch's waiters from this rather than re-reading what it just stored.
    pub fn just_fetched(items: T, fetched_at: i64) -> Stored<T> {
        Stored {
            items,
            fetched_at,
            age_secs: 0,
            expired: false,
        }
    }
}

pub struct Cache {
    conn: Connection,
    clock: Clock,
}

/// Schema steps, applied in order inside one transaction each; `PRAGMA user_version` is the
/// count applied. Append, never edit: an installed database at version n runs steps n.. only.
const MIGRATIONS: &[&str] = &[
    // v1 — M3a. `station_json` is the whole `Station` (the boundary type), so a favourite stays
    // playable after its country list is replaced or it drops out of the cap; the columns beside
    // it are what lists and a local search need without parsing the blob.
    "CREATE TABLE countries (
        code TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        station_count INTEGER NOT NULL,
        fetched_at INTEGER NOT NULL
    );
    CREATE TABLE station_lists (
        cc TEXT PRIMARY KEY,
        fetched_at INTEGER NOT NULL,
        source_rows INTEGER NOT NULL,
        kept_rows INTEGER NOT NULL
    );
    CREATE TABLE stations (
        uuid TEXT NOT NULL,
        cc TEXT NOT NULL,
        rank INTEGER NOT NULL,
        name TEXT NOT NULL,
        url TEXT NOT NULL,
        votes INTEGER NOT NULL,
        station_json TEXT NOT NULL,
        PRIMARY KEY (cc, uuid)
    );
    CREATE INDEX stations_cc_rank ON stations (cc, rank);
    CREATE TABLE favourites (
        uuid TEXT PRIMARY KEY,
        added_at INTEGER NOT NULL,
        station_json TEXT NOT NULL
    );
    CREATE TABLE recents (
        uuid TEXT PRIMARY KEY,
        played_at INTEGER NOT NULL,
        station_json TEXT NOT NULL
    );",
    // v2 — M3b commit 5 (review F3). The click endpoint looks a played station up by uuid alone
    // (the shell has only the id the page handed to `play`); the primary key is `(cc, uuid)`,
    // so without this the lookup on the play path is a scan of every cached list.
    "CREATE INDEX IF NOT EXISTS stations_uuid ON stations (uuid);",
];

impl Cache {
    /// Open (or create) the database at `path` and bring it to the current schema.
    pub fn open(path: &Path, clock: Clock) -> Result<Cache, CacheError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        Self::with_connection(conn, clock)
    }

    /// An in-memory database, for tests.
    pub fn in_memory(clock: Clock) -> Result<Cache, CacheError> {
        Self::with_connection(Connection::open_in_memory()?, clock)
    }

    fn with_connection(mut conn: Connection, clock: Clock) -> Result<Cache, CacheError> {
        migrate(&mut conn)?;
        Ok(Cache { conn, clock })
    }

    pub fn schema_version(&self) -> Result<u32, CacheError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    /// The store (`store.rs`) shares the connection: favourites and recents live in the same
    /// file, on the same thread.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn stored<T>(&self, items: T, fetched_at: i64, ttl: i64) -> Stored<T> {
        let age = (self.now() - fetched_at).max(0);
        Stored {
            items,
            fetched_at,
            age_secs: age as u64,
            expired: age >= ttl,
        }
    }

    /// The countries list, if one was ever stored. Ordered as stored (by name, as served).
    pub fn countries(&self) -> Result<Option<Stored<Vec<Country>>>, CacheError> {
        let fetched_at: Option<i64> = self
            .conn
            .query_row("SELECT MIN(fetched_at) FROM countries", [], |r| r.get(0))
            .optional()?
            .flatten();
        let Some(fetched_at) = fetched_at else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare("SELECT code, name, station_count FROM countries ORDER BY rowid")?;
        let items = stmt
            .query_map([], |r| {
                Ok(Country {
                    code: r.get(0)?,
                    name: r.get(1)?,
                    station_count: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(self.stored(items, fetched_at, TTL_COUNTRIES)))
    }

    /// Replace the countries list in one transaction.
    pub fn put_countries(&mut self, items: &[Country], fetched_at: i64) -> Result<(), CacheError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM countries", [])?;
        {
            let mut ins = tx.prepare("INSERT INTO countries (code, name, station_count, fetched_at) VALUES (?1, ?2, ?3, ?4)")?;
            for c in items {
                ins.execute(params![c.code, c.name, c.station_count, fetched_at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The published `station_count` for a code, for the truncation guard's `expected`.
    pub fn station_count(&self, cc: &str) -> Result<Option<u32>, CacheError> {
        Ok(self
            .conn
            .query_row(
                "SELECT station_count FROM countries WHERE code = ?1",
                [cc],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// One country's ranked list, if stored, in rank order.
    pub fn stations(&self, cc: &str) -> Result<Option<Stored<Vec<Station>>>, CacheError> {
        let fetched_at: Option<i64> = self
            .conn
            .query_row(
                "SELECT fetched_at FROM station_lists WHERE cc = ?1",
                [cc],
                |r| r.get(0),
            )
            .optional()?;
        let Some(fetched_at) = fetched_at else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare("SELECT station_json FROM stations WHERE cc = ?1 ORDER BY rank")?;
        let rows = stmt
            .query_map([cc], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let items = rows
            .iter()
            .map(|j| serde_json::from_str::<Station>(j))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(self.stored(items, fetched_at, TTL_STATIONS)))
    }

    /// Replace one country's list (already ranked and capped) in one transaction.
    pub fn put_stations(
        &mut self,
        cc: &str,
        ranked: &[Station],
        source_rows: usize,
        fetched_at: i64,
    ) -> Result<(), CacheError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM stations WHERE cc = ?1", [cc])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO stations (uuid, cc, rank, name, url, votes, station_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (rank, s) in ranked.iter().enumerate() {
                ins.execute(params![
                    s.uuid,
                    cc,
                    rank as i64,
                    s.name,
                    s.url,
                    s.votes,
                    serde_json::to_string(s)?
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO station_lists (cc, fetched_at, source_rows, kept_rows) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(cc) DO UPDATE SET fetched_at = excluded.fetched_at, source_rows = excluded.source_rows, kept_rows = excluded.kept_rows",
            params![cc, fetched_at, source_rows as i64, ranked.len() as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Local search over every cached list (the offline fallback for `search_stations`): a
    /// substring match on the name, by votes. Case folding is SQLite `LIKE`'s, which is
    /// **ASCII only** (`RÁDIO` does not match `rádio`); proper folding is an M3b decision.
    /// `%`, `_` and `\` in the query are escaped, so they match themselves: until 2026-09-22
    /// they were stripped, `Radio_1` could never be found and a query of `%` became `LIKE '%%'`
    /// — the top 50 of every cached list (`/code-review` finding 9).
    /// A station by uuid from any cached list, else the favourites, else the recents — the
    /// snapshot the click endpoint records as a recent (M3b commit 5). `None` for an id that
    /// is not a directory station's.
    pub fn station_by_uuid(&self, uuid: &str) -> Result<Option<Station>, CacheError> {
        for sql in [
            "SELECT station_json FROM stations WHERE uuid = ?1 LIMIT 1",
            "SELECT station_json FROM favourites WHERE uuid = ?1",
            "SELECT station_json FROM recents WHERE uuid = ?1",
        ] {
            let json: Option<String> = self.conn.query_row(sql, [uuid], |r| r.get(0)).optional()?;
            if let Some(j) = json {
                return Ok(Some(serde_json::from_str::<Station>(&j)?));
            }
        }
        Ok(None)
    }

    pub fn search_local(&self, query: &str, limit: usize) -> Result<Vec<Station>, CacheError> {
        let like = format!("%{}%", like_escape(query.trim()));
        let mut stmt = self.conn.prepare(
            "SELECT station_json FROM stations WHERE name LIKE ?1 ESCAPE '\\' ORDER BY votes DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![like, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .iter()
            .map(|j| serde_json::from_str::<Station>(j))
            .collect::<Result<Vec<_>, _>>()?)
    }
}

/// Escape SQLite `LIKE`'s metacharacters (and the escape itself) with `\`.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Bring `conn` to `MIGRATIONS.len()`: each pending step and the version stamp in one
/// transaction, so a crash mid-step leaves the version untouched and the step reruns whole.
pub fn migrate(conn: &mut Connection) -> Result<(), CacheError> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, step) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", (i + 1) as u32)?;
        tx.commit()?;
        log::info!("cache schema migrated to version {}", i + 1);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::model::Codec;

    /// A settable clock: `(clock, set)`.
    pub fn fake_clock(start: i64) -> (Clock, Arc<Mutex<i64>>) {
        let cell = Arc::new(Mutex::new(start));
        let c = cell.clone();
        (Arc::new(move || *c.lock().unwrap()), cell)
    }

    pub fn st(uuid: &str, votes: i64) -> Station {
        Station {
            uuid: uuid.into(),
            name: format!("Station {uuid}"),
            url: format!("http://{uuid}/"),
            homepage: String::new(),
            favicon: String::new(),
            country_code: "PT".into(),
            codec: Codec::Mp3,
            codec_raw: "MP3".into(),
            bitrate_kbps: Some(128),
            hls: false,
            video: false,
            votes,
            click_count: 0,
            click_trend: 0,
            geo: None,
            last_check_ok: true,
        }
    }

    const T0: i64 = 1_800_000_000;

    #[test]
    fn migrations_are_idempotent_and_versioned() {
        let (clock, _) = fake_clock(T0);
        let cache = Cache::in_memory(clock.clone()).unwrap();
        assert_eq!(cache.schema_version().unwrap(), MIGRATIONS.len() as u32);
        // A v0 database (the version pragma unset) upgrades and is stamped in the same
        // transaction; running again applies nothing (a rerun of step 1 would fail on CREATE
        // TABLE) and keeps the version.
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            0
        );
        migrate(&mut conn).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            MIGRATIONS.len() as u32
        );
        migrate(&mut conn).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            MIGRATIONS.len() as u32
        );
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            tables,
            [
                "countries",
                "favourites",
                "recents",
                "station_lists",
                "stations"
            ]
        );
    }

    #[test]
    fn a_stored_list_is_fresh_within_its_ttl_and_expired_one_second_past_it() {
        // The boundary, both sides. Fails if the comparison is inverted, `<=`, or off by the unit.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache
            .put_stations("PT", &[st("a", 5), st("b", 3)], 10, T0)
            .unwrap();
        let s = cache.stations("PT").unwrap().unwrap();
        assert_eq!((s.age_secs, s.expired), (0, false));
        assert_eq!(
            s.items.iter().map(|x| x.uuid.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "rank order"
        );
        *now.lock().unwrap() = T0 + TTL_STATIONS - 1;
        let s = cache.stations("PT").unwrap().unwrap();
        assert_eq!((s.age_secs, s.expired), ((TTL_STATIONS - 1) as u64, false));
        *now.lock().unwrap() = T0 + TTL_STATIONS;
        assert!(
            cache.stations("PT").unwrap().unwrap().expired,
            "expired at exactly the TTL"
        );
        *now.lock().unwrap() = T0 + TTL_STATIONS + 1;
        let s = cache.stations("PT").unwrap().unwrap();
        assert_eq!((s.age_secs, s.expired), ((TTL_STATIONS + 1) as u64, true));
    }

    #[test]
    fn an_expired_list_is_kept_with_its_true_age_and_no_ceiling() {
        // Nine days old: still there, age reported as nine days. Fails if an age ceiling drops
        // the list or the age is measured from anything but fetched_at.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("a", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + 9 * 24 * 3600;
        let s = cache.stations("PT").unwrap().expect("no age ceiling");
        assert_eq!(s.age_secs, 9 * 24 * 3600);
        assert!(s.expired);
        assert_eq!(s.fetched_at, T0);
        assert_eq!(s.items.len(), 1);
        assert!(
            cache.stations("ES").unwrap().is_none(),
            "a country never fetched has no list"
        );
    }

    #[test]
    fn put_replaces_the_list_atomically_and_updates_the_fetch_time() {
        let (clock, _) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache
            .put_stations("PT", &[st("a", 1), st("b", 1)], 2, T0)
            .unwrap();
        cache
            .put_stations("PT", &[st("c", 1)], 5, T0 + 100)
            .unwrap();
        let s = cache.stations("PT").unwrap().unwrap();
        assert_eq!(
            s.items.iter().map(|x| x.uuid.as_str()).collect::<Vec<_>>(),
            ["c"]
        );
        assert_eq!(s.fetched_at, T0 + 100);
        // Another country's list is untouched.
        cache.put_stations("ES", &[st("z", 1)], 1, T0).unwrap();
        cache.put_stations("PT", &[], 0, T0 + 200).unwrap();
        assert_eq!(cache.stations("ES").unwrap().unwrap().items.len(), 1);
    }

    #[test]
    fn countries_round_trip_with_their_ttl_and_station_count() {
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        assert!(cache.countries().unwrap().is_none());
        let cs = vec![
            Country {
                code: "PT".into(),
                name: "Portugal".into(),
                station_count: 371,
            },
            Country {
                code: "MT".into(),
                name: "Malta".into(),
                station_count: 14,
            },
        ];
        cache.put_countries(&cs, T0).unwrap();
        let c = cache.countries().unwrap().unwrap();
        assert_eq!(c.items, cs);
        assert!(!c.expired);
        assert_eq!(cache.station_count("PT").unwrap(), Some(371));
        assert_eq!(cache.station_count("XX").unwrap(), None);
        *now.lock().unwrap() = T0 + TTL_COUNTRIES;
        assert!(cache.countries().unwrap().unwrap().expired);
    }

    #[test]
    fn local_search_matches_like_metacharacters_literally() {
        // Finding 9. Fails on the stripping code: `Radio_1` became `%Radio1%` (no hit), and a
        // bare `%` became `%%` (every row).
        let (clock, _) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        let mut a = st("a", 9);
        a.name = "Radio_1".into();
        let mut b = st("b", 5);
        b.name = "Radio 100% Hits".into();
        let mut c = st("c", 7);
        c.name = "Bay".into();
        cache.put_stations("PT", &[a, b, c], 3, T0).unwrap();
        let ids = |q: &str| {
            cache
                .search_local(q, 10)
                .unwrap()
                .iter()
                .map(|s| s.uuid.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("Radio_1"), ["a"]);
        assert_eq!(
            ids("_"),
            ["a"],
            "an underscore is a character, not a wildcard"
        );
        assert_eq!(
            ids("%"),
            ["b"],
            "a percent sign is a character, not a wildcard"
        );
        assert_eq!(ids("100%"), ["b"]);
        assert!(
            ids("\\").is_empty(),
            "the escape character matches only itself"
        );
        assert_eq!(ids("radio"), ["a", "b"], "ASCII case folded, by votes");
    }

    #[test]
    fn local_search_is_a_case_insensitive_substring_over_every_cached_list() {
        let (clock, _) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        let mut a = st("a", 9);
        a.name = "Rádio Comercial".into();
        let mut b = st("b", 3);
        b.name = "RÁDIO Renascença".into();
        let mut c = st("c", 7);
        c.name = "Bay".into();
        cache.put_stations("PT", &[a, b], 2, T0).unwrap();
        cache.put_stations("MT", &[c], 1, T0).unwrap();
        let hits = cache.search_local("comercial", 10).unwrap();
        assert_eq!(
            hits.iter().map(|s| s.uuid.as_str()).collect::<Vec<_>>(),
            ["a"]
        );
        let hits = cache.search_local("r", 10).unwrap();
        assert_eq!(
            hits.iter().map(|s| s.uuid.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "by votes, across countries; SQLite LIKE folds ASCII case"
        );
    }

    /// M3b commit 5 (F6 test 13, review F3): v2 adds the uuid index; a v1 database migrates to
    /// it, a second `migrate` is a no-op, and the lookup finds a row under any country. Fails
    /// if the index, the step or the lookup is missing.
    #[test]
    fn migration_v2_adds_the_uuid_index_and_the_lookup_uses_any_country() {
        let (clock, _) = fake_clock(T0);
        let mut conn = Connection::open_in_memory().unwrap();
        // Bring the connection to v1 only, as an installed M3a database is.
        let tx = conn.transaction().unwrap();
        tx.execute_batch(MIGRATIONS[0]).unwrap();
        tx.pragma_update(None, "user_version", 1u32).unwrap();
        tx.commit().unwrap();
        migrate(&mut conn).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'stations_uuid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1, "the uuid index exists");
        migrate(&mut conn).unwrap();
        let again: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(again, 2, "idempotent");

        let mut cache = Cache::with_connection(conn, clock).unwrap();
        cache.put_stations("DE", &[st("u1", 5)], 344, T0).unwrap();
        assert_eq!(
            cache.station_by_uuid("u1").unwrap().map(|s| s.uuid),
            Some("u1".to_string()),
            "found under DE without knowing the country"
        );
        assert_eq!(cache.station_by_uuid("nope").unwrap(), None);
    }
}
