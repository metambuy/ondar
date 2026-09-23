//! The service: one thread owns the database and answers every request from it **without
//! awaiting the network**; fetches run as tasks on a small runtime and report back through
//! the same channel. Plan F3 (the split) and F5 (stale-while-revalidate, the `stations:updated`
//! event) are implemented here.
//!
//! Read path for a country's list: fresh in the cache → answered at once as `Cached`. Expired
//! → answered at once as `Cached { refreshing: true }` and a refresh started if none is in
//! flight. Missing → the caller waits under the fetch's key; concurrent callers for one
//! country share one fetch. When a fetch lands the list is replaced in one transaction, every
//! waiter gets `Fresh`, and the event sink receives `StationsUpdated` for the page to
//! re-request. When it fails, waiters get the error (they had nothing to fall back on), an
//! expired list stays where it was, with no age ceiling, and the sink receives the same event
//! with `outcome: Failed` — every fetch ends with exactly one event, so the page can clear its
//! `refreshing` flag (2026-09-22, acceptance item 6). A fetch whose list the cache refuses to
//! store ends exactly like a failed one: `Failed`, nothing replaced — the outcome is derived
//! from the write, never assumed from the fetch (`/code-review` finding 1, 2026-09-22).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use crate::cache::{Cache, CacheError, Stored, system_clock};
use crate::client::{Client, ClientError};
use crate::filter;
use crate::model::{
    CacheSource, Country, ListedCountries, ListedStations, RefreshOutcome, Station,
};
use crate::store;

/// What the shell forwards as Tauri events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A background refresh of this country's list ended. `Landed`: the stored list was
    /// replaced — re-request it. `Failed`: nothing changed; the expired list stays.
    StationsUpdated {
        country_code: String,
        outcome: RefreshOutcome,
    },
    /// The same for the countries list.
    CountriesUpdated { outcome: RefreshOutcome },
    /// A play was recorded (`record_played` succeeded): the recents list changed. Carries
    /// nothing — the page re-requests `list_recents` if it is showing them (M3b commit 4).
    RecentsUpdated,
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ServiceError {
    #[error("{0}")]
    Client(ClientError),
    #[error("cache: {0}")]
    Cache(String),
    #[error("invalid country code {0:?}")]
    InvalidCountry(String),
    #[error("the stations service is not running")]
    Closed,
    /// The cache could not be opened even after its file was moved aside, so the service never
    /// started; every call answers this and the app runs without a directory (`/code-review`
    /// finding 2, 2026-09-22).
    #[error("station directory unavailable: {0}")]
    Unavailable(String),
}

impl From<CacheError> for ServiceError {
    fn from(e: CacheError) -> Self {
        ServiceError::Cache(e.to_string())
    }
}

type Reply<T> = oneshot::Sender<Result<T, ServiceError>>;

enum Msg {
    ListCountries(Reply<ListedCountries>),
    ListStations(String, Reply<ListedStations>),
    Search(String, Reply<Vec<Station>>),
    ListFavourites(Reply<Vec<Station>>),
    AddFavourite(Box<Station>, Reply<()>),
    RemoveFavourite(String, Reply<bool>),
    ListRecents(Reply<Vec<Station>>),
    RecordPlayed(Box<Station>, Reply<()>),
    CountriesDone(Result<Vec<Country>, ClientError>),
    StationsDone(String, Result<Vec<Station>, ClientError>),
    SearchDone(
        String,
        Result<Vec<Station>, ClientError>,
        Reply<Vec<Station>>,
    ),
}

/// The handle the shell keeps; cloneable, `Send + Sync`. Every method is answered by the DB
/// thread; none blocks the caller's thread. A degraded handle (`unavailable`) has no thread
/// behind it and answers every call with `ServiceError::Unavailable`.
#[derive(Clone)]
pub struct StationsHandle {
    inner: Handle,
}

#[derive(Clone)]
enum Handle {
    Live(mpsc::Sender<Msg>),
    Unavailable(Arc<str>),
}

impl StationsHandle {
    /// A handle with no service behind it: every method answers `Unavailable(reason)`.
    pub fn unavailable(reason: impl Into<String>) -> StationsHandle {
        StationsHandle {
            inner: Handle::Unavailable(Arc::from(reason.into())),
        }
    }

