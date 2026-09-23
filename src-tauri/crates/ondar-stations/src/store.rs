//! Favourites and recently played, in the cache's database. Both keep the whole `Station` as
//! JSON so an entry stays playable after its country list is replaced or the station drops
//! out of the cap.
//!
//! Recents (decision 3, 2026-09-21): **20 entries, a replay moves the entry to the top** —
//! the same uuid is never listed twice, and the list is ordered by last play.

use rusqlite::params;

use crate::cache::{Cache, CacheError};
use crate::model::Station;

pub const RECENTS_CAP: usize = 20;

/// Favourites, most recently added first.
pub fn list_favourites(cache: &Cache) -> Result<Vec<Station>, CacheError> {
    let mut stmt = cache
        .conn()
        .prepare("SELECT station_json FROM favourites ORDER BY added_at DESC, rowid DESC")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .iter()
        .map(|j| serde_json::from_str::<Station>(j))
        .collect::<Result<Vec<_>, _>>()?)
}

/// Add (or refresh the snapshot of) a favourite. Idempotent on uuid.
pub fn add_favourite(cache: &Cache, station: &Station) -> Result<(), CacheError> {
    cache.conn().execute(
        "INSERT INTO favourites (uuid, added_at, station_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(uuid) DO UPDATE SET station_json = excluded.station_json",
        params![station.uuid, cache.now(), serde_json::to_string(station)?],
    )?;
    Ok(())
}

pub fn remove_favourite(cache: &Cache, uuid: &str) -> Result<bool, CacheError> {
    Ok(cache
        .conn()
        .execute("DELETE FROM favourites WHERE uuid = ?1", [uuid])?
        > 0)
}

pub fn is_favourite(cache: &Cache, uuid: &str) -> Result<bool, CacheError> {
    let n: i64 = cache.conn().query_row(
        "SELECT COUNT(*) FROM favourites WHERE uuid = ?1",
        [uuid],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Recents, most recently played first, at most `RECENTS_CAP`.
pub fn list_recents(cache: &Cache) -> Result<Vec<Station>, CacheError> {
    let mut stmt = cache
        .conn()
        .prepare("SELECT station_json FROM recents ORDER BY played_at DESC, rowid DESC LIMIT ?1")?;
    let rows = stmt
        .query_map([RECENTS_CAP as i64], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .iter()
        .map(|j| serde_json::from_str::<Station>(j))
        .collect::<Result<Vec<_>, _>>()?)
}

/// Record a play: a replay of a listed station moves it to the top (its `played_at` and
/// snapshot updated, no second row); the list is then trimmed to `RECENTS_CAP`, oldest out.
/// Called by the shell on the first `Playing` after a `play` (M3b wires it, with the click).
pub fn record_played(cache: &Cache, station: &Station) -> Result<(), CacheError> {
    let conn = cache.conn();
    conn.execute(
        "INSERT INTO recents (uuid, played_at, station_json) VALUES (?1, ?2, ?3)
         ON CONFLICT(uuid) DO UPDATE SET played_at = excluded.played_at, station_json = excluded.station_json",
        params![station.uuid, cache.now(), serde_json::to_string(station)?],
    )?;
    conn.execute(
        "DELETE FROM recents WHERE uuid NOT IN (SELECT uuid FROM recents ORDER BY played_at DESC, rowid DESC LIMIT ?1)",
        [RECENTS_CAP as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{fake_clock, st};

    const T0: i64 = 1_800_000_000;

    #[test]
    fn replay_moves_to_top_without_duplicate() {
        // Fails if a replay appends (two rows for "a") or does not reorder (b stays first).
        let (clock, now) = fake_clock(T0);
        let cache = Cache::in_memory(clock).unwrap();
        record_played(&cache, &st("a", 1)).unwrap();
        *now.lock().unwrap() = T0 + 10;
        record_played(&cache, &st("b", 1)).unwrap();
        *now.lock().unwrap() = T0 + 20;
        record_played(&cache, &st("a", 1)).unwrap();
        let ids: Vec<String> = list_recents(&cache)
            .unwrap()
            .into_iter()
            .map(|s| s.uuid)
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn recents_cap_evicts_the_oldest() {
        let (clock, now) = fake_clock(T0);
        let cache = Cache::in_memory(clock).unwrap();
        for i in 0..(RECENTS_CAP + 5) {
            *now.lock().unwrap() = T0 + i as i64;
            record_played(&cache, &st(&format!("s{i:02}"), 1)).unwrap();
        }
        let ids: Vec<String> = list_recents(&cache)
            .unwrap()
            .into_iter()
            .map(|s| s.uuid)
            .collect();
        assert_eq!(ids.len(), RECENTS_CAP);
        assert_eq!(ids[0], "s24", "newest first");
        assert_eq!(ids[RECENTS_CAP - 1], "s05", "s00..s04 evicted");
        let stored: i64 = cache
            .conn()
            .query_row("SELECT COUNT(*) FROM recents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            stored, RECENTS_CAP as i64,
            "trimmed in the table, not only in the query"
        );
    }

    #[test]
    fn favourite_survives_list_replacement() {
        // The favourite's snapshot is its own row: replacing PT's list, or dropping the
        // station from it, leaves the favourite listed and playable.
        let (clock, _) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        let fav = st("fav", 5);
        cache
            .put_stations("PT", &[fav.clone(), st("x", 1)], 2, T0)
            .unwrap();
        add_favourite(&cache, &fav).unwrap();
        cache.put_stations("PT", &[st("y", 1)], 1, T0 + 1).unwrap();
        let favs = list_favourites(&cache).unwrap();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].url, "http://fav/");
        assert!(is_favourite(&cache, "fav").unwrap());
    }

    #[test]
    fn add_is_idempotent_and_remove_reports_whether_it_removed() {
        let (clock, now) = fake_clock(T0);
        let cache = Cache::in_memory(clock).unwrap();
        add_favourite(&cache, &st("a", 1)).unwrap();
        *now.lock().unwrap() = T0 + 1;
        add_favourite(&cache, &st("b", 1)).unwrap();
        add_favourite(&cache, &st("a", 1)).unwrap();
        let ids: Vec<String> = list_favourites(&cache)
            .unwrap()
            .into_iter()
            .map(|s| s.uuid)
            .collect();
        assert_eq!(
            ids,
            ["b", "a"],
            "most recently added first; a re-add keeps its place"
        );
        assert!(remove_favourite(&cache, "a").unwrap());
        assert!(!remove_favourite(&cache, "a").unwrap());
        assert!(!is_favourite(&cache, "a").unwrap());
        assert_eq!(list_favourites(&cache).unwrap().len(), 1);
    }
}
