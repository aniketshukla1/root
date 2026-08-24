//! Redis monitor.
//!
//! Everything here comes from `INFO` plus `SLOWLOG GET`, which is deliberate:
//! both are cheap, both are available to a read-only user, and neither scans
//! the keyspace. `KEYS` and `SCAN` are conspicuously absent — a monitoring tool
//! that walks a production keyspace every poll is a monitoring tool that causes
//! the incident it is meant to report.
//!
//! `INFO` returns a flat `field:value` text blob grouped by `# Section`
//! headers. Parsing it into a map once and reading typed values out of that is
//! the whole trick; the rest is deciding what counts as unhealthy.

use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

pub struct RedisCollector {
    client: redis::Client,
    memory_warn_percent: f64,
    slowlog_count: usize,
    endpoint: String,
    profile_id: String,
    profile_name: String,
    /// Slowlog entry ids already turned into log lines. Redis keeps serving the
    /// same entries until the log is reset, so without this every poll would
    /// re-report them.
    seen_slowlog: Mutex<Vec<i64>>,
}

impl RedisCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim();
        if raw.is_empty() {
            return Err(AppError::Config(
                "Redis connections need a URL, e.g. redis://cache.internal:6379".to_string(),
            ));
        }

        // Credentials live in the keychain, not the URL, so they are spliced in
        // here rather than asking the user to embed a password in a field that
        // is displayed back to them.
        let mut url = url::Url::parse(raw)
            .map_err(|e| AppError::Config(format!("`{raw}` is not a valid Redis URL: {e}")))?;
        if let Some(secret) = secret.as_deref().filter(|s| !s.is_empty()) {
            let username = profile.username.clone().unwrap_or_default();
            url.set_username(&username)
                .map_err(|_| AppError::Config("could not apply the username".to_string()))?;
            url.set_password(Some(secret))
                .map_err(|_| AppError::Config("could not apply the password".to_string()))?;
        }

        let client = redis::Client::open(url.to_string())
            .map_err(|e| AppError::Config(format!("could not open a Redis client: {e}")))?;

        Ok(RedisCollector {
            client,
            memory_warn_percent: profile
                .options
                .u64("memoryWarnPercent")
                .unwrap_or(85)
                .clamp(1, 100) as f64,
            slowlog_count: profile.options.u64("slowlogCount").unwrap_or(20).min(128) as usize,
            // Displayed and logged, so it must never contain the password.
            endpoint: raw.to_string(),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            seen_slowlog: Mutex::new(Vec::new()),
        })
    }
}

