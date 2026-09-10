//! Memcached monitor, over the text protocol.
//!
//! Connect, send `stats\r\n`, read `STAT <key> <value>` lines until `END`.
//! That is the whole protocol, and it needs no client library.
//!
//! The number that matters is not the hit rate. A cache can sit at 96% hits
//! and still be badly undersized, because the hot keys keep answering while
//! everything else is thrown out to make room — the hit rate stays high
//! precisely *because* the survivors are the popular ones. `evictions` is what
//! exposes that, and it is the difference between "this cache is working" and
//! "this cache is working for the top thousand keys and nothing else".
//!
//! Evictions are also easy to misread on their own. Memcached counts an
//! eviction whenever it reclaims a valid item to make space; it counts
//! `expired_unfetched` separately for items that simply timed out. Only the
//! first means pressure, so only the first is treated as a fault here.
//!
//! Both counters are cumulative since the daemon started, so the rate matters
//! rather than the total. A server up for a year with a million evictions may
//! be fine; a hundred in the last minute is not.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Enough for the stats block, which is a few kilobytes at most. A cap rather
/// than a read-to-end so a wrong port answering with a stream cannot hang the
/// poll or grow without bound.
const MAX_RESPONSE: usize = 64 * 1024;

pub struct MemcachedCollector {
    address: String,
    /// Percent of `limit_maxbytes` in use above which the cache is degraded.
    fill_warn: f64,
    /// Evictions per second above which the cache is degraded.
    eviction_warn: f64,
    profile_id: String,
    profile_name: String,
    previous: Mutex<Option<Sample>>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    evictions: f64,
    at: DateTime<Utc>,
}

/// The `STAT` lines, as numbers where they parse and strings where they do not.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Stats {
    pub fields: BTreeMap<String, String>,
}

impl Stats {
    pub fn number(&self, key: &str) -> Option<f64> {
        self.fields.get(key).and_then(|v| v.parse::<f64>().ok())
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// Hits as a percentage of lookups.
    ///
    /// `None` before anything has been looked up, rather than 0% — a cache
    /// nobody has asked for anything is not a cache that is missing
    /// everything, and reporting the second would raise an alarm about an
    /// idle server.
    pub fn hit_rate(&self) -> Option<f64> {
        let hits = self.number("get_hits")?;
        let misses = self.number("get_misses")?;
        let lookups = hits + misses;
        (lookups > 0.0).then(|| (hits / lookups) * 100.0)
    }

    /// Bytes stored as a percentage of the configured ceiling.
    pub fn fill(&self) -> Option<f64> {
        let bytes = self.number("bytes")?;
        let limit = self.number("limit_maxbytes")?;
        (limit > 0.0).then(|| (bytes / limit) * 100.0)
    }
}

impl MemcachedCollector {
    pub fn new(profile: &ConnectionProfile) -> Result<Self> {
        let raw = profile.endpoint.trim();
        if raw.is_empty() {
            return Err(AppError::Config(
                "Memcached connections need a host and port, e.g. cache.internal:11211".into(),
            ));
        }

        // Accept `memcached://host:port` and a bare `host:port` alike, and
        // supply the default port when it is missing — nobody runs memcached
        // anywhere else, and making it required would be pedantry.
        let stripped = raw
            .strip_prefix("memcached://")
            .or_else(|| raw.strip_prefix("tcp://"))
            .unwrap_or(raw)
            .trim_end_matches('/');
        let address = if stripped.contains(':') {
            stripped.to_string()
        } else {
            format!("{stripped}:11211")
        };

        Ok(MemcachedCollector {
            address,
            fill_warn: profile
                .options
                .get("fillWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(90.0),
            eviction_warn: profile
                .options
                .get("evictionWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(1.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
        })
    }

    async fn fetch_stats(&self) -> std::result::Result<Stats, String> {
        let connect = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&self.address));
        let mut stream = match connect.await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(format!("{}: {e}", self.address)),
            Err(_) => return Err(format!("{} did not answer within 10s", self.address)),
        };

        stream
            .write_all(b"stats\r\n")
            .await
            .map_err(|e| format!("could not send stats to {}: {e}", self.address))?;

        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = tokio::time::timeout(CONNECT_TIMEOUT, stream.read(&mut chunk))
                .await
                .map_err(|_| format!("{} stopped responding mid-stats", self.address))?
                .map_err(|e| format!("could not read stats from {}: {e}", self.address))?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);

