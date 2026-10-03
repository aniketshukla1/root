//! What the demo estate prints.
//!
//! Lines are generated per second from the same seed and the same story
//! effects the snapshots read, so a burst of `too many clients` errors in the
//! log histogram lands exactly where the database's connection chart peaks.
//! Each connection speaks only through the channel its real collector has:
//! container stdout from the Docker host, transitions from an HTTP check,
//! finished builds from Jenkins, warning events from Kubernetes, stalled
//! groups from Kafka. Everything else is silent, as it would be.

use chrono::{DateTime, Duration, Utc};

use super::estate::Member;
use super::kinds::{self, Ctx, CONTAINERS, INGEST_PODS};
use super::noise::{hash, key, unit};
use super::story::{effects_at, BuildResult, Instance, Script, Story};
use crate::model::{LogEvent, LogLevel, LogStream, ServiceKind};

/// Lines between two moments, oldest first, at most `limit`.
pub fn between(
    script: &Script,
    member: &'static Member,
    profile_id: &str,
    profile_name: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    limit: usize,
) -> Vec<LogEvent> {
    if until <= since {
        return Vec::new();
    }
    let instances = script.instances(until);
    let line = |source: &str, level: LogLevel, message: String, at: DateTime<Utc>| {
        LogEvent::new(profile_id, profile_name, member.kind, source, level, message, at)
    };

    let mut out = match member.kind {
        ServiceKind::Docker => containers(script, &instances, since, until, &line),
        ServiceKind::HttpEndpoint => transitions(script, &instances, member, since, until, &line),
        ServiceKind::Jenkins => builds(script, until, since, &line),
        ServiceKind::Kafka => stalls(script, &instances, member, profile_id, profile_name, since, until, &line),
        ServiceKind::Kubernetes => pod_events(&instances, since, until, &line),
        _ => Vec::new(),
    };

    out.sort_by_key(|e| e.timestamp);
    out.truncate(limit);
    out
}

type Line<'a> = dyn Fn(&str, LogLevel, String, DateTime<Utc>) -> LogEvent + 'a;

/// Every whole second in `(since, until]`.
fn seconds(since: DateTime<Utc>, until: DateTime<Utc>) -> impl Iterator<Item = i64> {
    let first = since.timestamp() + 1;
    let last = until.timestamp();
    first..=last
}

fn at(second: i64, fraction: f64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(second * 1000 + (fraction.clamp(0.0, 0.999) * 1000.0) as i64)
        .unwrap_or_else(Utc::now)
}

/// How many lines a source emits this second, given its rate per second.
fn count(seed: u64, source: u64, second: i64, salt: u64, rate: f64) -> u32 {
    if rate <= 0.0 {
        return 0;
    }
    let whole = rate.floor();
    let u = unit(hash(&[seed, source, second as u64, salt]));
    whole as u32 + u32::from(u < rate - whole)
}

fn pick<'a>(h: u64, options: &[&'a str]) -> &'a str {
    options[(h % options.len() as u64) as usize]
}

// ------------------------------------------------------------ containers

const PRODUCTS: &[&str] = &["1842", "2210", "3307", "4491", "5128", "6730", "7015", "8844", "9302"];
const QUERIES: &[&str] = &["running shoes", "rain jacket", "espresso", "desk lamp", "headphones", "yoga mat"];

