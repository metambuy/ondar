//! Server discovery: the `_api._tcp.radio-browser.info` SRV record, ordered by priority and
//! weight, with the **measured** fallback list behind it.
//!
//! The M3 Step 0 census (2026-09-21) found one target, `de1`, with `all.api` the same
//! address and the older mirror names gone; so the fallbacks are those two names and nothing
//! else. The SRV TTL observed was ~5 min: resolve at service start and again between retry
//! attempts 1 and 2, never persist.

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;

pub const SRV_NAME: &str = "_api._tcp.radio-browser.info.";

/// The measured set, 2026-09-21. `all.api` is a round-robin name the docs mention; it resolved to
/// `de1`'s address in the census.
pub const FALLBACK_HOSTS: [&str; 2] = ["de1.api.radio-browser.info", "all.api.radio-browser.info"];

/// The port `https://` implies, written out by [`order`] only when a record names another.
const HTTPS_PORT: u16 = 443;

/// One SRV answer, the fields the ordering needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// Hosts in the order to try: lower priority first, then higher weight, then name (stable).
/// Targets are returned without their trailing dot, and with their port when it is not 443
/// (`host:port`, the authority `client.rs` puts after `https://`; 0.1.1). Empty in → the
/// fallback list.
pub fn order(mut records: Vec<SrvRecord>) -> Vec<String> {
    if records.is_empty() {
        return FALLBACK_HOSTS.iter().map(ToString::to_string).collect();
    }
    records.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.target.cmp(&b.target))
    });
    let mut hosts: Vec<String> = records
        .into_iter()
        .map(|r| {
            let host = r.target.trim_end_matches('.');
            if r.port == HTTPS_PORT {
                host.to_string()
            } else {
                format!("{host}:{}", r.port)
            }
        })
        .collect();
    hosts.dedup();
    hosts
}

/// Live lookup through the system resolver. Any failure degrades to the fallbacks, logged.
pub async fn discover() -> Vec<String> {
    let lookup = async {
        let resolver = TokioResolver::builder_tokio()?.build()?;
        resolver.srv_lookup(SRV_NAME).await
    };
    match lookup.await {
        Ok(lookup) => {
            let records: Vec<SrvRecord> = lookup
                .answers()
                .iter()
                .filter_map(|rec| match &rec.data {
                    RData::SRV(srv) => Some(SrvRecord {
                        priority: srv.priority,
                        weight: srv.weight,
                        port: srv.port,
                        target: srv.target.to_utf8(),
                    }),
                    _ => None,
                })
                .collect();
            log::info!("srv {SRV_NAME}: {} record(s)", records.len());
            order(records)
        }
        Err(e) => {
            log::warn!("srv {SRV_NAME} lookup failed ({e}); using the fallback hosts");
            order(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(priority: u16, weight: u16, target: &str) -> SrvRecord {
        SrvRecord {
            priority,
            weight,
            port: 443,
            target: target.into(),
        }
    }

    /// Priority / weight order: lower priority first, then higher weight, then name, the
    /// trailing dot stripped from the target.
    #[test]
    fn lower_priority_first_then_higher_weight() {
        let hosts = order(vec![
            rec(2, 50, "b."),
            rec(1, 10, "c."),
            rec(1, 90, "a."),
            rec(2, 50, "a2."),
        ]);
        assert_eq!(hosts, ["a", "c", "a2", "b"]);
    }

    /// 0.1.1 item 5: a record's port is kept: a target on a port other than 443 comes back as
    /// `host:port`, which `client.rs`'s `https://{host}…` URLs then carry; 443, the measured
    /// record's, stays the bare host, so today's URLs and logs do not change. **Fails if** the
    /// port is dropped again (the M3 latent defect: every record was assumed to be 443), or if
    /// 443 starts being written out.
    #[test]
    fn a_record_on_another_port_keeps_its_port() {
        let hosts = order(vec![
            SrvRecord {
                port: 8443,
                ..rec(1, 10, "b.example.")
            },
            rec(1, 90, "a.example."),
        ]);
        assert_eq!(hosts, ["a.example", "b.example:8443"]);
    }

    #[test]
    fn no_records_means_the_measured_fallbacks() {
        // Fails if the fallback list grows a name that was never measured to answer.
        assert_eq!(
            order(Vec::new()),
            ["de1.api.radio-browser.info", "all.api.radio-browser.info"]
        );
    }
}
