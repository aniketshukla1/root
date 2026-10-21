//! The contract between this backend and the screens, checked by a compiler.
//!
//! The UI's types are written by hand in `src/types.ts`, and so were the
//! fixtures the screens were built against — so a field the backend renamed,
//! or never sent, could hide for months behind fixtures that agreed with the
//! types and not with the server. This test serialises real backend values,
//! built the way the commands build them, into `src/contracts.generated.ts`
//! as typed constants. `tsc` then holds them against the types: a field one
//! side has and the other lacks, a wrong type, or a value outside a union
//! fails the frontend build. CI also fails when the file is not current.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::commands::{ExploreResult, ObservedEdge, ServiceMap};
use crate::history::{MetricSeries, Point};
use crate::incident_log::{Change, IncidentLog, StatusPageSettings};
use crate::incidents::{Confidence, Incident};
use crate::model::*;
use crate::rules::{AlertRule, Condition, Firing, Op, RulePreview, Silence};
use crate::slo::{Indicator, Slo, SloStatus};
use crate::traces::TraceSummary;

fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minutes)
}

fn points() -> Vec<Point> {
    (0..4).map(|i| Point { t: at(i), v: 40.0 + i as f64 }).collect()
}

fn series() -> MetricSeries {
    MetricSeries {
        key: "cpu".into(),
        label: "CPU".into(),
        unit: MetricUnit::Percent,
        warn_above: Some(75.0),
        points: points(),
        change_pct: Some(7.5),
        min: 40.0,
        max: 43.0,
        latest: 43.0,
        trend: Some(crate::forecast::Trend { per_second: 0.016, fit: 0.9, window_secs: 180.0, samples: 4, seconds_to_limit: Some(2000.0) }),
    }
}

fn alert() -> Alert {
    Alert {
        id: "a1".into(),
        fingerprint: "orders:containers:worker".into(),
        profile_id: "orders".into(),
        profile_name: "orders-api".into(),
        kind: ServiceKind::Docker,
        severity: AlertSeverity::Critical,
        title: "orders-api · worker".into(),
        detail: "exited with code 137".into(),
        component: Some("worker".into()),
        first_seen: at(0),
        last_seen: at(3),
        active: true,
        acknowledged: false,
    }
}

fn snapshot() -> HealthSnapshot {
    let mut containers = Section::new(
        "containers",
        "Containers",
        vec![
            ColumnSpec::text("name", "Name").primary(),
            ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
            ColumnSpec::metric("memory", "Memory", MetricUnit::Bytes),
            ColumnSpec::timestamp("started", "Started"),
        ],
    )
    .tracked();
    containers.rows = vec![Row::new("worker", "worker", Health::Down)
        .summary("exited with code 137")
        .cell("name", CellValue::text("worker"))
        .cell("cpu", CellValue::Gauge { value: 12.0, max: Some(100.0), warn_above: Some(80.0) })
        .cell("memory", CellValue::number(512.0 * 1024.0 * 1024.0))
        .cell("started", CellValue::timestamp(at(-60)))
        .cell("state", CellValue::Status { value: Health::Down })
        .cell("ports", CellValue::Tags { value: vec!["8080".into()] })
        .cell("uptime", CellValue::Duration { value: 3600.0 })
        .cell("note", CellValue::Null)];
    HealthSnapshot {
        profile_id: "orders".into(),
        profile_name: "orders-api".into(),
        kind: ServiceKind::Docker,
        health: Health::Down,
        summary: "1 of 3 containers exited".into(),
        metrics: vec![Metric { warn_above: Some(75.0), ..Metric::new("cpu", "CPU", 43.0, MetricUnit::Percent) }],
        sections: vec![containers],
        checked_at: at(3),
        latency_ms: 42,
        error: None,
    }
}

fn record() -> (IncidentLog, Vec<crate::incident_log::IncidentRecord>) {
    let mut log = IncidentLog::default();
    let incident = Incident {
        id: "inc-1".into(),
        title: "orders-api — 3 containers exited".into(),
        severity: AlertSeverity::Critical,
        health: Health::Down,
        started: at(0),
        last_seen: at(3),
        profile_ids: vec!["orders".into()],
        alert_ids: vec!["a1".into()],
        roots: vec!["orders".into()],
        lead_profile_id: "orders".into(),
        acknowledged: false,
        confidence: Confidence::High,
        grouping: Vec::new(),
    };
    let names: HashMap<String, String> = [("orders".to_string(), "orders-api".to_string())].into();
    log.sync(&[incident], &names, at(1));
    log.apply("inc-1", Change::Owner { owner: Some("Priya".into()) }, Some("Priya".into()), at(2)).unwrap();
    log.apply("inc-1", Change::Update { text: "Rolling back.".into() }, Some("Priya".into()), at(3)).unwrap();
    log.set_postmortem("inc-1", "## Summary\nA bad deploy.".into(), at(4)).unwrap();
    let records = log.recent(10);
    (log, records)
}

