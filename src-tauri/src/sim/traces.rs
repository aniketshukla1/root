//! Traces and profiles for the demo's telemetry store.
//!
//! One request per endpoint is sampled every few seconds. A trace id encodes
//! which slot and which endpoint it was, so the waterfall can be rebuilt from
//! the id alone — nothing is stored, and a trace opened an hour later is the
//! same trace. Its shape comes from the story effects at that moment: during
//! the bad deploy a checkout trace is forty sequential `order_items` queries
//! waiting on a starved pool, and the per-service breakdown says so.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};

use super::noise::{hash, unit};
use super::story::{effects_at, Effects, Instance, Moment, Script};
use crate::error::{AppError, Result};
use crate::profiles::{Frame, HotSymbol, Profile};
use crate::traces::{Span, SpanEvent, SpanStatus, Trace, TraceSummary};

/// Seconds between sampled requests per endpoint.
const SLOT_SECS: i64 = 4;

struct Endpoint {
    root_service: &'static str,
    name: &'static str,
}

const ENDPOINTS: [Endpoint; 4] = [
    Endpoint { root_service: "web", name: "GET /" },
    Endpoint { root_service: "web", name: "GET /search" },
    Endpoint { root_service: "web", name: "POST /checkout" },
    Endpoint { root_service: "web", name: "GET /cart" },
];

fn slot_time(slot: i64, endpoint: usize) -> DateTime<Utc> {
    let offset = (hash(&[slot as u64, endpoint as u64, 0x51]) % 3000) as i64;
    DateTime::from_timestamp_millis(slot * SLOT_SECS * 1000 + offset).unwrap_or_else(Utc::now)
}

fn trace_id(seed: u64, slot: i64, endpoint: usize) -> String {
    let check = hash(&[seed, slot as u64, endpoint as u64]) & 0x00FF_FFFF_FFFF_FFFF;
    format!("{:016x}{:02x}{:014x}", slot as u64, endpoint, check)
}

fn decode(seed: u64, id: &str) -> Option<(i64, usize)> {
    if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let slot = u64::from_str_radix(&id[..16], 16).ok()? as i64;
    let endpoint = usize::from_str_radix(&id[16..18], 16).ok()?;
    (endpoint < ENDPOINTS.len() && trace_id(seed, slot, endpoint) == id.to_ascii_lowercase())
        .then_some((slot, endpoint))
}

/// The one random number per request everything else is drawn from.
fn dice(seed: u64, slot: i64, endpoint: usize, salt: u64) -> f64 {
    unit(hash(&[seed, slot as u64, endpoint as u64, salt]))
}

/// How long the request took, and whether it failed — cheap enough to work
/// out for every sampled request in a search window.
fn outcome(seed: u64, e: &Effects, slot: i64, endpoint: usize) -> (f64, bool) {
    let spread = 0.8 + 0.55 * dice(seed, slot, endpoint, 1);
    let fail = dice(seed, slot, endpoint, 2);
    match endpoint {
        0 => ((62.0 + 700.0 * e.search_slow) * spread, false),
        1 => {
            let failed = e.es_node_out && fail < 0.35;
            if failed {
                (2004.0 + 30.0 * spread, true)
            } else {
                ((84.0 + 2200.0 * e.search_slow) * spread, false)
            }
        }
        2 => {
            let failed = fail < e.checkout_errors * 0.8;
            let base = 380.0 + e.n_plus_one * (1900.0 + 1700.0 * e.db_saturation);
            if failed {
                (base.max(1200.0) * (0.9 + 0.2 * spread), true)
            } else {
                (base * spread, false)
            }
        }
        _ => (46.0 * spread, false),
    }
}

fn summary(seed: u64, instances: &[Instance], slot: i64, endpoint: usize) -> TraceSummary {
    let started = slot_time(slot, endpoint);
    let e = effects_at(instances, started);
    let (duration, failed) = outcome(seed, &e, slot, endpoint);
    TraceSummary {
        trace_id: trace_id(seed, slot, endpoint),
        root_service: ENDPOINTS[endpoint].root_service.to_string(),
        root_name: ENDPOINTS[endpoint].name.to_string(),
        started_at: started,
        duration_ms: (duration * 10.0).round() / 10.0,
        error_count: Some(usize::from(failed) * 2),
    }
}

