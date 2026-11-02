//! Server discovery through macOS (0.1.2, G): `getaddrinfo` on `all.api.radio-browser.info`, then
//! a reverse lookup (`getnameinfo`) of each address, which is the method radio-browser documents
//! as primary ("Do a DNS-lookup of 'all.api.radio-browser.info' … reverse DNS-Lookup",
//! api.radio-browser.info; the SRV record is its alternative). Both calls go through the system
//! resolver, mDNSResponder, as every other app's lookups do, so no DNS server is queried by
//! Ondar itself (F3: the hickory resolver 0.1.1 used read the `SystemConfiguration` store and sent
//! its own queries, which a firewall shows; on a network that lists `fe80::1%en0` it could not
//! parse it and failed). The **measured** fallback list stands behind it.
//!
//! The M3 Step 0 census (2026-09-21) found one server, `de1`, with `all.api` the same address and
//! the older mirror names gone; so the fallbacks are those two names and nothing else. Resolve at
//! service start and again between retry attempts 1 and 2, never persist.

use std::io;
use std::net::IpAddr;
use std::time::Duration;

/// The round-robin name radio-browser documents for discovery.
pub const LOOKUP_NAME: &str = "all.api.radio-browser.info";

/// The measured set, 2026-09-21. `all.api` is a round-robin name the docs mention; it resolved to
/// `de1`'s address in the census.
pub const FALLBACK_HOSTS: [&str; 2] = ["de1.api.radio-browser.info", "all.api.radio-browser.info"];

/// How long the system lookup may take before the fallbacks are used (0.1.2 review, answer 1):
/// one attempt of hickory's old defaults. `getaddrinfo` cannot be cancelled: on a timeout its
/// `spawn_blocking` thread outlives the future until the system gives up, at most once per
/// discovery (at start, and between attempts 1 and 2); accepted.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// A reverse-looked-up name is used only if it is a radio-browser API server: an address that
/// reverses to a provider's name would put a host in the URL whose certificate is not
/// radio-browser's.
const API_SUFFIX: &str = ".api.radio-browser.info";

/// Hosts in the order to try: the names sorted and deduplicated (two addresses of one server, its
/// IPv6 and IPv4, reverse to one name), without a trailing dot, radio-browser's only. None left
/// → the fallback list.
pub fn order(names: Vec<String>) -> Vec<String> {
    let mut hosts: Vec<String> = names
        .into_iter()
        .map(|n| n.trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| n.ends_with(API_SUFFIX))
        .collect();
    hosts.sort();
    hosts.dedup();
    if hosts.is_empty() {
        return FALLBACK_HOSTS.iter().map(ToString::to_string).collect();
    }
    hosts
}

/// The live lookup. Any failure, timeout or empty answer degrades to the fallbacks, logged.
pub async fn discover() -> Vec<String> {
    discover_with(system_lookup, LOOKUP_TIMEOUT).await
}

/// [`discover`] with the blocking lookup and the timeout given, for the tests.
pub async fn discover_with<F>(lookup: F, timeout: Duration) -> Vec<String>
where
    F: FnOnce() -> io::Result<Vec<String>> + Send + 'static,
{
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(lookup)).await {
        Ok(Ok(Ok(names))) => {
            let hosts = order(names);
            log::info!("discovery {LOOKUP_NAME}: {}", hosts.join(", "));
            hosts
        }
        Ok(Ok(Err(e))) => {
            log::warn!("discovery {LOOKUP_NAME} failed ({e}); using the fallback hosts");
            order(Vec::new())
        }
        Ok(Err(e)) => {
            log::warn!("discovery {LOOKUP_NAME} did not finish ({e}); using the fallback hosts");
            order(Vec::new())
        }
        Err(_) => {
            log::warn!(
                "discovery {LOOKUP_NAME} took over {} s; using the fallback hosts",
                timeout.as_secs()
            );
            order(Vec::new())
        }
    }
}