fn containers(
    script: &Script,
    instances: &[Instance],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    line: &Line,
) -> Vec<LogEvent> {
    let seed = script.seed;
    let mut out = Vec::new();

    // Startup lines, wherever a deploy or crash restarted something.
    let moment = script.moment(until);
    for d in &moment.deploys {
        for c in CONTAINERS.iter().filter(|c| c.service == d.service) {
            let started = d.at + Duration::seconds(15 * c.replica as i64);
            let ready = started + Duration::milliseconds(1400);
            let inside = |t: DateTime<Utc>| t > since && t <= until;
            if inside(started) {
                out.push(line(c.name, LogLevel::Info, format!("INFO starting {} {} on :8080", c.service, d.version), started));
            }
            if inside(ready) {
                out.push(line(
                    c.name,
                    LogLevel::Info,
                    format!("INFO ready in {}ms", 900 + hash(&[seed, key(c.name), d.at.timestamp() as u64]) % 700),
                    ready,
                ));
            }
        }
    }
    for i in instances.iter().filter(|i| i.story == Story::BadDeploy) {
        let dying = i.at(240.0) - Duration::milliseconds(300);
        if dying > since && dying <= until {
            out.push(line(
                "checkout-api-2",
                LogLevel::Fatal,
                "FATAL runtime: out of memory: cannot allocate 268435456-byte block (1043333120 in use)".to_string(),
                dying,
            ));
        }
        let back = i.at(262.0);
        if back > since && back <= until {
            out.push(line(
                "checkout-api-2",
                LogLevel::Info,
                "INFO starting checkout-api after restart (exit 137)".to_string(),
                back,
            ));
        }
    }

    for second in seconds(since, until) {
        let t = at(second, 0.0);
        let e = effects_at(instances, t);
        let rps = kinds::traffic_at(seed, t);
        let crashed = e.checkout_crashed;

        for c in CONTAINERS {
            let source = key(c.name);
            if c.name == "checkout-api-2" && crashed {
                continue;
            }
            let base = match c.service {
                "web" => 0.0026 * rps,
                "catalog-api" => 0.0018 * rps,
                "cart-api" => 0.0011 * rps,
                "checkout-api" => 0.0009 * rps,
                "payments-api" => 0.0005 * rps,
                "order-worker" => 0.0012 * rps,
                "email-worker" => 0.03,
                _ => 0.004,
            };
            for n in 0..count(seed, source, second, 1, base) {
                let h = hash(&[seed, source, second as u64, n as u64]);
                let ms = 4 + h % 60;
                let msg = match c.service {
                    "web" => match h % 5 {
                        0 => format!("INFO GET / 200 {}ms", ms + 30),
                        1 => format!("INFO GET /products/{} 200 {ms}ms", pick(h >> 8, PRODUCTS)),
                        2 => format!("INFO GET /search?q={} 200 {}ms", pick(h >> 8, QUERIES).replace(' ', "+"), ms + 50),
                        3 => format!("INFO GET /cart 200 {ms}ms"),
                        _ => format!("INFO GET /assets/app.{:x}.js 304 1ms", (h >> 20) & 0xFFFFF),
                    },
                    "catalog-api" => match h % 3 {
                        0 => format!("INFO GET /api/products/{} 200 {ms}ms", pick(h >> 8, PRODUCTS)),
                        1 => format!(
                            "INFO GET /api/products?q={} 200 {}ms (search {}ms)",
                            pick(h >> 8, QUERIES).replace(' ', "+"),
                            ms + 20,
                            ms + 8
                        ),
                        _ => format!("INFO GET /api/featured 200 {}ms", ms + 12),
                    },
                    "cart-api" => match h % 2 {
                        0 => format!("INFO GET /api/cart/{:x} 200 {}ms", h >> 40, 2 + ms / 6),
                        _ => format!("INFO PUT /api/cart/{:x}/items 200 {}ms", h >> 40, 3 + ms / 5),
                    },
                    "checkout-api" => format!(
                        "INFO POST /api/checkout 201 {}ms order=ord_{:x}",
                        120 + ms * 3,
                        h >> 36
                    ),
                    "payments-api" => format!(
                        "INFO POST /v1/authorize 200 {}ms provider=stripe outcome=approved",
                        150 + ms * 2
                    ),
                    "order-worker" => match h % 2 {
                        0 => format!(
                            "INFO consumed orders partition={} offset={}",
                            h % 12,
                            8_100_000 + second % 1_000_000
                        ),
                        _ => format!("INFO processed ord_{:x} in {ms}ms", h >> 36),
                    },
                    "email-worker" => "INFO sent template=order_confirmation".to_string(),
                    _ => format!("INFO exporter otlp: sent {} spans", 200 + h % 900),
                };
                out.push(line(c.name, LogLevel::Info, msg, at(second, unit(h >> 3))));
            }

            // The stories, as the services would complain about them.
            if c.service == "checkout-api" && e.n_plus_one > 0.2 {
                for n in 0..count(seed, source, second, 2, 0.7 * e.n_plus_one) {
                    let h = hash(&[seed, source, second as u64, 100 + n as u64]);
                    out.push(line(
                        c.name,
                        LogLevel::Warn,
                        format!(
                            "WARN slow query {}ms: SELECT oi.id, oi.sku, oi.quantity, oi.unit_price FROM order_items oi WHERE oi.order_id = $1",
                            300 + h % 900
                        ),
                        at(second, unit(h)),
                    ));
                }
            }
            if c.service == "checkout-api" && e.db_saturation > 0.5 {
                for n in 0..count(seed, source, second, 3, 0.6 * e.db_saturation) {
                    let h = hash(&[seed, source, second as u64, 200 + n as u64]);
                    out.push(line(
                        c.name,
                        LogLevel::Warn,
                        "WARN db pool exhausted: waited 3000ms for a connection (40/40 in use)".to_string(),
                        at(second, unit(h)),
                    ));
                }
            }
            if c.service == "checkout-api" && e.checkout_errors > 0.1 {
                for n in 0..count(seed, source, second, 4, 1.4 * e.checkout_errors) {
                    let h = hash(&[seed, source, second as u64, 300 + n as u64]);
                    let msg = if h.is_multiple_of(3) {
                        "ERROR POST /api/checkout 503 5004ms: context deadline exceeded acquiring connection".to_string()
                    } else {
                        "ERROR POST /api/checkout 500: pq: sorry, too many clients already".to_string()
                    };
                    out.push(line(c.name, LogLevel::Error, msg, at(second, unit(h))));
                }
            }
            if c.service == "web" && e.checkout_errors > 0.1 {
                for n in 0..count(seed, source, second, 5, 0.35 * e.checkout_errors) {
                    let h = hash(&[seed, source, second as u64, 400 + n as u64]);
                    out.push(line(
                        c.name,
                        LogLevel::Error,
                        "ERROR POST /checkout 502: upstream checkout-api returned 503".to_string(),
                        at(second, unit(h)),
                    ));
                }
            }
            if c.service == "catalog-api" && e.search_slow > 0.2 {
                for n in 0..count(seed, source, second, 6, 0.8 * e.search_slow) {
                    let h = hash(&[seed, source, second as u64, 500 + n as u64]);
                    let msg = if e.es_node_out && h.is_multiple_of(3) {
                        "ERROR elasticsearch: request to search.acme.internal:9200 timed out after 2000ms".to_string()
                    } else {
                        format!(
                            "WARN search took {}ms (products-v7, q=\"{}\")",
                            1500 + h % 1100,
                            pick(h >> 8, QUERIES)
                        )
                    };
                    let level = if msg.starts_with("ERROR") { LogLevel::Error } else { LogLevel::Warn };
                    out.push(line(c.name, level, msg, at(second, unit(h))));
                }
            }
        }
    }
    out
}