    async fn ask<T>(&self, build: impl FnOnce(Reply<T>) -> Msg) -> Result<T, ServiceError> {
        let sender = match &self.inner {
            Handle::Live(tx) => tx,
            Handle::Unavailable(reason) => {
                return Err(ServiceError::Unavailable(reason.to_string()));
            }
        };
        let (tx, rx) = oneshot::channel();
        sender.send(build(tx)).map_err(|_| ServiceError::Closed)?;
        rx.await.unwrap_or(Err(ServiceError::Closed))
    }
    pub async fn list_countries(&self) -> Result<ListedCountries, ServiceError> {
        self.ask(Msg::ListCountries).await
    }
    pub async fn list_stations(&self, cc: &str) -> Result<ListedStations, ServiceError> {
        let cc = cc.trim().to_ascii_uppercase();
        if cc.len() != 2 || !cc.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(ServiceError::InvalidCountry(cc));
        }
        self.ask(|r| Msg::ListStations(cc, r)).await
    }
    pub async fn search_stations(&self, query: &str) -> Result<Vec<Station>, ServiceError> {
        self.ask(|r| Msg::Search(query.to_string(), r)).await
    }
    pub async fn list_favourites(&self) -> Result<Vec<Station>, ServiceError> {
        self.ask(Msg::ListFavourites).await
    }
    pub async fn add_favourite(&self, station: Station) -> Result<(), ServiceError> {
        self.ask(|r| Msg::AddFavourite(Box::new(station), r)).await
    }
    pub async fn remove_favourite(&self, uuid: &str) -> Result<bool, ServiceError> {
        self.ask(|r| Msg::RemoveFavourite(uuid.to_string(), r))
            .await
    }
    pub async fn list_recents(&self) -> Result<Vec<Station>, ServiceError> {
        self.ask(Msg::ListRecents).await
    }
    pub async fn record_played(&self, station: Station) -> Result<(), ServiceError> {
        self.ask(|r| Msg::RecordPlayed(Box::new(station), r)).await
    }
}

pub struct StationsService;

impl StationsService {
    /// Production: the database at `db_path` (created if missing), the live client, real time.
    /// Never fails: a file that cannot be opened or migrated is moved aside and recreated, and
    /// if that fails too the handle is degraded (`ServiceError::Unavailable` on every call), so
    /// the tray, the popover and audio still come up — until 2026-09-22 the error propagated
    /// to `setup` and the whole app refused to launch (`/code-review` finding 2).
    pub fn start(db_path: PathBuf, user_agent: &str, sink: EventSink) -> StationsHandle {
        match open_or_recover(&db_path) {
            Ok(cache) => Self::start_with(cache, Arc::new(Client::production(user_agent)), sink),
            Err(reason) => {
                log::error!("stations service not started: {reason}");
                StationsHandle::unavailable(reason)
            }
        }
    }

    /// The pieces injected — the tests' entry point.
    pub fn start_with(cache: Cache, client: Arc<Client>, sink: EventSink) -> StationsHandle {
        let (tx, rx) = mpsc::channel::<Msg>();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("ondar-stations-fetch")
            .build()
            .expect("stations fetch runtime");
        let handle = StationsHandle {
            inner: Handle::Live(tx.clone()),
        };
        thread::Builder::new()
            .name("ondar-stations-db".into())
            .spawn(move || {
                let mut svc = Service {
                    cache,
                    client,
                    sink,
                    tx,
                    runtime,
                    pending_stations: HashMap::new(),
                    pending_countries: None,
                };
                while let Ok(msg) = rx.recv() {
                    svc.handle(msg);
                }
                log::info!("stations service stopped");
            })
            .expect("spawn stations db thread");
        handle
    }
}

/// Open the cache; if that fails, move the file (with its `-wal`/`-shm`) aside as
/// `<name>.corrupt-<unix seconds>` and open once more on a fresh file. The error text carries
/// both attempts when the second fails too.
fn open_or_recover(db_path: &Path) -> Result<Cache, String> {
    let first = match Cache::open(db_path, system_clock()) {
        Ok(cache) => {
            log::info!("stations cache at {}", db_path.display());
            return Ok(cache);
        }
        Err(e) => e,
    };
    log::warn!(
        "stations cache at {} cannot be opened ({first}); moving it aside and recreating",
        db_path.display()
    );
    let aside = move_aside(db_path)
        .map_err(|e| format!("{first}; and moving the file aside failed: {e}"))?;
    match Cache::open(db_path, system_clock()) {
        Ok(cache) => {
            log::warn!(
                "stations cache recreated at {} (the old file is {})",
                db_path.display(),
                aside.display()
            );
            Ok(cache)
        }
        Err(second) => Err(format!("{first}; on a recreated file: {second}")),
    }
}

