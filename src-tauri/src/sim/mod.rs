//! The live demo: a simulated estate that runs inside root.
//!
//! A monitoring tool with nothing connected is an empty room, and the depth in
//! this one — incidents grouped from a dependency graph, a root cause read
//! against a deploy, config rules that turn a climbing number into a date —
//! only shows when something is going wrong. This gives it something going
//! wrong, on a loop, without Docker or a single real service.
//!
//! ## What it is, exactly
//!
//! Thirteen ordinary connection profiles, flagged `simulated`, each answered
//! by a [`SimCollector`] instead of the network. Everything downstream — the
//! alert engine, the change engine, incident grouping, the config rules, the
//! forecaster, the assistant's tools — reads them through the same code it
//! reads a real estate through. Nothing in the screens knows the difference
//! except the banner that says so.
//!
//! ## Deterministic
//!
//! Every reading is a function of the seed, the connection and the moment. So
//! two hours of history are drawn at startup by asking for two hours of
//! moments, and a restart redraws exactly what was there before.
//!
//! ## Never outward
//!
//! A simulated connection is never written to a history database or a
//! warehouse, never notifies a channel, and every action against it is a
//! no-op that says so. An invented outage that paged a real on-call, or a
//! fake series in somebody's Postgres, would be the worst thing this could do.

pub mod estate;
pub mod kinds;
pub mod logs;
mod noise;
pub mod story;
pub mod traces;

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::collectors::{Collector, CollectorAction, DetailQuery};
use crate::config::{AppConfig, ConnectionProfile, DemoSettings, DemoTrigger};
use crate::configsource::ConfigDocument;
use crate::error::{AppError, Result};
use crate::model::{HealthSnapshot, LogEvent, ServiceKind};
use crate::state::AppState;

use estate::Member;
use kinds::Ctx;
use story::{Script, Story, BACKFILL_SECS};

// ------------------------------------------------------------------ world

static WORLD: OnceLock<RwLock<Option<Script>>> = OnceLock::new();

fn world() -> &'static RwLock<Option<Script>> {
    WORLD.get_or_init(|| RwLock::new(None))
}

/// Point the simulation at the demo settings — or at nothing, when none is
/// running. Called whenever the config is loaded or the demo changes.
pub fn configure(demo: &DemoSettings) {
    let script = demo.started_at.map(|started_at| Script {
        started_at,
        seed: demo.seed,
        triggers: demo
            .triggers
            .iter()
            .filter_map(|t| Some((t.at, Story::from_id(&t.story)?)))
            .collect(),
    });
    if let Ok(mut slot) = world().write() {
        *slot = script;
    }
}

fn script() -> Option<Script> {
    world().read().ok()?.clone()
}

// -------------------------------------------------------------- collector

/// Answers for one simulated connection.
pub struct SimCollector {
    id: String,
    name: String,
    member: &'static Member,
}

impl SimCollector {
    pub fn new(profile: &ConnectionProfile) -> Result<Self> {
        let member = estate::member(&profile.id).ok_or_else(|| {
            AppError::Config(format!(
                "`{}` is marked simulated but is not part of the demo estate",
                profile.name
            ))
        })?;
        Ok(SimCollector {
            id: profile.id.clone(),
            name: profile.name.clone(),
            member,
        })
    }

    fn running(&self) -> Result<Script> {
        script().ok_or_else(|| {
            AppError::Collector("the live demo is not running, so this connection has nothing behind it".to_string())
        })
    }

    fn snapshot_at(&self, script: &Script, t: DateTime<Utc>) -> HealthSnapshot {
        let m = script.moment(t);
        kinds::snapshot(&Ctx {
            m: &m,
            member: self.member,
            id: &self.id,
            name: &self.name,
        })
    }
}

