//! The splice, against a real Postgres.
//!
//! The ring buffer and the database overlap by construction: the store has
//! been recording the same hours the buffer is still holding. That overlap is
//! the whole risk of this layer, and it fails in a way that looks fine —
//! a chart that draws the last three hours twice, at two resolutions, with the
//! seam somewhere in the middle and nothing to say which half is which.
//!
//! So these tests assert on the joined series as a whole rather than on either
//! source: timestamps strictly increasing, no hour appearing twice, and the
//! live samples surviving at the resolution they were collected at.
//!
//! Skips without a server, exactly like `store.rs`:
//!
//! ```bash
//! docker run --rm -d -p 55433:5432 -e POSTGRES_PASSWORD=testpw \
//!   -e POSTGRES_DB=roottest --name root-store-test timescale/timescaledb:latest-pg16
//! ```

use std::sync::Arc;

use chrono::{Duration, Utc};
use inframonitoring_lib::commands;
use inframonitoring_lib::config::AppConfig;
use inframonitoring_lib::model::{Health, HealthSnapshot, Metric, MetricUnit, ServiceKind};
use inframonitoring_lib::state::AppState;
use inframonitoring_lib::store::Store;

const URL: &str = "postgres://postgres:testpw@localhost:55433/roottest";

async fn store_for(test: &str) -> Option<Store> {
    let admin = match sqlx::PgPool::connect(URL).await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("skipped: no Postgres at {URL} ({e})");
            return None;
        }
    };
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
        health: Health::Healthy,
        summary: "9 of 9 containers up".into(),
        metrics: vec![Metric::new("memory", "Memory", memory, MetricUnit::Bytes)],
        sections: Vec::new(),
        checked_at: at,
        latency_ms: 42,
        error: None,
    }
}

/// An estate where the database goes back two days and the buffer holds the
/// last two hours — the normal case, and the one where the two sources overlap.
async fn estate(test: &str) -> Option<Arc<AppState>> {
    let store = store_for(test).await?;
    let state = Arc::new(AppState::new(
        AppConfig::default(),
        std::path::PathBuf::from("/tmp/root-read-path-test.json"),
    ));

    // Two days on disk, one sample every fifteen minutes.
    let start = Utc::now() - Duration::days(2);
    for step in 0..192 {
        let at = start + Duration::minutes(step * 15);
        store.write_snapshot(&snapshot(1.0e9, at)).await.unwrap();
    }

    // The last two hours in memory, at the poll interval. These are the same
    // hours the store also has — that is the overlap.
    {
        let mut history = state.history.write().await;
        for step in 0..120 {
            let at = Utc::now() - Duration::hours(2) + Duration::minutes(step);
            history.record(&snapshot(2.0e9, at));
        }
    }

    *state.store.write().await = Some(store);
    Some(state)
}

#[tokio::test]
async fn a_long_window_reaches_past_the_buffer_and_onto_disk() {
    let Some(state) = estate("read_path_reaches_disk").await else {
        return;
    };

    // Two hours: inside the buffer, so the store is never asked.
    let short = commands::get_metric_history(&state, "p1".into(), Some(120))
        .await
        .unwrap();
    // Two days: past the buffer, so the rest comes off disk.
    let long = commands::get_metric_history(&state, "p1".into(), Some(2 * 24 * 60))
        .await
        .unwrap();

    let short_points = short[0].points.len();
    let long_points = long[0].points.len();
    assert!(
        long_points > short_points,
        "the long window should have reached further: {long_points} vs {short_points}"
    );

    let oldest = long[0].points.first().unwrap().t;
    assert!(
        oldest < Utc::now() - Duration::hours(6),
        "the series should start well before the buffer does, started {oldest}"
    );
}

