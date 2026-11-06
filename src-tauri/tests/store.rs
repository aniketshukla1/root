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

use chrono::{Duration, DurationRound, Utc};
use inframonitoring_lib::model::{
    Alert, AlertSeverity, ChangeEvent, ChangeKind, ChangeSource, Health, HealthSnapshot, LogEvent,
    LogLevel, LogStream, Metric, MetricUnit, ServiceKind,
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
        .metric_buckets("p1", "memory", base - Duration::hours(1))
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
        .metric_buckets("p1", "ratio", at - Duration::hours(1))
        .await
        .expect("read");
    let good = store
        .metric_buckets("p1", "real", at - Duration::hours(1))
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
        .metric_buckets("p1", "memory", at - Duration::hours(1))
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


// ---------------------------------------------------------------- read path
//
// The buffer and the store overlap by construction: the database has been
// recording the same hours the ring buffer is still holding. Everything below
// is about that seam, because a chart that draws the same afternoon twice at
// two resolutions is worse than one that stops short.

#[tokio::test]
async fn the_rollup_answers_further_back_than_a_ring_buffer_reaches() {
    // The whole reason this layer exists. forecast.rs refuses a window under
    // fifteen minutes and has only ever had three hours of buffer to look at;
    // this is where a longer window comes from.
    let Some(store) = store_for("rollup_reaches_further").await else {
        return;
    };

    // Two days of hourly samples.
    let start = Utc::now() - Duration::days(2);
    for hour in 0..48 {
        store
            .write_snapshot(&snapshot(1.0e9 + hour as f64 * 1.0e8, start + Duration::hours(hour)))
            .await
            .unwrap();
    }

    let recent = store
        .metric_buckets("p1", "memory", Utc::now() - Duration::hours(3))
        .await
        .unwrap();
    let everything = store
        .metric_buckets("p1", "memory", Utc::now() - Duration::days(3))
        .await
        .unwrap();

    assert!(
        everything.len() > recent.len(),
        "a longer window should reach more hours: {} vs {}",
        everything.len(),
        recent.len()
    );
    assert!(everything.len() >= 40, "two days should be ~48 buckets, got {}", everything.len());
}

#[tokio::test]
async fn every_bucket_carries_the_extremes_and_not_only_the_average() {
    // An hour whose peak breached a threshold has to still read as a breach
    // after it is averaged into one point, or a rollup quietly erases every
    // spike that ever mattered.
    let Some(store) = store_for("buckets_keep_extremes").await else {
        return;
    };

    // The start of an hour, so the three samples share a bucket whatever minute
    // the suite runs at. Without this it failed in the last ten minutes of every hour.
    let hour = (Utc::now() - Duration::hours(5)).duration_trunc(Duration::hours(1)).unwrap();
    for (i, value) in [1.0e9, 9.0e9, 2.0e9].into_iter().enumerate() {
        store
            .write_snapshot(&snapshot(value, hour + Duration::minutes(i as i64 * 5)))
            .await
            .unwrap();
    }

    let rows = store
        .metric_buckets("p1", "memory", hour - Duration::hours(1))
        .await
        .unwrap();
    let bucket = rows.iter().find(|r| r.count == 3).expect("one bucket of three samples");

    assert_eq!(bucket.min, 1.0e9);
    assert_eq!(bucket.max, 9.0e9, "the spike must survive the averaging");
    assert!(bucket.avg > 3.9e9 && bucket.avg < 4.1e9, "avg was {}", bucket.avg);
}

#[tokio::test]
async fn asking_for_a_window_with_nothing_in_it_is_empty_and_not_an_error() {
    // A connection added this morning has no history from last week, and the
    // chart should say so by being short rather than by failing.
    let Some(store) = store_for("empty_window_is_empty").await else {
        return;
    };
    store.write_snapshot(&snapshot(1.0e9, Utc::now())).await.unwrap();

    let ancient = store
        .metric_buckets("p1", "memory", Utc::now() - Duration::days(400))
        .await
        .expect("a question about the distant past is still a question");
    let unknown_metric = store
        .metric_buckets("p1", "nothing_by_this_name", Utc::now() - Duration::days(1))
        .await
        .unwrap();

    assert!(!ancient.is_empty(), "today's sample is inside a 400-day window");
    assert!(unknown_metric.is_empty());
}

#[tokio::test]
async fn history_survives_for_a_connection_that_is_no_longer_configured() {
    // Deleting a connection drops it from the live state. The record of what
    // it did is the point of keeping a record, so it stays until somebody
    // clears the history deliberately.
    let Some(store) = store_for("history_outlives_the_connection").await else {
        return;
    };
    store
        .write_alert(&alert("gone", Utc::now() - Duration::days(1), false), false)
        .await
        .unwrap();

    let history = store
        .alert_history("p1", Utc::now() - Duration::days(7), 50)
        .await
        .unwrap();
    assert_eq!(history.len(), 1, "the row outlives the connection");
}


#[tokio::test]
async fn a_change_reads_back_with_every_column_it_was_written_with() {
    // The read used to return three of the twelve columns the write had always
    // stored, which made the table unable to answer questions it was already
    // holding the data for.
    let Some(store) = store_for("changes_round_trip").await else {
        return;
    };

    let event = ChangeEvent {
        id: "c1".into(),
        profile_id: "p1".into(),
        profile_name: "prod-docker".into(),
        kind: ServiceKind::Docker,
        change: ChangeKind::Deployed,
        source: ChangeSource::Observed,
        section: "Containers".into(),
        component: "checkout-api".into(),
        from: Some("v1.4.2".into()),
        to: "v1.5.0".into(),
        at: Utc::now() - Duration::hours(2),
        summary: "checkout-api v1.4.2 to v1.5.0".into(),
    };
    store.write_change(&event).await.unwrap();

    let back = store
        .changes_since(None, Utc::now() - Duration::days(1), 50)
        .await
        .unwrap();
    assert_eq!(back.len(), 1);
    let row = &back[0];

    assert_eq!(row.id, "c1");
    assert_eq!(row.component.as_deref(), Some("checkout-api"));
    assert_eq!(row.section.as_deref(), Some("Containers"));
    assert_eq!(row.from.as_deref(), Some("v1.4.2"), "the previous revision is the point");
    assert_eq!(row.to.as_deref(), Some("v1.5.0"));
    assert_eq!(row.summary, "checkout-api v1.4.2 to v1.5.0");
}

#[tokio::test]
async fn changes_can_be_asked_for_one_connection_or_for_the_estate() {
    let Some(store) = store_for("changes_scoping").await else {
        return;
    };

    for (id, profile) in [("c1", "p1"), ("c2", "p2"), ("c3", "p1")] {
        let mut event = ChangeEvent {
            id: id.into(),
            profile_id: profile.into(),
            profile_name: profile.into(),
            kind: ServiceKind::Docker,
            change: ChangeKind::Deployed,
            source: ChangeSource::Observed,
            section: "Containers".into(),
            component: "api".into(),
            from: None,
            to: "v2".into(),
            at: Utc::now() - Duration::minutes(30),
            summary: format!("{id} changed"),
        };
        event.summary = format!("{id} changed");
        store.write_change(&event).await.unwrap();
    }

    let since = Utc::now() - Duration::days(1);
    let estate = store.changes_since(None, since, 50).await.unwrap();
    let one = store.changes_since(Some("p1"), since, 50).await.unwrap();

    assert_eq!(estate.len(), 3, "the estate sees every connection's changes");
    assert_eq!(one.len(), 2, "one connection sees only its own");
    assert!(one.iter().all(|c| c.profile_id == "p1"));
}


#[tokio::test]
async fn a_multi_word_kind_is_stored_the_way_the_rest_of_the_app_spells_it() {
    // `ServiceKind::LinuxHost` is `linux_host` to serde and `linuxhost` to
    // `Debug`. The column used to be written with the second and is now read
    // back, and the frontend resolves a kind against the registry — so the
    // wrong spelling is not a cosmetic difference, it is a row that renders as
    // an unknown service.
    let Some(store) = store_for("kind_spelling").await else {
        return;
    };

    let event = ChangeEvent {
        id: "c1".into(),
        profile_id: "p1".into(),
        profile_name: "build-box".into(),
        kind: ServiceKind::LinuxHost,
        change: ChangeKind::Appeared,
        source: ChangeSource::Reported,
        section: "Units".into(),
        component: "nginx.service".into(),
        from: None,
        to: "active".into(),
        at: Utc::now(),
        summary: "nginx.service appeared".into(),
    };
    store.write_change(&event).await.unwrap();

    let back = store
        .changes_since(None, Utc::now() - Duration::hours(1), 10)
        .await
        .unwrap();

    assert_eq!(back[0].kind, "linux_host", "not the Debug spelling");
    assert_eq!(back[0].change, "appeared");
    assert_eq!(back[0].source, "reported");

    // And the value serde itself produces, so this stays true if the rename
    // rule on the enum ever changes.
    let expected = serde_json::to_value(ServiceKind::LinuxHost).unwrap();
    assert_eq!(back[0].kind, expected.as_str().unwrap());
}
