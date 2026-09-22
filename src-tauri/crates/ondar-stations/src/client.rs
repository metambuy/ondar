//! The radio-browser client: bounded requests, same-host retries under a wall-clock budget,
//! an explicit limit on every list request and a truncation guard on what comes back.
//!
//! Every rule here is a measured one (M3 Step 0, 2026-09-21; plan F2, F4, F5, F6, G3):
//! one server (same-host retries), a silent default of 1000 rows (explicit limit + guard),
//! `hidebroken=true` is the broken filter, no compression, DNS inside `connect_timeout`.
//! Network I/O goes through [`Transport`] and time through [`Timing`], so the tests inject
//! both.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::filter;
use crate::model::{Country, Station};
use crate::normalise::{self, ParseError};

/// The limit sent with every station-list request. Measured to return the whole country
/// (US 8 191 rows); a full page at this size is treated as truncation (rule 1).
pub const STATIONS_LIMIT: u32 = 100_000;
/// What the server returns when no `limit` is sent — silently, as a 200 (measured on six of
/// eight countries). Rule 2 of [`check_complete`].
pub const SERVER_DEFAULT_LIMIT: usize = 1000;
/// Rule 2 fires only when the country is big enough that 1000 rows cannot be its whole
/// `hidebroken` list: 1000 / (1 − 0.146), the largest broken share measured (BR). Below this
/// `station_count`, 1000 rows is plausible and a real 1000-station country must not be refused
/// forever (F6).
pub const RULE2_MIN_EXPECTED: u32 = 1172;
/// Rule 3: fewer than this share of the country's published `station_count` is truncation.
/// The only legitimate gap is the broken share, 4.6–14.6 % measured, so 0.5 sits ≥ 35 points
/// below it and catches any 1000-row truncation of a country above 2 000 stations.
pub const TRUNCATION_FACTOR: f64 = 0.5;
/// Search page size (server-side search is for `search_stations` only, G1).
pub const SEARCH_LIMIT: u32 = 50;
/// Per-request totals sized from the byte count (F4): countries and search are ≤ 240 KB;
/// a station list is up to 9.5 MB (US), which a 0.5 Mbit/s line delivers in 152 s.
pub const TOTAL_SMALL: Duration = Duration::from_secs(30);
pub const TOTAL_LIST: Duration = Duration::from_secs(180);
/// No bytes for this long → the request fails (F4: bound the stall, not just the total).
/// The slowest first byte measured was 971 ms.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Attempts per fetch, same host (there is one), with `BACKOFF` between them.
pub const ATTEMPTS: u32 = 3;
pub const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
/// Wall-clock budget for the whole retry sequence (F5): attempts × `TOTAL_LIST` may not add
/// up to ~9 minutes while a usable cached list waits.
pub const RETRY_BUDGET: Duration = Duration::from_secs(200);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A response as the client sees it: status and body, redirects already followed.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// HTTP GET, bounded by `total`. Implemented by reqwest and by the tests' fake.
pub trait Transport: Send + Sync {
    fn get(&self, url: String, total: Duration) -> BoxFuture<'_, Result<Response, String>>;
}

/// The hosts to try, re-resolved between attempts 1 and 2. Implemented by [`crate::srv`] and by
/// the tests' fake (which records how often it was asked).
pub trait HostSource: Send + Sync {
    fn hosts(&self) -> BoxFuture<'_, Vec<String>>;
}