#[tokio::test]
async fn the_overlap_is_spliced_and_not_concatenated() {
    // The bug this whole design is shaped around. Both sources hold the last
    // two hours; the joined series must contain them once.
    let Some(state) = estate("read_path_no_double_count").await else {
        return;
    };

    let series = commands::get_metric_history(&state, "p1".into(), Some(2 * 24 * 60))
        .await
        .unwrap();
    let points = &series[0].points;

    let mut previous = points[0].t;
    for point in &points[1..] {
        let at = point.t;
        assert!(
            at > previous,
            "timestamps must strictly increase across the seam: {previous} then {at}"
        );
        previous = at;
    }

    // Said a second way, because increasing timestamps alone would also hold
    // for a series that interleaved both sources neatly. The two fixtures
    // carry different values on purpose, so a point's value names its source:
    // nothing from the store may appear inside the hours the buffer covers.
    let buffer_starts = Utc::now() - Duration::hours(2);
    let intruders = points
        .iter()
        .filter(|p| p.t >= buffer_starts && p.v != 2.0e9)
        .count();
    assert_eq!(
        intruders, 0,
        "the store answered for hours the buffer already had"
    );
}

#[tokio::test]
async fn the_live_samples_keep_their_own_resolution() {
    // The buffer stays authoritative for what it covers. If the rollup were
    // allowed to win the overlap, the most recent hours — the ones anybody is
    // actually looking at — would be the coarsest on the chart.
    let Some(state) = estate("read_path_buffer_wins").await else {
        return;
    };

    let series = commands::get_metric_history(&state, "p1".into(), Some(2 * 24 * 60))
        .await
        .unwrap();
    let points = &series[0].points;

    let recent = points
        .iter()
        .filter(|p| p.t > Utc::now() - Duration::hours(2))
        .count();
    assert!(
        recent > 60,
        "the last two hours should still be minute-resolution, got {recent} points"
    );

    // The buffer wrote 2.0e9 and the store 1.0e9, so the value says which
    // source a point came from without needing to inspect the seam.
    let newest = points.last().unwrap().v;
    assert_eq!(newest, 2.0e9, "the newest point must come from the buffer");
}

#[tokio::test]
async fn an_hour_that_spiked_still_reads_as_a_spike_after_averaging() {
    // A rollup that reported only averages would quietly erase every breach
    // older than the buffer, which is the opposite of why history is kept.
    let Some(state) = estate("read_path_extremes_survive").await else {
        return;
    };

    // One enormous sample, a day ago, straight to disk.
    {
        let store = state.store.read().await;
        store
            .as_ref()
            .unwrap()
            .write_snapshot(&snapshot(9.9e9, Utc::now() - Duration::days(1)))
            .await
            .unwrap();
    }

    let series = commands::get_metric_history(&state, "p1".into(), Some(2 * 24 * 60))
        .await
        .unwrap();

    assert!(
        series[0].max >= 9.9e9,
        "the spike must reach the series max, which was {}",
        series[0].max
    );
}

#[tokio::test]
async fn without_a_database_the_buffer_answers_alone() {
    // The desktop default. Every one of these commands has to degrade to
    // "less history", never to an error.
    let state = Arc::new(AppState::new(
        AppConfig::default(),
        std::path::PathBuf::from("/tmp/root-read-path-nodb.json"),
    ));
    {
        let mut history = state.history.write().await;
        history.record(&snapshot(1.0e9, Utc::now() - Duration::minutes(5)));
        history.record(&snapshot(1.1e9, Utc::now()));
    }

    let series = commands::get_metric_history(&state, "p1".into(), Some(2 * 24 * 60))
        .await
        .expect("no database is not an error");
    assert_eq!(series[0].points.len(), 2);

    assert!(commands::get_alert_history(&state, "p1".into(), Some(30)).await.unwrap().is_empty());
    assert!(commands::get_flapping(&state, Some(30)).await.unwrap().is_empty());
    assert!(commands::get_change_history(&state, None, Some(30)).await.unwrap().is_empty());
    assert!(!commands::get_history_stats(&state).await.unwrap().configured);
}