fn slots(from: DateTime<Utc>, to: DateTime<Utc>) -> std::ops::RangeInclusive<i64> {
    (from.timestamp() / SLOT_SECS)..=(to.timestamp() / SLOT_SECS)
}

/// The slowest traces of the last `minutes`, for the store's trace table.
pub fn recent(m: &Moment, minutes: i64, limit: usize) -> Vec<TraceSummary> {
    let from = m.t - Duration::minutes(minutes);
    let mut found: Vec<TraceSummary> = slots(from, m.t)
        .flat_map(|slot| (0..ENDPOINTS.len()).map(move |e| (slot, e)))
        .map(|(slot, e)| summary(m.seed, &m.instances, slot, e))
        .filter(|t| t.started_at > from && t.started_at <= m.t)
        .collect();
    found.sort_by(|a, b| b.duration_ms.partial_cmp(&a.duration_ms).unwrap_or(std::cmp::Ordering::Equal));
    found.truncate(limit);
    found
}

/// Traces overlapping a window, for the correlation lanes.
pub fn in_window(script: &Script, from: DateTime<Utc>, to: DateTime<Utc>, limit: usize) -> Vec<TraceSummary> {
    let instances = script.instances(to);
    let mut found: Vec<TraceSummary> = slots(from, to)
        .flat_map(|slot| (0..ENDPOINTS.len()).map(move |e| (slot, e)))
        .map(|(slot, e)| summary(script.seed, &instances, slot, e))
        .filter(|t| t.started_at >= from && t.started_at <= to)
        .collect();
    found.sort_by(|a, b| {
        b.error_count
            .cmp(&a.error_count)
            .then(b.duration_ms.partial_cmp(&a.duration_ms).unwrap_or(std::cmp::Ordering::Equal))
    });
    found.truncate(limit);
    found
}

// ------------------------------------------------------------------ spans

/// Spans laid out in milliseconds from the start of the request, then
/// scaled so the root lasts exactly as long as the summary said.
struct Builder {
    seed: u64,
    slot: i64,
    endpoint: usize,
    spans: Vec<(usize, Option<usize>, String, String, f64, f64, SpanStatus, Option<String>, BTreeMap<String, String>)>,
}

impl Builder {
    fn span(
        &mut self,
        parent: Option<usize>,
        service: &str,
        name: &str,
        start: f64,
        duration: f64,
        attrs: &[(&str, &str)],
    ) -> usize {
        let index = self.spans.len();
        self.spans.push((
            index,
            parent,
            service.to_string(),
            name.to_string(),
            start,
            duration.max(0.05),
            SpanStatus::Ok,
            None,
            attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ));
        index
    }

    fn fail(&mut self, index: usize, message: &str) {
        self.spans[index].6 = SpanStatus::Error;
        self.spans[index].7 = Some(message.to_string());
    }

    fn end(&self, index: usize) -> f64 {
        self.spans[index].4 + self.spans[index].5
    }

    fn set_end(&mut self, index: usize, end: f64) {
        self.spans[index].5 = (end - self.spans[index].4).max(0.05);
    }

    fn jitter(&self, salt: u64) -> f64 {
        dice(self.seed, self.slot, self.endpoint, 100 + salt)
    }

    fn finish(self, trace_id: &str, started: DateTime<Utc>, total: f64) -> Trace {
        let natural = self.spans.first().map(|s| s.5).unwrap_or(1.0).max(0.001);
        let scale = total / natural;
        let span_id = |i: usize| format!("{:016x}", hash(&[self.seed, self.slot as u64, self.endpoint as u64, 7000 + i as u64]));
        let spans = self
            .spans
            .iter()
            .map(|(i, parent, service, name, start, duration, status, message, attrs)| {
                let at = started + Duration::microseconds((start * scale * 1000.0) as i64);
                let mut events = Vec::new();
                if *status == SpanStatus::Error {
                    events.push(SpanEvent {
                        at: at + Duration::microseconds((duration * scale * 900.0) as i64),
                        name: "exception".to_string(),
                    });
                }
                Span {
                    span_id: span_id(*i),
                    parent_span_id: parent.map(span_id),
                    name: name.clone(),
                    service: service.clone(),
                    started_at: at,
                    duration_ms: (duration * scale * 1000.0).round() / 1000.0,
                    status: *status,
                    status_message: message.clone(),
                    attributes: attrs.clone(),
                    events,
                }
            })
            .collect();
        Trace::assemble(trace_id, spans)
    }
}

