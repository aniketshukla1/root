//! Database collector tests against real PostgreSQL and MySQL servers.
//!
//! The SQL in this collector is the part least likely to fail loudly: a wrong
//! column name or an unsupported function shows up as a silently-zero metric,
//! not a crash, because the collector deliberately falls back rather than
//! erroring a whole snapshot. So these run the real queries against real
//! servers and assert the values actually arrived.
//!
//! Both tests skip cleanly when no server is reachable, so the suite still
//! passes on a machine without them. To run them:
//!
//! ```bash
//! docker run -d --name pg -e POSTGRES_PASSWORD=testpw -e POSTGRES_USER=monitor \
//!     -e POSTGRES_DB=appdb -p 55432:5432 postgres:16-alpine
//! docker run -d --name mysql -e MYSQL_ROOT_PASSWORD=testpw -e MYSQL_DATABASE=appdb \
//!     -p 53306:3306 mysql:8
//! ```

use inframonitoring_lib::collectors::{build_collector, Collector, DetailQuery};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{Health, HealthSnapshot, ServiceKind};

use chrono::Utc;

const PG_URL: &str = "postgres://localhost:55432/appdb";
const MYSQL_URL: &str = "mysql://localhost:53306/appdb";

fn profile(kind: ServiceKind, endpoint: &str, username: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: "db-test".to_string(),
        name: "db".to_string(),
        kind,
        endpoint: endpoint.to_string(),
        username: Some(username.to_string()),
        has_secret: false,
        enabled: true,
        poll_interval_secs: 10,
        options: {
            // Everything already running counts as "long" so the long-query
            // path is exercised rather than skipped.
            let mut options = ProfileOptions::default();
            options.set("slowQuerySecs", serde_json::json!(0));
            options
        },
        depends_on: Vec::new(),
            config_sources: Vec::new(),
            history_source: None,
            snapshot_sink: None,
            runtime: Default::default(),
        metadata: ProfileOptions::default(),
    }
}

fn metric(snapshot: &HealthSnapshot, key: &str) -> f64 {
    snapshot
        .metrics
        .iter()
        .find(|m| m.key == key)
        .unwrap_or_else(|| {
            panic!(
                "no `{key}` metric; got {:?}",
                snapshot.metrics.iter().map(|m| &m.key).collect::<Vec<_>>()
            )
        })
        .value
}

/// Build a collector and confirm the server is actually up, or signal a skip.
async fn connect(
    kind: ServiceKind,
    url: &str,
    user: &str,
    password: &str,
) -> Option<(std::sync::Arc<dyn Collector>, HealthSnapshot)> {
    let collector = build_collector(&profile(kind, url, user), Some(password.to_string()))
        .await
        .ok()?;
    let snapshot = collector.collect().await.ok()?;
    if snapshot.error.is_some() {
        eprintln!("skipped: no {} at {url}", kind.label());
        return None;
    }
    Some((collector, snapshot))
}

