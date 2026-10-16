//! Distributed traces — the one shape root's snapshot model cannot express.
//!
//! Everything else root collects is a list of things with numbers on them, and
//! `Section` plus `Row` renders that. A span is not that. It has a duration, a
//! parent, and a position in a tree, and forcing it into a table produces
//! something technically present and useless: the whole value of a trace is
//! seeing that the 900 ms was spent *here*, three levels down, in a service
//! nobody suspected.
//!
//! So traces get their own type. This module owns the shape; a collector fills
//! it from whatever its backend speaks.
//!
//! ## The tree is assembled here, not in the UI
//!
//! A backend hands back a flat list of spans with parent ids. Assembling that
//! into a tree in the frontend means doing it on every render and getting the
//! edge cases wrong once per component that tries. It happens once, here,
//! where the edge cases can be tested.
//!
//! ## Nothing is dropped
//!
//! Three things go wrong in real traces, and the tempting response to each is
//! to discard a span:
//!
//! - **A parent that is not in the trace.** Sampling dropped it, or a batch
//!   failed to export, or the query window cut it off. The child is often the
//!   most interesting span there is — it is where the request was when the
//!   trace went dark.
//! - **A cycle.** Malformed instrumentation can produce one. Descending it
//!   recursively does not terminate.
//! - **A duplicate span id.** A retried export.
//!
//! In all three the span is kept and attached at the root, flagged. A span
//! that vanishes silently is worse than a span in the wrong place, because the
//! operator cannot tell the difference between "it did not happen" and "we did
//! not show it".
//!
//! ## Self time, not total time
//!
//! A span that took 900 ms because its one child took 890 ms did not spend
//! 900 ms; it spent 10. Total time answers "how long did this take", which the
//! waterfall already shows. Self time answers "where did it go", which is the
//! question someone is actually asking at 3am, and it is the number the
//! per-service breakdown is built from.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// OTLP's three status codes. Anything unrecognized is `Unset`, which is what
/// OTLP itself says an unset status means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    #[default]
    Unset,
    Ok,
    Error,
}

impl SpanStatus {
    /// OTLP status codes as the wire carries them: `0` unset, `1` ok, `2`
    /// error. Some encoders send the enum name instead, so both are accepted.
    pub fn from_wire(value: &serde_json::Value) -> SpanStatus {
        match value {
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(1) => SpanStatus::Ok,
                Some(2) => SpanStatus::Error,
                _ => SpanStatus::Unset,
            },
            serde_json::Value::String(s) => {
                let upper = s.to_ascii_uppercase();
                if upper.contains("ERROR") {
                    SpanStatus::Error
                } else if upper.contains("OK") {
                    SpanStatus::Ok
                } else {
                    SpanStatus::Unset
                }
            }
            _ => SpanStatus::Unset,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvent {
    pub at: DateTime<Utc>,
    pub name: String,
}

/// One span, as root understands it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub name: String,
    pub service: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: f64,
    pub status: SpanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Sorted, because a waterfall that reorders attributes between renders is
    /// unreadable. `BTreeMap` does this by construction.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SpanEvent>,
}

/// A span placed in the tree, with everything a waterfall needs precomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceNode {
    pub span: Span,
    /// Milliseconds from the start of the trace. The renderer draws bars from
    /// this and never does date arithmetic.
    pub offset_ms: f64,
    /// Duration minus the time its children accounted for.
    pub self_ms: f64,
    pub depth: usize,
    /// This span named a parent the trace does not contain, or was reachable
    /// only through a cycle. It is shown at the root rather than hidden.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orphaned: bool,
    pub children: Vec<TraceNode>,
}

/// Where a trace spent its time, per service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTime {
    pub service: String,
    pub self_ms: f64,
    pub span_count: usize,
    pub error_count: usize,
}

/// One assembled trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trace {
    pub trace_id: String,
    pub root_service: String,
    pub root_name: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: f64,
    pub span_count: usize,
    pub error_count: usize,
    /// Spans whose parent is missing. Nonzero means the trace is partial, and
    /// the UI says so rather than drawing a confident tree of fragments.
    pub orphan_count: usize,
    /// Descending by self time: the first row is where the time went.
    pub services: Vec<ServiceTime>,
    pub roots: Vec<TraceNode>,
}

/// One row in a trace list, before anyone asks for the spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_service: String,
    pub root_name: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: f64,
    /// Present only when the backend reported it; a search result usually
    /// carries no error count, and inventing a zero would read as "no errors".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_count: Option<usize>,
}

