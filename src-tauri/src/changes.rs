//! Deploy and change detection.
//!
//! Follows the same rule as the alert engine: collectors express intent and the
//! platform derives the signal. A collector tags one column as its revision —
//! the container image, the build number — and this module diffs that column
//! between consecutive snapshots. Nothing here knows what Docker or Jenkins is,
//! so a new collector gets deploy markers by tagging a column and writing no
//! detection code at all.
//!
//! Changes are deliberately *not* alerts. Nothing is wrong when a deploy
//! happens; there is no severity and nothing to acknowledge. They exist so a
//! spike in a chart can be read next to the thing that caused it.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};

use crate::model::{CellValue, ChangeEvent, ChangeKind, ChangeSource, HealthSnapshot};

/// Changes retained across all connections. Generous, because they are rare
/// compared with log lines and are what makes an old chart readable.
const CAPACITY: usize = 500;

/// The comparable form of a revision cell.
///
/// Only text and numbers identify a version. Tagging a duration or a gauge as
/// the revision column is a no-op rather than a source of phantom deploys —
/// the value moves every poll and would mark the timeline continuously.
fn revision_text(value: &CellValue) -> Option<String> {
    match value {
        CellValue::Text { value } if !value.trim().is_empty() => Some(value.trim().to_string()),
        // Build numbers arrive as metrics; render them without a trailing `.0`.
        CellValue::Number { value } if value.is_finite() => Some(format!("{}", *value as i64)),
        _ => None,
    }
}

/// Revision per row id, for one snapshot: `(section id, row id) -> (label, section label, revision)`.
fn revisions(snapshot: &HealthSnapshot) -> HashMap<(String, String), (String, String, String)> {
    let mut out = HashMap::new();

    for section in &snapshot.sections {
        let Some(column) = section.columns.iter().find(|c| c.revision) else {
            continue;
        };
        for row in &section.rows {
            let Some(cell) = row.cells.get(&column.key) else {
                continue;
            };
            let Some(revision) = revision_text(cell) else {
                continue;
            };
            out.insert(
                (section.id.clone(), row.id.clone()),
                (row.name.clone(), section.label.clone(), revision),
            );
        }
    }

    out
}

/// Diff two consecutive snapshots of the same connection.
///
/// Returns nothing when `previous` is absent — the first poll would otherwise
/// report every container in the estate as a fresh deploy.
pub fn changes_from_snapshots(
    previous: Option<&HealthSnapshot>,
    current: &HealthSnapshot,
) -> Vec<ChangeEvent> {
    let Some(previous) = previous else {
        return Vec::new();
    };

    let before = revisions(previous);
    let after = revisions(current);
    if before.is_empty() && after.is_empty() {
        return Vec::new();
    }

    let mut events = Vec::new();

    for (key, (component, section, revision)) in &after {
        let (change, from) = match before.get(key) {
            Some((_, _, old)) if old == revision => continue,
            Some((_, _, old)) => (ChangeKind::Deployed, Some(old.clone())),
            // A component the previous poll did not have. Only meaningful when
            // the section existed before; a collector that has just started
            // reporting a whole new section is not a wave of deploys.
            None if before.keys().any(|(s, _)| s == &key.0) => (ChangeKind::Appeared, None),
            None => continue,
        };

        let summary = match &from {
            Some(old) => format!("{component}: {old} → {revision}"),
            None => format!("{component} appeared at {revision}"),
        };

        events.push(ChangeEvent {
            // Deterministic: the same observation reported twice collapses
            // rather than drawing two markers a poll apart.
            id: format!("{}:{}:{}:{}", current.profile_id, key.0, key.1, revision),
            profile_id: current.profile_id.clone(),
            profile_name: current.profile_name.clone(),
            kind: current.kind,
            change,
            source: ChangeSource::Observed,
            section: section.clone(),
            component: component.clone(),
            from,
            to: revision.clone(),
            at: current.checked_at,
            summary,
        });
    }

    // Stable ordering so a poll that sees several changes always reports them
    // the same way.
    events.sort_by(|a, b| a.component.cmp(&b.component));
    events
}