/// Rename `<db>`, `<db>-wal` and `<db>-shm` (those that exist) to `<db>.corrupt-<ts>` and the
/// same with the suffixes; returns the base name they were moved to.
fn move_aside(db_path: &Path) -> std::io::Result<PathBuf> {
    let aside = with_suffix(db_path, &format!(".corrupt-{}", (system_clock())()));
    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(db_path, suffix);
        if from.exists() {
            std::fs::rename(&from, with_suffix(&aside, suffix))?;
        }
    }
    Ok(aside)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

struct Service {
    cache: Cache,
    client: Arc<Client>,
    sink: EventSink,
    tx: mpsc::Sender<Msg>,
    runtime: tokio::runtime::Runtime,
    /// A key present means a fetch is in flight; the vector holds the callers waiting for it
    /// (empty for a background refresh).
    pending_stations: HashMap<String, Vec<Reply<ListedStations>>>,
    pending_countries: Option<Vec<Reply<ListedCountries>>>,
}

fn listed_stations(
    cc: &str,
    s: Stored<Vec<Station>>,
    source: CacheSource,
    refreshing: bool,
) -> ListedStations {
    ListedStations {
        country_code: cc.to_string(),
        items: s.items,
        fetched_at: s.fetched_at,
        age_secs: s.age_secs,
        source,
        refreshing,
    }
}

fn listed_countries(
    s: Stored<Vec<Country>>,
    source: CacheSource,
    refreshing: bool,
) -> ListedCountries {
    ListedCountries {
        items: s.items,
        fetched_at: s.fetched_at,
        age_secs: s.age_secs,
        source,
        refreshing,
    }
}