impl Trace {
    /// Assemble a flat span list into a tree.
    ///
    /// Total ordering is by start time at every level, so two renders of the
    /// same trace are identical.
    pub fn assemble(trace_id: &str, spans: Vec<Span>) -> Trace {
        // First writer wins on a duplicate id: a retried export sends the same
        // span twice and the second copy is not a second occurrence.
        let mut by_id: HashMap<String, Span> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for span in spans {
            if !by_id.contains_key(&span.span_id) {
                order.push(span.span_id.clone());
                by_id.insert(span.span_id.clone(), span);
            }
        }

        let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut root_ids: Vec<String> = Vec::new();
        let mut orphan_ids: HashSet<String> = HashSet::new();

        for id in &order {
            let span = &by_id[id];
            match span.parent_span_id.as_ref() {
                // A span cannot be its own parent; treating it as one would
                // build a self-edge that the descent then has to defend.
                Some(parent) if parent != id && by_id.contains_key(parent) => {
                    children_of.entry(parent.clone()).or_default().push(id.clone());
                }
                Some(_) => {
                    orphan_ids.insert(id.clone());
                    root_ids.push(id.clone());
                }
                None => root_ids.push(id.clone()),
            }
        }

        let started_at = order
            .iter()
            .map(|id| by_id[id].started_at)
            .min()
            .unwrap_or_else(Utc::now);
        let ended_at = order
            .iter()
            .map(|id| {
                by_id[id].started_at
                    + chrono::Duration::microseconds((by_id[id].duration_ms * 1000.0) as i64)
            })
            .max()
            .unwrap_or(started_at);

        let sort_key = |by_id: &HashMap<String, Span>, id: &String| by_id[id].started_at;
        root_ids.sort_by_key(|id| sort_key(&by_id, id));

        let mut visited: HashSet<String> = HashSet::new();
        let mut roots: Vec<TraceNode> = root_ids
            .iter()
            .filter_map(|id| {
                build_node(
                    id,
                    &by_id,
                    &children_of,
                    &mut visited,
                    started_at,
                    0,
                    orphan_ids.contains(id),
                )
            })
            .collect();

        // Anything still unvisited was reachable only through a cycle. It is
        // attached at the root rather than disappearing: a span the operator
        // cannot see is indistinguishable from one that never happened.
        let stranded: Vec<String> = order
            .iter()
            .filter(|id| !visited.contains(*id))
            .cloned()
            .collect();
        for id in stranded {
            orphan_ids.insert(id.clone());
            if let Some(node) = build_node(
                &id,
                &by_id,
                &children_of,
                &mut visited,
                started_at,
                0,
                true,
            ) {
                roots.push(node);
            }
        }
        roots.sort_by(|a, b| {
            a.span
                .started_at
                .cmp(&b.span.started_at)
                .then_with(|| a.span.span_id.cmp(&b.span.span_id))
        });

        let span_count = by_id.len();
        let error_count = by_id
            .values()
            .filter(|s| s.status == SpanStatus::Error)
            .count();

        let mut per_service: BTreeMap<String, ServiceTime> = BTreeMap::new();
        collect_service_time(&roots, &mut per_service);
        let mut services: Vec<ServiceTime> = per_service.into_values().collect();
        services.sort_by(|a, b| {
            b.self_ms
                .partial_cmp(&a.self_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.service.cmp(&b.service))
        });

        // The root of the trace is the earliest span that is not an orphan;
        // a partial trace falls back to whatever came first, which is at least
        // true rather than empty.
        let root = roots
            .iter()
            .find(|n| !n.orphaned)
            .or_else(|| roots.first());

        Trace {
            trace_id: trace_id.to_string(),
            root_service: root.map(|n| n.span.service.clone()).unwrap_or_default(),
            root_name: root.map(|n| n.span.name.clone()).unwrap_or_default(),
            started_at,
            duration_ms: (ended_at - started_at).num_microseconds().unwrap_or(0) as f64 / 1000.0,
            span_count,
            error_count,
            orphan_count: orphan_ids.len(),
            services,
            roots,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_node(
    id: &str,
    by_id: &HashMap<String, Span>,
    children_of: &HashMap<String, Vec<String>>,
    visited: &mut HashSet<String>,
    trace_start: DateTime<Utc>,
    depth: usize,
    orphaned: bool,
) -> Option<TraceNode> {
    // The cycle guard. A span already placed is not placed again; without this
    // a parent loop descends until the stack runs out.
    if !visited.insert(id.to_string()) {
        return None;
    }
    let span = by_id.get(id)?.clone();

    let mut child_ids = children_of.get(id).cloned().unwrap_or_default();
    child_ids.sort_by_key(|child| by_id[child].started_at);

    let children: Vec<TraceNode> = child_ids
        .iter()
        .filter_map(|child| {
            build_node(
                child,
                by_id,
                children_of,
                visited,
                trace_start,
                depth + 1,
                false,
            )
        })
        .collect();

    // Children can overlap each other and can outlive their parent when a
    // clock skews. Summing raw durations would produce a negative self time,
    // so it is clamped — an unmeasurable number is reported as zero, never as
    // a negative one that would sort to the top of "where did the time go".
    let child_total: f64 = children.iter().map(|c| c.span.duration_ms).sum();
    let self_ms = (span.duration_ms - child_total).max(0.0);

    let offset_ms =
        (span.started_at - trace_start).num_microseconds().unwrap_or(0) as f64 / 1000.0;

    Some(TraceNode {
        span,
        offset_ms,
        self_ms,
        depth,
        orphaned,
        children,
    })
}

fn collect_service_time(nodes: &[TraceNode], out: &mut BTreeMap<String, ServiceTime>) {
    for node in nodes {
        let entry = out
            .entry(node.span.service.clone())
            .or_insert_with(|| ServiceTime {
                service: node.span.service.clone(),
                self_ms: 0.0,
                span_count: 0,
                error_count: 0,
            });
        entry.self_ms += node.self_ms;
        entry.span_count += 1;
        if node.span.status == SpanStatus::Error {
            entry.error_count += 1;
        }
        collect_service_time(&node.children, out);
    }
}

/// One call between services, as a trace records it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub from: String,
    pub to: String,
    pub ms: f64,
    pub error: bool,
}

/// The calls between services in one trace: a span whose child ran in another
/// service is that service calling the other. Spans within one service are
/// its own business, not an edge on a map.
pub fn calls(trace: &Trace) -> Vec<Call> {
    fn walk(node: &TraceNode, out: &mut Vec<Call>) {
        for child in &node.children {
            if child.span.service != node.span.service {
                out.push(Call {
                    from: node.span.service.clone(),
                    to: child.span.service.clone(),
                    ms: child.span.duration_ms,
                    error: child.span.status == SpanStatus::Error,
                });
            }
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for root in &trace.roots {
        walk(root, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(id: &str, parent: Option<&str>, service: &str, start_ms: i64, duration_ms: f64) -> Span {
        Span {
            span_id: id.to_string(),
            parent_span_id: parent.map(|p| p.to_string()),
            name: format!("{service} {id}"),
            service: service.to_string(),
            started_at: DateTime::from_timestamp(1_767_225_600, 0).unwrap()
                + chrono::Duration::milliseconds(start_ms),
            duration_ms,
            status: SpanStatus::Unset,
            status_message: None,
            attributes: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn a_child_in_another_service_is_a_call_and_one_in_the_same_is_not() {
        let mut failed = span("d", Some("c"), "payments-db", 30, 12.0);
        failed.status = SpanStatus::Error;
        let trace = Trace::assemble(
            "t1",
            vec![
                span("a", None, "checkout-web", 0, 120.0),
                span("b", Some("a"), "checkout-web", 5, 100.0),
                span("c", Some("b"), "orders-api", 10, 80.0),
                failed,
            ],
        );
        assert_eq!(
            calls(&trace),
            vec![
                Call { from: "checkout-web".into(), to: "orders-api".into(), ms: 80.0, error: false },
                Call { from: "orders-api".into(), to: "payments-db".into(), ms: 12.0, error: true },
            ]
        );
    }

    #[test]
    fn a_flat_list_becomes_a_tree() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("b", Some("a"), "checkout", 10, 60.0),
                span("c", Some("b"), "postgres", 20, 40.0),
            ],
        );

        assert_eq!(trace.roots.len(), 1);
        assert_eq!(trace.roots[0].span.span_id, "a");
        assert_eq!(trace.roots[0].children[0].span.span_id, "b");
        assert_eq!(trace.roots[0].children[0].children[0].span.span_id, "c");
        assert_eq!(trace.roots[0].children[0].children[0].depth, 2);
        assert_eq!(trace.span_count, 3);
        assert_eq!(trace.root_service, "gateway");
    }

    #[test]
    fn offsets_are_relative_to_the_trace_start() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("b", Some("a"), "checkout", 25, 50.0),
            ],
        );
        assert_eq!(trace.roots[0].offset_ms, 0.0);
        assert_eq!(trace.roots[0].children[0].offset_ms, 25.0);
    }

    #[test]
    fn self_time_excludes_what_children_accounted_for() {
        // The whole point: `a` took 100ms but only spent 40 of them itself.
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("b", Some("a"), "checkout", 10, 60.0),
            ],
        );
        assert_eq!(trace.roots[0].self_ms, 40.0);
        assert_eq!(trace.roots[0].children[0].self_ms, 60.0);
    }