/// Rebuild one trace from its id.
pub fn trace(script: &Script, id: &str) -> Result<Trace> {
    let (slot, endpoint) = decode(script.seed, id).ok_or_else(|| {
        AppError::Collector(format!("no trace `{id}` in the demo's telemetry store"))
    })?;
    let started = slot_time(slot, endpoint);
    let instances = script.instances(started);
    let e = effects_at(&instances, started);
    let (total, failed) = outcome(script.seed, &e, slot, endpoint);
    let checkout_version = script.moment(started).version_at("checkout-api", started);
    let bad_build = checkout_version.ends_with(".0") && e.n_plus_one > 0.05;

    let mut b = Builder {
        seed: script.seed,
        slot,
        endpoint,
        spans: Vec::new(),
    };
    let ep = &ENDPOINTS[endpoint];
    let root = b.span(None, ep.root_service, ep.name, 0.0, 1.0, &[("http.method", ep.name.split(' ').next().unwrap_or("GET")), ("http.route", ep.name.split(' ').nth(1).unwrap_or("/"))]);

    match endpoint {
        // The home page: featured products from search, the session from cache.
        0 => {
            let session = b.span(Some(root), "web", "redis GET session", 1.2, 0.6, &[("db.system", "redis"), ("peer.service", "session-cache")]);
            let call = b.span(Some(root), "catalog-api", "GET /api/featured", b.end(session) + 0.8, 1.0, &[("http.method", "GET")]);
            let es_time = 9.0 + 20.0 * b.jitter(1) + 650.0 * e.search_slow;
            let es = b.span(Some(call), "catalog-api", "elasticsearch POST /products-v7/_search", 2.0 + b.spans[call].4, es_time, &[("db.system", "elasticsearch"), ("peer.service", "search-es")]);
            let render = b.span(Some(call), "catalog-api", "render featured", b.end(es) + 0.4, 3.0 + 4.0 * b.jitter(2), &[]);
            let end = b.end(render) + 1.5;
            b.set_end(call, end);
            let html = b.span(Some(root), "web", "render page", end + 0.6, 8.0 + 9.0 * b.jitter(3), &[]);
            let end = b.end(html) + 1.0;
            b.set_end(root, end);
        }
        // Search: the query goes to Elasticsearch, which is where the time goes.
        1 => {
            let call = b.span(Some(root), "catalog-api", "GET /api/products", 1.5, 1.0, &[("http.method", "GET")]);
            let es_time = if failed { 2000.0 } else { 14.0 + 30.0 * b.jitter(1) + 2100.0 * e.search_slow };
            let es = b.span(Some(call), "catalog-api", "elasticsearch POST /products-v7/_search", b.spans[call].4 + 1.0, es_time, &[("db.system", "elasticsearch"), ("peer.service", "search-es"), ("db.operation", "search")]);
            if failed {
                b.fail(es, "request timed out after 2000ms");
                b.fail(call, "search unavailable");
                b.fail(root, "upstream catalog-api returned 504");
            }
            let shape = b.span(Some(call), "catalog-api", "rank results", b.end(es) + 0.3, 2.0 + 3.0 * b.jitter(2), &[]);
            let end = b.end(shape) + 1.0;
            b.set_end(call, end);
            b.set_end(root, end + 2.0);
        }
        // Checkout: session, cart, the order, the items, payment, the event.
        2 => {
            let call = b.span(Some(root), "checkout-api", "POST /api/checkout", 2.0, 1.0, &[("http.method", "POST"), ("service.version", &checkout_version)]);
            let mut cursor = b.spans[call].4 + 0.6;
            let session = b.span(Some(call), "checkout-api", "redis GET session", cursor, 0.7, &[("db.system", "redis"), ("peer.service", "session-cache")]);
            cursor = b.end(session) + 0.4;
            let cart = b.span(Some(call), "cart-api", "GET /api/cart/{id}", cursor, 1.0, &[("http.method", "GET")]);
            let hget = b.span(Some(cart), "cart-api", "redis HGETALL cart", cursor + 0.8, 0.9, &[("db.system", "redis"), ("peer.service", "session-cache")]);
            let end = b.end(hget) + 6.0 + 6.0 * b.jitter(1);
            b.set_end(cart, end);
            cursor = end + 0.5;

            let wait = if e.db_saturation > 0.35 { 120.0 + 900.0 * e.db_saturation * (0.6 + 0.4 * b.jitter(2)) } else { 0.0 };
            if wait > 0.0 {
                let acquire = b.span(Some(call), "checkout-api", "pgxpool.Acquire", cursor, wait, &[("db.system", "postgresql"), ("pool.max", "40")]);
                cursor = b.end(acquire) + 0.2;
            }
            let insert = b.span(Some(call), "checkout-api", "INSERT INTO orders", cursor, 3.5 + 2.0 * b.jitter(3), &[("db.system", "postgresql"), ("peer.service", "orders-db"), ("db.operation", "INSERT")]);
            cursor = b.end(insert) + 0.3;

            if bad_build {
                // One query per line item: the regression the deploy shipped.
                let items = 18 + (24.0 * e.n_plus_one) as usize + (b.jitter(4) * 6.0) as usize;
                for i in 0..items {
                    let each = 5.0 + 45.0 * e.db_saturation * (0.5 + b.jitter(10 + i as u64));
                    let q = b.span(Some(call), "checkout-api", "SELECT order_items", cursor, each, &[("db.system", "postgresql"), ("peer.service", "orders-db"), ("db.statement", "SELECT oi.id, oi.sku, oi.quantity, oi.unit_price FROM order_items oi WHERE oi.order_id = $1")]);
                    cursor = b.end(q) + 0.15;
                }
            } else {
                let q = b.span(Some(call), "checkout-api", "SELECT order_items", cursor, 5.0 + 3.0 * b.jitter(5), &[("db.system", "postgresql"), ("peer.service", "orders-db"), ("db.statement", "SELECT oi.id, oi.sku, oi.quantity, oi.unit_price FROM order_items oi WHERE oi.order_id = ANY($1)")]);
                cursor = b.end(q) + 0.3;
            }

            if failed {
                let err = b.span(Some(call), "checkout-api", "pgxpool.Acquire", cursor, 40.0, &[("db.system", "postgresql")]);
                b.fail(err, "pq: sorry, too many clients already");
                b.fail(call, "pq: sorry, too many clients already");
                b.fail(root, "upstream checkout-api returned 503");
                cursor = b.end(err) + 0.5;
            } else {
                let pay = b.span(Some(call), "payments-api", "POST /v1/authorize", cursor, 1.0, &[("http.method", "POST")]);
                let stripe = b.span(Some(pay), "payments-api", "POST api.stripe.com/v1/payment_intents", cursor + 2.0, 120.0 + 70.0 * b.jitter(6), &[("http.method", "POST"), ("peer.service", "stripe")]);
                let end = b.end(stripe) + 3.0;
                b.set_end(pay, end);
                let publish = b.span(Some(call), "checkout-api", "orders publish", end + 0.5, 1.8 + b.jitter(7), &[("messaging.system", "kafka"), ("messaging.destination", "orders"), ("peer.service", "events-kafka")]);
                cursor = b.end(publish) + 0.4;
            }
            b.set_end(call, cursor + 1.5);
            let end = b.end(call) + 2.0;
            b.set_end(root, end);
        }
        // The cart: one service, one cache read.
        _ => {
            let call = b.span(Some(root), "cart-api", "GET /api/cart/{id}", 1.2, 1.0, &[("http.method", "GET")]);
            let hget = b.span(Some(call), "cart-api", "redis HGETALL cart", 2.0, 0.8 + b.jitter(1), &[("db.system", "redis"), ("peer.service", "session-cache")]);
            let end = b.end(hget) + 4.0 + 6.0 * b.jitter(2);
            b.set_end(call, end);
            b.set_end(root, end + 1.5);
        }
    }

    Ok(b.finish(&trace_id(script.seed, slot, endpoint), started, total))
}