#[tokio::test]
async fn postgres_collects_real_metrics_and_diagnostics() {
    let Some((collector, snapshot)) =
        connect(ServiceKind::Postgres, PG_URL, "monitor", "testpw").await
    else {
        return;
    };

    eprintln!("postgres: {}", snapshot.summary);

    assert_eq!(snapshot.kind, ServiceKind::Postgres);
    assert_ne!(snapshot.health, Health::Down);

    // Each of these comes from a separate query. A wrong column name or an
    // unsupported function degrades to the 0.0 fallback, so a non-zero value is
    // proof the SQL actually ran.
    assert!(metric(&snapshot, "connections") > 0.0, "pg_stat_activity query works");
    assert!(
        metric(&snapshot, "max_connections") > 0.0,
        "pg_settings lookup works"
    );
    assert!(metric(&snapshot, "db_size") > 0.0, "pg_database_size works");
    assert!(
        metric(&snapshot, "cache_hit_ratio") > 0.0,
        "the pg_stat_database aggregate works"
    );
    // A fresh primary: no lag, no blocked locks.
    assert_eq!(metric(&snapshot, "replication_lag"), 0.0);
    assert_eq!(metric(&snapshot, "blocked_locks"), 0.0);

    // Databases are enumerated in their own section, separate from queries.
    let databases = snapshot
        .sections
        .iter()
        .find(|s| s.id == "databases")
        .expect("a databases section exists");
    assert!(
        databases.rows.iter().any(|r| r.name.contains("appdb")),
        "the target database is listed: {:?}",
        databases.rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );

    // Active queries, when present, are their own section — and the statement
    // must arrive whole rather than truncated into a shared detail column.
    if let Some(queries) = snapshot.sections.iter().find(|s| s.id == "active_queries") {
        assert!(
            queries.columns.iter().any(|c| c.key == "query"),
            "the statement has its own column"
        );
        for row in &queries.rows {
            assert!(row.cells.contains_key("pid"), "pid is a cell, not part of the name");
            assert!(row.cells.contains_key("duration"));
            assert!(
                row.actions.iter().any(|a| a.tool == "terminate_database_query"),
                "each running query offers a terminate action"
            );
        }
    }

    // Every named diagnostic must actually execute — these are what the
    // assistant calls, and `make_interval`/`pg_blocking_pids` are exactly the
    // sort of thing that is easy to get wrong.
    for check in ["connections", "long_queries", "table_sizes", "locks", "replication"] {
        let value = collector
            .detail(&DetailQuery::DbDiagnostic {
                check: check.to_string(),
            })
            .await
            .unwrap_or_else(|e| panic!("diagnostic `{check}` failed: {e}"));
        eprintln!("  postgres/{check}: {value}");

        // An empty array proves the statement ran but says nothing about
        // whether the columns were read correctly, so check the shape too.
        if check == "table_sizes" {
            if let Some(first) = value.as_array().and_then(|a| a.first()) {
                assert!(first["table"].as_str().is_some_and(|t| !t.is_empty()));
                assert!(first["bytes"].as_i64().unwrap_or(0) > 0, "size was read");
            }
        }
    }

    let replication = collector
        .detail(&DetailQuery::DbDiagnostic {
            check: "replication".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(replication["role"], "primary");

    // Unknown checks are rejected rather than silently returning nothing.
    assert!(collector
        .detail(&DetailQuery::DbDiagnostic {
            check: "drop_everything".to_string(),
        })
        .await
        .is_err());

    // Synthesized log events must not error even when nothing is slow.
    collector
        .fetch_logs(Utc::now(), 50)
        .await
        .expect("log synthesis works");
}

#[tokio::test]
async fn mysql_collects_real_metrics_and_diagnostics() {
    let Some((collector, snapshot)) =
        connect(ServiceKind::Mysql, MYSQL_URL, "root", "testpw").await
    else {
        return;
    };

    eprintln!("mysql: {}", snapshot.summary);

    assert_eq!(snapshot.kind, ServiceKind::Mysql);
    assert_ne!(snapshot.health, Health::Down);

    // SHOW GLOBAL STATUS/VARIABLES return their values as strings, so these
    // being non-zero proves the row was found *and* parsed.
    assert!(
        metric(&snapshot, "connections") > 0.0,
        "Threads_connected was read and parsed"
    );
    assert!(
        metric(&snapshot, "max_connections") > 0.0,
        "the max_connections variable was read and parsed"
    );
    assert!(metric(&snapshot, "threads_running") > 0.0);

    for check in ["connections", "long_queries", "table_sizes", "locks", "replication"] {
        let value = collector
            .detail(&DetailQuery::DbDiagnostic {
                check: check.to_string(),
            })
            .await
            .unwrap_or_else(|e| panic!("diagnostic `{check}` failed: {e}"));
        eprintln!("  mysql/{check}: {value}");

        if check == "table_sizes" {
            if let Some(first) = value.as_array().and_then(|a| a.first()) {
                assert!(first["table"].as_str().is_some_and(|t| !t.is_empty()));
                assert!(first["bytes"].as_f64().unwrap_or(0.0) > 0.0, "size was read");
            }
        }

        // The event scheduler sits in the processlist forever. Counting it as a
        // long-running query would mean a permanent false alert on every
        // MySQL server.
        if check == "long_queries" {
            let queries = value["queries"].as_array().cloned().unwrap_or_default();
            assert!(
                queries.iter().all(|q| {
                    let text = q["query"].as_str().unwrap_or("");
                    !text.is_empty() && !q["state"].as_str().unwrap_or("").contains("empty queue")
                }),
                "idle daemon threads leaked into long_queries: {queries:?}"
            );
        }
    }

    // No replica configured, so this reports a primary with no lag.
    let replication = collector
        .detail(&DetailQuery::DbDiagnostic {
            check: "replication".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(replication["role"], "primary");

    collector
        .fetch_logs(Utc::now(), 50)
        .await
        .expect("log synthesis works");
}

#[tokio::test]
async fn a_bad_password_is_reported_as_unreachable_not_an_error() {
    let collector = build_collector(
        &profile(ServiceKind::Postgres, PG_URL, "monitor"),
        Some("definitely-wrong".to_string()),
    )
    .await
    .expect("a lazy pool builds regardless of credentials");

    let snapshot = collector.collect().await.expect("returns a snapshot, not Err");
    if snapshot.error.is_none() {
        eprintln!("skipped: no PostgreSQL to reject the connection");
        return;
    }
    // The dashboard needs a red tile with the reason, not a vanished row.
    assert_eq!(snapshot.health, Health::Down);
    assert!(snapshot.error.is_some());
}
