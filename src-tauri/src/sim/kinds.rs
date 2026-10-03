//! One snapshot per collector kind, shaped exactly as the real collector
//! shapes it.
//!
//! Same section ids, same columns, same metric keys and thresholds, same
//! health rules, same summary sentences. That is what makes the demo honest:
//! the alert engine, the change engine, the config rules, the forecaster and
//! the incident grouper all read these through the same code paths they read
//! a real estate through, so anything they say about the demo they would say
//! about the real thing. A builder here that drifts from its collector is a
//! demo that shows something the product cannot do.

use std::f64::consts::TAU;

use chrono::{DateTime, Duration, Local, Timelike, Utc};

use super::estate::Member;
use super::noise::{hash, jitter, key, smooth, unit, wander};
use super::story::{BuildResult, Moment, Story};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, Metric, MetricUnit, Row, RowAction, Section,
    ServiceKind,
};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

/// Who is asking, and when.
pub struct Ctx<'a> {
    pub m: &'a Moment,
    pub member: &'static Member,
    pub id: &'a str,
    pub name: &'a str,
}

impl Ctx<'_> {
    fn secs(&self) -> f64 {
        self.m.secs()
    }

    /// A gauge's slow drift, keyed by what it measures.
    fn wander(&self, what: &str) -> f64 {
        wander(self.m.seed, key(&format!("{}:{what}", self.member.key)), self.secs())
    }

    fn jitter(&self, what: &str) -> f64 {
        jitter(self.m.seed, key(&format!("{}:{what}", self.member.key)), self.secs())
    }

    /// How long the poll took, for `latency_ms`.
    fn took(&self, base: f64) -> u64 {
        (base * (1.0 + 0.4 * self.jitter("took").abs())) as u64
    }

    fn snapshot(
        &self,
        health: Health,
        summary: String,
        metrics: Vec<Metric>,
        sections: Vec<Section>,
        latency_ms: u64,
    ) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: self.id.to_string(),
            profile_name: self.name.to_string(),
            kind: self.member.kind,
            health,
            summary,
            metrics,
            sections,
            checked_at: self.m.t,
            latency_ms,
            error: None,
        }
    }

    /// Seconds into the story playing now, when it is the one asked about.
    fn tau_of(&self, story: Story) -> Option<f64> {
        self.m.active.filter(|i| i.story == story).map(|i| i.tau(self.m.t))
    }
}

/// Requests per second arriving at the edge: a day-shaped curve peaking in
/// the early afternoon wherever the demo is being shown, with some drift.
pub fn traffic(m: &Moment) -> f64 {
    traffic_at(m.seed, m.t)
}

pub fn traffic_at(seed: u64, t: DateTime<Utc>) -> f64 {
    let secs = t.timestamp_millis() as f64 / 1000.0;
    120.0 * diurnal(t) * (1.0 + 0.07 * wander(seed, key("traffic"), secs))
}

fn diurnal(t: DateTime<Utc>) -> f64 {
    let local = t.with_timezone(&Local);
    let hour = local.hour() as f64 + local.minute() as f64 / 60.0;
    0.62 + 0.38 * ((hour - 14.0) / 24.0 * TAU).cos()
}

/// A long-lived thing's start time, some hours or days before the demo.
fn born(m: &Moment, what: &str, min_hours: u64, spread_hours: u64) -> DateTime<Utc> {
    let hours = min_hours + hash(&[m.seed, key(what)]) % spread_hours.max(1);
    m.epoch - Duration::hours(hours as i64) - Duration::minutes((hash(&[key(what), 7]) % 60) as i64)
}

/// A repeating 0→1 ramp with the given period, offset per series — the
/// shape of something that fills and is periodically cleared.
fn ramp(m: &Moment, period: f64, at_start: f64) -> f64 {
    let since = (m.t - m.started_at).num_milliseconds() as f64 / 1000.0;
    (since / period + at_start).rem_euclid(1.0)
}

/// Docker's own rendering of an uptime: "Up 3 hours", "Up About a minute".
fn docker_human(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 1.0 {
        "Less than a second".to_string()
    } else if s < 60.0 {
        format!("{} seconds", s as u64)
    } else if s < 120.0 {
        "About a minute".to_string()
    } else if s < 3600.0 {
        format!("{} minutes", (s / 60.0) as u64)
    } else if s < 7200.0 {
        "About an hour".to_string()
    } else if s < 48.0 * 3600.0 {
        format!("{} hours", (s / 3600.0) as u64)
    } else {
        format!("{} days", (s / 86400.0) as u64)
    }
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn snapshot(cx: &Ctx) -> HealthSnapshot {
    match cx.member.kind {
        ServiceKind::HttpEndpoint => http_endpoint(cx),
        ServiceKind::Haproxy => haproxy(cx),
        ServiceKind::Docker => docker(cx),
        ServiceKind::Postgres => postgres(cx),
        ServiceKind::Redis => redis(cx),
        ServiceKind::Kafka => kafka(cx),
        ServiceKind::Rabbitmq => rabbitmq(cx),
        ServiceKind::Elasticsearch => elasticsearch(cx),
        ServiceKind::Kubernetes => kubernetes(cx),
        ServiceKind::Jenkins => jenkins(cx),
        ServiceKind::LinuxHost => linux_host(cx),
        ServiceKind::Gigapipe => gigapipe(cx),
        other => cx.snapshot(
            Health::Unknown,
            format!("{} is not part of the demo", other.label()),
            Vec::new(),
            Vec::new(),
            1,
        ),
    }
}

// ------------------------------------------------------------------ http

/// What one HTTP check sees at one moment: status, milliseconds.
pub fn http_probe(cx: &Ctx) -> (u16, u64) {
    let e = &cx.m.effects;
    match cx.member.key {
        "checkout" => {
            let latency = 46.0 + 14.0 * cx.wander("latency") + 1500.0 * e.n_plus_one
                + 25.0 * cx.jitter("latency");
            if e.checkout_503 {
                (503, (1800.0 + 900.0 * cx.jitter("503").abs()) as u64)
            } else {
                (200, latency.max(8.0) as u64)
            }
        }
        _ => {
            let latency = 182.0 + 55.0 * cx.wander("latency") + 1350.0 * e.search_slow
                + 260.0 * e.edge_queue
                + 30.0 * cx.jitter("latency");
            (200, latency.max(40.0) as u64)
        }
    }
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "200 OK",
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    }
}

fn http_endpoint(cx: &Ctx) -> HealthSnapshot {
    let url = cx.member.endpoint;
    let slow = 1000u64;
    let (status, latency) = http_probe(cx);
    let status_ok = status == 200;

    let health = if !status_ok {
        if status >= 500 { Health::Down } else { Health::Degraded }
    } else if latency > slow {
        Health::Degraded
    } else {
        Health::Healthy
    };
    let summary = if !status_ok {
        format!("{} (expected 200) in {latency}ms", status_text(status))
    } else if latency > slow {
        format!("{} in {latency}ms — slower than {slow}ms", status_text(status))
    } else {
        format!("{} in {latency}ms", status_text(status))
    };

    cx.snapshot(
        health,
        summary.clone(),
        vec![
            Metric::new(
                "up",
                "Up",
                if health == Health::Down { 0.0 } else { 1.0 },
                MetricUnit::Count,
            ),
            Metric::new("latency", "Response time", latency as f64, MetricUnit::Millis)
                .warn_above(slow as f64),
            Metric::new("status", "Status code", status as f64, MetricUnit::Count),
        ],
        vec![Section::new(
            "checks",
            "Check",
            vec![
                ColumnSpec::text("url", "URL").primary(),
                ColumnSpec::text("method", "Method"),
                ColumnSpec::metric("status", "Status", MetricUnit::Count),
                ColumnSpec::metric("latency", "Response time", MetricUnit::Millis),
                ColumnSpec::text("body", "Body check"),
            ],
        )
        .rows(vec![Row::new(url, url, health)
            .summary(summary)
            .cell("url", CellValue::text(url))
            .cell("method", CellValue::text("GET"))
            .cell("status", CellValue::number(status as f64))
            .cell("latency", CellValue::number(latency as f64))
            .cell("body", CellValue::Null)])
        .priority(10)],
        latency,
    )
}

// ---------------------------------------------------------------- docker

/// One container on the demo's Docker host.
pub struct Container {
    pub name: &'static str,
    pub service: &'static str,
    pub replica: usize,
    pub image: &'static str,
    /// Pinned version, for services the stories never redeploy.
    pub fixed: Option<&'static str>,
    pub mem_limit: f64,
    pub base_mem: f64,
    pub base_cpu: f64,
    /// Share of edge traffic it serves.
    pub share: f64,
    pub healthcheck: bool,
    pub restart_policy: &'static str,
}

pub const CONTAINERS: &[Container] = &[
    Container { name: "web-1", service: "web", replica: 0, image: "ghcr.io/acme/web", fixed: None, mem_limit: 512.0 * MIB, base_mem: 182.0 * MIB, base_cpu: 3.0, share: 0.23, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "web-2", service: "web", replica: 1, image: "ghcr.io/acme/web", fixed: None, mem_limit: 512.0 * MIB, base_mem: 176.0 * MIB, base_cpu: 3.0, share: 0.23, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "checkout-api-1", service: "checkout-api", replica: 0, image: "ghcr.io/acme/checkout-api", fixed: None, mem_limit: GIB, base_mem: 338.0 * MIB, base_cpu: 5.0, share: 0.04, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "checkout-api-2", service: "checkout-api", replica: 1, image: "ghcr.io/acme/checkout-api", fixed: None, mem_limit: GIB, base_mem: 351.0 * MIB, base_cpu: 5.0, share: 0.04, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "cart-api-1", service: "cart-api", replica: 0, image: "ghcr.io/acme/cart-api", fixed: Some("1.9.4"), mem_limit: 512.0 * MIB, base_mem: 148.0 * MIB, base_cpu: 2.0, share: 0.12, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "catalog-api-1", service: "catalog-api", replica: 0, image: "ghcr.io/acme/catalog-api", fixed: None, mem_limit: GIB, base_mem: 418.0 * MIB, base_cpu: 4.0, share: 0.15, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "catalog-api-2", service: "catalog-api", replica: 1, image: "ghcr.io/acme/catalog-api", fixed: None, mem_limit: GIB, base_mem: 405.0 * MIB, base_cpu: 4.0, share: 0.15, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "payments-api-1", service: "payments-api", replica: 0, image: "ghcr.io/acme/payments-api", fixed: Some("5.2.1"), mem_limit: 768.0 * MIB, base_mem: 262.0 * MIB, base_cpu: 2.0, share: 0.04, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "order-worker-1", service: "order-worker", replica: 0, image: "ghcr.io/acme/order-worker", fixed: Some("2.7.0"), mem_limit: GIB, base_mem: 377.0 * MIB, base_cpu: 6.0, share: 0.0, healthcheck: true, restart_policy: "UNLESS_STOPPED" },
    Container { name: "email-worker-1", service: "email-worker", replica: 0, image: "ghcr.io/acme/email-worker", fixed: Some("1.4.2"), mem_limit: 0.0, base_mem: 121.0 * MIB, base_cpu: 1.0, share: 0.0, healthcheck: false, restart_policy: "NO" },
    Container { name: "otel-collector", service: "otel-collector", replica: 0, image: "otel/opentelemetry-collector-contrib", fixed: Some("0.98.0"), mem_limit: 0.0, base_mem: 214.0 * MIB, base_cpu: 4.0, share: 0.0, healthcheck: false, restart_policy: "UNLESS_STOPPED" },
];