// ---------------------------------------------------------------- profile

/// A frame in the call tree, before it is laid out.
struct Node {
    name: &'static str,
    own: f64,
    children: Vec<Node>,
}

fn node(name: &'static str, own: f64, children: Vec<Node>) -> Node {
    Node { name, own, children }
}

fn total(n: &Node) -> f64 {
    n.own + n.children.iter().map(total).sum::<f64>()
}

/// Where checkout-api spends its CPU, weighted by how much of the window the
/// bad build was live in.
fn checkout_tree(bad: f64) -> Node {
    node("total", 0.0, vec![
        node("runtime.main", 0.0, vec![node("main.main", 0.0, vec![node("net/http.(*conn).serve", 2.0, vec![
            node("net/http.serverHandler.ServeHTTP", 1.0, vec![
                node("github.com/acme/checkout/api.(*Router).ServeHTTP", 1.5, vec![
                    node("github.com/acme/checkout/api.(*Handler).PlaceOrder", 2.0, vec![
                        node("github.com/acme/checkout/orders.(*Service).Place", 2.0, vec![
                            node("github.com/acme/checkout/orders.(*Repo).LoadItems", 1.0 + 6.0 * bad, vec![
                                node("github.com/jackc/pgx/v5.(*Conn).Query", 2.0 + 24.0 * bad, vec![
                                    node("github.com/jackc/pgx/v5/pgconn.(*PgConn).ReceiveMessage", 2.0 + 14.0 * bad, vec![
                                        node("net.(*conn).Read", 3.0 + 9.0 * bad, vec![]),
                                    ]),
                                    node("github.com/jackc/pgx/v5/pgproto3.(*Frontend).Send", 1.0 + 7.0 * bad, vec![]),
                                ]),
                                node("github.com/jackc/pgx/v5/pgxpool.(*Pool).Acquire", 0.5 + 9.0 * bad, vec![
                                    node("sync.(*Cond).Wait", 0.2 + 6.0 * bad, vec![]),
                                ]),
                            ]),
                            node("github.com/acme/checkout/orders.(*Repo).Insert", 3.0, vec![
                                node("github.com/jackc/pgx/v5.(*Conn).Exec", 4.0, vec![]),
                            ]),
                            node("github.com/acme/checkout/payments.(*Client).Authorize", 2.0, vec![
                                node("net/http.(*Client).Do", 5.0, vec![node("crypto/tls.(*Conn).Write", 3.0, vec![])]),
                            ]),
                            node("encoding/json.Marshal", 7.0, vec![]),
                        ]),
                    ]),
                    node("github.com/acme/checkout/api.(*Handler).GetOrder", 2.0, vec![
                        node("encoding/json.Marshal", 3.0, vec![]),
                    ]),
                    node("github.com/acme/checkout/api.(*Handler).Health", 1.0, vec![]),
                ]),
            ]),
        ])])]),
        node("runtime.gcBgMarkWorker", 1.0, vec![node("runtime.gcDrain", 3.0, vec![node("runtime.scanobject", 7.0 + 4.0 * bad, vec![])])]),
        node("runtime.mcall", 1.0, vec![node("runtime.schedule", 2.0, vec![node("runtime.findRunnable", 2.0, vec![])])]),
    ])
}