impl Service {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::ListStations(cc, reply) => self.list_stations(cc, reply),
            Msg::StationsDone(cc, result) => self.stations_done(cc, result),
            Msg::ListCountries(reply) => self.list_countries(reply),
            Msg::CountriesDone(result) => self.countries_done(result),
            Msg::Search(query, reply) => {
                let client = self.client.clone();
                let tx = self.tx.clone();
                self.runtime.spawn(async move {
                    let result = client.search(&query).await;
                    let _ = tx.send(Msg::SearchDone(query, result, reply));
                });
            }
            Msg::SearchDone(query, result, reply) => {
                let out = match result {
                    Ok(rows) => Ok(rows),
                    Err(e) => {
                        log::warn!("search {query:?} failed ({e}); answering from the cache");
                        self.cache
                            .search_local(&query, crate::client::SEARCH_LIMIT as usize)
                            .map_err(ServiceError::from)
                    }
                };
                let _ = reply.send(out);
            }
            Msg::ListFavourites(reply) => {
                let _ = reply.send(store::list_favourites(&self.cache).map_err(ServiceError::from));
            }
            Msg::AddFavourite(station, reply) => {
                let _ = reply
                    .send(store::add_favourite(&self.cache, &station).map_err(ServiceError::from));
            }
            Msg::RemoveFavourite(uuid, reply) => {
                let _ = reply
                    .send(store::remove_favourite(&self.cache, &uuid).map_err(ServiceError::from));
            }
            Msg::ListRecents(reply) => {
                let _ = reply.send(store::list_recents(&self.cache).map_err(ServiceError::from));
            }
            Msg::RecordPlayed(station, reply) => {
                let result = store::record_played(&self.cache, &station);
                if result.is_ok() {
                    (self.sink)(Event::RecentsUpdated);
                }
                let _ = reply.send(result.map_err(ServiceError::from));
            }
        }
    }

    fn list_stations(&mut self, cc: String, reply: Reply<ListedStations>) {
        match self.cache.stations(&cc) {
            Err(e) => {
                let _ = reply.send(Err(e.into()));
            }
            Ok(Some(stored)) if !stored.expired => {
                let refreshing = self.pending_stations.contains_key(&cc);
                let _ = reply.send(Ok(listed_stations(
                    &cc,
                    stored,
                    CacheSource::Cached,
                    refreshing,
                )));
            }
            Ok(Some(stored)) => {
                // Expired: serve it now, refresh behind it (F5).
                let _ = reply.send(Ok(listed_stations(&cc, stored, CacheSource::Cached, true)));
                self.ensure_stations_fetch(&cc, None);
            }
            Ok(None) => self.ensure_stations_fetch(&cc, Some(reply)),
        }
    }

    /// Coalesce onto a fetch in flight, or start one. A `waiter` is only added when the caller
    /// had no list to be answered with.
    fn ensure_stations_fetch(&mut self, cc: &str, waiter: Option<Reply<ListedStations>>) {
        if let Some(waiters) = self.pending_stations.get_mut(cc) {
            waiters.extend(waiter);
            return;
        }
        self.pending_stations
            .insert(cc.to_string(), waiter.into_iter().collect());
        let expected = self.cache.station_count(cc).unwrap_or(None);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let cc = cc.to_string();
        log::info!("stations fetch cc={cc} expected={expected:?}");
        self.runtime.spawn(async move {
            let result = client.stations(&cc, expected).await;
            let _ = tx.send(Msg::StationsDone(cc, result));
        });
    }

    fn stations_done(&mut self, cc: String, result: Result<Vec<Station>, ClientError>) {
        let waiters = self.pending_stations.remove(&cc).unwrap_or_default();
        let stored = match result {
            Ok(rows) => {
                let source_rows = rows.len();
                let ranked = filter::rank(rows, filter::CAP);
                let fetched_at = self.cache.now();
                match self
                    .cache
                    .put_stations(&cc, &ranked, source_rows, fetched_at)
                {
                    Ok(()) => {
                        log::info!(
                            "stations fetched cc={cc} source=fresh rows={source_rows} kept={}",
                            ranked.len()
                        );
                        Ok(Stored::just_fetched(ranked, fetched_at))
                    }
                    Err(e) => {
                        log::warn!(
                            "stations fetched cc={cc} rows={source_rows} but the cache refused \
                             the list: {e} (nothing replaced)"
                        );
                        Err(ServiceError::from(e))
                    }
                }
            }
            Err(e) => {
                log::warn!(
                    "stations fetch failed cc={cc}: {e}{}",
                    if waiters.is_empty() {
                        " (expired list stays)"
                    } else {
                        " (no cache)"
                    }
                );
                Err(ServiceError::Client(e))
            }
        };
        let outcome = finish(
            waiters,
            stored.map(|s| listed_stations(&cc, s, CacheSource::Fresh, false)),
        );
        (self.sink)(Event::StationsUpdated {
            country_code: cc,
            outcome,
        });
    }

    fn list_countries(&mut self, reply: Reply<ListedCountries>) {
        match self.cache.countries() {
            Err(e) => {
                let _ = reply.send(Err(e.into()));
            }
            Ok(Some(stored)) if !stored.expired => {
                let refreshing = self.pending_countries.is_some();
                let _ = reply.send(Ok(listed_countries(
                    stored,
                    CacheSource::Cached,
                    refreshing,
                )));
            }
            Ok(Some(stored)) => {
                let _ = reply.send(Ok(listed_countries(stored, CacheSource::Cached, true)));
                self.ensure_countries_fetch(None);
            }
            Ok(None) => self.ensure_countries_fetch(Some(reply)),
        }
    }

    fn ensure_countries_fetch(&mut self, waiter: Option<Reply<ListedCountries>>) {
        if let Some(waiters) = self.pending_countries.as_mut() {
            waiters.extend(waiter);
            return;
        }
        self.pending_countries = Some(waiter.into_iter().collect());
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let result = client.countries().await;
            let _ = tx.send(Msg::CountriesDone(result));
        });
    }

    fn countries_done(&mut self, result: Result<Vec<Country>, ClientError>) {
        let waiters = self.pending_countries.take().unwrap_or_default();
        let stored = match result {
            Ok(items) => {
                let fetched_at = self.cache.now();
                match self.cache.put_countries(&items, fetched_at) {
                    Ok(()) => {
                        log::info!("countries fetched source=fresh rows={}", items.len());
                        Ok(Stored::just_fetched(items, fetched_at))
                    }
                    Err(e) => {
                        log::warn!(
                            "countries fetched rows={} but the cache refused the list: {e} \
                             (nothing replaced)",
                            items.len()
                        );
                        Err(ServiceError::from(e))
                    }
                }
            }
            Err(e) => {
                log::warn!("countries fetch failed: {e}");
                Err(ServiceError::Client(e))
            }
        };
        let outcome = finish(
            waiters,
            stored.map(|s| listed_countries(s, CacheSource::Fresh, false)),
        );
        (self.sink)(Event::CountriesUpdated { outcome });
    }
}