/// The host's memory, which is what Docker reports as the limit of a
/// container that has none.
const HOST_MEMORY: f64 = 32.0 * GIB;

/// A container's state at one moment, as Docker would describe it.
pub struct ContainerState {
    pub running: bool,
    pub health: Health,
    pub status: String,
    pub started: DateTime<Utc>,
    pub restarts: u32,
    pub image: String,
    pub cpu: f64,
    pub memory: f64,
    pub unhealthy: bool,
}

pub fn container_state(m: &Moment, c: &Container) -> ContainerState {
    let e = &m.effects;
    let birth = born(m, c.name, 20, 70);
    let (started, restarts, exited_at) = m.container_life(c.service, c.replica, birth);
    let version = match c.fixed {
        Some(v) => v.to_string(),
        None => m.version_at(c.service, started),
    };
    let image = format!("{}:{version}", c.image);

    let unhealthy = match c.service {
        "checkout-api" => e.checkout_unhealthy[c.replica.min(1)],
        "catalog-api" => c.replica == 0 && e.catalog_unhealthy,
        _ => false,
    };

    let secs = m.secs();
    let name_key = key(c.name);
    let load = traffic(m) * c.share;
    let mut cpu = c.base_cpu + load * 0.9 + 2.5 * wander(m.seed, name_key ^ 1, secs);
    let mut memory = c.base_mem * (1.0 + 0.06 * wander(m.seed, name_key ^ 2, secs));
    if c.service == "checkout-api" {
        cpu += 58.0 * e.n_plus_one;
        // The second replica is the one that grows into its limit and is
        // OOM-killed; both carry the extra connections.
        memory += e.n_plus_one * if c.replica == 1 { 640.0 * MIB } else { 220.0 * MIB };
    }
    if c.service == "catalog-api" {
        cpu += 9.0 * e.search_slow;
        memory += e.search_slow * 140.0 * MIB;
    }
    if c.service == "order-worker" {
        cpu += 0.25 * traffic(m);
    }
    let uptime = (m.t - started).num_milliseconds() as f64 / 1000.0;

    if let Some(at) = exited_at {
        let ago = (m.t - at).num_seconds().max(1);
        return ContainerState {
            running: false,
            health: Health::Down,
            status: format!("Exited (137) {ago} seconds ago"),
            started,
            restarts,
            image,
            cpu: 0.0,
            memory: 0.0,
            unhealthy: false,
        };
    }

    // Fresh containers start cold.
    if uptime < 25.0 {
        memory *= 0.55 + 0.45 * (uptime / 25.0);
    }
    let up = format!("Up {}", docker_human(uptime));
    let (health, status) = if !c.healthcheck {
        (Health::Healthy, up)
    } else if uptime < 20.0 {
        (Health::Unknown, format!("{up} (health: starting)"))
    } else if unhealthy {
        (Health::Degraded, format!("{up} (unhealthy)"))
    } else {
        (Health::Healthy, format!("{up} (healthy)"))
    };

    ContainerState {
        running: true,
        health,
        status,
        started,
        restarts,
        image,
        cpu: cpu.clamp(0.1, 100.0),
        memory: if c.mem_limit > 0.0 { memory.min(c.mem_limit * 0.98) } else { memory },
        unhealthy,
    }
}

/// A stable 64-hex id per container, as Docker prints one.
pub fn container_id(c: &Container) -> String {
    let a = hash(&[key(c.name), 1]);
    let b = hash(&[key(c.name), 2]);
    let d = hash(&[key(c.name), 3]);
    let f = hash(&[key(c.name), 4]);
    format!("{a:016x}{b:016x}{d:016x}{f:016x}")
}

fn docker(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let mut rows = Vec::new();
    let mut running = 0usize;
    let mut unhealthy = 0usize;
    let mut total_cpu = 0.0;
    let mut total_mem = 0.0;

    for c in CONTAINERS {
        let state = container_state(m, c);
        if state.running {
            running += 1;
            total_cpu += state.cpu;
            total_mem += state.memory;
        }
        if matches!(state.health, Health::Down | Health::Degraded) {
            unhealthy += 1;
        }

        let uptime = (m.t - state.started).num_milliseconds() as f64 / 1000.0;
        let rate = 1200.0 + traffic(m) * c.share * 2400.0;
        let mut row = Row::new(container_id(c), c.name, state.health)
            .summary(state.status.clone())
            .cell("name", CellValue::text(c.name))
            .cell("state", CellValue::text(&state.status))
            .cell("kind", CellValue::text("—"))
            .cell("image", CellValue::text(&state.image))
            .cell("project", CellValue::text("acme-shop"))
            .extra(serde_json::json!({ "hostPorts": [] }));

        row = if state.running {
            row.cell("cpu", CellValue::gauge_warn((state.cpu * 100.0).round() / 100.0, Some(100.0), 85.0))
                .cell(
                    "memory",
                    CellValue::gauge(
                        state.memory,
                        Some(if c.mem_limit > 0.0 { c.mem_limit } else { HOST_MEMORY }),
                    ),
                )
                .cell("net_rx", CellValue::number((rate * uptime).round()))
                .cell("net_tx", CellValue::number((rate * 2.6 * uptime).round()))
                .action(RowAction::new(
                    "restart_container",
                    "Restart",
                    serde_json::json!({ "container": c.name }),
                ))
                .action(
                    RowAction::new("stop_container", "Stop", serde_json::json!({ "container": c.name }))
                        .destructive(),
                )
        } else {
            row.cell("cpu", CellValue::Null)
                .cell("memory", CellValue::Null)
                .cell("net_rx", CellValue::Null)
                .cell("net_tx", CellValue::Null)
                .action(RowAction::new(
                    "start_container",
                    "Start",
                    serde_json::json!({ "container": c.name }),
                ))
        };
        rows.push(row);
    }

    let health = rows
        .iter()
        .map(|r| r.health)
        .filter(|h| *h != Health::Unknown)
        .max()
        .unwrap_or(Health::Healthy);

    let section = Section::new(
        "containers",
        "Containers",
        vec![
            ColumnSpec::text("name", "Container").primary(),
            ColumnSpec::text("state", "State"),
            ColumnSpec::text("kind", "Type"),
            ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
            ColumnSpec::gauge("memory", "Memory", MetricUnit::Bytes),
            ColumnSpec::text("image", "Image").secondary().revision(),
            ColumnSpec::text("project", "Project").secondary(),
            ColumnSpec::metric("net_rx", "Net RX", MetricUnit::Bytes).secondary(),
            ColumnSpec::metric("net_tx", "Net TX", MetricUnit::Bytes).secondary(),
        ],
    )
    .rows(rows)
    .priority(10)
    .empty_text("This engine has no containers.")
    .tracked()
    .sorted_by_health();

    cx.snapshot(
        health,
        format!(
            "Engine 27.1.1 — {running}/{} containers running{}",
            CONTAINERS.len(),
            if unhealthy > 0 {
                format!(", {unhealthy} unhealthy")
            } else {
                String::new()
            }
        ),
        vec![
            Metric::new("containers", "Containers", CONTAINERS.len() as f64, MetricUnit::Count),
            Metric::new("running", "Running", running as f64, MetricUnit::Count),
            Metric::new("unhealthy", "Unhealthy", unhealthy as f64, MetricUnit::Count).warn_above(0.0),
            Metric::new(
                "cpu_total",
                "Total CPU",
                (total_cpu * 100.0).round() / 100.0,
                MetricUnit::Percent,
            ),
            Metric::new("mem_total", "Total memory", total_mem, MetricUnit::Bytes),
        ],
        vec![section],
        cx.took(9.0),
    )
}

// --------------------------------------------------------------- haproxy

struct Server {
    backend: &'static str,
    server: &'static str,
}

const SERVERS: &[Server] = &[
    Server { backend: "web", server: "web-1" },
    Server { backend: "web", server: "web-2" },
    Server { backend: "catalog", server: "catalog-api-1" },
    Server { backend: "catalog", server: "catalog-api-2" },
    Server { backend: "checkout", server: "checkout-api-1" },
    Server { backend: "checkout", server: "checkout-api-2" },
    Server { backend: "cart", server: "cart-api-1" },
    Server { backend: "payments", server: "payments-api-1" },
];

const BACKENDS: &[(&str, f64, f64)] = &[
    // name, share of traffic, typical seconds per request
    ("web", 0.46, 0.045),
    ("catalog", 0.30, 0.070),
    ("checkout", 0.08, 0.160),
    ("cart", 0.12, 0.035),
    ("payments", 0.04, 0.230),
];

fn server_down(m: &Moment, server: &str) -> bool {
    let e = &m.effects;
    match server {
        "checkout-api-1" => e.checkout_lb_down[0],
        "checkout-api-2" => e.checkout_lb_down[1],
        "catalog-api-1" => e.catalog_lb_down,
        _ => false,
    }
}

/// Errors a backend has answered with since the balancer started: a trickle
/// always, and a flood for as long as each story had it failing.
fn backend_errors(m: &Moment, backend: &str) -> f64 {
    let since = (m.t - m.epoch).num_seconds().max(0) as f64 + 6.0 * 86400.0;
    let mut errors = since * match backend {
        "checkout" => 0.0011,
        "payments" => 0.0006,
        _ => 0.0003,
    };
    for instance in &m.instances {
        let tau = instance.tau(m.t).max(0.0);
        errors += match (instance.story, backend) {
            (Story::BadDeploy, "checkout") => (tau.min(345.0) - 150.0).max(0.0) * 5.5,
            (Story::SearchHeap, "catalog") => (tau.min(255.0) - 180.0).max(0.0) * 1.8,
            _ => 0.0,
        };
    }
    errors.floor()
}