/// Every sample: its constant name, the TS type it must satisfy, and the value.
fn samples() -> Vec<(&'static str, &'static str, Value)> {
    fn v(value: impl Serialize) -> Value {
        serde_json::to_value(value).unwrap()
    }
    let rule = AlertRule {
        id: "r1".into(),
        name: "Checkout errors".into(),
        enabled: true,
        severity: AlertSeverity::Warning,
        labels: BTreeMap::from([("team".to_string(), "payments".to_string())]),
        condition: Condition::Log { profile_id: "orders".into(), pattern: "declined".into(), window_secs: 300, op: Op::Above, threshold: 10.0 },
        for_secs: 300,
    };
    let slo = Slo {
        id: "s1".into(),
        profile_id: "orders".into(),
        name: "Orders availability".into(),
        objective: 99.9,
        window_days: 30,
        indicator: Indicator::Metric { metric_key: "error_rate".into(), op: Op::Below, threshold: 1.0 },
    };
    let (_, records) = record();
    let config: crate::config::AppConfig = serde_json::from_value(json!({
        "profiles": [{ "id": "orders", "name": "orders-api", "kind": "docker", "endpoint": "unix:///var/run/docker.sock" }],
        "dashboards": [{ "id": "d1", "title": "Checkout", "panels": [
            { "id": "p1", "title": "CPU", "kind": "timeseries", "w": 6, "h": 3, "query": { "source": "metric", "profileId": "$service", "metricKey": "cpu" } },
            { "id": "p2", "title": "Health", "kind": "state", "w": 6, "h": 2, "query": { "source": "health", "profileIds": [] } }
        ], "variables": { "service": "orders" } }],
        "alertRules": [rule],
        "silences": [{ "id": "x1", "ruleId": "r1", "startsAt": at(0), "endsAt": at(60), "comment": "deploying" }],
        "slos": [slo],
        "statusPage": { "enabled": true, "title": "Acme", "profileIds": ["orders"] }
    }))
    .unwrap();
    let dashboard = DashboardState {
        snapshots: vec![snapshot()],
        alerts: vec![alert()],
        predictions: Vec::new(),
        silenced: HashMap::from([("orders:containers:worker".to_string(), at(60))]),
        overall_health: Health::Down,
        services_total: 1,
        services_healthy: 0,
        services_down: 1,
        generated_at: at(3),
    };
    let explore = ExploreResult {
        series: vec![series()],
        matched: 1,
        lines: vec![LogEvent::new("telemetry", "telemetry", ServiceKind::Otlp, "orders", LogLevel::Error, "payment declined".into(), at(2))],
        traces: vec![TraceSummary { trace_id: "ab".repeat(16), root_service: "checkout".into(), root_name: "POST /checkout".into(), started_at: at(1), duration_ms: 250.0, error_count: Some(1) }],
    };
    let map = ServiceMap {
        edges: vec![ObservedEdge { from: "checkout".into(), to: "orders".into(), calls: 120, errors: 3, mean_ms: 42.5 }],
        traces: 20,
        unknown: vec!["stripe-api".into()],
    };
    let budget = SloStatus {
        slo: Slo { indicator: Indicator::Up, ..config.slos[0].clone() },
        sli: Some(99.95),
        covered: 0.5,
        remaining: Some(0.5),
        burn_1h: Some(2.0),
        burn_6h: None,
        burn_3d: Some(0.4),
        budget: points(),
        unit: MetricUnit::Percent,
    };
    let preview = RulePreview {
        series: Some(series()),
        firings: vec![Firing { start: at(1), end: at(2), peak: 12.0, ongoing: false }],
        summary: "Would have fired 1 time in the last 7 days, for 1m in all.".into(),
    };
    let status = crate::incident_log::status_view(
        &StatusPageSettings { enabled: true, title: "Acme".into(), profile_ids: Vec::new() },
        &[("orders".to_string(), "orders-api".to_string(), Health::Down)],
        &records,
        at(5),
    );
    let audit = crate::audit::AuditEntry {
        at: at(6),
        actor: "Priya".into(),
        email: "priya@example.com".into(),
        role: crate::auth::Role::Viewer,
        command: "save_silence".into(),
        target: Some("x1".into()),
        outcome: crate::audit::Outcome::Refused,
        detail: Some("`save_silence` changes something, and this account can only read".into()),
    };
    let update = crate::tauri_cmds::UpdateInfo { version: "0.2.0".into(), current_version: "0.1.0".into(), notes: None };
    let silence = Silence { id: "x2".into(), rule_id: None, profile_id: Some("orders".into()), fingerprint: None, starts_at: at(10), ends_at: at(70), comment: "maintenance".into() };
    vec![
        ("dashboardState", "DashboardState", v(dashboard)),
        ("appConfig", "AppConfig", v(&config)),
        ("metricSeries", "MetricSeries", v(series())),
        ("exploreResult", "ExploreResult", v(explore)),
        ("serviceMap", "ServiceMap", v(map)),
        ("sloStatus", "SloStatus", v(budget)),
        ("rulePreview", "RulePreview", v(preview)),
        ("silence", "Silence", v(silence)),
        ("incidentRecord", "IncidentRecord", v(&records[0])),
        ("statusPageView", "StatusPageView", v(status)),
        ("auditEntry", "AuditEntry", v(audit)),
        ("updateInfo", "UpdateInfo", v(update)),
    ]
}

#[test]
fn contracts_are_written_where_tsc_will_check_them() {
    let samples = samples();
    let mut types: Vec<&str> = samples.iter().map(|(_, ty, _)| *ty).collect();
    types.sort();
    types.dedup();
    let mut out = String::from(
        "// Generated by `cargo test --lib contracts` — do not edit by hand.\n\
         // Real values from the backend's own serialisation (src-tauri/src/contracts.rs),\n\
         // held against src/types.ts by tsc: a field one side has and the other lacks fails\n\
         // the build. Regenerate after changing either side.\n",
    );
    out.push_str(&format!("import type {{ {} }} from \"./types\";\n", types.join(", ")));
    for (name, ty, value) in samples {
        out.push_str(&format!("\nexport const {name}: {ty} = {};\n", serde_json::to_string_pretty(&value).unwrap()));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/contracts.generated.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(out.as_str()) {
        std::fs::write(&path, out).expect("contracts written");
    }
}
