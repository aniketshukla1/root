//! The estate's history, against a real Postgres.
//!
//! There is no mocking a database here. The whole value of this layer is what
//! survives the process, and a fake that answers from memory tests the one
//! property that does not matter. So these run against a server or they skip.
//!
//! What they pin, in order of how expensive it would be to get wrong:
//!
//!   - A snapshot written and then read back through the hourly rollup, since
//!     the aggregate and the fallback are two different queries that must
//!     agree. This is the one that catches a Timescale install diverging from
//!     a plain-Postgres one.
//!   - An alert that fires, resolves and fires again landing as *two* rows.
//!     The id is regenerated each time the engine raises it, so a key on the
//!     id would silently record one occurrence and make "how often does this
//!     happen" answer wrong rather than answer nothing.
//!   - Predictions staying out of the alert history, which is the same split
//!     the dashboard payload makes and is worth nothing if the database
//!     collapses it again.
//!   - Schema creation being idempotent, because it runs on every open.
//!
//! ```bash
//! docker run --rm -d -p 55433:5432 -e POSTGRES_PASSWORD=testpw \
//!   -e POSTGRES_DB=roottest --name root-store-test timescale/timescaledb:latest-pg16
//! ```
//!
//! Plain `postgres:16-alpine` works too — the Timescale statements are
//! optional by design, and `timescale_is_optional_not_required` is the test
//! that says so.

use chrono::{Duration, Utc};
use inframonitoring_lib::model::{
    Alert, AlertSeverity, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, ServiceKind,
};
use inframonitoring_lib::store::Store;

const URL: &str = "postgres://postgres:testpw@localhost:55433/roottest";

/// Open a store in a schema of this test's own, or signal a skip.
///
/// One schema per test, rather than truncating shared tables between them.
/// `cargo test` runs these in parallel, and a suite where each test wipes what
/// the others are asserting on is worse than no suite at all: the first run
/// passes, and every run after it fails somewhere unrelated to the change that
/// broke it. Asked for by name so a schema left behind after a failure says
/// which test left it.
async fn store_for(test: &str) -> Option<Store> {
    let admin = match sqlx::PgPool::connect(URL).await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("skipped: no Postgres at {URL} ({e})");
            return None;
        }
    };

    // The name is a literal in this file, never input, so interpolating it is
    // safe — but sqlx is right to make that decision explicit rather than
    // implicit.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {test} CASCADE")))
        .execute(&admin)
        .await
        .expect("drop schema");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {test}")))
        .execute(&admin)
        .await
        .expect("create schema");

    let scoped = format!("{URL}?options=-csearch_path%3D{test}");
    Some(Store::open(&scoped).await.expect("open in schema"))
}

fn snapshot(memory: f64, at: chrono::DateTime<Utc>) -> HealthSnapshot {
    HealthSnapshot {
        profile_id: "p1".into(),
        profile_name: "prod-docker".into(),
        kind: ServiceKind::Docker,
        health: Health::Degraded,
        summary: "2 of 9 containers unhealthy".into(),
        metrics: vec![
            Metric::new("memory", "Memory", memory, MetricUnit::Bytes),
            Metric::new("cpu", "CPU", 41.5, MetricUnit::Percent),
        ],
        sections: Vec::new(),
        checked_at: at,
        latency_ms: 42,
        error: None,
    }
}

fn alert(fingerprint: &str, first_seen: chrono::DateTime<Utc>, active: bool) -> Alert {
    Alert {
        // Regenerated every time the engine raises it — which is exactly why
        // the store must not key on it.
        id: uuid::Uuid::new_v4().to_string(),
        fingerprint: fingerprint.to_string(),
        profile_id: "p1".into(),
        profile_name: "prod-docker".into(),
        kind: ServiceKind::Docker,
        severity: AlertSeverity::Critical,
        // Varies with the condition, the way real titles do. A fixture where
        // every alert reads the same is what let a frequency query keyed on
        // the title look correct.
        title: format!("{fingerprint} exited"),
        detail: "Exited (137)".into(),
        component: Some("checkout-api".into()),
        first_seen,
        last_seen: first_seen + Duration::minutes(2),
        active,
        acknowledged: false,
    }
}