fn haproxy(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let rps = traffic(m);

    let mut backend_rows = Vec::new();
    let mut dead: Vec<&str> = Vec::new();
    let mut last_server: Vec<&str> = Vec::new();
    let mut queued_total = 0.0;
    let mut servers_down = 0usize;

    for (backend, share, per_request) in BACKENDS {
        let servers: Vec<&Server> = SERVERS.iter().filter(|s| s.backend == *backend).collect();
        let down = servers.iter().filter(|s| server_down(m, s.server)).count();
        let up = servers.len() - down;
        servers_down += down;

        let mut latency = *per_request;
        let mut queued = 0.0;
        if *backend == "checkout" {
            latency += 2.4 * e.n_plus_one;
            queued = (e.edge_queue * 34.0 + 3.0 * cx.jitter("queue").abs() * e.edge_queue).round();
        }
        if *backend == "catalog" {
            latency += 1.9 * e.search_slow;
        }
        queued_total += queued;
        let sessions = (rps * share * latency * (1.0 + 0.1 * cx.wander(backend))).max(0.0).round();

        let health = if up == 0 {
            dead.push(backend);
            Health::Down
        } else if queued >= 1.0 || (up == 1 && servers.len() > 1) || down > 0 {
            if up == 1 && servers.len() > 1 {
                last_server.push(backend);
            }
            Health::Degraded
        } else {
            Health::Healthy
        };

        backend_rows.push(
            Row::new(format!("backend:{backend}"), *backend, health)
                .summary(format!("{up} of {} servers up", servers.len()))
                .cell("backend", CellValue::text(*backend))
                .cell("state", CellValue::status(health))
                .cell("servers", CellValue::text(format!("{up}/{}", servers.len())))
                .cell("sessions", CellValue::number(if up == 0 { 0.0 } else { sessions }))
                .cell("queued", CellValue::number(queued))
                .cell("errors", CellValue::number(backend_errors(m, backend))),
        );
    }

    let server_rows: Vec<Row> = SERVERS
        .iter()
        .map(|s| {
            let down = server_down(m, s.server);
            let health = if down { Health::Down } else { Health::Healthy };
            let since = if down {
                m.active.map(|i| i.tau(m.t) - if s.server.starts_with("catalog") { 180.0 } else { 190.0 }).unwrap_or(0.0)
            } else {
                let c = CONTAINERS.iter().find(|c| c.name == s.server);
                c.map(|c| {
                    let state = container_state(m, c);
                    (m.t - state.started).num_seconds() as f64
                })
                .unwrap_or(86400.0)
            };
            let check = if down { "L7STS/503 in 2003ms" } else { "L7OK/200 in 3ms" };
            let status = if down { "DOWN" } else { "UP" };
            Row::new(
                format!("server:{}:{}", s.backend, s.server),
                format!("{}/{}", s.backend, s.server),
                health,
            )
            .summary(format!("{status} · last check {check}"))
            .cell("server", CellValue::text(s.server))
            .cell("backend", CellValue::text(s.backend))
            .cell("state", CellValue::status(health))
            .cell(
                "sessions",
                CellValue::number(if down { 0.0 } else { (3.0 + 4.0 * cx.wander(s.server).abs()).round() }),
            )
            .cell("for", CellValue::duration(since.max(1.0)))
        })
        .collect();

    let https_sessions = (rps * 0.35 * (1.0 + 0.2 * e.edge_queue + 0.1 * cx.wander("fe"))).round();
    let frontend_rows = vec![
        Row::new("frontend:https-in", "https-in", Health::Healthy)
            .cell("frontend", CellValue::text("https-in"))
            .cell("state", CellValue::status(Health::Healthy))
            .cell("sessions", CellValue::gauge(https_sessions, Some(20000.0))),
        Row::new("frontend:http-in", "http-in", Health::Healthy)
            .cell("frontend", CellValue::text("http-in"))
            .cell("state", CellValue::status(Health::Healthy))
            .cell("sessions", CellValue::gauge((rps * 0.02).round(), Some(20000.0))),
    ];
    let sessions = https_sessions + (rps * 0.02).round();

    let health = if !dead.is_empty() {
        Health::Down
    } else if !last_server.is_empty() || queued_total >= 1.0 {
        Health::Degraded
    } else {
        Health::Healthy
    };
    let summary = if !dead.is_empty() {
        format!(
            "{} with no servers up: {}",
            if dead.len() == 1 { "backend" } else { "backends" },
            dead.join(", ")
        )
    } else if queued_total >= 1.0 {
        format!("{queued_total:.0} requests queued waiting for a free server")
    } else if !last_server.is_empty() {
        format!("{} running on a single server", last_server.join(", "))
    } else {
        format!("2 frontends · {} backends · {sessions:.0} sessions", BACKENDS.len())
    };

    cx.snapshot(
        health,
        summary,
        vec![
            Metric::new("sessions", "Sessions", sessions, MetricUnit::Count),
            Metric::new("queued", "Queued requests", queued_total, MetricUnit::Count).warn_above(1.0),
            Metric::new("frontends", "Frontends", 2.0, MetricUnit::Count),
            Metric::new("backends", "Backends", BACKENDS.len() as f64, MetricUnit::Count),
            Metric::new("servers_down", "Servers down", servers_down as f64, MetricUnit::Count)
                .warn_above(0.0),
        ],
        vec![
            Section::new(
                "backends",
                "Backends",
                vec![
                    ColumnSpec::text("backend", "Backend").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::text("servers", "Servers up"),
                    ColumnSpec::metric("sessions", "Sessions", MetricUnit::Count),
                    ColumnSpec::metric("queued", "Queued", MetricUnit::Count),
                    ColumnSpec::metric("errors", "Errors", MetricUnit::Count),
                ],
            )
            .rows(backend_rows)
            .tracked()
            .empty_text("No backends are configured.")
            .priority(10),
            Section::new(
                "servers",
                "Servers",
                vec![
                    ColumnSpec::text("server", "Server").primary(),
                    ColumnSpec::text("backend", "Backend"),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("sessions", "Sessions", MetricUnit::Count),
                    ColumnSpec::duration("for", "In state for"),
                ],
            )
            .rows(server_rows)
            .tracked()
            .empty_text("No servers are configured behind any backend.")
            .priority(20),
            Section::new(
                "frontends",
                "Frontends",
                vec![
                    ColumnSpec::text("frontend", "Frontend").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::gauge("sessions", "Sessions", MetricUnit::Count),
                ],
            )
            .rows(frontend_rows)
            .tracked()
            .priority(30),
        ],
        cx.took(4.0),
    )
}

// -------------------------------------------------------------- postgres

pub const MAX_CONNECTIONS: f64 = 200.0;

/// The statements stuck behind the N+1, as `pg_stat_activity` shows them.
pub fn long_queries(cx: &Ctx) -> Vec<(String, f64, String, String)> {
    let e = &cx.m.effects;
    let Some(tau) = cx.tau_of(Story::BadDeploy) else {
        return Vec::new();
    };
    if e.n_plus_one < 0.55 || tau < 145.0 || tau > 372.0 {
        return Vec::new();
    }
    let count = 1 + (e.db_saturation * 5.0).floor() as usize;
    (0..count)
        .map(|i| {
            let pid = 48_210 + (hash(&[cx.m.seed, i as u64, 0xDB]) % 900) as i64;
            let seconds = (6.0 + (tau - 145.0) * 0.18 + i as f64 * 4.5).min(57.0);
            let (state, query) = if i == count - 1 && count > 2 {
                (
                    "active".to_string(),
                    "UPDATE orders SET status = $1, updated_at = now() WHERE id = $2".to_string(),
                )
            } else {
                (
                    "active".to_string(),
                    "SELECT oi.id, oi.sku, oi.quantity, oi.unit_price FROM order_items oi WHERE oi.order_id = $1"
                        .to_string(),
                )
            };
            (pid.to_string(), seconds, state, query)
        })
        .collect()
}

pub fn connections(cx: &Ctx) -> f64 {
    let e = &cx.m.effects;
    (36.0 + traffic(cx.m) * 0.18 + 6.0 * cx.wander("conns") + 158.0 * e.db_saturation)
        .round()
        .clamp(5.0, MAX_CONNECTIONS - 3.0)
}

fn postgres(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let conns = connections(cx);
    let pct = conns / MAX_CONNECTIONS * 100.0;
    let hours = (m.t - m.epoch).num_seconds() as f64 / 3600.0;
    let size = 52.8e9 + hours * 31.0e6;
    let blocked = if e.db_saturation > 0.6 { (1.0 + e.db_saturation * 3.0).floor() } else { 0.0 };
    let long = long_queries(cx);

    let health = if pct > 90.0 || blocked > 0.0 || !long.is_empty() {
        Health::Degraded
    } else {
        Health::Healthy
    };

    let query_rows: Vec<Row> = long
        .iter()
        .map(|(pid, seconds, state, query)| {
            let health = if *seconds > 60.0 { Health::Down } else { Health::Degraded };
            Row::new(format!("query:{pid}"), format!("pid {pid}"), health)
                .summary(format!("running {seconds:.0}s: {}", truncate(query, 120)))
                .cell("pid", CellValue::text(pid))
                .cell("duration", CellValue::duration(*seconds))
                .cell("state", CellValue::text(state))
                .cell("query", CellValue::text(query))
                .action(
                    RowAction::new(
                        "terminate_database_query",
                        "Terminate",
                        serde_json::json!({ "pid": pid }),
                    )
                    .destructive(),
                )
        })
        .collect();

    let mut sections = Vec::new();
    if !query_rows.is_empty() {
        sections.push(
            Section::new(
                "active_queries",
                "Active queries",
                vec![
                    ColumnSpec::text("pid", "PID").primary(),
                    ColumnSpec::duration("duration", "Running for"),
                    ColumnSpec::text("state", "State"),
                    ColumnSpec::code("query", "Statement"),
                ],
            )
            .rows(query_rows)
            .priority(5)
            .empty_text("Nothing has been running longer than 5s.")
            .sorted_by_health(),
        );
    }
    let databases = [("orders", size * 0.885), ("inventory", size * 0.115), ("postgres", 7.6e6)];
    sections.push(
        Section::new(
            "databases",
            "Databases",
            vec![
                ColumnSpec::text("database", "Database").primary(),
                ColumnSpec::metric("size", "Size", MetricUnit::Bytes),
            ],
        )
        .rows(
            databases
                .iter()
                .map(|(name, bytes)| {
                    Row::new(format!("db:{name}"), *name, Health::Healthy)
                        .summary(human_bytes(*bytes))
                        .cell("database", CellValue::text(*name))
                        .cell("size", CellValue::number(bytes.round()))
                })
                .collect(),
        )
        .priority(20),
    );

    cx.snapshot(
        health,
        format!(
            "PostgreSQL 16.4 — {conns:.0}/{MAX_CONNECTIONS:.0} connections, {} long-running queries",
            long.len()
        ),
        vec![
            Metric::new("connections", "Connections", conns, MetricUnit::Count),
            Metric::new("connection_pct", "Connection use", pct, MetricUnit::Percent).warn_above(80.0),
            Metric::new("max_connections", "Max connections", MAX_CONNECTIONS, MetricUnit::Count),
            Metric::new("db_size", "Database size", size.round(), MetricUnit::Bytes),
            Metric::new("long_queries", "Long queries", long.len() as f64, MetricUnit::Count)
                .warn_above(0.0),
            Metric::new("blocked_locks", "Blocked locks", blocked, MetricUnit::Count).warn_above(0.0),
            Metric::new(
                "cache_hit_ratio",
                "Cache hit ratio",
                99.45 - 1.7 * e.db_saturation + 0.08 * cx.wander("hit"),
                MetricUnit::Percent,
            ),
            Metric::new("deadlocks", "Deadlocks", 0.0, MetricUnit::Count).warn_above(0.0),
            Metric::new("replication_lag", "Replication lag", 0.0, MetricUnit::Seconds)
                .warn_above(30.0),
        ],
        sections,
        cx.took(6.0 + 30.0 * e.db_saturation),
    )
}