/// Monotonic time and sleeping, so the retry budget is testable without waiting.
pub trait Timing: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()>;
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The server answered with a status that is not retried (a 4xx other than 429).
    #[error("radio-browser answered {status}")]
    Http { status: u16 },
    /// Every attempt failed, or the budget ran out first.
    #[error("radio-browser unreachable after {attempts} attempt(s) in {elapsed:?}: {last}")]
    Exhausted {
        last: String,
        attempts: u32,
        elapsed: Duration,
    },
    /// The list came back shorter than the country: rule 1, 2 or 3 of [`check_complete`].
    #[error("station list truncated: got {got} rows (expected {expected:?}) — {reason}")]
    Truncated {
        got: usize,
        expected: Option<u32>,
        reason: &'static str,
    },
    #[error("{0}")]
    Parse(String),
    /// `/json/countries` answered `200 []` (or nothing that survives normalisation). The
    /// directory has ~240 countries; an empty answer is a broken server, not data — stored as a
    /// list it would be re-fetched on every `landed` event (`/code-review` finding 3).
    #[error("the countries list came back empty")]
    EmptyCountries,
}

impl From<ParseError> for ClientError {
    fn from(e: ParseError) -> Self {
        ClientError::Parse(e.to_string())
    }
}

/// The three truncation rules, on the quantity that moved in the census: the server ignoring
/// our limit and answering with its default.
pub fn check_complete(
    rows: usize,
    requested_limit: u32,
    expected: Option<u32>,
) -> Result<(), ClientError> {
    let truncated = |reason| {
        Err(ClientError::Truncated {
            got: rows,
            expected,
            reason,
        })
    };
    if rows >= requested_limit as usize {
        return truncated("full page");
    }
    if requested_limit as usize > SERVER_DEFAULT_LIMIT
        && rows == SERVER_DEFAULT_LIMIT
        && expected.is_none_or(|e| e >= RULE2_MIN_EXPECTED)
    {
        return truncated("server default 1000");
    }
    if let Some(e) = expected
        && (rows as f64) < (e as f64) * TRUNCATION_FACTOR
    {
        return truncated("far below station_count");
    }
    Ok(())
}

/// Percent-encode a query value (RFC 3986 unreserved kept; UTF-8 bytes otherwise).
pub fn qenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn countries_path() -> String {
    "/json/countries?hidebroken=true".to_string()
}

/// `bycountrycodeexact` — the endpoint measured returning a whole > 1000 country (F1). Always
/// carries an explicit `limit`; dropping it would reinstate the silent 1000.
pub fn stations_path(cc: &str) -> String {
    format!(
        "/json/stations/bycountrycodeexact/{}?hidebroken=true&limit={STATIONS_LIMIT}",
        qenc(&cc.to_ascii_uppercase())
    )
}

pub fn search_path(query: &str) -> String {
    format!(
        "/json/stations/search?name={}&hidebroken=true&order=votes&reverse=true&limit={SEARCH_LIMIT}",
        qenc(query.trim())
    )
}

pub struct Client {
    transport: Arc<dyn Transport>,
    hosts: Arc<dyn HostSource>,
    timing: Arc<dyn Timing>,
    current: Mutex<Vec<String>>,
    budget: Duration,
}

impl Client {
    pub fn new(
        transport: Arc<dyn Transport>,
        hosts: Arc<dyn HostSource>,
        timing: Arc<dyn Timing>,
    ) -> Client {
        Client {
            transport,
            hosts,
            timing,
            current: Mutex::new(Vec::new()),
            budget: RETRY_BUDGET,
        }
    }

    /// The retry budget as a parameter, for the test that drives it with a fake clock.
    pub fn with_budget(mut self, budget: Duration) -> Client {
        self.budget = budget;
        self
    }

    /// The production client: reqwest with the measured timeouts, the live SRV lookup, real time.
    pub fn production(user_agent: &str) -> Client {
        Client::new(
            Arc::new(ReqwestTransport::new(user_agent)),
            Arc::new(LiveHosts),
            Arc::new(RealTiming),
        )
    }

    /// The countries list, normalised. An empty answer is refused (`EmptyCountries`).
    pub async fn countries(&self) -> Result<Vec<Country>, ClientError> {
        let body = self
            .fetch_with_retries(&countries_path(), TOTAL_SMALL)
            .await?;
        let items = normalise::countries(&body)?;
        if items.is_empty() {
            return Err(ClientError::EmptyCountries);
        }
        Ok(items)
    }