    #[test]
    fn a_child_outliving_its_parent_reports_zero_self_time_not_a_negative() {
        // Clock skew between two hosts. A negative self time would sort to the
        // top of "where did the time go" and point at the wrong service.
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 50.0),
                span("b", Some("a"), "checkout", 0, 90.0),
            ],
        );
        assert_eq!(trace.roots[0].self_ms, 0.0);
    }

    #[test]
    fn services_are_ranked_by_where_the_time_actually_went() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 200.0),
                span("b", Some("a"), "checkout", 5, 190.0),
                span("c", Some("b"), "postgres", 10, 180.0),
            ],
        );
        // postgres holds 180 of the 200ms; gateway holds 10.
        assert_eq!(trace.services[0].service, "postgres");
        assert_eq!(trace.services[0].self_ms, 180.0);
        assert_eq!(trace.services.last().unwrap().self_ms, 10.0);
    }

    #[test]
    fn a_span_whose_parent_is_missing_is_kept_at_the_root() {
        // Sampling dropped the parent. `b` is where the request was when the
        // trace went dark, which makes it the most interesting span present.
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("b", Some("missing"), "checkout", 10, 60.0),
            ],
        );
        assert_eq!(trace.roots.len(), 2);
        assert_eq!(trace.orphan_count, 1);
        let orphan = trace.roots.iter().find(|n| n.orphaned).expect("kept");
        assert_eq!(orphan.span.span_id, "b");
    }

    #[test]
    fn a_partial_trace_still_names_a_root() {
        let trace = Trace::assemble("abc", vec![span("b", Some("gone"), "checkout", 10, 60.0)]);
        assert_eq!(trace.root_service, "checkout");
        assert_eq!(trace.orphan_count, 1);
    }

    #[test]
    fn a_parent_cycle_terminates_and_keeps_every_span() {
        // Malformed instrumentation: a points at b, b points at a. Descending
        // this recursively does not terminate, and dropping the pair would
        // hide two real spans.
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", Some("b"), "gateway", 0, 100.0),
                span("b", Some("a"), "checkout", 10, 60.0),
            ],
        );
        assert_eq!(trace.span_count, 2);
        let rendered = count_nodes(&trace.roots);
        assert_eq!(rendered, 2, "every span must appear exactly once");
    }

    #[test]
    fn a_span_is_never_its_own_parent() {
        let trace = Trace::assemble("abc", vec![span("a", Some("a"), "gateway", 0, 100.0)]);
        assert_eq!(count_nodes(&trace.roots), 1);
        assert_eq!(trace.roots[0].children.len(), 0);
    }

    #[test]
    fn a_retried_export_does_not_double_the_span() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("a", None, "gateway", 0, 100.0),
            ],
        );
        assert_eq!(trace.span_count, 1);
        assert_eq!(count_nodes(&trace.roots), 1);
    }

    #[test]
    fn errors_are_counted_per_service_and_for_the_trace() {
        let mut failing = span("b", Some("a"), "checkout", 10, 60.0);
        failing.status = SpanStatus::Error;
        let trace = Trace::assemble("abc", vec![span("a", None, "gateway", 0, 100.0), failing]);

        assert_eq!(trace.error_count, 1);
        let checkout = trace
            .services
            .iter()
            .find(|s| s.service == "checkout")
            .expect("present");
        assert_eq!(checkout.error_count, 1);
    }

    #[test]
    fn siblings_are_ordered_by_start_time_not_arrival() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 100.0),
                span("late", Some("a"), "b", 50, 10.0),
                span("early", Some("a"), "b", 5, 10.0),
            ],
        );
        let names: Vec<&str> = trace.roots[0]
            .children
            .iter()
            .map(|c| c.span.span_id.as_str())
            .collect();
        assert_eq!(names, vec!["early", "late"]);
    }

    #[test]
    fn trace_duration_spans_the_earliest_start_to_the_latest_end() {
        let trace = Trace::assemble(
            "abc",
            vec![
                span("a", None, "gateway", 0, 40.0),
                // Ends at 200ms, well past the root.
                span("b", Some("a"), "checkout", 20, 180.0),
            ],
        );
        assert_eq!(trace.duration_ms, 200.0);
    }

    #[test]
    fn otlp_status_codes_map_from_both_encodings() {
        assert_eq!(
            SpanStatus::from_wire(&serde_json::json!(2)),
            SpanStatus::Error
        );
        assert_eq!(SpanStatus::from_wire(&serde_json::json!(1)), SpanStatus::Ok);
        assert_eq!(
            SpanStatus::from_wire(&serde_json::json!(0)),
            SpanStatus::Unset
        );
        assert_eq!(
            SpanStatus::from_wire(&serde_json::json!("STATUS_CODE_ERROR")),
            SpanStatus::Error
        );
        assert_eq!(
            SpanStatus::from_wire(&serde_json::json!("nonsense")),
            SpanStatus::Unset
        );
    }

    fn count_nodes(nodes: &[TraceNode]) -> usize {
        nodes
            .iter()
            .map(|n| 1 + count_nodes(&n.children))
            .sum()
    }
}