fn truncate(text: &str, max: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        format!("{}…", collapsed.chars().take(max).collect::<String>())
    }
}

// ----------------------------------------------------------------- redis

pub const REDIS_MAXMEMORY: f64 = 2.0 * GIB;

/// Sessions written without a TTL pile up until a deploy that sets one
/// clears them: a ten-hour climb, forecastable long before it matters.
pub fn redis_memory(cx: &Ctx) -> f64 {
    let fill = ramp(cx.m, 10.0 * 3600.0, 0.22);
    1.07e9 + 0.86e9 * fill + 9.0e6 * cx.wander("mem")
}

fn redis(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let used = redis_memory(cx);
    let percent = used / REDIS_MAXMEMORY * 100.0;
    let warn = 85.0;
    let rps = traffic(m);
    let clients = (118.0 + rps * 0.55 + 6.0 * cx.wander("clients")).round();
    let ops = (rps * 7.4 * (1.0 + 0.05 * cx.wander("ops"))).round();
    let fragmentation = 1.08 + 0.03 * cx.wander("frag");
    let memory_critical = percent >= warn;
    let health = if memory_critical { Health::Degraded } else { Health::Healthy };
    let summary = if memory_critical {
        format!("memory at {percent:.0}% of maxmemory")
    } else {
        format!("master · {} clients · {ops:.0} ops/sec", clients as u64)
    };
    let uptime = (m.t - born(m, "redis", 400, 80)).num_seconds() as f64;
    let keys = (used / 712.0).round();
    let expires = (keys * 0.042).round();

    cx.snapshot(
        health,
        summary.clone(),
        vec![
            Metric::new("used_memory", "Memory used", used.round(), MetricUnit::Bytes),
            Metric::new("clients", "Connected clients", clients, MetricUnit::Count),
            Metric::new("blocked", "Blocked clients", 0.0, MetricUnit::Count),
            Metric::new("ops", "Operations/sec", ops, MetricUnit::PerSecond),
            Metric::new("evicted", "Evicted keys", 0.0, MetricUnit::Count),
            Metric::new("fragmentation", "Memory fragmentation", fragmentation, MetricUnit::Count)
                .warn_above(1.5),
            Metric::new("memory_percent", "Memory used", percent, MetricUnit::Percent).warn_above(warn),
            Metric::new("hit_rate", "Cache hit rate", 93.6 + 0.9 * cx.wander("hits"), MetricUnit::Percent),
        ],
        vec![
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
            .rows(vec![Row::new("server", cx.member.endpoint, health)
                .summary(&summary)
                .cell("role", CellValue::text("master"))
                .cell("version", CellValue::text("7.2.5"))
                .cell("uptime", CellValue::duration(uptime))
                .cell("clients", CellValue::number(clients))
                .cell("blocked", CellValue::number(0.0))
                .cell("ops", CellValue::number(ops))])
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
            .rows(vec![Row::new(
                "memory",
                "Memory",
                if memory_critical { Health::Degraded } else { Health::Healthy },
            )
            .summary(format!("{percent:.0}% of maxmemory"))
            .cell("used", CellValue::number(used.round()))
            .cell("limit", CellValue::number(REDIS_MAXMEMORY))
            .cell("pressure", CellValue::gauge_warn(percent, Some(100.0), warn))
            .cell("fragmentation", CellValue::number(fragmentation))
            .cell("evicted", CellValue::number(0.0))])
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
            .rows(vec![Row::new("keyspace:db0", "db0", Health::Healthy)
                .summary(format!("{keys:.0} keys"))
                .cell("db", CellValue::text("db0"))
                .cell("keys", CellValue::number(keys))
                .cell("expires", CellValue::number(expires))])
            .priority(20)
            .tracked()
            .empty_text("No keys stored."),
        ],
        cx.took(2.0),
    )
}

// ----------------------------------------------------------------- kafka

const TOPICS: &[(&str, u32, f64)] = &[
    // name, partitions, messages per edge request
    ("orders", 12, 0.08),
    ("payments", 6, 0.05),
    ("inventory-updates", 6, 0.21),
    ("clickstream", 24, 1.9),
    ("emails", 3, 0.06),
    ("analytics-events", 12, 0.7),
];

/// The analytics consumer's lag: modest while it keeps up, tens of
/// thousands while it is crash-looping.
pub fn ingest_lag(cx: &Ctx) -> f64 {
    let e = &cx.m.effects;
    (240.0 + 160.0 * cx.wander("lag-ingest").abs() + 61_000.0 * e.ingest_lag).round()
}

// The health chain mirrors the real collector's, arm for arm, including the
// arms that land on the same answer.
#[allow(clippy::if_same_then_else)]
fn kafka(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let rps = traffic(m);
    let lag_warn = 10_000.0;
    let alive = (m.t - m.epoch).num_seconds() as f64 + 31.0 * 86400.0;

    let reindex = cx.tau_of(Story::SearchHeap).map(|tau| super::noise::envelope(tau, 10.0, 60.0, 290.0, 60.0)).unwrap_or(0.0);
    let groups: Vec<(&str, &str, f64, f64)> = vec![
        ("order-workers", "Stable", 2.0, (12.0 + 40.0 * cx.wander("lag-orders").abs()).round()),
        (
            "analytics-ingest",
            if e.ingest_members == 0 {
                "Empty"
            } else if e.ingest_crashloop > 0.0 && e.ingest_members < 3 {
                "PreparingRebalance"
            } else {
                "Stable"
            },
            e.ingest_members as f64,
            ingest_lag(cx),
        ),
        ("email-dispatch", "Stable", 1.0, (3.0 * cx.wander("lag-email").abs()).round()),
        (
            "search-indexer",
            "Stable",
            2.0,
            (35.0 + 60.0 * cx.wander("lag-search").abs() + 3800.0 * reindex).round(),
        ),
    ];

    let mut group_rows: Vec<Row> = groups
        .iter()
        .map(|(name, state, members, lag)| {
            let health = if *lag > 0.0 && *members == 0.0 {
                Health::Down
            } else if *lag > lag_warn {
                Health::Degraded
            } else if *state == "PreparingRebalance" || *state == "CompletingRebalance" {
                Health::Degraded
            } else {
                Health::Healthy
            };
            Row::new(format!("group:{name}"), *name, health)
                .summary(if *members == 0.0 && *lag > 0.0 {
                    format!("{lag:.0} behind with no members")
                } else {
                    format!("{lag:.0} behind · {members:.0} members")
                })
                .cell("group", CellValue::text(*name))
                .cell("state", CellValue::text(*state))
                .cell("members", CellValue::number(*members))
                .cell("lag", CellValue::number(*lag))
        })
        .collect();
    group_rows.sort_by_key(|r| std::cmp::Reverse(r.health));

    let topic_rows: Vec<Row> = TOPICS
        .iter()
        .map(|(name, partitions, per_request)| {
            let messages = (alive * rps * per_request * 0.9).round();
            Row::new(format!("topic:{name}"), *name, Health::Healthy)
                .summary(format!("{partitions} partitions · {messages:.0} messages"))
                .cell("topic", CellValue::text(*name))
                .cell("partitions", CellValue::number(*partitions as f64))
                .cell("offline", CellValue::number(0.0))
                .cell("under_replicated", CellValue::number(0.0))
                .cell("messages", CellValue::number(messages))
        })
        .collect();

    let broker_rows: Vec<Row> = (1..=3)
        .map(|id| {
            Row::new(format!("broker:{id}"), format!("broker {id}"), Health::Healthy)
                .summary(format!("kafka-{id}.acme.internal:9092"))
                .cell("id", CellValue::number(id as f64))
                .cell("host", CellValue::text(format!("kafka-{id}.acme.internal")))
                .cell("port", CellValue::number(9092.0))
        })
        .collect();

    let stalled = groups.iter().filter(|g| g.2 == 0.0 && g.3 > 0.0).count();
    let total_lag: f64 = groups.iter().map(|g| g.3).sum();
    let partitions: u32 = TOPICS.iter().map(|t| t.1).sum();
    let health = if stalled > 0 {
        Health::Down
    } else if total_lag > lag_warn {
        Health::Degraded
    } else {
        Health::Healthy
    };
    let summary = if stalled > 0 {
        format!(
            "{stalled} consumer group{} behind with no members",
            if stalled == 1 { "" } else { "s" }
        )
    } else if total_lag > lag_warn {
        format!("{total_lag:.0} messages of consumer lag")
    } else {
        format!("3 brokers · {} topics · {} groups", TOPICS.len(), groups.len())
    };

    cx.snapshot(
        health,
        summary,
        vec![
            Metric::new("brokers", "Brokers", 3.0, MetricUnit::Count),
            Metric::new("topics", "Topics", TOPICS.len() as f64, MetricUnit::Count),
            Metric::new("partitions", "Partitions", partitions as f64, MetricUnit::Count),
            Metric::new("groups", "Consumer groups", groups.len() as f64, MetricUnit::Count),
            Metric::new("lag", "Total consumer lag", total_lag, MetricUnit::Count).warn_above(lag_warn),
            Metric::new("offline_partitions", "Offline partitions", 0.0, MetricUnit::Count)
                .warn_above(0.0),
            Metric::new(
                "under_replicated",
                "Under-replicated partitions",
                0.0,
                MetricUnit::Count,
            )
            .warn_above(0.0),
        ],
        vec![
            Section::new(
                "groups",
                "Consumer groups",
                vec![
                    ColumnSpec::text("group", "Group").primary(),
                    ColumnSpec::text("state", "State"),
                    ColumnSpec::metric("members", "Members", MetricUnit::Count),
                    ColumnSpec::metric("lag", "Lag", MetricUnit::Count),
                ],
            )
            .rows(group_rows)
            .priority(5)
            .tracked()
            .empty_text("No consumer groups."),
            Section::new(
                "topics",
                "Topics",
                vec![
                    ColumnSpec::text("topic", "Topic").primary(),
                    ColumnSpec::metric("partitions", "Partitions", MetricUnit::Count),
                    ColumnSpec::metric("offline", "Offline", MetricUnit::Count),
                    ColumnSpec::metric("under_replicated", "Under-replicated", MetricUnit::Count),
                    ColumnSpec::metric("messages", "Messages", MetricUnit::Count),
                ],
            )
            .rows(topic_rows)
            .priority(10)
            .empty_text("No topics yet."),
            Section::new(
                "brokers",
                "Brokers",
                vec![
                    ColumnSpec::text("host", "Host").primary(),
                    ColumnSpec::metric("id", "ID", MetricUnit::Count),
                    ColumnSpec::metric("port", "Port", MetricUnit::Count),
                ],
            )
            .rows(broker_rows)
            .priority(15)
            .tracked()
            .empty_text("No brokers reachable."),
        ],
        cx.took(14.0),
    )
}