    /// One country's whole `hidebroken` list, normalised, **not** ranked (the service ranks and
    /// caps, G1). `expected` is the country's published `station_count` for the guard.
    pub async fn stations(
        &self,
        cc: &str,
        expected: Option<u32>,
    ) -> Result<Vec<Station>, ClientError> {
        let body = self
            .fetch_with_retries(&stations_path(cc), TOTAL_LIST)
            .await?;
        let rows = normalise::stations(&body)?;
        check_complete(rows.len(), STATIONS_LIMIT, expected)?;
        Ok(rows)
    }

    /// Server-side search, then the same drop/dedupe/sort rules as a list, without the cap (S1).
    pub async fn search(&self, query: &str) -> Result<Vec<Station>, ClientError> {
        let body = self
            .fetch_with_retries(&search_path(query), TOTAL_SMALL)
            .await?;
        Ok(filter::rank(normalise::stations(&body)?, usize::MAX))
    }

    /// The host to use; resolved on first use and again when `refresh` is set (before attempt
    /// 2). The guard never lives across the await, so the future stays `Send`.
    async fn host(&self, refresh: bool) -> String {
        let known = {
            let current = self.current.lock().unwrap();
            if refresh || current.is_empty() {
                None
            } else {
                Some(current[0].clone())
            }
        };
        if let Some(h) = known {
            return h;
        }
        let hosts = self.hosts.hosts().await;
        let hosts = if hosts.is_empty() {
            crate::srv::order(Vec::new())
        } else {
            hosts
        };
        let first = hosts[0].clone();
        *self.current.lock().unwrap() = hosts;
        first
    }

    /// G3 + F5: `ATTEMPTS` on the same host with `BACKOFF` between them, the host list
    /// re-resolved before attempt 2, the whole sequence stopped when the next attempt could not
    /// finish inside `budget`. A 4xx other than 429 is returned at once, not retried.
    pub async fn fetch_with_retries(
        &self,
        path_query: &str,
        total: Duration,
    ) -> Result<Vec<u8>, ClientError> {
        let start = self.timing.now();
        let mut last = String::new();
        let mut attempts = 0;
        for attempt in 1..=ATTEMPTS {
            if attempt > 1 {
                let backoff = BACKOFF[(attempt - 2) as usize];
                let spent = self.timing.now().saturating_duration_since(start);
                if spent + backoff + total > self.budget {
                    log::warn!(
                        "stations fetch {path_query}: budget {:?} would be exceeded by attempt {attempt}; giving up after {spent:?}",
                        self.budget
                    );
                    break;
                }
                self.timing.sleep(backoff).await;
            }
            let host = self.host(attempt == 2).await;
            let url = format!("https://{host}{path_query}");
            attempts = attempt;
            match self.transport.get(url, total).await {
                Ok(r) if (200..300).contains(&r.status) => return Ok(r.body),
                Ok(r) if r.status == 429 || r.status >= 500 => {
                    last = format!("HTTP {}", r.status);
                    log::warn!("stations fetch {path_query} attempt {attempt}: {last}");
                }
                Ok(r) => return Err(ClientError::Http { status: r.status }),
                Err(e) => {
                    last = e;
                    log::warn!("stations fetch {path_query} attempt {attempt}: {last}");
                }
            }
        }
        Err(ClientError::Exhausted {
            last,
            attempts,
            elapsed: self.timing.now().saturating_duration_since(start),
        })
    }
}

/// reqwest with the measured bounds: connect 10 s, 15 s without a byte, `total` per request
/// (`RequestBuilder::timeout`, not on the client — a list needs 180 s, a search 30 s), no gzip.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new(user_agent: &str) -> ReqwestTransport {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("reqwest client with static configuration");
        ReqwestTransport { client }
    }
}