#[async_trait]
impl Collector for SimCollector {
    fn kind(&self) -> ServiceKind {
        self.member.kind
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        match script() {
            Some(script) => Ok(self.snapshot_at(&script, Utc::now())),
            None => Ok(HealthSnapshot::unreachable(
                &self.id,
                &self.name,
                self.member.kind,
                "the live demo is not running".to_string(),
                0,
            )),
        }
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let Some(script) = script() else {
            return Ok(Vec::new());
        };
        Ok(logs::between(&script, self.member, &self.id, &self.name, since, Utc::now(), limit))
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        let script = self.running()?;
        let now = Utc::now();
        match (self.member.kind, query) {
            (ServiceKind::Gigapipe, DetailQuery::Trace { trace_id }) => {
                json(&traces::trace(&script, trace_id)?)
            }
            (ServiceKind::Gigapipe, DetailQuery::TracesInWindow { from, to, limit }) => {
                json(&traces::in_window(&script, *from, *to, *limit))
            }
            (ServiceKind::Gigapipe, DetailQuery::Profile { query, from, to }) => {
                json(&traces::profile(&script, query, *from, *to)?)
            }
            (ServiceKind::Gigapipe, DetailQuery::GigapipeRead(_)) => Err(AppError::Collector(
                "the live demo's telemetry store answers traces and profiles, not raw PromQL, LogQL or TraceQL"
                    .to_string(),
            )),
            (ServiceKind::Docker, DetailQuery::Container { container }) => {
                inspect(&script, now, container)
            }
            (ServiceKind::Postgres, DetailQuery::DbDiagnostic { check }) => {
                diagnostic(&script, now, self, check)
            }
            (ServiceKind::Jenkins, DetailQuery::JenkinsBuildLog { job, build, tail_lines }) => {
                console(&script, now, job, *build, *tail_lines)
            }
            _ => Err(AppError::UnsupportedAction(self.member.kind.label().to_string())),
        }
    }

    async fn config(&self) -> Result<Vec<ConfigDocument>> {
        let Some(script) = script() else {
            return Ok(Vec::new());
        };
        Ok(config_documents(&script, Utc::now(), &self.id, self.member))
    }

    async fn run_action(&self, action: &CollectorAction) -> Result<String> {
        Ok(format!(
            "Simulated: {}. This is the live demo, so nothing real was touched.",
            action.describe()
        ))
    }
}

/// `serde_json::to_value`, with the error a collector error rather than a panic.
fn json<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|e| AppError::Collector(format!("could not serialize: {e}")))
}

// ----------------------------------------------------------- drill-downs

fn inspect(script: &Script, now: DateTime<Utc>, name: &str) -> Result<serde_json::Value> {
    let m = script.moment(now);
    let c = kinds::CONTAINERS
        .iter()
        .find(|c| c.name == name || kinds::container_id(c).starts_with(name))
        .ok_or_else(|| AppError::Collector(format!("no such container: {name}")))?;
    let state = kinds::container_state(&m, c);
    Ok(serde_json::json!({
        "Id": kinds::container_id(c),
        "Name": format!("/{}", c.name),
        "Created": (state.started - Duration::seconds(4)).to_rfc3339(),
        "RestartCount": state.restarts,
        "State": {
            "Status": if state.running { "running" } else { "exited" },
            "Running": state.running,
            "OOMKilled": !state.running,
            "ExitCode": if state.running { 0 } else { 137 },
            "StartedAt": state.started.to_rfc3339(),
            "Health": if c.healthcheck {
                serde_json::json!({ "Status": if state.unhealthy { "unhealthy" } else { "healthy" }, "FailingStreak": if state.unhealthy { 6 } else { 0 } })
            } else {
                serde_json::Value::Null
            },
        },
        "Config": {
            "Image": state.image,
            "Labels": { "com.docker.compose.project": "acme-shop", "com.docker.compose.service": c.service },
            "Healthcheck": if c.healthcheck {
                serde_json::json!({ "Test": ["CMD", "wget", "-qO-", "http://localhost:8080/healthz"], "Interval": 10_000_000_000u64, "Timeout": 2_000_000_000u64 })
            } else {
                serde_json::Value::Null
            },
        },
        "HostConfig": {
            "Memory": c.mem_limit as u64,
            "RestartPolicy": { "Name": c.restart_policy.to_lowercase().replace('_', "-") },
        },
    }))
}