/// Ring buffer of observed changes, newest last.
#[derive(Debug, Default)]
pub struct ChangeStore {
    events: VecDeque<ChangeEvent>,
}

impl ChangeStore {
    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(64),
        }
    }

    /// Adds only events not already recorded, returning the ones that were new.
    pub fn record(&mut self, incoming: Vec<ChangeEvent>) -> Vec<ChangeEvent> {
        let mut fresh = Vec::new();

        for event in incoming {
            if self.events.iter().any(|e| e.id == event.id) {
                continue;
            }
            if self.events.len() >= CAPACITY {
                self.events.pop_front();
            }
            self.events.push_back(event.clone());
            fresh.push(event);
        }

        fresh
    }

    /// Newest first, optionally scoped to one connection and a time window.
    pub fn query(
        &self,
        profile_id: Option<&str>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Vec<ChangeEvent> {
        self.events
            .iter()
            .rev()
            .filter(|e| profile_id.is_none_or(|id| e.profile_id == id))
            .filter(|e| since.is_none_or(|t| e.at >= t))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Drops everything for a connection that has been removed.
    pub fn forget(&mut self, profile_id: &str) {
        self.events.retain(|e| e.profile_id != profile_id);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellValue, ColumnSpec, Health, Row, Section, ServiceKind};
    use std::collections::HashMap;

    fn snapshot(rows: Vec<(&str, &str)>) -> HealthSnapshot {
        let section = Section::new(
            "containers",
            "Containers",
            vec![
                ColumnSpec::text("name", "Container").primary(),
                ColumnSpec::text("image", "Image").revision(),
            ],
        )
        .rows(
            rows.into_iter()
                .map(|(name, image)| {
                    let mut cells = HashMap::new();
                    cells.insert("image".to_string(), CellValue::text(image));
                    Row {
                        id: name.to_string(),
                        name: name.to_string(),
                        health: Health::Healthy,
                        summary: String::new(),
                        cells,
                        actions: vec![],
                        extra: serde_json::Value::Null,
                    }
                })
                .collect(),
        );

        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "prod-docker".into(),
            kind: ServiceKind::Docker,
            health: Health::Healthy,
            summary: String::new(),
            metrics: vec![],
            sections: vec![section],
            checked_at: Utc::now(),
            latency_ms: 1,
            error: None,
        }
    }

    #[test]
    fn first_poll_reports_nothing() {
        let current = snapshot(vec![("api", "acme/api:1.0")]);
        assert!(changes_from_snapshots(None, &current).is_empty());
    }

    #[test]
    fn unchanged_revision_reports_nothing() {
        let a = snapshot(vec![("api", "acme/api:1.0")]);
        let b = snapshot(vec![("api", "acme/api:1.0")]);
        assert!(changes_from_snapshots(Some(&a), &b).is_empty());
    }

    #[test]
    fn moved_revision_is_a_deploy() {
        let a = snapshot(vec![("api", "acme/api:1.0")]);
        let b = snapshot(vec![("api", "acme/api:1.1")]);
        let events = changes_from_snapshots(Some(&a), &b);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].change, ChangeKind::Deployed);
        assert_eq!(events[0].from.as_deref(), Some("acme/api:1.0"));
        assert_eq!(events[0].to, "acme/api:1.1");
    }

    #[test]
    fn new_row_in_an_existing_section_appeared() {
        let a = snapshot(vec![("api", "acme/api:1.0")]);
        let b = snapshot(vec![("api", "acme/api:1.0"), ("web", "acme/web:2.0")]);
        let events = changes_from_snapshots(Some(&a), &b);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].change, ChangeKind::Appeared);
        assert_eq!(events[0].component, "web");
    }

    #[test]
    fn store_deduplicates_by_id() {
        let a = snapshot(vec![("api", "acme/api:1.0")]);
        let b = snapshot(vec![("api", "acme/api:1.1")]);
        let events = changes_from_snapshots(Some(&a), &b);

        let mut store = ChangeStore::new();
        assert_eq!(store.record(events.clone()).len(), 1);
        // The same deploy observed again must not draw a second marker.
        assert_eq!(store.record(events).len(), 0);
        assert_eq!(store.len(), 1);
    }
}