/// `getaddrinfo`, then `getnameinfo` with `NI_NAMEREQD` (`dns-lookup` 4.0.2, `lookup.rs:30-63`)
/// for each address. A reverse lookup that fails drops that address only; an error only when the
/// forward lookup fails. Blocking: run it in `spawn_blocking`.
fn system_lookup() -> io::Result<Vec<String>> {
    let addrs: Vec<IpAddr> = dns_lookup::lookup_host(LOOKUP_NAME)?.collect();
    Ok(reverse_each(&addrs, |a| {
        dns_lookup::lookup_addr(a).map_err(io::Error::from)
    }))
}

/// Each address's name, skipping those whose reverse lookup fails.
fn reverse_each(addrs: &[IpAddr], reverse: impl Fn(&IpAddr) -> io::Result<String>) -> Vec<String> {
    addrs.iter().filter_map(|a| reverse(a).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn no_names_means_the_measured_fallbacks() {
        // Fails if the fallback list grows a name that was never measured to answer.
        assert_eq!(
            order(Vec::new()),
            ["de1.api.radio-browser.info", "all.api.radio-browser.info"]
        );
    }

    /// The measured answer (Step 0 G, 2026-10-30): two addresses, IPv6 and IPv4, both reversing to
    /// `de1`. **Fails if** the duplicate is kept (one server tried twice) or a trailing dot or
    /// case survives into the URL host.
    #[test]
    fn two_addresses_of_one_server_give_one_host() {
        assert_eq!(
            order(names(&[
                "de1.api.radio-browser.info",
                "DE1.api.radio-browser.info."
            ])),
            ["de1.api.radio-browser.info"]
        );
    }

    /// **Fails if** a name outside `.api.radio-browser.info` (a provider's reverse name) is used
    /// as a host: its certificate would not be radio-browser's.
    #[test]
    fn only_radio_browser_api_names_are_kept() {
        assert_eq!(
            order(names(&[
                "static.4.78.98.91.clients.example.net",
                "fi1.api.radio-browser.info"
            ])),
            ["fi1.api.radio-browser.info"]
        );
        assert_eq!(order(names(&["example.net"])), order(Vec::new()));
    }

    /// One address whose reverse lookup fails is dropped, the others kept.
    #[test]
    fn a_failed_reverse_lookup_drops_only_that_address() {
        let a: IpAddr = "91.98.4.78".parse().unwrap();
        let b: IpAddr = "2a01:4f8:1c1d:699::1".parse().unwrap();
        let got = reverse_each(&[a, b], |x| {
            if *x == a {
                Err(io::Error::other("no PTR"))
            } else {
                Ok("de1.api.radio-browser.info".into())
            }
        });
        assert_eq!(got, ["de1.api.radio-browser.info"]);
    }

    /// A forward lookup that fails gives the fallbacks.
    #[test]
    fn a_failed_lookup_gives_the_fallbacks() {
        let hosts = rt().block_on(discover_with(
            || Err(io::Error::other("nodename nor servname provided")),
            Duration::from_secs(5),
        ));
        assert_eq!(hosts, order(Vec::new()));
    }

    /// 0.1.2 review answer 1: a lookup that does not return gives the fallbacks once the timeout
    /// has passed, not when the system gives up. **Fails if** the lookup is awaited without a
    /// bound (the hickory lookup it replaces had none of its own: up to ~10 s of its defaults, and
    /// a blocking `getaddrinfo` can take longer): the hang guard below would then fire first.
    #[test]
    fn a_lookup_that_never_returns_gives_the_fallbacks_within_the_timeout() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = rt();
            let hosts = rt.block_on(discover_with(
                || {
                    std::thread::sleep(Duration::from_secs(30));
                    Ok(names(&["de1.api.radio-browser.info"]))
                },
                Duration::from_millis(200),
            ));
            let _ = tx.send(hosts);
            // A plain drop would wait for the still-sleeping blocking thread; the service's
            // runtime lives for the process, so production never drops one.
            rt.shutdown_background();
        });
        let hosts = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("discovery did not return within its timeout");
        assert_eq!(hosts, order(Vec::new()));
    }
}
