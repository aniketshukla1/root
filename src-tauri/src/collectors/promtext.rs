//! The Prometheus text exposition format.
//!
//! Parsed here rather than pulled in as a dependency: the subset real
//! exporters emit is plain `name{labels} value` lines, and the whole grammar
//! that matters fits in this file.
//!
//! Shared by every collector that reads a `/metrics` endpoint — etcd's own,
//! and node_exporter for a Linux host. It was written inside the etcd
//! collector first and moved out the moment a second caller existed, rather
//! than being copied.

use std::collections::BTreeMap;

/// One sample from the Prometheus text exposition format.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

/// Parse the Prometheus text format.
///
/// Only what etcd actually emits: `# HELP` and `# TYPE` comments, then
/// `name value` or `name{k="v",k2="v2"} value`. Histogram buckets come through
/// as ordinary samples with a `le` label, which is correct — nothing here
/// needs to reassemble them.
pub fn parse_prometheus(body: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // The value is the last whitespace-separated token; a label value may
        // contain spaces, so the split has to come from the right.
        let Some((head, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        // NaN, +Inf and -Inf are all legal in this format — and Rust parses
        // every one of them happily, so rejecting them takes an explicit
        // check. Letting one through means a quota percentage of NaN or a
        // byte count of infinity reaching the screen as if it were measured.
        let Ok(value) = value.trim().parse::<f64>() else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }

        let head = head.trim();
        let (name, labels) = match head.split_once('{') {
            Some((name, rest)) => (name, parse_labels(rest.trim_end_matches('}'))),
            None => (head, BTreeMap::new()),
        };

        out.push(Sample {
            name: name.trim().to_string(),
            labels,
            value,
        });
    }
    out
}

fn parse_labels(body: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    for pair in body.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            labels.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    labels
}

/// First sample with this name, ignoring labels.
pub fn gauge(samples: &[Sample], name: &str) -> Option<f64> {
    samples.iter().find(|s| s.name == name).map(|s| s.value)
}

/// Every sample with this name added together, for counters split by label.
pub fn total(samples: &[Sample], name: &str) -> Option<f64> {
    let matching: Vec<f64> = samples
        .iter()
        .filter(|s| s.name == name)
        .map(|s| s.value)
        .collect();
    (!matching.is_empty()).then(|| matching.iter().sum())
}