/// The one exit of a fetch: every waiter gets the same result, and the outcome the sink
/// announces is derived from it — `Landed` only when the list was stored. Until 2026-09-22 the
/// `Ok` arm announced `Landed` after the `match` on the put regardless of how the put went, so
/// a cache that refused the write (read-only, full disk) made the page's re-request start
/// another full-country fetch, forever (`/code-review` finding 1). The reply is built from
/// what was just written (`Stored::just_fetched`), so there is no re-read and no arm in which
/// a waiter could be dropped without an answer.
fn finish<T: Clone>(waiters: Vec<Reply<T>>, result: Result<T, ServiceError>) -> RefreshOutcome {
    let outcome = if result.is_ok() {
        RefreshOutcome::Landed
    } else {
        RefreshOutcome::Failed
    };
    for w in waiters {
        let _ = w.send(result.clone());
    }
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::cache::TTL_STATIONS;
    use crate::cache::tests::{fake_clock, st};
    use crate::client::fakes::*;
    use crate::model::Codec;

    const T0: i64 = 1_800_000_000;

    fn events() -> (EventSink, Arc<Mutex<Vec<Event>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        (Arc::new(move |e| l.lock().unwrap().push(e)), log)
    }

    fn service(
        transport: Arc<FakeTransport>,
        clock_start: i64,
    ) -> (
        StationsHandle,
        Arc<Mutex<i64>>,
        Arc<Mutex<Vec<Event>>>,
        Cache,
    ) {
        let (clock, now) = fake_clock(clock_start);
        let cache = Cache::in_memory(clock.clone()).unwrap();
        let (sink, log) = events();
        // A second cache on its own connection cannot see an in-memory database; the tests that
        // need a pre-filled list fill this one before handing it over.
        let client = Arc::new(client(transport, FakeHosts::new(&["h"]), FakeTiming::new()));
        let cache_for_assertions = Cache::in_memory(clock).unwrap();
        (
            StationsService::start_with(cache, client, sink),
            now,
            log,
            cache_for_assertions,
        )
    }

    fn service_with(
        cache: Cache,
        transport: Arc<FakeTransport>,
    ) -> (StationsHandle, Arc<Mutex<Vec<Event>>>) {
        let (sink, log) = events();
        let client = Arc::new(client(transport, FakeHosts::new(&["h"]), FakeTiming::new()));
        (StationsService::start_with(cache, client, sink), log)
    }

    async fn within<T>(ms: u64, f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_millis(ms), f)
            .await
            .expect("did not complete in time — the reply awaited the fetch")
    }

    /// Wait (≤ 2 s) for the first event, then a beat for a would-be second one.
    async fn settle_events(log: &Mutex<Vec<Event>>) {
        let mut waited = 0;
        while log.lock().unwrap().is_empty() && waited < 2000 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited += 10;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[test]
    fn a_blocked_fetch_does_not_delay_store_calls() {
        // F3: a held fetch for US must not delay list_favourites. Fails if the handler awaits
        // the fetch inline (the 100 ms guard elapses).
        let transport = FakeTransport::gated(vec![ok(&rows(3))]);
        let (h, _, _, _) = service(transport.clone(), T0);
        block_on(async {
            let h2 = h.clone();
            let us = tokio::spawn(async move { h2.list_stations("US").await });
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(transport.calls(), 1, "the fetch started");
            let favs = within(100, h.list_favourites()).await.unwrap();
            assert!(favs.is_empty());
            transport.release();
            let us = within(2000, us).await.unwrap().unwrap();
            assert_eq!(us.source, CacheSource::Fresh);
            assert_eq!(us.items.len(), 3);
            assert!(!us.refreshing);
        });
    }

    #[test]
    fn a_recorded_play_emits_recents_updated_once() {
        // M3b commit 4: the page showing recents learns of a play from this event, not by
        // polling. Fails if the RecordPlayed arm emits nothing, or emits on a failed write.
        let transport = FakeTransport::new(vec![]);
        let (h, _, log, _) = service(transport, T0);
        block_on(async {
            within(1000, h.record_played(st("u1", 1))).await.unwrap();
            let events = log.lock().unwrap().clone();
            assert_eq!(events, vec![Event::RecentsUpdated]);
            let recents = within(1000, h.list_recents()).await.unwrap();
            assert_eq!(recents.len(), 1);
        });
    }

    #[test]
    fn concurrent_requests_for_one_country_share_one_fetch() {
        // Fails if the pending map is missing (three transport calls).
        let transport = FakeTransport::gated(vec![ok(&rows(2))]);
        let (h, _, _, _) = service(transport.clone(), T0);
        block_on(async {
            let tasks: Vec<_> = (0..3)
                .map(|_| {
                    let h = h.clone();
                    tokio::spawn(async move { h.list_stations("us").await })
                })
                .collect();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(transport.calls(), 1, "one fetch for three callers");
            transport.release();
            for t in tasks {
                let r = within(2000, t).await.unwrap().unwrap();
                assert_eq!(r.source, CacheSource::Fresh);
                assert_eq!(r.country_code, "US");
            }
            assert_eq!(transport.calls(), 1);
        });
    }

    #[test]
    fn an_expired_list_is_served_before_the_refresh_completes() {
        // F5. Fails if the reply awaits the fetch.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("old", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + 25 * 3600;
        let transport = FakeTransport::gated(vec![ok(&rows(4))]);
        let (h, _) = service_with(cache, transport.clone());
        block_on(async {
            let r = within(100, h.list_stations("PT")).await.unwrap();
            assert_eq!(r.source, CacheSource::Cached);
            assert!(r.refreshing);
            assert_eq!(r.age_secs, 25 * 3600);
            assert_eq!(r.items[0].uuid, "old");
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                transport.calls(),
                1,
                "the refresh started in the background"
            );
            transport.release();
        });
    }

    #[test]
    fn stations_updated_fires_once_when_the_refresh_lands() {
        // F5. Fails if the event fires zero or two times, or the stored list is not the new one.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("old", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + TTL_STATIONS + 1;
        let transport = FakeTransport::gated(vec![ok(&rows(4))]);
        let (h, log) = service_with(cache, transport.clone());
        block_on(async {
            let first = within(100, h.list_stations("PT")).await.unwrap();
            assert!(first.refreshing);
            transport.release();
            let mut waited = 0;
            while log.lock().unwrap().is_empty() && waited < 2000 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                waited += 10;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "PT".into(),
                    outcome: RefreshOutcome::Landed,
                }]
            );
            let again = within(100, h.list_stations("PT")).await.unwrap();
            assert_eq!(again.source, CacheSource::Cached);
            assert!(!again.refreshing);
            assert_eq!(again.age_secs, 0, "fetched_at is the landing time");
            assert_eq!(again.items.len(), 4, "the new list");
            assert_eq!(transport.calls(), 1, "a fresh list is not refetched");
        });
    }

    #[test]
    fn a_missing_list_returns_the_error_when_every_attempt_fails_and_an_expired_one_is_kept() {
        let transport = FakeTransport::new(vec![Err("connect refused".into())]);
        let (h, _, log, _) = service(transport.clone(), T0);
        block_on(async {
            let err = within(2000, h.list_stations("US")).await.unwrap_err();
            assert!(
                matches!(
                    err,
                    ServiceError::Client(ClientError::Exhausted { attempts: 3, .. })
                ),
                "{err:?}"
            );
            // The sink runs on the DB thread just after the waiter's reply; give it a moment.
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "US".into(),
                    outcome: RefreshOutcome::Failed,
                }],
                "a fetch that failed with nothing cached still ends with its event"
            );
        });
        // With an expired list present: served, the refresh fails, the list is still there and
        // the failure was announced (checked before the second request, which starts another
        // refresh).
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("old", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + 9 * 24 * 3600;
        let transport = FakeTransport::new(vec![Err("connect refused".into())]);
        let (h, log) = service_with(cache, transport.clone());
        block_on(async {
            let r = within(100, h.list_stations("PT")).await.unwrap();
            assert!(r.refreshing && r.age_secs == 9 * 24 * 3600);
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "PT".into(),
                    outcome: RefreshOutcome::Failed,
                }]
            );
            let r = within(100, h.list_stations("PT")).await.unwrap();
            assert_eq!(r.items[0].uuid, "old", "no age ceiling");
        });
    }

    #[test]
    fn a_failed_refresh_ends_with_one_failed_event() {
        // Acceptance item 6 (2026-09-22): the page's `refreshing…` never cleared because the
        // failure arm emitted nothing. Fails if the failure arm emits nothing (the 2 s wait
        // elapses with an empty log), or emits twice, or reports `Landed`.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("old", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + TTL_STATIONS + 1;
        let transport = FakeTransport::new(vec![Err("connect refused".into())]);
        let (h, log) = service_with(cache, transport.clone());
        block_on(async {
            let first = within(100, h.list_stations("PT")).await.unwrap();
            assert!(first.refreshing);
            let mut waited = 0;
            while log.lock().unwrap().is_empty() && waited < 2000 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                waited += 10;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "PT".into(),
                    outcome: RefreshOutcome::Failed,
                }]
            );
            assert_eq!(transport.calls(), 3, "one fetch, three attempts");
        });
    }

    #[test]
    fn a_refused_cache_write_ends_as_failed_not_landed() {
        // `/code-review` finding 1 (2026-09-22): the fetch landed, `put_stations` failed, and
        // the event still read `Landed` — the page re-requested, the expired row was served, a
        // new fetch started: a full-country GET every few seconds for as long as the disk
        // stayed full. Fails on that code at the event assertion (`Landed`). The write is
        // refused with `PRAGMA query_only`, SQLite's own read-only switch, on the connection
        // the service will own.
        let (clock, now) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        cache.put_stations("PT", &[st("old", 1)], 1, T0).unwrap();
        *now.lock().unwrap() = T0 + TTL_STATIONS + 1;
        cache
            .conn()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        let transport = FakeTransport::new(vec![ok(&rows(4))]);
        let (h, log) = service_with(cache, transport.clone());
        block_on(async {
            let first = within(100, h.list_stations("PT")).await.unwrap();
            assert!(first.refreshing);
            settle_events(&log).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "PT".into(),
                    outcome: RefreshOutcome::Failed,
                }]
            );
            assert_eq!(
                transport.calls(),
                1,
                "one fetch; the service never re-fetches by itself"
            );
            let again = within(100, h.list_stations("PT")).await.unwrap();
            assert_eq!(again.items[0].uuid, "old", "nothing replaced");
        });
        // A caller with no list gets the cache's error — not a hang, not `Fresh`.
        let (clock, _) = fake_clock(T0);
        let cache = Cache::in_memory(clock).unwrap();
        cache
            .conn()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        let transport = FakeTransport::new(vec![ok(&rows(4))]);
        let (h, log) = service_with(cache, transport);
        block_on(async {
            let err = within(2000, h.list_stations("US")).await.unwrap_err();
            assert!(matches!(err, ServiceError::Cache(_)), "{err:?}");
            settle_events(&log).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::StationsUpdated {
                    country_code: "US".into(),
                    outcome: RefreshOutcome::Failed,
                }]
            );
        });
    }

    #[test]
    fn search_falls_back_to_the_cache_when_offline_and_invalid_codes_are_refused() {
        let (clock, _) = fake_clock(T0);
        let mut cache = Cache::in_memory(clock).unwrap();
        let mut a = st("a", 5);
        a.name = "Rádio Comercial".into();
        a.codec = Codec::Aac;
        cache.put_stations("PT", &[a], 1, T0).unwrap();
        let transport = FakeTransport::new(vec![Err("connect refused".into())]);
        let (h, _) = service_with(cache, transport);
        block_on(async {
            let hits = within(3000, h.search_stations("comercial")).await.unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].codec, Codec::Aac);
            assert_eq!(
                h.list_stations("P").await.unwrap_err(),
                ServiceError::InvalidCountry("P".into())
            );
            assert_eq!(
                h.list_stations("p1").await.unwrap_err(),
                ServiceError::InvalidCountry("P1".into())
            );
        });
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ondar-stations-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_corrupt_database_is_moved_aside_and_the_service_starts_on_a_fresh_one() {
        // `/code-review` finding 2 (2026-09-22): a truncated `ondar.sqlite` made `start`
        // return `Err`, `setup` propagated it, and `.build().expect(..)` ended the app on every
        // launch. Fails if `start` gives up on the first open error (`list_favourites` answers
        // `Unavailable`), leaves the garbage in place (no `.corrupt-` sibling), or does not
        // recreate a real database (header check).
        // No stale `-wal` beside it: the bundled SQLite deletes an invalid WAL itself during
        // the failed open (probed 2026-09-22), so that sibling rename is not observable here.
        let dir = temp_dir("corrupt");
        let db = dir.join("ondar.sqlite");
        std::fs::write(&db, [b"not a database\n".as_slice(); 300].concat()).unwrap();
        let (sink, _) = events();
        let h = StationsService::start(db.clone(), "Ondar/test", sink);
        block_on(async {
            let favs = within(2000, h.list_favourites()).await.unwrap();
            assert!(favs.is_empty(), "a fresh database");
        });
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n.starts_with("ondar.sqlite.corrupt-")),
            "{names:?}"
        );
        assert!(
            std::fs::read(&db)
                .unwrap()
                .starts_with(b"SQLite format 3\0"),
            "the new file is a real database"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unopenable_path_degrades_the_handle_instead_of_aborting() {
        // The second half of finding 2: when even a fresh file cannot be created, `start`
        // still returns — a handle that answers `Unavailable` at once. Fails if `start` panics
        // or a call hangs (the 500 ms guard).
        let db = temp_dir("missing").join("no-such-dir").join("ondar.sqlite");
        let (sink, _) = events();
        let h = StationsService::start(db, "Ondar/test", sink);
        block_on(async {
            let err = within(500, h.list_favourites()).await.unwrap_err();
            assert!(matches!(err, ServiceError::Unavailable(_)), "{err:?}");
            let err = within(500, h.list_stations("PT")).await.unwrap_err();
            assert!(matches!(err, ServiceError::Unavailable(_)), "{err:?}");
        });
    }

    #[test]
    fn an_empty_countries_answer_is_an_error_for_every_waiter_and_a_failed_refresh() {
        // Finding 3 (2026-09-22): `200 []` wrote no row, the re-read found nothing, the
        // `Ok(None)` arm dropped every waiter's oneshot (`ask` reported "the stations service is
        // not running") and then announced `Landed`, so the page re-requested and re-fetched on
        // every event. Fails on that code at the first assertion (`Closed`), and on a client
        // that accepts the empty list at the outcome (`Landed`).
        let transport = FakeTransport::new(vec![ok(b"[]")]);
        let (h, _, log, _) = service(transport.clone(), T0);
        block_on(async {
            let err = within(2000, h.list_countries()).await.unwrap_err();
            assert_eq!(err, ServiceError::Client(ClientError::EmptyCountries));
            settle_events(&log).await;
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::CountriesUpdated {
                    outcome: RefreshOutcome::Failed
                }]
            );
            assert_eq!(transport.calls(), 1);
        });
    }

    #[test]
    fn countries_follow_the_same_path() {
        const COUNTRIES: &[u8] = include_bytes!("../fixtures/countries.json");
        let transport = FakeTransport::new(vec![ok(COUNTRIES)]);
        let (h, _, log, _) = service(transport.clone(), T0);
        block_on(async {
            let c = within(2000, h.list_countries()).await.unwrap();
            assert_eq!(
                (c.source.clone(), c.items.len(), c.refreshing),
                (CacheSource::Fresh, 240, false)
            );
            let c = within(100, h.list_countries()).await.unwrap();
            assert_eq!(c.source, CacheSource::Cached);
            assert_eq!(transport.calls(), 1);
            assert_eq!(
                *log.lock().unwrap(),
                vec![Event::CountriesUpdated {
                    outcome: RefreshOutcome::Landed
                }]
            );
        });
    }
}