#[tokio::test]
async fn a_snapshot_written_now_is_readable_as_history_later() {
    let Some(store) = store_for("a_snapshot_written_now_is_readable_as_history_later").await
    else {
        return;
    };

    let base = Utc::now() - Duration::hours(2);
    for i in 0..4 {
        store
            .write_snapshot(&snapshot(1.0e9 * (i + 1) as f64, base + Duration::minutes(i * 5)))
            .await
            .expect("write");
    }

    let hours = store
        .metric_hours("p1", "memory", base - Duration::hours(1))
        .await
        .expect("read");

    assert!(!hours.is_empty(), "nothing came back");
    let total: i64 = hours.iter().map(|h| h.count).sum();
    assert_eq!(total, 4, "every sample should be in a bucket");
    assert_eq!(hours[0].min, 1.0e9);
    assert_eq!(hours.last().unwrap().max, 4.0e9);
}

#[tokio::test]
async fn a_value_that_is_not_a_number_is_left_out_rather_than_zeroed() {
    // A zero here would draw a cliff in a chart that nothing in the estate
    // did, which is worse than a gap.
    let Some(store) = store_for("a_value_that_is_not_a_number_is_left_out_rather_than_zeroed").await
    else {
        return;
    };

    let at = Utc::now() - Duration::minutes(10);
    let mut snap = snapshot(0.0, at);
    snap.metrics = vec![
        Metric::new("ratio", "Ratio", f64::NAN, MetricUnit::Count),
        Metric::new("real", "Real", 3.0, MetricUnit::Count),
    ];
    store.write_snapshot(&snap).await.expect("write");

    let bad = store
        .metric_hours("p1", "ratio", at - Duration::hours(1))
        .await
        .expect("read");
    let good = store
        .metric_hours("p1", "real", at - Duration::hours(1))
        .await
        .expect("read");

    assert!(bad.is_empty(), "NaN was stored");
    assert_eq!(good.len(), 1);
}

#[tokio::test]
async fn the_same_condition_firing_twice_is_two_rows() {
    // The reason the key is (fingerprint, first_seen) and not the alert id.
    // Keyed on the id this would be two rows for one occurrence; keyed on the
    // fingerprint alone it would be one row for two. Both make "how often
    // does this happen" answer wrongly rather than not at all.
    let Some(store) = store_for("the_same_condition_firing_twice_is_two_rows").await
    else {
        return;
    };

    let monday = Utc::now() - Duration::days(3);
    let tuesday = Utc::now() - Duration::days(2);

    store.write_alert(&alert("f1", monday, true), false).await.unwrap();
    store.write_alert(&alert("f1", monday, false), false).await.unwrap();
    store.write_alert(&alert("f1", tuesday, true), false).await.unwrap();

    let history = store
        .alert_history("p1", Utc::now() - Duration::days(7), 50)
        .await
        .expect("read");

    assert_eq!(history.len(), 2, "two occurrences, not one and not three");
    assert!(history[0].first_seen > history[1].first_seen, "newest first");
    assert!(
        history[1].resolved_at.is_some(),
        "the first occurrence ended and the row should say when"
    );
    assert!(history[0].resolved_at.is_none(), "the second is still open");
}

#[tokio::test]
async fn a_condition_that_keeps_firing_is_findable_by_how_often() {
    // One alert that fired forty times and forty that fired once look the
    // same in a list ordered by recency. This is the query that tells them
    // apart.
    let Some(store) = store_for("a_condition_that_keeps_firing_is_findable_by_how_often").await
    else {
        return;
    };

    for day in 1..=5 {
        store
            .write_alert(&alert("flappy", Utc::now() - Duration::days(day), false), false)
            .await
            .unwrap();
    }
    store
        .write_alert(&alert("once", Utc::now() - Duration::days(1), false), false)
        .await
        .unwrap();

    let frequency = store
        .alert_frequency(Utc::now() - Duration::days(30))
        .await
        .expect("read");

    assert_eq!(frequency.len(), 1, "a single occurrence is not a pattern");
    assert_eq!(frequency[0].2, 5, "five occurrences of one condition");
    assert!(frequency[0].1.contains("flappy"), "{:?}", frequency[0]);
}

