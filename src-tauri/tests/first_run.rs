//! First run, end to end. The one click onboarding offers — watch this
//! computer — has to reach real data quickly, on any machine, with nothing
//! installed or typed. This saves the connection the way the onboarding card
//! does and waits for the real poller's first snapshot.

use std::sync::Arc;
use std::time::{Duration, Instant};

use inframonitoring_lib::commands;
use inframonitoring_lib::config::{AppConfig, ConnectionProfile};
use inframonitoring_lib::state::AppState;

#[tokio::test(flavor = "multi_thread")]
async fn watching_this_computer_shows_real_data_within_seconds() {
    let dir = std::env::temp_dir().join(format!("root-first-run-{}", uuid::Uuid::new_v4()));
    let state = Arc::new(AppState::new(AppConfig::default(), dir.join("config.json")));
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id": "this-computer",
        "name": "This computer",
        "kind": "this_machine",
        "endpoint": "",
        "pollIntervalSecs": 5,
    }))
    .unwrap();

    let started = Instant::now();
    commands::save_profile(&state, profile, None).await.expect("the profile saves");
    loop {
        if let Some(snapshot) = state.snapshots.read().await.get("this-computer").cloned() {
            let memory = snapshot.metrics.iter().find(|m| m.key == "memory_used");
            assert!(memory.is_some_and(|m| m.value > 0.0), "{:?}", snapshot.metrics);
            let processes = snapshot.sections.iter().find(|s| s.id == "processes").unwrap();
            assert!(!processes.rows.is_empty());
            println!("first snapshot after {:?}: {}", started.elapsed(), snapshot.summary);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "no snapshot within a minute");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