// -------------------------------------------------------------- rabbitmq

fn rabbitmq(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let depth_warn = 1000.0;
    let rps = traffic(m);

    // name, consumers, ready, unacked
    let queues: Vec<(&str, f64, f64, f64)> = vec![
        ("email.send", 2.0, (4.0 + 9.0 * cx.wander("email").abs()).round(), 2.0),
        ("webhooks.deliver", 4.0, (18.0 + rps * 0.3 * (1.0 + cx.wander("hooks"))).round(), 6.0),
        (
            "webhooks.retry",
            1.0,
            (240.0 + 140.0 * cx.wander("retry") + 1450.0 * e.checkout_errors).round(),
            1.0,
        ),
        ("invoices.generate", 1.0, (2.0 * cx.wander("inv").abs()).round(), 0.0),
        ("exports.csv", 1.0, 0.0, 0.0),
    ];

    let queue_rows: Vec<Row> = queues
        .iter()
        .map(|(name, consumers, ready, unacked)| {
            let health = if *ready > 0.0 && *consumers == 0.0 {
                Health::Down
            } else if *ready >= depth_warn {
                Health::Degraded
            } else {
                Health::Healthy
            };
            Row::new(format!("queue:/{name}"), *name, health)
                .summary(format!(
                    "{ready:.0} ready · {consumers:.0} consumer{}",
                    if *consumers == 1.0 { "" } else { "s" }
                ))
                .cell("name", CellValue::text(*name))
                .cell("vhost", CellValue::text("/"))
                .cell("ready", CellValue::number(*ready))
                .cell("unacked", CellValue::number(*unacked))
                .cell("consumers", CellValue::number(*consumers))
                .cell("state", CellValue::text("running"))
        })
        .collect();
    let mut queue_rows = queue_rows;
    queue_rows.sort_by_key(|r| std::cmp::Reverse(r.health));

    let node_rows: Vec<Row> = ["rabbit@mq-1", "rabbit@mq-2"]
        .iter()
        .map(|name| {
            let mem = 24.0 + 6.0 * cx.wander(name);
            Row::new(format!("node:{name}"), *name, Health::Healthy)
                .summary(format!("running · memory {mem:.0}%"))
                .cell("name", CellValue::text(*name))
                .cell("state", CellValue::text("running"))
                .cell("memory", CellValue::gauge_warn(mem, Some(100.0), 80.0))
                .cell("disk_free", CellValue::number(41.6e9))
                .cell("fds", CellValue::text("212/65536"))
                .cell("alarms", CellValue::tags(Vec::new()))
        })
        .collect();

    let ready: f64 = queues.iter().map(|q| q.2).sum();
    let unacked: f64 = queues.iter().map(|q| q.3).sum();
    let consumers: f64 = queues.iter().map(|q| q.1).sum();
    let deep = queues.iter().filter(|q| q.2 >= depth_warn).count();
    let health = if deep > 0 { Health::Degraded } else { Health::Healthy };
    let summary = if deep > 0 {
        format!("{deep} queue{} over the depth threshold", if deep == 1 { "" } else { "s" })
    } else {
        format!("{} queues · {:.0} messages · {consumers:.0} consumers", queues.len(), ready + unacked)
    };

    cx.snapshot(
        health,
        summary,
        vec![
            Metric::new("messages", "Messages", ready + unacked, MetricUnit::Count),
            Metric::new("messages_ready", "Ready", ready, MetricUnit::Count),
            Metric::new("messages_unacked", "Unacknowledged", unacked, MetricUnit::Count),
            Metric::new("queues", "Queues", queues.len() as f64, MetricUnit::Count),
            Metric::new("consumers", "Consumers", consumers, MetricUnit::Count),
            Metric::new("connections", "Connections", 18.0, MetricUnit::Count),
            Metric::new("channels", "Channels", 42.0, MetricUnit::Count),
            Metric::new("queues_stuck", "Queues without consumers", 0.0, MetricUnit::Count)
                .warn_above(0.0),
        ],
        vec![
            Section::new(
                "queues",
                "Queues",
                vec![
                    ColumnSpec::text("name", "Queue").primary(),
                    ColumnSpec::text("vhost", "Vhost"),
                    ColumnSpec::metric("ready", "Ready", MetricUnit::Count),
                    ColumnSpec::metric("unacked", "Unacked", MetricUnit::Count),
                    ColumnSpec::metric("consumers", "Consumers", MetricUnit::Count),
                    ColumnSpec::text("state", "State"),
                ],
            )
            .rows(queue_rows)
            .priority(5)
            .tracked()
            .empty_text("No queues declared."),
            Section::new(
                "nodes",
                "Nodes",
                vec![
                    ColumnSpec::text("name", "Node").primary(),
                    ColumnSpec::text("state", "State"),
                    ColumnSpec::gauge("memory", "Memory", MetricUnit::Percent),
                    ColumnSpec::metric("disk_free", "Disk free", MetricUnit::Bytes),
                    ColumnSpec::text("fds", "File descriptors"),
                    ColumnSpec::tags("alarms", "Alarms"),
                ],
            )
            .rows(node_rows)
            .priority(10)
            .sorted_by_health()
            .tracked()
            .empty_text("No nodes reported."),
            Section::new(
                "broker",
                "Broker",
                vec![
                    ColumnSpec::text("cluster", "Cluster").primary(),
                    ColumnSpec::text("version", "Version"),
                    ColumnSpec::metric("connections", "Connections", MetricUnit::Count),
                    ColumnSpec::metric("channels", "Channels", MetricUnit::Count),
                    ColumnSpec::metric("consumers", "Consumers", MetricUnit::Count),
                ],
            )
            .rows(vec![Row::new("broker", "rabbit@mq-1", Health::Healthy)
                .summary("RabbitMQ 3.13.2")
                .cell("cluster", CellValue::text("rabbit@mq-1"))
                .cell("version", CellValue::text("3.13.2"))
                .cell("connections", CellValue::number(18.0))
                .cell("channels", CellValue::number(42.0))
                .cell("consumers", CellValue::number(consumers))])
            .priority(20),
        ],
        cx.took(11.0),
    )
}

// --------------------------------------------------------- elasticsearch

fn elasticsearch(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let secs = cx.secs();
    let recovering = cx
        .tau_of(Story::SearchHeap)
        .is_some_and(|tau| (250.0..300.0).contains(&tau));

    let nodes: [(&str, &[&str]); 4] = [
        ("es-master-1", &["master"]),
        ("es-data-1", &["data", "ingest"]),
        ("es-data-2", &["data", "ingest"]),
        ("es-data-3", &["data", "ingest"]),
    ];

    let mut node_rows = Vec::new();
    let mut max_heap: f64 = 0.0;
    let mut present = 0;
    for (name, roles) in nodes {
        if name == "es-data-2" && e.es_node_out {
            continue;
        }
        present += 1;
        // The JVM's sawtooth: heap climbs until a collection drops it.
        let period = 170.0 + (hash(&[key(name)]) % 90) as f64;
        let phase = ((secs + (hash(&[key(name), 9]) % 300) as f64) / period).rem_euclid(1.0);
        let mut heap = if name == "es-master-1" { 34.0 + 10.0 * phase } else { 47.0 + 21.0 * phase };
        let mut cpu = 9.0 + 7.0 * smooth(m.seed, key(name), secs, 60.0).abs();
        if name == "es-data-2" {
            heap = heap * (1.0 - e.es_heap) + (91.0 + 4.0 * phase) * e.es_heap;
            cpu += 52.0 * e.es_heap;
        }
        max_heap = max_heap.max(heap);
        let disk_used = if name == "es-master-1" { 12.0 } else { 57.0 + 3.0 * unit(hash(&[key(name), 3])) };
        let disk_total = if name == "es-master-1" { 50.0e9 } else { 400.0e9 };
        let disk_free = disk_total * (1.0 - disk_used / 100.0);
        let health = if heap > 90.0 || disk_used > 90.0 { Health::Degraded } else { Health::Healthy };
        let node_id = format!("{:x}", hash(&[key(name)]) & 0xFFFF_FFFF_FFFF);
        node_rows.push(
            Row::new(node_id, name, health)
                .summary(format!("heap {heap:.0}%, cpu {cpu:.0}%, disk {disk_used:.0}% used"))
                .cell("node", CellValue::text(name))
                .cell("roles", CellValue::tags(roles.iter().map(|r| r.to_string()).collect()))
                .cell("heap", CellValue::gauge_warn(heap, Some(100.0), 85.0))
                .cell("cpu", CellValue::gauge_warn(cpu, Some(100.0), 85.0))
                .cell("disk_used", CellValue::gauge_warn(disk_used, Some(100.0), 85.0))
                .cell("disk_free", CellValue::number(disk_free.round())),
        );
    }

    let unassigned = if e.es_node_out { 6.0 } else { 0.0 };
    let initializing = if recovering { 2.0 } else { 0.0 };
    let relocating = if recovering { 3.0 } else { 0.0 };
    let total_shards = 42.0;
    let active = total_shards - unassigned - initializing;
    let status = if e.es_node_out || recovering { "yellow" } else { "green" };
    let health = if status == "yellow" { Health::Degraded } else { Health::Healthy };

    let indices: [(&str, f64, f64, f64, f64); 4] = [
        ("products-v7", 2_140_512.0, 3.62e9, 3.0, 1.0),
        ("products-suggest-v3", 412_090.0, 0.41e9, 1.0, 1.0),
        ("orders-2026.10", 88_310.0 + 220.0 * (secs.rem_euclid(86400.0) / 3600.0), 0.21e9, 3.0, 1.0),
        ("orders-2026.09", 2_618_004.0, 4.88e9, 3.0, 1.0),
    ];
    let mut store_total = 0.0;
    let index_rows: Vec<Row> = indices
        .iter()
        .map(|(name, docs, size, pri, rep)| {
            store_total += size;
            let index_health = if (e.es_node_out || recovering) && name.starts_with("products") {
                Health::Degraded
            } else {
                Health::Healthy
            };
            Row::new(format!("index:{name}"), *name, index_health)
                .summary(format!("{docs:.0} docs, {}", human_bytes(*size)))
                .cell("index", CellValue::text(*name))
                .cell("health", CellValue::status(index_health))
                .cell("docs", CellValue::number(docs.round()))
                .cell("size", CellValue::number(*size))
                .cell("primaries", CellValue::number(*pri))
                .cell("replicas", CellValue::number(*rep))
                .cell("status", CellValue::text("open"))
        })
        .collect();

    cx.snapshot(
        health,
        format!(
            "Cluster `acme-search` is {status} — {present} nodes, {unassigned:.0} unassigned shards"
        ),
        vec![
            Metric::new("nodes", "Nodes", present as f64, MetricUnit::Count),
            Metric::new("data_nodes", "Data nodes", (present - 1) as f64, MetricUnit::Count),
            Metric::new("active_shards", "Active shards", active, MetricUnit::Count),
            Metric::new("unassigned_shards", "Unassigned shards", unassigned, MetricUnit::Count)
                .warn_above(0.0),
            Metric::new("relocating_shards", "Relocating", relocating, MetricUnit::Count),
            Metric::new("initializing_shards", "Initializing", initializing, MetricUnit::Count),
            Metric::new(
                "active_shards_pct",
                "Shards active",
                active / total_shards * 100.0,
                MetricUnit::Percent,
            ),
            Metric::new("max_heap", "Peak node heap", max_heap, MetricUnit::Percent).warn_above(85.0),
            Metric::new("store_total", "Total store", store_total, MetricUnit::Bytes),
        ],
        vec![
            Section::new(
                "nodes",
                "Nodes",
                vec![
                    ColumnSpec::text("node", "Node").primary(),
                    ColumnSpec::gauge("heap", "JVM heap", MetricUnit::Percent),
                    ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
                    ColumnSpec::gauge("disk_used", "Disk used", MetricUnit::Percent),
                    ColumnSpec::metric("disk_free", "Disk free", MetricUnit::Bytes),
                    ColumnSpec::tags("roles", "Roles").secondary(),
                ],
            )
            .rows(node_rows)
            .priority(10)
            .empty_text("No node stats were returned.")
            .tracked()
            .sorted_by_health(),
            Section::new(
                "indices",
                "Indices",
                vec![
                    ColumnSpec::text("index", "Index").primary(),
                    ColumnSpec::status("health", "Health"),
                    ColumnSpec::metric("size", "Size", MetricUnit::Bytes),
                    ColumnSpec::metric("docs", "Docs", MetricUnit::Count),
                    ColumnSpec::metric("primaries", "Primaries", MetricUnit::Count).secondary(),
                    ColumnSpec::metric("replicas", "Replicas", MetricUnit::Count).secondary(),
                    ColumnSpec::text("status", "State").secondary(),
                ],
            )
            .rows(index_rows)
            .priority(20)
            .empty_text("No user indices. (Internal dot-indices are hidden.)")
            .sorted_by_health(),
        ],
        cx.took(18.0 + 60.0 * e.es_heap),
    )
}