/// A flame graph of checkout-api over a window.
pub fn profile(script: &Script, query: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Profile> {
    if !query.trim().is_empty() && !query.contains("checkout-api") {
        return Err(AppError::Collector(
            "the demo profiles checkout-api only — try `process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"checkout-api\"}`"
                .to_string(),
        ));
    }
    let instances = script.instances(to);
    let window = (to - from).num_seconds().max(1);
    let steps = (window / 30).clamp(1, 2000);
    let bad: f64 = (0..steps)
        .map(|i| effects_at(&instances, from + Duration::seconds(i * window / steps)).n_plus_one)
        .sum::<f64>()
        / steps as f64;

    let tree = checkout_tree(bad);
    let samples = (window as f64 * 100.0 * 0.6).max(100.0);
    let scale = samples / total(&tree);

    let mut frames: Vec<Frame> = Vec::new();
    fn lay(n: &Node, depth: usize, offset: i64, parent: Option<usize>, scale: f64, frames: &mut Vec<Frame>) -> i64 {
        let index = frames.len();
        let width = (total(n) * scale).round() as i64;
        frames.push(Frame {
            name: n.name.to_string(),
            depth,
            offset,
            total: width,
            own: 0,
            parent,
        });
        let mut cursor = offset;
        for child in &n.children {
            cursor += lay(child, depth + 1, cursor, Some(index), scale, frames);
        }
        frames[index].own = (width - (cursor - offset)).max(0);
        width
    }
    lay(&tree, 0, 0, None, scale, &mut frames);

    let profile_total = frames.first().map(|f| f.total).unwrap_or(0);
    let mut by_name: HashMap<&str, (i64, usize)> = HashMap::new();
    for frame in frames.iter().skip(1) {
        let slot = by_name.entry(frame.name.as_str()).or_insert((0, 0));
        slot.0 += frame.own;
        slot.1 += 1;
    }
    let mut hot: Vec<HotSymbol> = by_name
        .into_iter()
        .filter(|(_, (own, _))| *own > 0)
        .map(|(name, (own, sites))| HotSymbol {
            name: name.to_string(),
            own,
            share: if profile_total > 0 { own as f64 / profile_total as f64 } else { 0.0 },
            sites,
        })
        .collect();
    hot.sort_by(|a, b| b.own.cmp(&a.own).then_with(|| a.name.cmp(&b.name)));
    hot.truncate(12);
    let max_self = frames.iter().map(|f| f.own).max().unwrap_or(0);

    Ok(Profile {
        frames,
        hot,
        total: profile_total,
        max_self,
        unit: "samples".to_string(),
        sample_rate: 100,
        name: "cpu".to_string(),
        spy: "gospy".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Script {
        Script {
            started_at: DateTime::parse_from_rfc3339("2026-10-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            seed: 11,
            triggers: Vec::new(),
        }
    }

    #[test]
    fn a_trace_id_rebuilds_the_same_trace() {
        let s = script();
        let m = s.moment(s.started_at + Duration::minutes(5));
        let first = recent(&m, 15, 12).into_iter().next().expect("a trace");
        let trace = trace(&s, &first.trace_id).expect("rebuilt");
        assert_eq!(trace.trace_id, first.trace_id);
        assert!((trace.duration_ms - first.duration_ms).abs() < 1.0, "{} vs {}", trace.duration_ms, first.duration_ms);
        assert_eq!(trace.orphan_count, 0);
    }

    #[test]
    fn an_unknown_id_is_refused() {
        assert!(trace(&script(), "00000000000000000000000000000000").is_err());
        assert!(trace(&script(), "not-a-trace").is_err());
    }

    #[test]
    fn the_bad_deploy_shows_as_a_wall_of_item_queries() {
        let s = script();
        let deploy = s.next(s.started_at).unwrap();
        let m = s.moment(deploy.at(280.0));
        let slow = recent(&m, 3, 40)
            .into_iter()
            .find(|t| t.root_name == "POST /checkout" && t.error_count == Some(0))
            .expect("a successful checkout during the incident");
        let trace = trace(&s, &slow.trace_id).unwrap();
        let services: Vec<&str> = trace.services.iter().map(|s| s.service.as_str()).collect();
        assert_eq!(services[0], "checkout-api", "{services:?}");
        assert!(trace.span_count > 20, "{}", trace.span_count);
    }

    #[test]
    fn the_profile_frames_nest() {
        let s = script();
        let p = profile(&s, "", s.started_at, s.started_at + Duration::minutes(30)).unwrap();
        assert!(!p.frames.is_empty());
        for f in &p.frames {
            if let Some(parent) = f.parent {
                let pf = &p.frames[parent];
                assert!(f.offset >= pf.offset && f.offset + f.total <= pf.offset + pf.total + 1, "{} in {}", f.name, pf.name);
            }
        }
        assert!(!p.hot.is_empty());
    }
}