impl Transport for ReqwestTransport {
    fn get(&self, url: String, total: Duration) -> BoxFuture<'_, Result<Response, String>> {
        Box::pin(async move {
            let resp = self
                .client
                .get(&url)
                .timeout(total)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok(Response { status, body })
        })
    }
}

pub struct LiveHosts;

impl HostSource for LiveHosts {
    fn hosts(&self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(crate::srv::discover())
    }
}

pub struct RealTiming;

impl Timing for RealTiming {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(d))
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    //! Test doubles shared with the cache/service tests: a scripted transport that records
    //! every URL, a host source that counts resolutions, a clock the transport advances.

    use super::*;

    pub struct FakeTiming {
        pub now: Mutex<Instant>,
        pub slept: Mutex<Vec<Duration>>,
    }

    impl FakeTiming {
        pub fn new() -> Arc<FakeTiming> {
            Arc::new(FakeTiming {
                now: Mutex::new(Instant::now()),
                slept: Mutex::new(Vec::new()),
            })
        }
        pub fn advance(&self, d: Duration) {
            let mut now = self.now.lock().unwrap();
            *now += d;
        }
    }

    impl Timing for FakeTiming {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
        fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
            self.slept.lock().unwrap().push(d);
            self.advance(d);
            Box::pin(async {})
        }
    }

    pub struct FakeHosts {
        pub calls: Mutex<u32>,
        pub list: Vec<String>,
    }

    impl FakeHosts {
        pub fn new(list: &[&str]) -> Arc<FakeHosts> {
            Arc::new(FakeHosts {
                calls: Mutex::new(0),
                list: list.iter().map(|s| s.to_string()).collect(),
            })
        }
    }

    impl HostSource for FakeHosts {
        fn hosts(&self) -> BoxFuture<'_, Vec<String>> {
            *self.calls.lock().unwrap() += 1;
            let list = self.list.clone();
            Box::pin(async move { list })
        }
    }

    /// Answers in order from `script`; the last entry repeats. Each call may advance the fake
    /// clock by `takes` to simulate a slow attempt.
    pub struct FakeTransport {
        pub script: Mutex<Vec<Result<Response, String>>>,
        pub urls: Mutex<Vec<String>>,
        pub totals: Mutex<Vec<Duration>>,
        pub timing: Option<Arc<FakeTiming>>,
        pub takes: Duration,
        /// When set, every call waits for one permit before answering — the service tests hold
        /// a fetch open with it (`release()` lets exactly one call through).
        pub gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl FakeTransport {
        pub fn new(script: Vec<Result<Response, String>>) -> Arc<FakeTransport> {
            Arc::new(FakeTransport {
                script: Mutex::new(script),
                urls: Mutex::new(Vec::new()),
                totals: Mutex::new(Vec::new()),
                timing: None,
                takes: Duration::ZERO,
                gate: None,
            })
        }
        pub fn slow(
            script: Vec<Result<Response, String>>,
            timing: Arc<FakeTiming>,
            takes: Duration,
        ) -> Arc<FakeTransport> {
            Arc::new(FakeTransport {
                script: Mutex::new(script),
                urls: Mutex::new(Vec::new()),
                totals: Mutex::new(Vec::new()),
                timing: Some(timing),
                takes,
                gate: None,
            })
        }
        pub fn gated(script: Vec<Result<Response, String>>) -> Arc<FakeTransport> {
            Arc::new(FakeTransport {
                script: Mutex::new(script),
                urls: Mutex::new(Vec::new()),
                totals: Mutex::new(Vec::new()),
                timing: None,
                takes: Duration::ZERO,
                gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
            })
        }
        pub fn release(&self) {
            self.gate.as_ref().expect("gated transport").add_permits(1);
        }
        pub fn calls(&self) -> usize {
            self.urls.lock().unwrap().len()
        }
    }

    impl Transport for FakeTransport {
        fn get(&self, url: String, total: Duration) -> BoxFuture<'_, Result<Response, String>> {
            self.urls.lock().unwrap().push(url);
            self.totals.lock().unwrap().push(total);
            if let Some(t) = &self.timing {
                t.advance(self.takes);
            }
            let answer = {
                let mut script = self.script.lock().unwrap();
                if script.len() > 1 {
                    script.remove(0)
                } else {
                    script[0].clone()
                }
            };
            let gate = self.gate.clone();
            Box::pin(async move {
                if let Some(g) = gate {
                    g.acquire().await.expect("gate open").forget();
                }
                answer
            })
        }
    }

    pub fn ok(body: &[u8]) -> Result<Response, String> {
        Ok(Response {
            status: 200,
            body: body.to_vec(),
        })
    }

    pub fn status(code: u16) -> Result<Response, String> {
        Ok(Response {
            status: code,
            body: Vec::new(),
        })
    }

    /// A JSON array of `n` minimal station rows, all checked-OK and distinct.
    pub fn rows(n: usize) -> Vec<u8> {
        let mut s = String::from("[");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                r#"{{"stationuuid":"u{i}","name":"Station {i}","url_resolved":"http://s{i}/","countrycode":"PT","codec":"MP3","bitrate":128,"hls":0,"lastcheckok":1,"votes":{},"clickcount":0,"clicktrend":0}}"#,
                n - i
            ));
        }
        s.push(']');
        s.into_bytes()
    }

    pub fn client(
        transport: Arc<dyn Transport>,
        hosts: Arc<FakeHosts>,
        timing: Arc<FakeTiming>,
    ) -> Client {
        Client::new(transport, hosts, timing)
    }

    pub fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().expect("runtime").block_on(f)
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;

    const COUNTRIES: &[u8] = include_bytes!("../fixtures/countries.json");

    #[test]
    fn stations_request_always_carries_an_explicit_limit() {
        // Dropping `limit=` would reinstate the silent 1000 the census measured.
        let path = stations_path("pt");
        assert!(path.contains(&format!("limit={STATIONS_LIMIT}")), "{path}");
        assert!(path.contains("hidebroken=true"));
        assert!(
            path.starts_with("/json/stations/bycountrycodeexact/PT?"),
            "{path}"
        );
    }

    #[test]
    fn a_full_page_is_reported_as_truncated() {
        // Rule 1. Fails if the guard is `>` or absent.
        assert!(matches!(
            check_complete(1000, 1000, None),
            Err(ClientError::Truncated {
                reason: "full page",
                ..
            })
        ));
        assert!(check_complete(999, 1000, None).is_ok());
    }

    #[test]
    fn the_server_default_of_1000_is_reported_as_truncated() {
        // Rule 2, the failure Step 0 measured: the server ignored limit=100000 and sent 1000.
        // Fails against rule 1 alone (1000 < 100 000).
        assert!(matches!(
            check_complete(1000, STATIONS_LIMIT, Some(8190)),
            Err(ClientError::Truncated {
                reason: "server default 1000",
                ..
            })
        ));
        assert!(matches!(
            check_complete(1000, STATIONS_LIMIT, None),
            Err(ClientError::Truncated {
                reason: "server default 1000",
                ..
            })
        ));
    }

    #[test]
    fn a_real_1000_station_country_is_not_truncated() {
        // F6: below RULE2_MIN_EXPECTED, 1000 rows is a plausible whole list. Boundary pair.
        assert!(check_complete(1000, STATIONS_LIMIT, Some(1050)).is_ok());
        assert!(check_complete(1000, STATIONS_LIMIT, Some(1171)).is_ok());
        assert!(matches!(
            check_complete(1000, STATIONS_LIMIT, Some(1172)),
            Err(ClientError::Truncated {
                reason: "server default 1000",
                ..
            })
        ));
    }

    #[test]
    fn far_below_station_count_is_reported_as_truncated() {
        // Rule 3: 1 200 of 8 190 is truncation; 7 235 (US after hidebroken) is not. Fails if the
        // factor is at or above 0.89 or the rule is missing.
        assert!(matches!(
            check_complete(1200, STATIONS_LIMIT, Some(8190)),
            Err(ClientError::Truncated {
                reason: "far below station_count",
                ..
            })
        ));
        assert!(check_complete(7235, STATIONS_LIMIT, Some(8190)).is_ok());
    }

    #[test]
    fn three_attempts_same_host_then_exhausted() {
        let transport = FakeTransport::new(vec![Err("connect refused".into())]);
        let hosts = FakeHosts::new(&["de1.api.radio-browser.info"]);
        let timing = FakeTiming::new();
        let c = client(transport.clone(), hosts.clone(), timing.clone());
        let err = block_on(c.fetch_with_retries("/json/countries", TOTAL_SMALL)).unwrap_err();
        assert!(
            matches!(err, ClientError::Exhausted { attempts: 3, .. }),
            "{err:?}"
        );
        let urls = transport.urls.lock().unwrap().clone();
        assert_eq!(urls.len(), 3, "a 4th attempt was made");
        assert!(
            urls.iter()
                .all(|u| u.starts_with("https://de1.api.radio-browser.info/")),
            "a second host was used: {urls:?}"
        );
        assert_eq!(
            *hosts.calls.lock().unwrap(),
            2,
            "resolved at first use and once more before attempt 2"
        );
        assert_eq!(*timing.slept.lock().unwrap(), BACKOFF.to_vec());
    }

    #[test]
    fn the_retry_sequence_stops_at_its_wall_clock_budget() {
        // F5: every attempt takes 400 ms of fake time; budget 1 s with total 100 ms → the third
        // attempt (spent 800 ms + backoff 2 s + 100 ms) cannot fit: 2 attempts, not 3. Fails if
        // the budget is not consulted or is applied per attempt.
        let timing = FakeTiming::new();
        let transport = FakeTransport::slow(
            vec![Err("timeout".into())],
            timing.clone(),
            Duration::from_millis(400),
        );
        let hosts = FakeHosts::new(&["de1.api.radio-browser.info"]);
        let c =
            client(transport.clone(), hosts, timing.clone()).with_budget(Duration::from_secs(4));
        let err = block_on(c.fetch_with_retries("/json/countries", Duration::from_millis(100)))
            .unwrap_err();
        // spent after attempt 1: 0.4 s; +1 s backoff +0.1 s = 1.5 s ≤ 4 s → attempt 2 runs (spent 1.8 s);
        // +2 s backoff +0.1 s = 3.9 s ≤ 4 s → attempt 3 runs. So budget 4 s allows all three…
        assert!(
            matches!(err, ClientError::Exhausted { attempts: 3, .. }),
            "{err:?}"
        );
        // …and budget 2 s stops after two.
        let timing = FakeTiming::new();
        let transport = FakeTransport::slow(
            vec![Err("timeout".into())],
            timing.clone(),
            Duration::from_millis(400),
        );
        let hosts = FakeHosts::new(&["de1.api.radio-browser.info"]);
        let c =
            client(transport.clone(), hosts, timing.clone()).with_budget(Duration::from_secs(2));
        let err = block_on(c.fetch_with_retries("/json/countries", Duration::from_millis(100)))
            .unwrap_err();
        assert!(
            matches!(err, ClientError::Exhausted { attempts: 2, .. }),
            "{err:?}"
        );
        assert_eq!(transport.calls(), 2);
    }

    #[test]
    fn four_xx_does_not_retry_but_five_xx_and_429_do() {
        let transport = FakeTransport::new(vec![status(404)]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        assert_eq!(
            block_on(c.fetch_with_retries("/x", TOTAL_SMALL)).unwrap_err(),
            ClientError::Http { status: 404 }
        );
        assert_eq!(transport.calls(), 1);

        let transport = FakeTransport::new(vec![status(503), status(429), ok(b"[]")]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        assert_eq!(
            block_on(c.fetch_with_retries("/x", TOTAL_SMALL)).unwrap(),
            b"[]"
        );
        assert_eq!(transport.calls(), 3);
    }

    #[test]
    fn list_requests_carry_the_long_total_and_small_ones_the_short() {
        let transport = FakeTransport::new(vec![ok(&rows(3))]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        // `expected` None: three rows against a real station_count would trip rule 3, correctly.
        block_on(c.stations("PT", None)).unwrap();
        let transport2 = FakeTransport::new(vec![ok(COUNTRIES)]);
        let c2 = client(
            transport2.clone(),
            FakeHosts::new(&["h"]),
            FakeTiming::new(),
        );
        block_on(c2.countries()).unwrap();
        assert_eq!(*transport.totals.lock().unwrap(), vec![TOTAL_LIST]);
        assert_eq!(*transport2.totals.lock().unwrap(), vec![TOTAL_SMALL]);
    }

    #[test]
    fn stations_applies_the_guard_to_the_parsed_rows() {
        // 1000 rows for a big country through the whole client path → Truncated.
        let transport = FakeTransport::new(vec![ok(&rows(1000))]);
        let c = client(transport, FakeHosts::new(&["h"]), FakeTiming::new());
        let err = block_on(c.stations("US", Some(8190))).unwrap_err();
        assert!(
            matches!(err, ClientError::Truncated { got: 1000, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_countries_answer_is_refused() {
        // Finding 3: `200 []` used to parse to an empty Vec that the service stored as nothing
        // and announced as `landed`. Fails if the client hands the empty list back as `Ok`.
        let transport = FakeTransport::new(vec![ok(b"[]")]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        let err = block_on(c.countries()).unwrap_err();
        assert_eq!(err, ClientError::EmptyCountries);
        assert_eq!(transport.calls(), 1, "not retried: the answer was a 200");
    }

    #[test]
    fn parses_the_countries_fixture_through_the_client() {
        let transport = FakeTransport::new(vec![ok(COUNTRIES)]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        let cs = block_on(c.countries()).unwrap();
        assert_eq!(cs.len(), 240);
        assert_eq!(
            transport.urls.lock().unwrap()[0],
            "https://h/json/countries?hidebroken=true"
        );
    }

    #[test]
    fn search_query_is_percent_encoded_and_results_are_ranked() {
        // S1: `Rádio &` must not split the parameter; results pass through rank without the cap.
        let mut body = String::from_utf8(rows(3)).unwrap();
        // add a broken row and a duplicate of "Station 0" with fewer votes
        body.truncate(body.len() - 1);
        body.push_str(r#",{"stationuuid":"b","name":"Broken","url_resolved":"http://b/","countrycode":"PT","codec":"MP3","bitrate":128,"hls":0,"lastcheckok":0,"votes":99,"clickcount":0,"clicktrend":0},{"stationuuid":"d","name":" station 0 ","url_resolved":"http://s0/","countrycode":"PT","codec":"MP3","bitrate":128,"hls":0,"lastcheckok":1,"votes":1,"clickcount":0,"clicktrend":0}]"#);
        let transport = FakeTransport::new(vec![ok(body.as_bytes())]);
        let c = client(transport.clone(), FakeHosts::new(&["h"]), FakeTiming::new());
        let out = block_on(c.search("Rádio &")).unwrap();
        let url = transport.urls.lock().unwrap()[0].clone();
        assert!(url.contains("name=R%C3%A1dio%20%26&"), "{url}");
        assert_eq!(
            out.len(),
            3,
            "broken row dropped, duplicate merged: {:?}",
            out.iter().map(|s| &s.uuid).collect::<Vec<_>>()
        );
        assert!(out.windows(2).all(|w| w[0].votes >= w[1].votes));
        assert_eq!(
            out[0].uuid, "u0",
            "the higher-votes row of the duplicate pair survives"
        );
    }
}