// ------------------------------------------------------------ kubernetes

/// The analytics-ingest pods while they crash-loop.
pub const INGEST_PODS: [&str; 3] = [
    "analytics-ingest-7f9c6b8d4-x2kqp",
    "analytics-ingest-7f9c6b8d4-m8zrt",
    "analytics-ingest-7f9c6b8d4-q4wvn",
];

const NODES: [&str; 4] = [
    "ip-10-0-12-41.eu-west-1.compute.internal",
    "ip-10-0-13-7.eu-west-1.compute.internal",
    "ip-10-0-14-118.eu-west-1.compute.internal",
    "ip-10-0-15-62.eu-west-1.compute.internal",
];

/// Restarts each ingest pod has had during the crash loop playing now.
pub fn ingest_restarts(cx: &Ctx, pod: usize) -> i64 {
    let Some(tau) = cx.tau_of(Story::ConsumerOom) else {
        return 0;
    };
    let start = 78.0 + pod as f64 * 11.0;
    if tau < start {
        return 0;
    }
    // Back-off doubles, so restarts slow down the longer it loops.
    let looped = (tau.min(320.0) - start).max(0.0);
    (1.0 + (looped / 18.0).sqrt() * 2.2).floor() as i64
}

fn kubernetes(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let e = &m.effects;
    let tau = cx.tau_of(Story::ConsumerOom);
    let looping = e.ingest_crashloop > 0.0;

    let node_rows: Vec<Row> = NODES
        .iter()
        .map(|name| {
            Row::new(format!("node:{name}"), *name, Health::Healthy)
                .summary("Ready")
                .cell("name", CellValue::text(*name))
                .cell("status", CellValue::text("Ready"))
                .cell("roles", CellValue::tags(vec!["worker".to_string()]))
                .cell("version", CellValue::text("v1.30.4-eks-a737599"))
                .cell("conditions", CellValue::tags(Vec::new()))
        })
        .collect();

    // name, namespace, type, desired, ready
    let workloads: Vec<(&str, &str, &str, i32, i32)> = vec![
        ("analytics-ingest", "data", "Deployment", 3, e.ingest_ready as i32),
        ("recommendations", "data", "Deployment", 2, 2),
        ("spark-operator", "spark", "Deployment", 1, 1),
        ("feature-store", "data", "StatefulSet", 3, 3),
        ("fluent-bit", "logging", "DaemonSet", 4, 4),
    ];
    let mut workloads_degraded = 0;
    let mut any_down = false;
    let mut workload_rows: Vec<Row> = workloads
        .iter()
        .map(|(name, ns, kind, desired, ready)| {
            let health = if *desired == 0 {
                Health::Healthy
            } else if *ready == 0 {
                Health::Down
            } else if ready < desired {
                Health::Degraded
            } else {
                Health::Healthy
            };
            if health != Health::Healthy {
                workloads_degraded += 1;
            }
            if health == Health::Down {
                any_down = true;
            }
            Row::new(format!("workload:{ns}/{kind}/{name}"), *name, health)
                .summary(format!("{kind} · {ready}/{desired} ready"))
                .cell("name", CellValue::text(*name))
                .cell("namespace", CellValue::text(*ns))
                .cell("type", CellValue::text(*kind))
                .cell("ready", CellValue::text(format!("{ready}/{desired}")))
                .cell(
                    "available",
                    CellValue::gauge(*ready as f64, Some((*desired).max(1) as f64)),
                )
        })
        .collect();
    workload_rows.sort_by(|a, b| b.health.cmp(&a.health).then_with(|| a.name.cmp(&b.name)));

    let mut pod_rows = Vec::new();
    let mut restarts_total = 7i64;
    for (i, pod) in INGEST_PODS.iter().enumerate() {
        let restarts = ingest_restarts(cx, i);
        restarts_total += restarts;
        let Some(tau) = tau else { continue };
        let crashing = looping && tau >= 78.0 + i as f64 * 11.0 && tau < 320.0;
        let rolling = (320.0..335.0).contains(&tau);
        if !crashing && !rolling {
            continue;
        }
        let reason = if rolling {
            "ContainerCreating"
        } else if ((tau / 9.0) as i64 + i as i64) % 3 == 0 {
            "OOMKilled"
        } else {
            "CrashLoopBackOff"
        };
        pod_rows.push(
            Row::new(format!("pod:data/{pod}"), *pod, Health::Degraded)
                .summary(format!("{reason} · {restarts} restarts"))
                .cell("name", CellValue::text(*pod))
                .cell("namespace", CellValue::text("data"))
                .cell("reason", CellValue::text(reason))
                .cell("ready", CellValue::text("0/1"))
                .cell("restarts", CellValue::number(restarts as f64))
                .cell("node", CellValue::text(NODES[i % NODES.len()])),
        );
    }

    let mut event_rows = Vec::new();
    if let Some(tau) = tau {
        if looping && tau >= 80.0 {
            for (i, pod) in INGEST_PODS.iter().enumerate() {
                let restarts = ingest_restarts(cx, i);
                if restarts == 0 {
                    continue;
                }
                let last = m.t - Duration::seconds(((tau as i64 + i as i64 * 7) % 17) + 2);
                event_rows.push(
                    Row::new(format!("event:backoff-{pod}"), *pod, Health::Degraded)
                        .summary(format!("Back-off restarting failed container ingest in pod {pod}"))
                        .cell("object", CellValue::text(*pod))
                        .cell("reason", CellValue::text("BackOff"))
                        .cell(
                            "message",
                            CellValue::text(format!(
                                "Back-off restarting failed container ingest in pod {pod}"
                            )),
                        )
                        .cell("count", CellValue::number((restarts * 2) as f64))
                        .cell("last_seen", CellValue::timestamp(last)),
                );
            }
            event_rows.push(
                Row::new("event:oom-analytics-ingest", NODES[0], Health::Degraded)
                    .summary("Memory cgroup out of memory: Killed process 1 (java)")
                    .cell("object", CellValue::text(NODES[0]))
                    .cell("reason", CellValue::text("OOMKilling"))
                    .cell(
                        "message",
                        CellValue::text(
                            "Memory cgroup out of memory: Killed process 1 (java) total-vm:3891204kB, anon-rss:524288kB",
                        ),
                    )
                    .cell("count", CellValue::number(restarts_total.saturating_sub(7) as f64))
                    .cell("last_seen", CellValue::timestamp(m.t - Duration::seconds(6))),
            );
        }
    }

    let pods_total = 3 + 2 + 1 + 3 + 4;
    let pods_unhealthy = pod_rows.len();
    let pods_running = pods_total - pods_unhealthy;
    let health = if any_down {
        Health::Down
    } else if workloads_degraded > 0 || pods_unhealthy > 0 {
        Health::Degraded
    } else {
        Health::Healthy
    };
    let summary = if workloads_degraded > 0 {
        format!(
            "{workloads_degraded} workload{} not fully available",
            if workloads_degraded == 1 { "" } else { "s" }
        )
    } else if pods_unhealthy > 0 {
        format!("{pods_unhealthy} pod{} need attention", if pods_unhealthy == 1 { "" } else { "s" })
    } else {
        format!("{} nodes ready · {pods_running} pods running", NODES.len())
    };

    cx.snapshot(
        health,
        summary,
        vec![
            Metric::new("nodes_ready", "Nodes ready", NODES.len() as f64, MetricUnit::Count),
            Metric::new("nodes_total", "Nodes", NODES.len() as f64, MetricUnit::Count),
            Metric::new("pods_running", "Pods running", pods_running as f64, MetricUnit::Count),
            Metric::new("pods_total", "Pods", pods_total as f64, MetricUnit::Count),
            Metric::new(
                "pods_unhealthy",
                "Pods needing attention",
                pods_unhealthy as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "workloads_degraded",
                "Workloads not available",
                workloads_degraded as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("restarts", "Container restarts", restarts_total as f64, MetricUnit::Count),
        ],
        vec![
            Section::new(
                "nodes",
                "Nodes",
                vec![
                    ColumnSpec::text("name", "Node").primary(),
                    ColumnSpec::text("status", "Status"),
                    ColumnSpec::tags("roles", "Roles"),
                    ColumnSpec::text("version", "Kubelet"),
                    ColumnSpec::tags("conditions", "Conditions"),
                ],
            )
            .rows(node_rows)
            .priority(10)
            .sorted_by_health()
            .tracked()
            .empty_text("No nodes reported."),
            Section::new(
                "workloads",
                "Workloads",
                vec![
                    ColumnSpec::text("name", "Workload").primary(),
                    ColumnSpec::text("namespace", "Namespace"),
                    ColumnSpec::text("type", "Type"),
                    ColumnSpec::text("ready", "Ready"),
                    ColumnSpec::metric("available", "Available", MetricUnit::Count),
                ],
            )
            .rows(workload_rows)
            .priority(20)
            .tracked()
            .empty_text("No workloads in scope."),
            Section::new(
                "pods",
                "Pods needing attention",
                vec![
                    ColumnSpec::text("name", "Pod").primary(),
                    ColumnSpec::text("namespace", "Namespace"),
                    ColumnSpec::text("reason", "Reason"),
                    ColumnSpec::text("ready", "Ready"),
                    ColumnSpec::metric("restarts", "Restarts", MetricUnit::Count),
                    ColumnSpec::text("node", "Node"),
                ],
            )
            .rows(pod_rows)
            .priority(5)
            .sorted_by_health()
            .empty_text("Every pod is running and ready."),
            Section::new(
                "events",
                "Warning events",
                vec![
                    ColumnSpec::text("object", "Object").primary(),
                    ColumnSpec::text("reason", "Reason"),
                    ColumnSpec::text("message", "Message"),
                    ColumnSpec::metric("count", "Count", MetricUnit::Count),
                    ColumnSpec::timestamp("last_seen", "Last seen"),
                ],
            )
            .rows(event_rows)
            .priority(30)
            .empty_text("No warning events."),
        ],
        cx.took(38.0),
    )
}

// --------------------------------------------------------------- jenkins

fn jenkins(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let mut failing = 0usize;
    let mut building = 0usize;

    let job_rows: Vec<Row> = super::story::JOBS
        .iter()
        .map(|(job, _)| {
            let builds = m.job_builds(job);
            let last = builds.last().copied();
            let in_flight = last.is_some_and(|b| b.building(m.t));
            let last_done = builds.iter().rev().find(|b| !b.building(m.t)).copied();
            let (health, label) = match last_done.map(|b| b.result) {
                Some(BuildResult::Success) => (Health::Healthy, "last build succeeded"),
                Some(BuildResult::Failure) => (Health::Down, "last build failed"),
                None => (Health::Unknown, "never built"),
            };
            if health == Health::Down {
                failing += 1;
            }
            if in_flight {
                building += 1;
            }
            let mut detail = label.to_string();
            if in_flight {
                detail.push_str(" · building now");
            }
            if let Some(b) = last {
                detail = format!("#{} — {detail}", b.number);
            }
            let recent: Vec<&&super::story::Build> = builds.iter().rev().take(20).collect();
            let recent_failures = recent
                .iter()
                .filter(|b| !b.building(m.t) && b.result == BuildResult::Failure)
                .count();
            let result_text = |b: &super::story::Build| match b.result {
                BuildResult::Success => "SUCCESS",
                BuildResult::Failure => "FAILURE",
            };
            let finished = |b: &super::story::Build| if b.building(m.t) { b.start } else { b.end };

            Row::new(*job, *job, health)
                .summary(detail)
                .cell("job", CellValue::text(*job))
                .cell(
                    "result",
                    CellValue::text(match last {
                        Some(_) if in_flight => "building".to_string(),
                        Some(b) => result_text(b).to_string(),
                        None => "never built".to_string(),
                    }),
                )
                .cell(
                    "build",
                    last.map(|b| CellValue::number(b.number as f64)).unwrap_or(CellValue::Null),
                )
                .cell(
                    "duration",
                    match last {
                        Some(b) if !in_flight => {
                            CellValue::duration((b.end - b.start).num_milliseconds() as f64 / 1000.0)
                        }
                        _ => CellValue::Null,
                    },
                )
                .cell(
                    "last_run",
                    last.map(|b| CellValue::timestamp(finished(b))).unwrap_or(CellValue::Null),
                )
                .cell("recent_failures", CellValue::number(recent_failures as f64))
                .action(RowAction::new(
                    "trigger_jenkins_build",
                    "Build now",
                    serde_json::json!({ "job": job }),
                ))
                .extra(serde_json::json!({
                    "builds": recent.iter().map(|b| {
                        let live = b.building(m.t);
                        serde_json::json!({
                            "number": b.number,
                            "result": if live { serde_json::Value::Null } else { serde_json::json!(result_text(b)) },
                            "building": live,
                            "durationMs": if live { 0 } else { (b.end - b.start).num_milliseconds() },
                            "finishedAt": finished(b).to_rfc3339(),
                        })
                    }).collect::<Vec<_>>(),
                    "buildable": true,
                }))
        })
        .collect();

    let agents = [("built-in", 2.0), ("agent-linux-1", 4.0), ("agent-linux-2", 4.0)];
    let agent_rows: Vec<Row> = agents
        .iter()
        .enumerate()
        .map(|(i, (name, executors))| {
            let busy = i > 0 && building >= i;
            let status = if busy { "busy" } else { "idle" };
            Row::new(format!("node:{name}"), *name, Health::Healthy)
                .summary(format!("online · {status}"))
                .cell("agent", CellValue::text(*name))
                .cell("status", CellValue::text(status))
                .cell("executors", CellValue::number(*executors))
                .cell("reason", CellValue::Null)
        })
        .collect();

    let health = if failing > 0 { Health::Degraded } else { Health::Healthy };
    let mut job_section = Section::new(
        "jobs",
        "Jobs",
        vec![
            ColumnSpec::text("job", "Job").primary(),
            ColumnSpec::text("result", "Last result"),
            ColumnSpec::metric("build", "Build", MetricUnit::Count).revision(),
            ColumnSpec::duration("duration", "Duration"),
            ColumnSpec::timestamp("last_run", "Finished"),
            ColumnSpec::metric("recent_failures", "Failures / 20", MetricUnit::Count).secondary(),
        ],
    )
    .rows(job_rows)
    .priority(10)
    .empty_text("No jobs are visible to this credential.")
    .tracked();
    job_section = job_section.sorted_by_health();

    cx.snapshot(
        health,
        format!("{} jobs — {failing} failing, 0 unstable, 0 queued", super::story::JOBS.len()),
        vec![
            Metric::new("jobs", "Jobs", super::story::JOBS.len() as f64, MetricUnit::Count),
            Metric::new("failing", "Failing", failing as f64, MetricUnit::Count).warn_above(0.0),
            Metric::new("unstable", "Unstable", 0.0, MetricUnit::Count).warn_above(0.0),
            Metric::new("building", "Building", building as f64, MetricUnit::Count),
            Metric::new("queue", "Queued", 0.0, MetricUnit::Count).warn_above(10.0),
            Metric::new("stuck", "Stuck in queue", 0.0, MetricUnit::Count).warn_above(0.0),
            Metric::new("executors_busy", "Busy executors", building as f64, MetricUnit::Count),
            Metric::new("executors_total", "Total executors", 10.0, MetricUnit::Count),
            Metric::new("nodes_offline", "Offline nodes", 0.0, MetricUnit::Count).warn_above(0.0),
        ],
        vec![
            job_section,
            Section::new(
                "agents",
                "Agents",
                vec![
                    ColumnSpec::text("agent", "Agent").primary(),
                    ColumnSpec::text("status", "Status"),
                    ColumnSpec::metric("executors", "Executors", MetricUnit::Count),
                    ColumnSpec::text("reason", "Offline reason"),
                ],
            )
            .rows(agent_rows)
            .priority(20)
            .empty_text("No agents are configured.")
            .tracked()
            .sorted_by_health(),
        ],
        cx.took(21.0),
    )
}

// ------------------------------------------------------------ linux host

/// The exports volume fills with nightly extracts and is pruned twice a day.
pub fn exports_used_pct(cx: &Ctx) -> f64 {
    let fill = ramp(cx.m, 14.0 * 3600.0, 0.43);
    62.0 + 31.0 * fill + 0.15 * cx.wander("exports")
}

fn linux_host(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let cores = 8.0;
    let memory_total = 32.0 * GIB;
    let memory_pct = 58.0 + 6.0 * cx.wander("mem");
    let available = memory_total * (1.0 - memory_pct / 100.0);
    let load1 = (2.0 + 1.1 * cx.wander("load1") + 0.3 * cx.jitter("load")).max(0.05);
    let load5 = (2.0 + 0.9 * cx.wander("load5")).max(0.05);
    let load15 = (2.0 + 0.6 * cx.wander("load15")).max(0.05);
    let per_core = load1 / cores;
    let uptime = (m.t - born(m, "batch-1", 980, 24)).num_seconds() as f64;

    let disk_warn = 85.0;
    let exports_pct = exports_used_pct(cx);
    let mounts = [
        ("/var/lib/exports", "/dev/nvme1n1", "xfs", 500.0e9, exports_pct),
        ("/", "/dev/nvme0n1p1", "ext4", 80.0e9, 46.0 + 0.4 * cx.wander("root")),
    ];
    let fullest = mounts
        .iter()
        .map(|mt| mt.4)
        .fold(0.0, f64::max);

    let disk_critical = fullest >= 98.0;
    let disk_high = fullest >= disk_warn;
    let health = if disk_critical {
        Health::Down
    } else if disk_high || memory_pct >= 90.0 || per_core >= 1.5 {
        Health::Degraded
    } else {
        Health::Healthy
    };
    let summary = if disk_high {
        let (mount, _, _, size, pct) = mounts[0];
        format!(
            "{mount} is {pct:.0}% full — {} free",
            human_bytes(size * (1.0 - pct / 100.0))
        )
    } else {
        format!("{cores:.0} cores · up {}d · {} filesystems", (uptime / 86400.0) as u64, mounts.len())
    };

    let disk_rows: Vec<Row> = mounts
        .iter()
        .map(|(mount, device, fstype, size, pct)| {
            let state = if *pct >= 98.0 {
                Health::Down
            } else if *pct >= disk_warn {
                Health::Degraded
            } else {
                Health::Healthy
            };
            let free = size * (1.0 - pct / 100.0);
            Row::new(format!("mount:{mount}"), *mount, state)
                .summary(format!("{} free of {} · {device}", human_bytes(free), human_bytes(*size)))
                .cell("mount", CellValue::text(*mount))
                .cell("state", CellValue::status(state))
                .cell(
                    "used",
                    CellValue::gauge_warn((size - free).round(), Some(*size), size * (disk_warn / 100.0)),
                )
                .cell("free", CellValue::number(free.round()))
                .cell("fstype", CellValue::text(*fstype))
        })
        .collect();

    cx.snapshot(
        health,
        summary,
        vec![
            Metric::new("disk_fullest", "Fullest filesystem", fullest, MetricUnit::Percent)
                .warn_above(disk_warn),
            Metric::new("memory_used", "Memory used", memory_pct, MetricUnit::Percent).warn_above(90.0),
            Metric::new("memory_available", "Memory available", available.round(), MetricUnit::Bytes),
            Metric::new("load_per_core", "Load per core", per_core, MetricUnit::Count).warn_above(1.5),
            Metric::new("load1", "Load (1m)", load1, MetricUnit::Count),
            Metric::new("load5", "Load (5m)", load5, MetricUnit::Count),
            Metric::new("load15", "Load (15m)", load15, MetricUnit::Count),
            Metric::new("cores", "Logical CPUs", cores, MetricUnit::Count),
            Metric::new("uptime", "Uptime", uptime, MetricUnit::Seconds),
        ],
        vec![Section::new(
            "filesystems",
            "Filesystems",
            vec![
                ColumnSpec::text("mount", "Mount").primary(),
                ColumnSpec::status("state", "State"),
                ColumnSpec::gauge("used", "Used", MetricUnit::Bytes),
                ColumnSpec::metric("free", "Free", MetricUnit::Bytes),
                ColumnSpec::text("fstype", "Type"),
            ],
        )
        .rows(disk_rows)
        .tracked()
        .empty_text("node_exporter reported no real filesystems.")
        .priority(10)],
        cx.took(7.0),
    )
}

// -------------------------------------------------------------- gigapipe

/// Pipelines the telemetry store ingests, with rows per edge request.
const PIPELINES: &[(&str, f64, f64)] = &[
    // name, rows per request, bytes per row
    ("loki", 0.9, 310.0),
    ("otlp-traces", 6.2, 540.0),
    ("prometheus", 14.0, 48.0),
    ("pyroscope", 0.05, 9200.0),
];

fn gigapipe(cx: &Ctx) -> HealthSnapshot {
    let m = cx.m;
    let rps = traffic(m);
    let alive = (m.t - m.epoch).num_seconds() as f64 + 9.0 * 86400.0;

    let signal_rows: Vec<Row> = [
        ("logs", "/loki/api/v1/labels", 11.0),
        ("metrics", "/api/v1/labels", 8.0),
        ("traces", "/api/echo", 4.0),
    ]
    .iter()
    .map(|(signal, path, base)| {
        let took = (base * (1.0 + 0.3 * cx.wander(signal).abs())).round();
        Row::new(format!("signal:{signal}"), *signal, Health::Healthy)
            .summary(format!("answering in {took} ms"))
            .cell("signal", CellValue::text(*signal))
            .cell("api", CellValue::text(*path))
            .cell("state", CellValue::status(Health::Healthy))
            .cell("latency", CellValue::number(took))
    })
    .collect();

    let selector = cx
        .member
        .options
        .iter()
        .find(|(k, _)| *k == "profileQuery")
        .map(|(_, v)| *v)
        .unwrap_or("");
    let profile_rows = vec![Row::new("profile:declared", selector, Health::Healthy)
        .summary("answering in 22 ms")
        .cell("selector", CellValue::text(selector))
        .cell("state", CellValue::status(Health::Healthy))
        .cell("latency", CellValue::number(22.0))];

    let mut total_rate = 0.0;
    let mut total_bytes = 0.0;
    let ingest_rows: Vec<Row> = PIPELINES
        .iter()
        .map(|(name, per_request, bytes_per_row)| {
            let rate = rps * per_request * (1.0 + 0.04 * cx.wander(name));
            let rows = (alive * rps * per_request).round();
            let bytes = rows * bytes_per_row;
            total_rate += rate;
            total_bytes += bytes;
            Row::new(format!("ingest:{name}"), *name, Health::Healthy)
                .summary(format!("{rate:.1} rows/s"))
                .cell("pipeline", CellValue::text(*name))
                .cell("rate", CellValue::number(rate))
                .cell("rows", CellValue::number(rows))
                .cell("bytes", CellValue::number(bytes))
        })
        .collect();

    let traces = super::traces::recent(m, 15, 12);
    let trace_rows: Vec<Row> = traces
        .iter()
        .map(|trace| {
            Row::new(format!("trace:{}", trace.trace_id), &trace.root_name, Health::Unknown)
                .summary(format!("{:.0} ms", trace.duration_ms))
                .cell("trace", CellValue::text(&trace.root_name))
                .cell("service", CellValue::text(&trace.root_service))
                .cell("duration", CellValue::number(trace.duration_ms))
                .cell("started", CellValue::timestamp(trace.started_at))
                .cell("id", CellValue::text(&trace.trace_id))
        })
        .collect();

    cx.snapshot(
        Health::Healthy,
        format!("{total_rate:.0} rows/s across {} pipelines", PIPELINES.len()),
        vec![
            Metric::new("ingest_rows_per_sec", "Ingest rate", total_rate, MetricUnit::PerSecond),
            Metric::new("signals_up", "Query APIs up", 4.0, MetricUnit::Count),
            Metric::new("parse_errors", "Rejected payloads", 3.0, MetricUnit::Count),
            Metric::new("conn_resets", "Backend resets", 0.0, MetricUnit::Count),
            Metric::new("ingest_bytes", "Bytes written", total_bytes, MetricUnit::Bytes),
            Metric::new("traces_listed", "Traces matched", trace_rows.len() as f64, MetricUnit::Count),
            Metric::new(
                "memory",
                "Resident memory",
                (1.31 * GIB) * (1.0 + 0.05 * cx.wander("rss")),
                MetricUnit::Bytes,
            ),
            Metric::new("goroutines", "Goroutines", (412.0 + 30.0 * cx.wander("go")).round(), MetricUnit::Count),
        ],
        vec![
            Section::new(
                "signals",
                "Query APIs",
                vec![
                    ColumnSpec::text("signal", "Signal").primary(),
                    ColumnSpec::text("api", "Endpoint"),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("latency", "Answered in", MetricUnit::Millis),
                ],
            )
            .rows(signal_rows)
            .priority(5)
            .sorted_by_health()
            .empty_text("No query APIs probed."),
            Section::new(
                "profiles",
                "Profiles",
                vec![
                    ColumnSpec::text("selector", "Profile").primary().profile(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("latency", "Answered in", MetricUnit::Millis),
                ],
            )
            .rows(profile_rows)
            .priority(6)
            .empty_text("No profile selector declared on this connection."),
            Section::new(
                "ingest",
                "Ingest pipelines",
                vec![
                    ColumnSpec::text("pipeline", "Pipeline").primary(),
                    ColumnSpec::metric("rate", "Rows/s", MetricUnit::PerSecond),
                    ColumnSpec::metric("rows", "Rows written", MetricUnit::Count),
                    ColumnSpec::metric("bytes", "Bytes written", MetricUnit::Bytes),
                ],
            )
            .rows(ingest_rows)
            .tracked()
            .priority(10)
            .empty_text("Nothing has been ingested since this instance started."),
            Section::new(
                "traces",
                "Traces",
                vec![
                    ColumnSpec::text("trace", "Trace").primary(),
                    ColumnSpec::text("service", "Root service"),
                    ColumnSpec::metric("duration", "Duration", MetricUnit::Millis),
                    ColumnSpec::timestamp("started", "Started"),
                    ColumnSpec::code("id", "Trace id").trace(),
                ],
            )
            .rows(trace_rows)
            .priority(8)
            .empty_text("No traces matched in the search window."),
            Section::new(
                "build",
                "Build",
                vec![
                    ColumnSpec::text("component", "Component").primary(),
                    ColumnSpec::text("version", "Version").revision(),
                ],
            )
            .rows(vec![Row::new("build:gigapipe", "gigapipe", Health::Healthy)
                .summary("3.4.1")
                .cell("component", CellValue::text("gigapipe"))
                .cell("version", CellValue::text("3.4.1"))])
            .priority(60)
            .empty_text("Version not reported."),
        ],
        cx.took(12.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::validate::assert_renderable;
    use crate::sim::estate::{profile_id, ESTATE};
    use crate::sim::story::Script;

    fn script() -> Script {
        Script {
            started_at: DateTime::parse_from_rfc3339("2026-10-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            seed: 3,
            triggers: Vec::new(),
        }
    }

    fn each_snapshot(t: DateTime<Utc>, mut check: impl FnMut(&HealthSnapshot)) {
        let s = script();
        let m = s.moment(t);
        for member in ESTATE {
            let id = profile_id(member.key);
            let cx = Ctx { m: &m, member, id: &id, name: member.name };
            check(&snapshot(&cx));
        }
    }

    #[test]
    fn every_kind_is_renderable_at_every_point_in_a_story() {
        let s = script();
        for step in 0..160 {
            let t = s.started_at - Duration::minutes(30) + Duration::seconds(step * 15);
            each_snapshot(t, |snap| {
                assert_renderable(snap);
                for metric in &snap.metrics {
                    assert!(metric.value.is_finite(), "{} {}", snap.profile_name, metric.key);
                }
            });
        }
    }

    #[test]
    fn the_estate_is_quiet_between_stories() {
        let s = script();
        // Ten minutes before the demo started is between the two past stories.
        let calm = s.started_at - Duration::seconds(30);
        assert!(s.active(calm).is_none());
        each_snapshot(calm, |snap| {
            // The smoke suite flakes now and then on purpose — a real CI does.
            if snap.kind == ServiceKind::Jenkins {
                return;
            }
            assert!(
                matches!(snap.health, Health::Healthy | Health::Unknown),
                "{} is {:?}: {}",
                snap.profile_name,
                snap.health,
                snap.summary
            );
        });
    }

    #[test]
    fn the_bad_deploy_takes_checkout_down_and_degrades_the_database() {
        let s = script();
        let deploy = s.next(s.started_at).unwrap();
        assert_eq!(deploy.story, Story::BadDeploy);
        let peak = deploy.at(250.0);
        let mut seen = std::collections::HashMap::new();
        each_snapshot(peak, |snap| {
            seen.insert(snap.profile_name.clone(), snap.health);
        });
        assert_eq!(seen["checkout-api"], Health::Down);
        assert_eq!(seen["orders-db"], Health::Degraded);
        assert_eq!(seen["prod-docker"], Health::Down);
        assert!(matches!(seen["edge-lb"], Health::Down | Health::Degraded));
    }
}