/// `INFO` output into a flat map, section headers dropped.
///
/// Keys are unique across sections in practice, and flattening avoids threading
/// a section name through every lookup.
fn parse_info(raw: &str) -> HashMap<String, String> {
    raw.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn num(info: &HashMap<String, String>, key: &str) -> Option<f64> {
    info.get(key)?.parse::<f64>().ok()
}

/// Keyspace lines look like `db0:keys=12,expires=3,avg_ttl=0`.
fn parse_keyspace(info: &HashMap<String, String>) -> Vec<(String, f64, f64)> {
    let mut rows: Vec<(String, f64, f64)> = info
        .iter()
        .filter(|(k, _)| k.starts_with("db") && k[2..].chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(db, spec)| {
            let mut keys = 0.0;
            let mut expires = 0.0;
            for part in spec.split(',') {
                match part.split_once('=') {
                    Some(("keys", v)) => keys = v.parse().unwrap_or(0.0),
                    Some(("expires", v)) => expires = v.parse().unwrap_or(0.0),
                    _ => {}
                }
            }
            Some((db.clone(), keys, expires))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Below this much data held, `mem_fragmentation_ratio` says nothing.
///
/// The ratio is RSS over logical memory, and RSS includes a fixed allocator and
/// process baseline of tens of megabytes. An empty Redis therefore reports
/// something like 14x — not fragmentation, just a small numerator. Acting on
/// that marks every idle or small instance permanently degraded, which is what
/// the live test caught.
const FRAGMENTATION_FLOOR_BYTES: f64 = 64.0 * 1024.0 * 1024.0;

/// Whether the fragmentation ratio is worth believing at this dataset size.
fn fragmentation_is_meaningful(used_memory: f64) -> bool {
    used_memory >= FRAGMENTATION_FLOOR_BYTES
}

/// Cache hit rate as a percentage, or `None` before any lookups happen.
///
/// A fresh instance has zero hits and zero misses; reporting that as 0% would
/// show an alarming red number for a cache that is simply new.
fn hit_rate(info: &HashMap<String, String>) -> Option<f64> {
    let hits = num(info, "keyspace_hits")?;
    let misses = num(info, "keyspace_misses")?;
    let total = hits + misses;
    (total > 0.0).then(|| hits / total * 100.0)
}

#[async_trait]
impl Collector for RedisCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Redis
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let mut conn = match self.client.get_multiplexed_async_connection().await {
            Ok(conn) => conn,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Redis,
                    format!("{}: {e}", self.endpoint),
                    elapsed_ms(start),
                ));
            }
        };

        let raw: String = match redis::cmd("INFO").query_async(&mut conn).await {
            Ok(raw) => raw,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Redis,
                    format!("INFO failed: {e}"),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);
        let info = parse_info(&raw);

        let used_memory = num(&info, "used_memory").unwrap_or(0.0);
        let max_memory = num(&info, "maxmemory").unwrap_or(0.0);
        let clients = num(&info, "connected_clients").unwrap_or(0.0);
        let blocked = num(&info, "blocked_clients").unwrap_or(0.0);
        let evicted = num(&info, "evicted_keys").unwrap_or(0.0);
        let ops = num(&info, "instantaneous_ops_per_sec").unwrap_or(0.0);
        let fragmentation = num(&info, "mem_fragmentation_ratio").unwrap_or(1.0);
        let role = info
            .get("role")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let link = info.get("master_link_status").cloned();

        // maxmemory=0 means unlimited, so a percentage of it is meaningless.
        let memory_percent = (max_memory > 0.0).then(|| used_memory / max_memory * 100.0);

        // A replica that has lost its master is the most serious thing INFO
        // reports, ahead of any resource pressure.
        let replication_broken = link.as_deref() == Some("down");
        let memory_critical = memory_percent.is_some_and(|p| p >= self.memory_warn_percent);
        let fragmented = fragmentation_is_meaningful(used_memory) && fragmentation > 1.5;

        let health = if replication_broken {
            Health::Down
        } else if memory_critical || fragmented {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if replication_broken {
            "replication link to master is down".to_string()
        } else if let Some(percent) = memory_percent.filter(|p| *p >= self.memory_warn_percent) {
            format!("memory at {percent:.0}% of maxmemory")
        } else if fragmented {
            format!("memory fragmentation {fragmentation:.2}x")
        } else {
            format!(
                "{role} · {} clients · {ops:.0} ops/sec",
                clients as u64
            )
        };

        // ------------------------------------------------------ server row
        let server_rows = vec![Row::new("server", &self.endpoint, health)
            .summary(&summary)
            .cell("role", CellValue::text(&role))
            .cell(
                "version",
                CellValue::maybe_text(info.get("redis_version").cloned()),
            )
            .cell(
                "uptime",
                CellValue::duration(num(&info, "uptime_in_seconds").unwrap_or(0.0)),
            )
            .cell("clients", CellValue::number(clients))
            .cell("blocked", CellValue::number(blocked))
            .cell("ops", CellValue::number(ops))];

        // ------------------------------------------------------ memory row
        let memory_rows = vec![Row::new(
            "memory",
            "Memory",
            if memory_critical { Health::Degraded } else { Health::Healthy },
        )
        .summary(match memory_percent {
            Some(p) => format!("{p:.0}% of maxmemory"),
            None => "no maxmemory limit set".to_string(),
        })
        .cell("used", CellValue::number(used_memory))
        .cell(
            "limit",
            if max_memory > 0.0 {
                CellValue::number(max_memory)
            } else {
                CellValue::text("unlimited")
            },
        )
        .cell(
            "pressure",
            match memory_percent {
                Some(p) => CellValue::gauge_warn(p, Some(100.0), self.memory_warn_percent),
                None => CellValue::Null,
            },
        )
        .cell("fragmentation", CellValue::number(fragmentation))
        .cell("evicted", CellValue::number(evicted))];

        // ---------------------------------------------------- keyspace rows
        let keyspace_rows: Vec<Row> = parse_keyspace(&info)
            .into_iter()
            .map(|(db, keys, expires)| {
                Row::new(format!("keyspace:{db}"), &db, Health::Healthy)
                    .summary(format!("{keys:.0} keys"))
                    .cell("db", CellValue::text(&db))
                    .cell("keys", CellValue::number(keys))
                    .cell("expires", CellValue::number(expires))
            })
            .collect();

        // ---------------------------------------------------- slowlog rows
        let slowlog = fetch_slowlog(&mut conn, self.slowlog_count).await;
        let slow_rows: Vec<Row> = slowlog
            .iter()
            .map(|entry| {
                Row::new(
                    format!("slow:{}", entry.id),
                    entry.command.clone(),
                    Health::Degraded,
                )
                .summary(format!("{:.1}ms", entry.micros / 1000.0))
                .cell("command", CellValue::text(&entry.command))
                .cell("duration", CellValue::number(entry.micros / 1000.0))
                .cell("at", CellValue::timestamp(entry.at))
            })
            .collect();

        let mut metrics = vec![
            Metric::new("used_memory", "Memory used", used_memory, MetricUnit::Bytes),
            Metric::new("clients", "Connected clients", clients, MetricUnit::Count),
            Metric::new("blocked", "Blocked clients", blocked, MetricUnit::Count),
            Metric::new("ops", "Operations/sec", ops, MetricUnit::PerSecond),
            Metric::new("evicted", "Evicted keys", evicted, MetricUnit::Count),
            // A bare multiplier (1.12x), so Count renders it unadorned —
            // there is no ratio unit and Percent would be wrong.
            Metric::new(
                "fragmentation",
                "Memory fragmentation",
                fragmentation,
                MetricUnit::Count,
            ),
        ];
        // Only threshold the ratio once the dataset is big enough for it to
        // mean anything — otherwise the tile is red on every idle instance.
        if fragmentation_is_meaningful(used_memory) {
            if let Some(metric) = metrics.iter_mut().find(|m| m.key == "fragmentation") {
                *metric = metric.clone().warn_above(1.5);
            }
        }
        if let Some(percent) = memory_percent {
            metrics.push(
                Metric::new("memory_percent", "Memory used", percent, MetricUnit::Percent)
                    .warn_above(self.memory_warn_percent),
            );
        }
        if let Some(rate) = hit_rate(&info) {
            metrics.push(Metric::new("hit_rate", "Cache hit rate", rate, MetricUnit::Percent));
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Redis,
            health,
            summary,
            metrics,
            sections: vec![
                Section::new(
                    "server",
                    "Server",
                    vec![
                        ColumnSpec::text("role", "Role").primary(),
                        ColumnSpec::text("version", "Version"),
                        ColumnSpec::duration("uptime", "Uptime"),
                        ColumnSpec::metric("clients", "Clients", MetricUnit::Count),
                        ColumnSpec::metric("blocked", "Blocked", MetricUnit::Count),
                        ColumnSpec::metric("ops", "Ops/sec", MetricUnit::PerSecond),
                    ],
                )
                .rows(server_rows)
                .priority(10),
                Section::new(
                    "memory",
                    "Memory",
                    vec![
                        ColumnSpec::text("used", "Used").primary(),
                        ColumnSpec::text("limit", "Limit"),
                        ColumnSpec::gauge("pressure", "Pressure", MetricUnit::Percent),
                        ColumnSpec::metric("fragmentation", "Fragmentation", MetricUnit::Count),
                        ColumnSpec::metric("evicted", "Evicted", MetricUnit::Count),
                    ],
                )
                .rows(memory_rows)
                .priority(15),
                Section::new(
                    "slowlog",
                    "Slow commands",
                    vec![
                        ColumnSpec::code("command", "Command").primary(),
                        ColumnSpec::metric("duration", "Duration", MetricUnit::Millis),
                        ColumnSpec::timestamp("at", "When"),
                    ],
                )
                .rows(slow_rows)
                .priority(5)
                .empty_text("No slow commands recorded."),
                Section::new(
                    "keyspace",
                    "Keyspace",
                    vec![
                        ColumnSpec::text("db", "Database").primary(),
                        ColumnSpec::metric("keys", "Keys", MetricUnit::Count),
                        ColumnSpec::metric("expires", "With TTL", MetricUnit::Count),
                    ],
                )
                .rows(keyspace_rows)
                .priority(20)
                .tracked()
                .empty_text("No keys stored."),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let Ok(mut conn) = self.client.get_multiplexed_async_connection().await else {
            return Ok(Vec::new());
        };
        let entries = fetch_slowlog(&mut conn, self.slowlog_count).await;

        let mut seen = self.seen_slowlog.lock().await;
        let mut out = Vec::new();
        for entry in entries {
            if seen.contains(&entry.id) {
                continue;
            }
            seen.push(entry.id);
            out.push(
                LogEvent::new(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Redis,
                    "slowlog",
                    LogLevel::Warn,
                    format!(
                        "slow command took {:.1}ms: {}",
                        entry.micros / 1000.0,
                        entry.command
                    ),
                    entry.at,
                )
                // A diagnostic, not something Redis printed — so it stays out
                // of the shared runtime stream, exactly like DB slow queries.
                .stream(LogStream::Diagnostic),
            );
        }

        // Bound the memory of the dedupe set; slowlog ids only increase.
        if seen.len() > 512 {
            let drop_to = seen.len() - 256;
            seen.drain(..drop_to);
        }

        out.sort_by_key(|e| e.timestamp);
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
struct SlowEntry {
    id: i64,
    at: DateTime<Utc>,
    micros: f64,
    command: String,
}

/// `SLOWLOG GET n` returns nested arrays: [id, unix_ts, micros, [argv...], ...].
///
/// Decoded by hand because the shape is positional and the trailing client
/// address/name fields are version-dependent — a struct mapping would break on
/// the first Redis that adds a field.
async fn fetch_slowlog(
    conn: &mut redis::aio::MultiplexedConnection,
    count: usize,
) -> Vec<SlowEntry> {
    if count == 0 {
        return Vec::new();
    }
    let raw: redis::Value = match redis::cmd("SLOWLOG")
        .arg("GET")
        .arg(count)
        .query_async(conn)
        .await
    {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    decode_slowlog(&raw)
}

fn decode_slowlog(raw: &redis::Value) -> Vec<SlowEntry> {
    let redis::Value::Array(entries) = raw else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let redis::Value::Array(fields) = entry else {
                return None;
            };
            let id = as_int(fields.first()?)?;
            let seconds = as_int(fields.get(1)?)?;
            let micros = as_int(fields.get(2)?)? as f64;

            let command = match fields.get(3) {
                Some(redis::Value::Array(argv)) => argv
                    .iter()
                    .filter_map(as_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };

            Some(SlowEntry {
                id,
                at: DateTime::from_timestamp(seconds, 0).unwrap_or_else(Utc::now),
                micros,
                command,
            })
        })
        .collect()
}

fn as_int(value: &redis::Value) -> Option<i64> {
    match value {
        redis::Value::Int(n) => Some(*n),
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().ok(),
        _ => None,
    }
}

fn as_string(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        redis::Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Server
redis_version:7.2.4
uptime_in_seconds:3600

# Clients
connected_clients:12
blocked_clients:1

# Memory
used_memory:1048576
maxmemory:2097152
mem_fragmentation_ratio:1.12

# Stats
instantaneous_ops_per_sec:250
evicted_keys:4
keyspace_hits:900
keyspace_misses:100

# Replication
role:master

# Keyspace
db0:keys=120,expires=30,avg_ttl=0
db1:keys=7,expires=0,avg_ttl=0
";

    #[test]
    fn parses_info_across_sections() {
        let info = parse_info(SAMPLE);
        assert_eq!(info.get("redis_version").unwrap(), "7.2.4");
        assert_eq!(num(&info, "connected_clients"), Some(12.0));
        assert_eq!(num(&info, "used_memory"), Some(1048576.0));
        // Section headers and blank lines must not become entries.
        assert!(!info.contains_key("# Server"));
    }

    #[test]
    fn parses_keyspace_lines() {
        let rows = parse_keyspace(&parse_info(SAMPLE));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("db0".to_string(), 120.0, 30.0));
        assert_eq!(rows[1], ("db1".to_string(), 7.0, 0.0));
    }

    #[test]
    fn keyspace_ignores_unrelated_keys() {
        // `db` prefixed but not a keyspace line — must not be parsed as one.
        let mut info = parse_info(SAMPLE);
        info.insert("dbfilename".to_string(), "dump.rdb".to_string());
        let rows = parse_keyspace(&info);
        assert_eq!(rows.len(), 2, "dbfilename must not appear as a database");
    }

    #[test]
    fn fragmentation_is_ignored_on_a_small_dataset() {
        // A real idle Redis reports ~14x here. The ratio is RSS over logical
        // memory and RSS carries a fixed allocator baseline, so at small sizes
        // it measures the baseline rather than fragmentation. Believing it
        // marks every idle instance degraded — which a live test caught.
        assert!(!fragmentation_is_meaningful(1_048_576.0), "1 MB");
        assert!(!fragmentation_is_meaningful(0.0), "empty");
        assert!(fragmentation_is_meaningful(128.0 * 1024.0 * 1024.0), "128 MB");
    }

    #[test]
    fn hit_rate_is_a_percentage() {
        let rate = hit_rate(&parse_info(SAMPLE)).expect("hits and misses present");
        assert!((rate - 90.0).abs() < 1e-9, "got {rate}");
    }

    #[test]
    fn hit_rate_is_absent_before_any_lookups() {
        // A brand-new cache has 0 hits and 0 misses. Reporting 0% would show an
        // alarming number for something that is merely idle.
        let mut info = HashMap::new();
        info.insert("keyspace_hits".to_string(), "0".to_string());
        info.insert("keyspace_misses".to_string(), "0".to_string());
        assert!(hit_rate(&info).is_none());
    }

    #[test]
    fn decodes_a_slowlog_entry() {
        let raw = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::Int(7),
            redis::Value::Int(1_700_000_000),
            redis::Value::Int(15_000),
            redis::Value::Array(vec![
                redis::Value::BulkString(b"GET".to_vec()),
                redis::Value::BulkString(b"big:key".to_vec()),
            ]),
        ])]);
        let entries = decode_slowlog(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 7);
        assert_eq!(entries[0].command, "GET big:key");
        assert!((entries[0].micros - 15_000.0).abs() < 1e-9);
    }

    #[test]
    fn tolerates_extra_trailing_slowlog_fields() {
        // Newer Redis appends client address and name. Positional decoding must
        // ignore what it does not recognise rather than dropping the entry.
        let raw = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::Int(9),
            redis::Value::Int(1_700_000_500),
            redis::Value::Int(2_500),
            redis::Value::Array(vec![redis::Value::BulkString(b"PING".to_vec())]),
            redis::Value::BulkString(b"10.0.0.5:6379".to_vec()),
            redis::Value::BulkString(b"worker-1".to_vec()),
        ])]);
        let entries = decode_slowlog(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "PING");
    }

    #[test]
    fn malformed_slowlog_is_skipped_not_fatal() {
        let raw = redis::Value::Array(vec![redis::Value::Nil, redis::Value::Int(3)]);
        assert!(decode_slowlog(&raw).is_empty());
    }
}