#[tokio::test]
async fn a_forecast_never_arrives_in_the_alert_history() {
    // The same separation the dashboard payload makes, and worth nothing if
    // the database collapses it again.
    let Some(store) = store_for("a_forecast_never_arrives_in_the_alert_history").await
    else {
        return;
    };

    store
        .write_alert(&alert("real", Utc::now() - Duration::hours(2), true), false)
        .await
        .unwrap();
    store
        .write_alert(&alert("predict:soon", Utc::now() - Duration::hours(1), true), true)
        .await
        .unwrap();

    let history = store
        .alert_history("p1", Utc::now() - Duration::days(1), 50)
        .await
        .expect("read");

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].fingerprint, "real");
    assert!(!history[0].predicted);

    // Both are on disk — the forecast is kept, just never counted as an alert.
    let counts = store.counts().await.expect("counts");
    assert_eq!(counts["alerts"], 2);
}

#[tokio::test]
async fn log_lines_survive_the_process_that_wrote_them() {
    let Some(store) = store_for("log_lines_survive_the_process_that_wrote_them").await
    else {
        return;
    };

    let events: Vec<LogEvent> = (0..250)
        .map(|i| LogEvent {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: "p1".into(),
            profile_name: "prod-docker".into(),
            kind: ServiceKind::Docker,
            source: "checkout-api".into(),
            level: if i % 10 == 0 { LogLevel::Error } else { LogLevel::Info },
            stream: LogStream::Runtime,
            message: format!("request {i} completed"),
            timestamp: Utc::now() - Duration::seconds(i),
        })
        .collect();

    store.write_logs(&events).await.expect("write");
    let counts = store.counts().await.expect("counts");
    assert_eq!(counts["log_lines"], 250);
}

#[tokio::test]
async fn opening_the_same_database_twice_changes_nothing() {
    // The schema is applied on every open, so this runs on every launch.
    let Some(store) = store_for("opening_the_same_database_twice_changes_nothing").await
    else {
        return;
    };
    store
        .write_alert(&alert("f1", Utc::now() - Duration::hours(1), true), false)
        .await
        .unwrap();

    let reopened = Store::open(&format!(
        "{URL}?options=-csearch_path%3Dopening_the_same_database_twice_changes_nothing"
    ))
    .await
    .expect("second open");
    let counts = reopened.counts().await.expect("counts");
    assert_eq!(counts["alerts"], 1, "a reopen dropped or duplicated a row");
}

#[tokio::test]
async fn timescale_is_optional_not_required() {
    // Whichever server is running, the store opens and answers. On plain
    // Postgres `has_timescale` is false, the reads use the fallback query,
    // and every assertion above still holds — that is the contract.
    let Some(store) = store_for("timescale_is_optional_not_required").await else { return };

    let at = Utc::now() - Duration::minutes(30);
    store.write_snapshot(&snapshot(5.0e9, at)).await.expect("write");
    let hours = store
        .metric_hours("p1", "memory", at - Duration::hours(1))
        .await
        .expect("read");

    assert_eq!(hours.len(), 1, "the fallback and the aggregate must agree");
    assert_eq!(hours[0].avg, 5.0e9);
    eprintln!(
        "timescale: {} (both paths are expected to pass)",
        store.has_timescale()
    );
}

#[tokio::test]
async fn clearing_history_leaves_nothing_behind() {
    let Some(store) = store_for("clearing_history_leaves_nothing_behind").await else { return };

    store.write_snapshot(&snapshot(1.0e9, Utc::now())).await.unwrap();
    store
        .write_alert(&alert("f1", Utc::now(), true), false)
        .await
        .unwrap();

    store.clear().await.expect("clear");
    let counts = store.counts().await.expect("counts");
    assert!(
        counts.values().all(|&n| n == 0),
        "something survived a clear: {counts:?}"
    );
}