            // The terminator, not the byte count, is what says the block is
            // complete — reading to EOF would wait for a connection memcached
            // keeps open.
            if buffer.ends_with(b"END\r\n") || buffer.len() >= MAX_RESPONSE {
                break;
            }
        }

        let text = String::from_utf8_lossy(&buffer);
        let stats = parse_stats(&text);
        if stats.fields.is_empty() {
            return Err(format!(
                "{} answered, but not with memcached stats — check the port",
                self.address
            ));
        }
        Ok(stats)
    }
}

pub(crate) fn parse_stats(body: &str) -> Stats {
    let mut fields = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line == "END" {
            break;
        }
        // `STAT <key> <value>`; a value may itself contain spaces (version
        // strings do), so only the first two tokens are split off.
        let Some(rest) = line.strip_prefix("STAT ") else {
            continue;
        };
        if let Some((key, value)) = rest.split_once(' ') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Stats { fields }
}

#[async_trait]
impl Collector for MemcachedCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Memcached
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let stats = match self.fetch_stats().await {
            Ok(stats) => stats,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Memcached,
                    message,
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let now = Utc::now();
        let evictions_total = stats.number("evictions").unwrap_or(0.0);
        let mut previous = self.previous.lock().await;
        let eviction_rate = previous.and_then(|last| {
            let seconds = (now - last.at).num_milliseconds() as f64 / 1000.0;
            // A counter that went backwards means the daemon restarted, which
            // is a fact worth not turning into a negative rate.
            if seconds <= 0.0 || evictions_total < last.evictions {
                return None;
            }
            Some((evictions_total - last.evictions) / seconds)
        });
        *previous = Some(Sample {
            evictions: evictions_total,
            at: now,
        });
        drop(previous);

        let fill = stats.fill();
        let hit_rate = stats.hit_rate();
        let evicting = eviction_rate.is_some_and(|rate| rate >= self.eviction_warn);
        let full = fill.is_some_and(|pct| pct >= self.fill_warn);

        let health = if evicting || full {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if evicting {
            format!(
                "evicting {:.1} items/sec — the cache is too small for what is being stored",
                eviction_rate.unwrap_or(0.0)
            )
        } else if full {
            format!("{:.0}% of the memory limit in use", fill.unwrap_or(0.0))
        } else {
            match hit_rate {
                Some(rate) => format!(
                    "{rate:.1}% hit rate · {:.0} items",
                    stats.number("curr_items").unwrap_or(0.0)
                ),
                None => "up, with nothing looked up yet".to_string(),
            }
        };

        let mut metrics = Vec::new();
        if let Some(rate) = hit_rate {
            metrics.push(Metric::new("hit_rate", "Hit rate", rate, MetricUnit::Percent));
        }
        if let Some(pct) = fill {
            metrics.push(
                Metric::new("fill", "Memory used", pct, MetricUnit::Percent)
                    .warn_above(self.fill_warn),
            );
        }
        if let Some(bytes) = stats.number("bytes") {
            metrics.push(Metric::new("bytes", "Bytes stored", bytes, MetricUnit::Bytes));
        }
        if let Some(items) = stats.number("curr_items") {
            metrics.push(Metric::new("items", "Items", items, MetricUnit::Count));
        }
        if let Some(connections) = stats.number("curr_connections") {
            metrics.push(Metric::new(
                "connections",
                "Connections",
                connections,
                MetricUnit::Count,
            ));
        }
        metrics.push(Metric::new(
            "evictions_total",
            "Evictions",
            evictions_total,
            MetricUnit::Count,
        ));
        if let Some(rate) = eviction_rate {
            metrics.push(
                Metric::new("eviction_rate", "Evictions/sec", rate, MetricUnit::PerSecond)
                    .warn_above(self.eviction_warn),
            );
        }
        if let Some(uptime) = stats.number("uptime") {
            metrics.push(Metric::new("uptime", "Uptime", uptime, MetricUnit::Seconds));
        }

        // Expiry and eviction are both items leaving, and they mean opposite
        // things: one is the cache working as designed, the other is it
        // running out of room. Shown side by side so they cannot be confused.
        let churn = Section::new(
            "churn",
            "Why items leave",
            vec![
                ColumnSpec::text("reason", "Reason").primary(),
                ColumnSpec::metric("count", "Count", MetricUnit::Count),
            ],
        )
        .rows(vec![
            Row::new(
                "evicted",
                "Evicted to make room",
                if evicting { Health::Degraded } else { Health::Healthy },
            )
            .summary("Valid items thrown out because the cache was full")
            .cell("reason", CellValue::text("Evicted to make room"))
            .cell("count", CellValue::number(evictions_total)),
            Row::new("expired", "Expired", Health::Healthy)
                .summary("Items that reached their TTL — the cache working as designed")
                .cell("reason", CellValue::text("Expired"))
                .cell(
                    "count",
                    CellValue::number(stats.number("expired_unfetched").unwrap_or(0.0)),
                ),
        ])
        .tracked()
        .priority(10);

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Memcached,
            health,
            summary,
            metrics,
            sections: vec![
                churn,
                Section::new(
                    "server",
                    "Server",
                    vec![
                        ColumnSpec::text("field", "Field").primary(),
                        ColumnSpec::text("value", "Value"),
                    ],
                )
                .rows(
                    [
                        ("version", "Version"),
                        ("pointer_size", "Pointer size"),
                        ("threads", "Threads"),
                        ("total_items", "Items stored, lifetime"),
                    ]
                    .into_iter()
                    .filter_map(|(key, label)| {
                        stats.text(key).map(|value| {
                            Row::new(key, label, Health::Healthy)
                                .cell("field", CellValue::text(label))
                                .cell("value", CellValue::text(value))
                        })
                    })
                    .collect(),
                )
                .priority(20),
            ],
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// Memcached keeps no log root can read. Its output goes to stderr on the
    /// host, which needs an agent this does not have.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "STAT pid 1\r\n\
                          STAT uptime 84213\r\n\
                          STAT version 1.6.21\r\n\
                          STAT curr_connections 34\r\n\
                          STAT curr_items 91234\r\n\
                          STAT bytes 858993459\r\n\
                          STAT limit_maxbytes 1073741824\r\n\
                          STAT get_hits 998000\r\n\
                          STAT get_misses 2000\r\n\
                          STAT evictions 41233\r\n\
                          STAT expired_unfetched 900\r\n\
                          END\r\n";

    #[test]
    fn parses_the_stat_block() {
        let stats = parse_stats(SAMPLE);
        assert_eq!(stats.number("curr_items"), Some(91_234.0));
        assert_eq!(stats.text("version"), Some("1.6.21"));
        assert_eq!(stats.number("evictions"), Some(41_233.0));
    }

    #[test]
    fn hit_rate_is_a_percentage_of_lookups() {
        let stats = parse_stats(SAMPLE);
        assert_eq!(stats.hit_rate(), Some(99.8));
    }

    /// An idle cache has not missed everything; it has been asked nothing.
    #[test]
    fn hit_rate_is_absent_before_any_lookup() {
        let stats = parse_stats("STAT get_hits 0\r\nSTAT get_misses 0\r\nEND\r\n");
        assert_eq!(stats.hit_rate(), None);
    }

    #[test]
    fn fill_is_measured_against_the_configured_limit() {
        let stats = parse_stats(SAMPLE);
        // 858993459 of 1073741824 is 80% to within a rounding error, and
        // asserting exact equality on a ratio of two integers that do not
        // divide evenly tests the float unit, not the collector.
        let fill = stats.fill().expect("bytes and limit are both present");
        assert!((fill - 80.0).abs() < 0.001, "{fill}");
    }

    #[test]
    fn fill_is_absent_without_a_limit() {
        let stats = parse_stats("STAT bytes 100\r\nSTAT limit_maxbytes 0\r\nEND\r\n");
        assert_eq!(stats.fill(), None);
    }

    /// Version strings contain dots and can contain spaces; splitting on every
    /// space would truncate them.
    #[test]
    fn a_value_may_contain_spaces() {
        let stats = parse_stats("STAT version 1.6.21 (custom build)\r\nEND\r\n");
        assert_eq!(stats.text("version"), Some("1.6.21 (custom build)"));
    }

    #[test]
    fn anything_after_end_is_ignored() {
        let stats = parse_stats("STAT curr_items 5\r\nEND\r\nSTAT curr_items 9999\r\n");
        assert_eq!(stats.number("curr_items"), Some(5.0));
    }

    #[test]
    fn a_non_memcached_response_yields_nothing() {
        assert!(parse_stats("HTTP/1.1 200 OK\r\n\r\n<html>").fields.is_empty());
    }

    /// The whole point of the module doc: a cache can be evicting hard and
    /// still report a near-perfect hit rate, because the survivors are the
    /// popular keys.
    #[test]
    fn a_high_hit_rate_does_not_mean_the_cache_is_big_enough() {
        let stats = parse_stats(SAMPLE);
        assert!(stats.hit_rate().unwrap() > 99.0);
        assert!(stats.number("evictions").unwrap() > 0.0);
    }
}