// ----------------------------------------------------------- transitions

/// An HTTP check reports only when it changes: up to down, down to up.
fn transitions(
    script: &Script,
    instances: &[Instance],
    member: &'static Member,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    line: &Line,
) -> Vec<LogEvent> {
    let step = member.poll_secs.max(1) as i64;
    let probe = |t: DateTime<Utc>| {
        let m = script.light_moment(instances, t);
        let id = super::estate::profile_id(member.key);
        let cx = Ctx { m: &m, member, id: &id, name: member.name };
        let (status, latency) = kinds::http_probe(&cx);
        let healthy = status == 200 && latency <= 1000;
        let detail = if status != 200 {
            format!("{status} {} (expected 200) in {latency}ms", reason(status))
        } else if latency > 1000 {
            format!("200 OK in {latency}ms — slower than 1000ms")
        } else {
            format!("200 OK in {latency}ms")
        };
        (healthy, detail)
    };

    let mut out = Vec::new();
    let mut poll = (since.timestamp() / step) * step;
    let mut previous = probe(at(poll, 0.0)).0;
    loop {
        poll += step;
        let t = at(poll, 0.0);
        if t > until {
            break;
        }
        let (healthy, detail) = probe(t);
        if healthy != previous {
            out.push(
                line(
                    member.endpoint,
                    if healthy { LogLevel::Info } else { LogLevel::Error },
                    if healthy {
                        format!("Endpoint recovered — {detail}")
                    } else {
                        format!("Endpoint check failed — {detail}")
                    },
                    t,
                )
                .stream(LogStream::Event),
            );
            previous = healthy;
        }
    }
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

// ---------------------------------------------------------------- builds

fn builds(script: &Script, until: DateTime<Utc>, since: DateTime<Utc>, line: &Line) -> Vec<LogEvent> {
    let moment = script.moment(until);
    moment
        .builds
        .iter()
        .filter(|b| b.end > since && b.end <= until)
        .map(|b| {
            let (level, result) = match b.result {
                BuildResult::Success => (LogLevel::Info, "SUCCESS"),
                BuildResult::Failure => (LogLevel::Error, "FAILURE"),
            };
            line(
                b.job,
                level,
                format!(
                    "Build #{} finished: {result} in {:.1}s",
                    b.number,
                    (b.end - b.start).num_milliseconds() as f64 / 1000.0
                ),
                b.end,
            )
            .stream(LogStream::Event)
        })
        .collect()
}

// ----------------------------------------------------------------- kafka

#[allow(clippy::too_many_arguments)]
fn stalls(
    script: &Script,
    instances: &[Instance],
    member: &'static Member,
    profile_id: &str,
    profile_name: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    line: &Line,
) -> Vec<LogEvent> {
    let mut out = Vec::new();
    for i in instances.iter().filter(|i| i.story == Story::ConsumerOom) {
        let stalled = i.at(150.0);
        if stalled > since && stalled <= until {
            let m = script.light_moment(instances, stalled);
            let cx = Ctx { m: &m, member, id: profile_id, name: profile_name };
            out.push(
                line(
                    "analytics-ingest",
                    LogLevel::Error,
                    format!(
                        "consumer group analytics-ingest is {:.0} behind with no members",
                        kinds::ingest_lag(&cx)
                    ),
                    stalled,
                )
                .stream(LogStream::Event),
            );
        }
    }
    out
}

// ------------------------------------------------------------ kubernetes

fn pod_events(instances: &[Instance], since: DateTime<Utc>, until: DateTime<Utc>, line: &Line) -> Vec<LogEvent> {
    let mut out = Vec::new();
    for i in instances.iter().filter(|i| i.story == Story::ConsumerOom) {
        for (p, pod) in INGEST_PODS.iter().enumerate() {
            let start = 78.0 + p as f64 * 11.0;
            // Each restart is an OOM kill and then a back-off.
            let mut restarts_before = 0;
            let mut tau = start;
            while tau < 320.0 {
                let looped = tau - start;
                let restarts = (1.0 + (looped / 18.0).sqrt() * 2.2).floor() as i64;
                if restarts != restarts_before {
                    restarts_before = restarts;
                    let t = i.at(tau);
                    if t > since && t <= until {
                        out.push(
                            line(
                                pod,
                                LogLevel::Warn,
                                format!("OOMKilled: container ingest in pod {pod} exceeded its 512Mi memory limit"),
                                t,
                            )
                            .stream(LogStream::Event),
                        );
                        out.push(
                            line(
                                pod,
                                LogLevel::Warn,
                                format!("BackOff: Back-off restarting failed container ingest in pod {pod}"),
                                t + Duration::seconds(2),
                            )
                            .stream(LogStream::Event),
                        );
                    }
                }
                tau += 1.0;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::estate::ESTATE;

    fn script() -> Script {
        Script {
            started_at: DateTime::parse_from_rfc3339("2026-10-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            seed: 5,
            triggers: Vec::new(),
        }
    }

    fn member(key: &str) -> &'static Member {
        ESTATE.iter().find(|m| m.key == key).unwrap()
    }

    #[test]
    fn the_same_window_prints_the_same_lines() {
        let s = script();
        let a = between(&s, member("prod-docker"), "sim-prod-docker", "prod-docker", s.started_at, s.started_at + Duration::seconds(60), 10_000);
        let b = between(&s, member("prod-docker"), "sim-prod-docker", "prod-docker", s.started_at, s.started_at + Duration::seconds(60), 10_000);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| x.message == y.message && x.timestamp == y.timestamp));
        assert!(!a.is_empty());
    }

    #[test]
    fn adjoining_windows_neither_repeat_nor_drop_a_line() {
        let s = script();
        let m = member("prod-docker");
        let mid = s.started_at + Duration::seconds(30);
        let whole = between(&s, m, "p", "p", s.started_at, s.started_at + Duration::seconds(60), 10_000);
        let first = between(&s, m, "p", "p", s.started_at, mid, 10_000);
        let second = between(&s, m, "p", "p", mid, s.started_at + Duration::seconds(60), 10_000);
        assert_eq!(whole.len(), first.len() + second.len());
    }

    #[test]
    fn the_bad_deploy_floods_checkout_with_errors() {
        let s = script();
        let deploy = s.next(s.started_at).unwrap();
        let lines = between(&s, member("prod-docker"), "p", "p", deploy.at(200.0), deploy.at(260.0), 10_000);
        let errors = lines.iter().filter(|l| l.level >= LogLevel::Error && l.source.starts_with("checkout")).count();
        assert!(errors > 20, "{errors}");
    }

    #[test]
    fn the_checkout_check_reports_going_down_and_coming_back() {
        let s = script();
        let deploy = s.next(s.started_at).unwrap();
        let lines = between(&s, member("checkout"), "p", "p", deploy.start, deploy.end(), 100);
        assert!(lines.iter().any(|l| l.message.starts_with("Endpoint check failed")));
        assert!(lines.iter().any(|l| l.message.starts_with("Endpoint recovered")));
    }
}