fn diagnostic(script: &Script, now: DateTime<Utc>, sim: &SimCollector, check: &str) -> Result<serde_json::Value> {
    let m = script.moment(now);
    let cx = Ctx {
        m: &m,
        member: sim.member,
        id: &sim.id,
        name: &sim.name,
    };
    let conns = kinds::connections(&cx);
    let long = kinds::long_queries(&cx);
    match check {
        "long_queries" => Ok(serde_json::json!({
            "thresholdSeconds": 5,
            "queries": long.iter().map(|(pid, seconds, state, query)| serde_json::json!({
                "pid": pid, "seconds": seconds, "state": state, "query": query,
            })).collect::<Vec<_>>(),
        })),
        "connections" => {
            let active = (long.len() as f64 + m.effects.db_saturation * 120.0).round();
            Ok(serde_json::json!([
                { "database": "orders", "user": "checkout", "state": "active", "count": active },
                { "database": "orders", "user": "checkout", "state": "idle in transaction", "count": (m.effects.db_saturation * 22.0).round() },
                { "database": "orders", "user": "checkout", "state": "idle", "count": (conns - active - 14.0).max(4.0) },
                { "database": "orders", "user": "order_worker", "state": "idle", "count": 8 },
                { "database": "inventory", "user": "catalog", "state": "idle", "count": 4 },
                { "database": "", "user": "", "state": "", "count": 2 },
            ]))
        }
        "table_sizes" => Ok(serde_json::json!([
            { "table": "public.order_items", "bytes": 21_400_000_000u64, "rows": 412_000_000u64 },
            { "table": "public.orders", "bytes": 14_100_000_000u64, "rows": 96_400_000u64 },
            { "table": "public.payments", "bytes": 6_800_000_000u64, "rows": 95_100_000u64 },
            { "table": "public.order_events", "bytes": 4_900_000_000u64, "rows": 310_000_000u64 },
        ])),
        "locks" => Ok(serde_json::Value::Array(
            if m.effects.db_saturation > 0.6 {
                long.iter()
                    .take(2)
                    .map(|(pid, _, _, query)| {
                        serde_json::json!({
                            "blockedPid": pid,
                            "blockedQuery": "UPDATE orders SET status = $1, updated_at = now() WHERE id = $2",
                            "blockingPid": "48211",
                            "blockingQuery": query,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            },
        )),
        "replication" => Ok(serde_json::json!({ "role": "primary", "lagSeconds": 0.0 })),
        other => Err(AppError::Collector(format!(
            "unknown diagnostic `{other}`; valid checks are: connections, long_queries, table_sizes, locks, replication"
        ))),
    }
}

fn console(
    script: &Script,
    now: DateTime<Utc>,
    job: &str,
    build: Option<i64>,
    tail_lines: Option<usize>,
) -> Result<serde_json::Value> {
    let m = script.moment(now);
    let builds = m.job_builds(job);
    let chosen = match build {
        Some(n) => builds.iter().find(|b| b.number == n).copied(),
        None => builds.last().copied(),
    }
    .ok_or_else(|| AppError::Collector(format!("no build {} of `{job}`", build.map(|n| n.to_string()).unwrap_or_else(|| "lastBuild".into()))))?;

    let mut lines = vec![
        format!("Started by GitHub push to main"),
        format!("Running on agent-linux-{} in /var/jenkins/workspace/{job}", 1 + chosen.number % 2),
        "[Pipeline] Start of Pipeline".to_string(),
        "[Pipeline] stage (Checkout)".to_string(),
        format!("Checking out Revision {:x} (refs/remotes/origin/main)", chosen.number as u64 * 0x9E37_79B9 & 0xFFFF_FFFF_FFFF),
    ];
    if job.starts_with("deploy-") {
        let service = job.trim_start_matches("deploy-");
        let version = chosen.what.rsplit(' ').next().unwrap_or("");
        lines.extend([
            "[Pipeline] stage (Build image)".to_string(),
            format!("#14 naming to ghcr.io/acme/{service}:{version} done"),
            "[Pipeline] stage (Test)".to_string(),
            format!("ok  github.com/acme/{service}/...  {:.1}s", 9.0 + (chosen.number % 7) as f64),
            "[Pipeline] stage (Deploy)".to_string(),
            format!("Rolling {service} to {version}: 1/2 replaced"),
        ]);
        if chosen.building(now) {
            lines.push(format!("Rolling {service} to {version}: waiting for health checks…"));
        } else {
            lines.push(format!("Rolling {service} to {version}: 2/2 replaced"));
            lines.push(chosen.what.clone());
        }
    } else if job == "e2e-smoke" {
        lines.extend([
            "[Pipeline] stage (Smoke)".to_string(),
            "Running 38 specs against https://shop.acme.dev".to_string(),
        ]);
        if !chosen.building(now) {
            if chosen.what.contains("failed") {
                lines.push(if chosen.what.contains("checkout") {
                    "  ✗ checkout › places an order (30012ms) — Timeout: POST /checkout did not complete".to_string()
                } else {
                    "  ✗ search › shows suggestions (8004ms) — Timeout waiting for #suggestions".to_string()
                });
            }
            lines.push(chosen.what.clone());
        }
    } else {
        lines.extend([
            "[Pipeline] stage (Reindex)".to_string(),
            "POST /products-v7/_bulk … 2,140,512 documents".to_string(),
        ]);
        if !chosen.building(now) {
            lines.push(chosen.what.clone());
        }
    }
    if !chosen.building(now) {
        lines.push(format!(
            "Finished: {}",
            if chosen.result == story::BuildResult::Success { "SUCCESS" } else { "FAILURE" }
        ));
    }

    let take = tail_lines.unwrap_or(200).clamp(1, 5000);
    let tail: Vec<&String> = lines.iter().skip(lines.len().saturating_sub(take)).collect();
    Ok(serde_json::json!({
        "job": job,
        "build": chosen.number.to_string(),
        "totalLines": lines.len(),
        "console": tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"),
    }))
}

fn config_documents(script: &Script, now: DateTime<Utc>, id: &str, member: &Member) -> Vec<ConfigDocument> {
    match member.kind {
        ServiceKind::Redis => vec![ConfigDocument::live(
            id,
            "Running configuration",
            "CONFIG GET *",
            &serde_json::json!({
                "maxmemory": "2147483648",
                "maxmemory-policy": "noeviction",
                "maxclients": "10000",
                "timeout": "0",
                "tcp-keepalive": "300",
                "appendonly": "no",
                "save": "3600 1 300 100 60 10000",
                "databases": "16",
                "hz": "10",
                "io-threads": "4",
                "lazyfree-lazy-eviction": "no",
            }),
        )],
        ServiceKind::Postgres => {
            let setting = |value: &str| serde_json::json!({ "value": value, "source": "configuration file" });
            vec![ConfigDocument::live(
                id,
                "Running parameters",
                "pg_settings (non-default)",
                &serde_json::json!({
                    "max_connections": setting("200"),
                    "shared_buffers": setting("4194304kB"),
                    "work_mem": setting("16384kB"),
                    "maintenance_work_mem": setting("524288kB"),
                    "effective_cache_size": setting("12582912kB"),
                    "statement_timeout": setting("0ms"),
                    "idle_in_transaction_session_timeout": setting("0ms"),
                    "log_min_duration_statement": setting("1000ms"),
                    "random_page_cost": setting("1.1"),
                    "max_wal_size": setting("4096MB"),
                    "checkpoint_timeout": setting("900s"),
                }),
            )]
        }
        ServiceKind::Docker => {
            let m = script.moment(now);
            let map: serde_json::Map<String, serde_json::Value> = kinds::CONTAINERS
                .iter()
                .map(|c| {
                    let state = kinds::container_state(&m, c);
                    (
                        c.name.to_string(),
                        serde_json::json!({
                            "image": state.image,
                            "memoryLimitBytes": c.mem_limit as i64,
                            "memoryReservationBytes": 0,
                            "memorySwapBytes": if c.mem_limit > 0.0 { (c.mem_limit * 2.0) as i64 } else { 0 },
                            "nanoCpus": if c.mem_limit > 0.0 { 1_000_000_000i64 } else { 0 },
                            "cpuShares": 0,
                            "cpuQuota": 0,
                            "pidsLimit": null,
                            "restartPolicy": c.restart_policy,
                            "readonlyRootfs": false,
                            "ulimits": null,
                        }),
                    )
                })
                .collect();
            vec![ConfigDocument::live(
                id,
                "Container limits",
                "inspect (HostConfig)",
                &serde_json::Value::Object(map),
            )]
        }
        ServiceKind::Kubernetes => {
            let m = script.moment(now);
            let fixed = !m.version_at("analytics-ingest", now).ends_with(".0");
            vec![ConfigDocument::live(
                id,
                "Workload sizing",
                "apps/v1 specs",
                &serde_json::json!({
                    "Deployment/analytics-ingest": {
                        "replicas": 3,
                        "strategy": "RollingUpdate",
                        "containers": { "ingest": {
                            "image": format!("ghcr.io/acme/analytics-ingest:{}", m.version_at("analytics-ingest", now)),
                            "requests": { "cpu": "500m", "memory": "384Mi" },
                            "limits": { "cpu": "1", "memory": if fixed { "1Gi" } else { "512Mi" } },
                        }},
                    },
                    "Deployment/recommendations": {
                        "replicas": 2,
                        "strategy": "RollingUpdate",
                        "containers": { "api": {
                            "image": "ghcr.io/acme/recommendations:0.9.12",
                            "requests": { "cpu": null, "memory": null },
                            "limits": { "cpu": null, "memory": null },
                        }},
                    },
                    "Deployment/spark-operator": {
                        "replicas": 1,
                        "strategy": "Recreate",
                        "containers": { "operator": {
                            "image": "ghcr.io/kubeflow/spark-operator:v1.4.6",
                            "requests": { "cpu": "100m", "memory": "128Mi" },
                            "limits": { "cpu": null, "memory": "256Mi" },
                        }},
                    },
                    "StatefulSet/feature-store": {
                        "replicas": 3,
                        "containers": { "redis": {
                            "image": "redis:7.2.5",
                            "requests": { "cpu": "250m", "memory": "2Gi" },
                            "limits": { "cpu": "1", "memory": "2Gi" },
                        }},
                    },
                }),
            )]
        }
        _ => Vec::new(),
    }
}

// --------------------------------------------------------------- backfill

/// How far back the replayed log stream reaches. Shorter than the metric
/// history so a modest log buffer is not filled with the past alone.
const LOG_BACKFILL_SECS: i64 = 90 * 60;

/// Draw the simulated past into the stores before the pollers start, so the
/// demo opens on two hours of history rather than on empty charts.
///
/// Snapshots go through the same engines a live poll does — history, change
/// detection, the alert store as of each moment — so deploy markers, resolved
/// alerts and health lanes in the past are exactly what polling would have
/// left behind. Only the moments differ.
pub async fn backfill(state: &Arc<AppState>, profiles: &[ConnectionProfile]) {
    let Some(script) = script() else {
        return;
    };
    let now = Utc::now();
    let from = now - Duration::seconds(BACKFILL_SECS);

    for profile in profiles {
        forget(state, &profile.id).await;
    }

    // One moment per tick, shared by every connection polled on that tick.
    let mut intervals: Vec<u64> = profiles.iter().map(|p| p.poll_interval_secs.clamp(2, 3600)).collect();
    intervals.sort_unstable();
    intervals.dedup();

    for interval in intervals {
        let group: Vec<(&ConnectionProfile, &'static Member)> = profiles
            .iter()
            .filter(|p| p.poll_interval_secs.clamp(2, 3600) == interval)
            .filter_map(|p| Some((p, estate::member(&p.id)?)))
            .collect();
        let mut previous: Vec<Option<HealthSnapshot>> = vec![None; group.len()];

        let step = Duration::seconds(interval as i64);
        let mut t = from;
        while t < now {
            let m = script.moment(t);
            let mut history = state.history.write().await;
            let mut changes = state.changes.write().await;
            let mut alerts = state.alerts.write().await;
            for (i, (profile, member)) in group.iter().enumerate() {
                let snapshot = kinds::snapshot(&Ctx {
                    m: &m,
                    member,
                    id: &profile.id,
                    name: &profile.name,
                });
                history.record(&snapshot);
                let observed = crate::changes::changes_from_snapshots(previous[i].as_ref(), &snapshot);
                if !observed.is_empty() {
                    changes.record(observed);
                }
                alerts.reconcile_at(&profile.id, crate::state::alerts_from_snapshot(&snapshot), t);
                previous[i] = Some(snapshot);
            }
            drop((history, changes, alerts));
            t += step;
        }

        let mut snapshots = state.snapshots.write().await;
        for ((profile, _), last) in group.iter().zip(previous) {
            if let Some(last) = last {
                snapshots.insert(profile.id.clone(), last);
            }
        }
    }

    let log_from = now - Duration::seconds(LOG_BACKFILL_SECS);
    let mut lines: Vec<LogEvent> = Vec::new();
    for profile in profiles {
        let Some(member) = estate::member(&profile.id) else { continue };
        lines.extend(logs::between(&script, member, &profile.id, &profile.name, log_from, now, usize::MAX));
        state.watermarks.write().await.insert(profile.id.clone(), now);
    }
    lines.sort_by_key(|l| l.timestamp);
    state.logs.write().await.push_many(lines);
}

/// Everything root holds about one connection, gone.
async fn forget(state: &Arc<AppState>, profile_id: &str) {
    state.snapshots.write().await.remove(profile_id);
    state.watermarks.write().await.remove(profile_id);
    state.history.write().await.drop_profile(profile_id);
    state.changes.write().await.forget(profile_id);
    state.alerts.write().await.drop_profile(profile_id);
    state.logs.write().await.drop_profile(profile_id);
}

// -------------------------------------------------------------- lifecycle

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// Simulated connections in the config.
    pub connections: usize,
    /// Real connections switched off while the demo runs.
    pub paused: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playing: Option<Playing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Upcoming>,
    pub stories: Vec<Choice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playing {
    pub story: &'static str,
    pub title: &'static str,
    pub phase: &'static str,
    pub started_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub manual: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Upcoming {
    pub story: &'static str,
    pub title: &'static str,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: &'static str,
    pub title: &'static str,
}

pub fn status(config: &AppConfig) -> DemoStatus {
    let now = Utc::now();
    let script = config.demo.started_at.map(|started_at| Script {
        started_at,
        seed: config.demo.seed,
        triggers: config
            .demo
            .triggers
            .iter()
            .filter_map(|t| Some((t.at, Story::from_id(&t.story)?)))
            .collect(),
    });
    let playing = script.as_ref().and_then(|s| s.active(now)).map(|i| Playing {
        story: i.story.id(),
        title: i.story.title(),
        phase: i.story.phase(i.tau(now)),
        started_at: i.start,
        ends_at: i.end(),
        manual: i.manual,
    });
    let next = script.as_ref().and_then(|s| s.next(now)).map(|i| Upcoming {
        story: i.story.id(),
        title: i.story.title(),
        at: i.start,
    });
    DemoStatus {
        running: config.demo.running(),
        started_at: config.demo.started_at,
        connections: config.profiles.iter().filter(|p| p.simulated).count(),
        paused: config.demo.paused.len(),
        playing,
        next,
        stories: Story::ALL
            .iter()
            .map(|s| Choice {
                id: s.id(),
                title: s.title(),
            })
            .collect(),
    }
}

/// Start the demo: add the simulated estate and, when asked, switch the real
/// connections off until it ends.
pub async fn start(state: &Arc<AppState>, pause_others: bool) -> Result<AppConfig> {
    let paused: Vec<String> = {
        let mut config = state.config.write().await;
        if config.demo.running() {
            return Ok(config.clone());
        }
        config.profiles.retain(|p| !p.simulated);
        let mut paused = Vec::new();
        if pause_others {
            for profile in config.profiles.iter_mut().filter(|p| p.enabled) {
                profile.enabled = false;
                paused.push(profile.id.clone());
            }
        }
        let now = Utc::now();
        config.demo = DemoSettings {
            // Whole seconds, so the story grid lands on round numbers.
            started_at: DateTime::from_timestamp(now.timestamp(), 0),
            seed: noise::hash(&[now.timestamp_nanos_opt().unwrap_or(0) as u64, 0xDE30]),
            paused: paused.clone(),
            triggers: Vec::new(),
        };
        config.profiles.extend(estate::profiles());
        paused
    };
    state.save_config().await?;

    // A switched-off connection's alerts would otherwise stay open forever:
    // nothing polls it to resolve them.
    for id in &paused {
        crate::supervisor::stop_profile(state, id).await;
        state.alerts.write().await.drop_profile(id);
    }
    crate::supervisor::reload_all(Arc::clone(state)).await;
    Ok(state.config.read().await.clone())
}

/// End the demo: remove the simulated estate and everything it left behind,
/// and switch back on what it switched off.
pub async fn stop(state: &Arc<AppState>) -> Result<AppConfig> {
    let removed: Vec<String> = {
        let mut config = state.config.write().await;
        let removed: Vec<String> = config.profiles.iter().filter(|p| p.simulated).map(|p| p.id.clone()).collect();
        config.profiles.retain(|p| !p.simulated);
        let paused = std::mem::take(&mut config.demo.paused);
        for profile in config.profiles.iter_mut().filter(|p| paused.contains(&p.id)) {
            profile.enabled = true;
        }
        config.demo = DemoSettings::default();
        removed
    };
    state.save_config().await?;
    configure(&DemoSettings::default());

    for id in &removed {
        crate::supervisor::stop_profile(state, id).await;
        forget(state, id).await;
        state.readings.write().await.retain(|key, _| !key.starts_with(&format!("{id}:")));
    }
    // Incidents are derived on read, but their cached analyses are not.
    state.analyses.write().await.clear();
    crate::supervisor::reload_all(Arc::clone(state)).await;
    Ok(state.config.read().await.clone())
}

/// Start a story now — or as soon as the one playing has finished.
pub async fn trigger(state: &Arc<AppState>, story: Option<String>) -> Result<DemoStatus> {
    {
        let mut config = state.config.write().await;
        if !config.demo.running() {
            return Err(AppError::Config("the live demo is not running".to_string()));
        }
        let script = script().ok_or_else(|| AppError::Config("the live demo is not running".to_string()))?;
        let now = Utc::now();
        let at = script.trigger_time(now);
        let chosen = match story.as_deref().map(Story::from_id) {
            Some(Some(story)) => story,
            Some(None) => {
                return Err(AppError::Config(format!(
                    "no story `{}`",
                    story.unwrap_or_default()
                )))
            }
            // The one after whatever played last, so repeated presses walk
            // through all three.
            None => {
                let last = script.instances(at).last().map(|i| i.story);
                let index = last
                    .and_then(|s| Story::ALL.iter().position(|x| *x == s))
                    .map(|i| (i + 1) % Story::ALL.len())
                    .unwrap_or(0);
                Story::ALL[index]
            }
        };
        config.demo.triggers.push(DemoTrigger {
            at,
            story: chosen.id().to_string(),
        });
        // Old triggers are part of the replayed past only for two hours.
        let horizon = now - Duration::seconds(BACKFILL_SECS + 3600);
        config.demo.triggers.retain(|t| t.at > horizon);
        configure(&config.demo);
    }
    state.save_config().await?;
    Ok(status(&*state.config.read().await))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The simulation's clock is process-wide; tests that set it take turns.
    static WORLD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn a_simulated_collector_answers_every_member() {
        let _turn = WORLD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        configure(&DemoSettings {
            started_at: Some(Utc::now() - Duration::minutes(3)),
            seed: 1,
            paused: Vec::new(),
            triggers: Vec::new(),
        });
        for profile in estate::profiles() {
            let collector = SimCollector::new(&profile).unwrap();
            let snapshot = collector.collect().await.unwrap();
            assert_eq!(snapshot.profile_id, profile.id);
            crate::collectors::validate::assert_renderable(&snapshot);
            let _ = collector.fetch_logs(Utc::now() - Duration::minutes(1), 500).await.unwrap();
            let _ = collector.config().await.unwrap();
        }
        configure(&DemoSettings::default());
    }

    /// The whole point, end to end: replay the past into a real state, stop
    /// at the worst of the bad deploy, and ask the incident engine what it
    /// sees. It must be one incident, led by the Docker host the deploy
    /// landed on, with the database it starved pulled in behind it.
    #[tokio::test]
    async fn the_bad_deploy_is_one_incident_led_by_the_host_it_landed_on() {
        let _turn = WORLD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let started_at = DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap()
            - Duration::seconds(story::LEAD_SECS + 250);
        let demo = DemoSettings {
            started_at: Some(started_at),
            seed: 42,
            paused: Vec::new(),
            triggers: Vec::new(),
        };
        let profiles = estate::profiles();
        let config = AppConfig {
            profiles: profiles.clone(),
            demo: demo.clone(),
            ..Default::default()
        };
        let state = Arc::new(AppState::new(config, std::env::temp_dir().join("root-sim-test.json")));
        configure(&demo);
        backfill(&state, &profiles).await;

        let alerts = state.alerts.read().await.all();
        let snapshots: Vec<HealthSnapshot> = state.snapshots.read().await.values().cloned().collect();
        let topology = crate::topology::Topology::build(&profiles);
        let incidents = crate::incidents::correlate(&alerts, &snapshots, &topology);

        let deploy = incidents
            .iter()
            .find(|i| i.profile_ids.iter().any(|p| p == "sim-prod-docker"))
            .unwrap_or_else(|| panic!("no incident on prod-docker: {incidents:#?}"));
        assert_eq!(deploy.lead_profile_id, "sim-prod-docker", "{deploy:#?}");
        for member in ["sim-orders-db", "sim-checkout", "sim-edge-lb"] {
            assert!(deploy.profile_ids.iter().any(|p| p == member), "{member} missing: {deploy:#?}");
        }

        // The deploy itself is on the timeline, before the trouble.
        let changes = state.changes.read().await.query(Some("sim-prod-docker"), None, 500);
        assert!(
            changes.iter().any(|c| c.component.starts_with("checkout-api") && c.to.ends_with(":2.41.0")),
            "{:#?}",
            changes.iter().map(|c| &c.summary).collect::<Vec<_>>()
        );

        // History is there to draw.
        let series = state.history.read().await.series("sim-orders-db", None);
        let connections = series.iter().find(|s| s.key == "connections").expect("a connections series");
        assert!(connections.points.len() > 600, "{}", connections.points.len());
        assert!(!state.logs.read().await.is_empty());
        configure(&DemoSettings::default());
    }
}
