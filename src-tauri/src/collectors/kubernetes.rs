//! Kubernetes cluster monitor.
//!
//! Auth comes from kubeconfig rather than a typed-in URL and token. That is not
//! laziness — kubeconfig already holds the cluster CA, the server URL, and the
//! auth method, and for EKS/GKE the auth method is an exec plugin that mints a
//! short-lived token per call. There is no static token to paste, so asking for
//! one would make the common cloud cases unusable.
//!
//! What this reports is deliberately *not* "every object in the cluster". A
//! real cluster has thousands of pods, and a table of all of them is the same
//! wall of noise the log tail used to be. Nodes and workloads are listed in
//! full because they are few and always relevant; pods appear only when
//! something is wrong with them.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use k8s_openapi::api::core::v1::{Event, Node, Pod};
use kube::api::{Api, ListParams};
use kube::config::{Config, KubeConfigOptions, Kubeconfig};
use kube::Client;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

/// Warning events kept per poll. Enough to see a pattern, bounded so a cluster
/// in meltdown cannot flood the shared log view.
const MAX_EVENTS: usize = 60;

pub struct KubernetesCollector {
    client: Client,
    /// Empty means every namespace.
    namespace: Option<String>,
    restart_threshold: i32,
    context: String,
    profile_id: String,
    profile_name: String,
}

impl KubernetesCollector {
    pub async fn new(profile: &ConnectionProfile) -> Result<Self> {
        let context = profile.endpoint.trim().to_string();
        let namespace = profile
            .options
            .string("namespace")
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        let restart_threshold = profile
            .options
            .u64("restartThreshold")
            .unwrap_or(5)
            .clamp(1, 1000) as i32;

        let config = Self::load_config(profile, &context).await?;
        let client = Client::try_from(config)
            .map_err(|e| AppError::Config(format!("could not build a Kubernetes client: {e}")))?;

        Ok(KubernetesCollector {
            client,
            namespace,
            restart_threshold,
            context: if context.is_empty() {
                "current context".to_string()
            } else {
                context
            },
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    /// Resolve kubeconfig, honouring an explicit path and context when given.
    async fn load_config(profile: &ConnectionProfile, context: &str) -> Result<Config> {
        let options = KubeConfigOptions {
            context: (!context.is_empty()).then(|| context.to_string()),
            ..Default::default()
        };

        let path = profile
            .options
            .string("kubeconfig")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());

        match path {
            Some(path) => {
                // `~` is what people actually type, and expanding it here beats
                // a confusing "no such file" for a path that plainly exists.
                let expanded = match path.strip_prefix("~/") {
                    Some(rest) => dirs_home()
                        .map(|home| home.join(rest))
                        .ok_or_else(|| AppError::Config("could not resolve `~`".to_string()))?,
                    None => std::path::PathBuf::from(&path),
                };
                let kubeconfig = Kubeconfig::read_from(&expanded).map_err(|e| {
                    AppError::Config(format!("could not read kubeconfig at {}: {e}", expanded.display()))
                })?;
                Config::from_custom_kubeconfig(kubeconfig, &options)
                    .await
                    .map_err(|e| AppError::Config(format!("kubeconfig is not usable: {e}")))
            }
            None => Config::from_kubeconfig(&options).await.map_err(|e| {
                AppError::Config(format!(
                    "could not load kubeconfig{}: {e}",
                    if context.is_empty() {
                        String::new()
                    } else {
                        format!(" for context `{context}`")
                    }
                ))
            }),
        }
    }

    fn params(&self) -> ListParams {
        ListParams::default()
    }

    fn namespaced<K>(&self) -> Api<K>
    where
        K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>,
        <K as kube::Resource>::DynamicType: Default,
    {
        match &self.namespace {
            Some(ns) => Api::namespaced(self.client.clone(), ns),
            None => Api::all(self.client.clone()),
        }
    }
}

/// `$HOME` as a path, for `~` expansion.
fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Whether a node's `Ready` condition is true, plus any pressure conditions.
fn node_state(node: &Node) -> (Health, String, Vec<String>) {
    let conditions = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .cloned()
        .unwrap_or_default();

    let ready = conditions
        .iter()
        .find(|c| c.type_ == "Ready")
        .map(|c| c.status == "True")
        .unwrap_or(false);

    // Every other condition is named for the bad state, so "True" is the
    // problem — the inverse of Ready. Reading these the same way is a classic
    // way to report a healthy node as broken.
    let pressures: Vec<String> = conditions
        .iter()
        .filter(|c| c.type_ != "Ready" && c.status == "True")
        .map(|c| c.type_.clone())
        .collect();

    let unschedulable = node.spec.as_ref().and_then(|s| s.unschedulable).unwrap_or(false);

    let health = if !ready {
        Health::Down
    } else if !pressures.is_empty() || unschedulable {
        Health::Degraded
    } else {
        Health::Healthy
    };

    let summary = if !ready {
        "NotReady".to_string()
    } else if unschedulable {
        "Ready, cordoned".to_string()
    } else if !pressures.is_empty() {
        format!("Ready, {}", pressures.join(" + "))
    } else {
        "Ready".to_string()
    };

    let mut tags = pressures;
    if unschedulable {
        tags.push("Cordoned".to_string());
    }
    (health, summary, tags)
}

/// Roles come from `node-role.kubernetes.io/<role>` labels.
fn node_roles(labels: &BTreeMap<String, String>) -> Vec<String> {
    let mut roles: Vec<String> = labels
        .keys()
        .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
        .filter(|r| !r.is_empty())
        .map(|r| r.to_string())
        .collect();
    roles.sort();
    if roles.is_empty() {
        roles.push("worker".to_string());
    }
    roles
}

/// A pod's phase, restart count, and whether it is worth surfacing.
struct PodState {
    health: Health,
    phase: String,
    reason: String,
    restarts: i32,
    ready: String,
}

fn pod_state(pod: &Pod, restart_threshold: i32) -> PodState {
    let status = pod.status.clone().unwrap_or_default();
    let phase = status.phase.clone().unwrap_or_else(|| "Unknown".to_string());
    let statuses = status.container_statuses.clone().unwrap_or_default();

    let restarts: i32 = statuses.iter().map(|c| c.restart_count).sum();
    let ready_count = statuses.iter().filter(|c| c.ready).count();
    let ready = format!("{ready_count}/{}", statuses.len());

    // The waiting reason is the useful part — "CrashLoopBackOff" says far more
    // than "Pending" does.
    let waiting_reason = statuses
        .iter()
        .find_map(|c| c.state.as_ref()?.waiting.as_ref()?.reason.clone())
        .or_else(|| {
            statuses.iter().find_map(|c| {
                let terminated = c.state.as_ref()?.terminated.as_ref()?;
                terminated.reason.clone()
            })
        });

    let terminating = pod.metadata.deletion_timestamp.is_some();

    let health = if terminating {
        Health::Degraded
    } else {
        match phase.as_str() {
            "Failed" => Health::Down,
            "Pending" => Health::Degraded,
            "Succeeded" => Health::Healthy,
            "Running" if ready_count < statuses.len() => Health::Degraded,
            "Running" if restarts >= restart_threshold => Health::Degraded,
            "Running" => Health::Healthy,
            _ => Health::Unknown,
        }
    };

    let reason = if terminating {
        "Terminating".to_string()
    } else {
        waiting_reason.unwrap_or_else(|| phase.clone())
    };

    PodState {
        health,
        phase,
        reason,
        restarts,
        ready,
    }
}

/// Desired vs available replicas for one workload.
struct WorkloadState {
    health: Health,
    ready: String,
    desired: i32,
    available: i32,
}

fn workload_state(desired: i32, ready: i32, available: i32) -> WorkloadState {
    let health = if desired == 0 {
        // Scaled to zero on purpose is not a fault.
        Health::Healthy
    } else if available == 0 {
        Health::Down
    } else if ready < desired {
        Health::Degraded
    } else {
        Health::Healthy
    };
    WorkloadState {
        health,
        ready: format!("{ready}/{desired}"),
        desired,
        available,
    }
}

#[async_trait]
impl Collector for KubernetesCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Kubernetes
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let nodes: Api<Node> = Api::all(self.client.clone());
        let node_list = match nodes.list(&self.params()).await {
            Ok(list) => list,
            Err(e) => {
                // Unreachable is a normal state for a laptop cluster that is
                // simply not running, so it is a Down snapshot, not an error.
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Kubernetes,
                    format!("{} unreachable: {e}", self.context),
                    elapsed_ms(start),
                ));
            }
        };

        let deployments: Api<Deployment> = self.namespaced();
        let statefulsets: Api<StatefulSet> = self.namespaced();
        let daemonsets: Api<DaemonSet> = self.namespaced();
        let pods: Api<Pod> = self.namespaced();
        let events: Api<Event> = self.namespaced();

        // One binding, borrowed by all five: a `&self.params()` temporary inside
        // the join would be dropped before the futures it is lent to complete.
        let params = self.params();
        let (deploy_list, sts_list, ds_list, pod_list, event_list) = tokio::join!(
            deployments.list(&params),
            statefulsets.list(&params),
            daemonsets.list(&params),
            pods.list(&params),
            events.list(&params),
        );

        let latency = elapsed_ms(start);

        // ---------------------------------------------------------- nodes
        let mut node_rows = Vec::new();
        let mut nodes_ready = 0usize;
        for node in &node_list.items {
            let name = node.metadata.name.clone().unwrap_or_default();
            let (health, summary, tags) = node_state(node);
            if health == Health::Healthy {
                nodes_ready += 1;
            }
            let info = node.status.as_ref().and_then(|s| s.node_info.as_ref());
            let roles = node_roles(node.metadata.labels.as_ref().unwrap_or(&BTreeMap::new()));

            node_rows.push(
                Row::new(format!("node:{name}"), &name, health)
                    .summary(&summary)
                    .cell("name", CellValue::text(&name))
                    .cell("status", CellValue::text(&summary))
                    .cell("roles", CellValue::tags(roles))
                    .cell(
                        "version",
                        CellValue::maybe_text(info.map(|i| i.kubelet_version.clone())),
                    )
                    .cell("conditions", CellValue::tags(tags)),
            );
        }

        // ------------------------------------------------------ workloads
        let mut workload_rows = Vec::new();
        let mut workloads_degraded = 0usize;

        if let Ok(list) = &deploy_list {
            for item in &list.items {
                let status = item.status.clone().unwrap_or_default();
                let desired = item.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
                let state = workload_state(
                    desired,
                    status.ready_replicas.unwrap_or(0),
                    status.available_replicas.unwrap_or(0),
                );
                if state.health != Health::Healthy {
                    workloads_degraded += 1;
                }
                workload_rows.push(workload_row(item.metadata.clone(), "Deployment", state));
            }
        }
        if let Ok(list) = &sts_list {
            for item in &list.items {
                let status = item.status.clone().unwrap_or_default();
                let desired = item.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
                let ready = status.ready_replicas.unwrap_or(0);
                let state = workload_state(desired, ready, status.available_replicas.unwrap_or(ready));
                if state.health != Health::Healthy {
                    workloads_degraded += 1;
                }
                workload_rows.push(workload_row(item.metadata.clone(), "StatefulSet", state));
            }
        }
        if let Ok(list) = &ds_list {
            for item in &list.items {
                let status = item.status.clone().unwrap_or_default();
                // A DaemonSet's "desired" is per-node, not a spec replica count.
                let state = workload_state(
                    status.desired_number_scheduled,
                    status.number_ready,
                    status.number_available.unwrap_or(status.number_ready),
                );
                if state.health != Health::Healthy {
                    workloads_degraded += 1;
                }
                workload_rows.push(workload_row(item.metadata.clone(), "DaemonSet", state));
            }
        }

        // ----------------------------------------------------------- pods
        let mut pod_rows = Vec::new();
        let mut pods_total = 0usize;
        let mut pods_running = 0usize;
        let mut total_restarts = 0i64;

        if let Ok(list) = &pod_list {
            for pod in &list.items {
                pods_total += 1;
                let state = pod_state(pod, self.restart_threshold);
                total_restarts += state.restarts as i64;
                if state.health == Health::Healthy && state.phase == "Running" {
                    pods_running += 1;
                }
                // Only unhealthy pods get a row. A healthy cluster shows an
                // empty section, which is the correct and readable answer.
                if state.health == Health::Healthy {
                    continue;
                }

                let name = pod.metadata.name.clone().unwrap_or_default();
                let namespace = pod.metadata.namespace.clone().unwrap_or_default();
                pod_rows.push(
                    Row::new(format!("pod:{namespace}/{name}"), &name, state.health)
                        .summary(format!("{} · {} restarts", state.reason, state.restarts))
                        .cell("name", CellValue::text(&name))
                        .cell("namespace", CellValue::text(&namespace))
                        .cell("reason", CellValue::text(&state.reason))
                        .cell("ready", CellValue::text(&state.ready))
                        .cell("restarts", CellValue::number(state.restarts as f64))
                        .cell(
                            "node",
                            CellValue::maybe_text(pod.spec.as_ref().and_then(|s| s.node_name.clone())),
                        ),
                );
            }
        }

        // --------------------------------------------------------- events
        let mut event_rows = Vec::new();
        if let Ok(list) = &event_list {
            let mut warnings: Vec<&Event> = list
                .items
                .iter()
                .filter(|e| e.type_.as_deref() == Some("Warning"))
                .collect();
            warnings.sort_by_key(|e| std::cmp::Reverse(event_time(e)));

            for event in warnings.into_iter().take(MAX_EVENTS) {
                let object = event
                    .involved_object
                    .name
                    .clone()
                    .unwrap_or_else(|| "cluster".to_string());
                let reason = event.reason.clone().unwrap_or_default();
                let count = event.count.unwrap_or(1);
                event_rows.push(
                    Row::new(
                        format!("event:{}", event.metadata.uid.clone().unwrap_or_default()),
                        &object,
                        Health::Degraded,
                    )
                    .summary(event.message.clone().unwrap_or_default())
                    .cell("object", CellValue::text(&object))
                    .cell("reason", CellValue::text(&reason))
                    .cell("message", CellValue::text(event.message.clone().unwrap_or_default()))
                    .cell("count", CellValue::number(count as f64))
                    .cell(
                        "last_seen",
                        match event_time(event) {
                            Some(t) => CellValue::timestamp(t),
                            None => CellValue::Null,
                        },
                    ),
                );
            }
        }

        // -------------------------------------------------------- rollup
        let nodes_total = node_list.items.len();
        let nodes_down = nodes_total.saturating_sub(nodes_ready);
        let pods_unhealthy = pod_rows.len();

        let health = if nodes_total == 0 {
            Health::Unknown
        } else if nodes_down > 0 || workload_rows.iter().any(|r| r.health == Health::Down) {
            Health::Down
        } else if workloads_degraded > 0 || pods_unhealthy > 0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if nodes_down > 0 {
            format!("{nodes_down} of {nodes_total} nodes not ready")
        } else if workloads_degraded > 0 {
            format!(
                "{workloads_degraded} workload{} not fully available",
                if workloads_degraded == 1 { "" } else { "s" }
            )
        } else if pods_unhealthy > 0 {
            format!(
                "{pods_unhealthy} pod{} need attention",
                if pods_unhealthy == 1 { "" } else { "s" }
            )
        } else {
            format!("{nodes_total} nodes ready · {pods_running} pods running")
        };

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Kubernetes,
            health,
            summary,
            metrics: vec![
                Metric::new("nodes_ready", "Nodes ready", nodes_ready as f64, MetricUnit::Count),
                Metric::new("nodes_total", "Nodes", nodes_total as f64, MetricUnit::Count),
                Metric::new("pods_running", "Pods running", pods_running as f64, MetricUnit::Count),
                Metric::new("pods_total", "Pods", pods_total as f64, MetricUnit::Count),
                Metric::new(
                    "pods_unhealthy",
                    "Pods needing attention",
                    pods_unhealthy as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new(
                    "workloads_degraded",
                    "Workloads not available",
                    workloads_degraded as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new("restarts", "Container restarts", total_restarts as f64, MetricUnit::Count),
            ],
            sections: vec![
                Section::new(
                    "nodes",
                    "Nodes",
                    vec![
                        ColumnSpec::text("name", "Node").primary(),
                        ColumnSpec::text("status", "Status"),
                        ColumnSpec::tags("roles", "Roles"),
                        ColumnSpec::text("version", "Kubelet"),
                        ColumnSpec::tags("conditions", "Conditions"),
                    ],
                )
                .rows(node_rows)
                .priority(10)
                .sorted_by_health()
                .tracked()
                .empty_text("No nodes reported."),
                Section::new(
                    "workloads",
                    "Workloads",
                    vec![
                        ColumnSpec::text("name", "Workload").primary(),
                        ColumnSpec::text("namespace", "Namespace"),
                        ColumnSpec::text("type", "Type"),
                        ColumnSpec::text("ready", "Ready"),
                        ColumnSpec::metric("available", "Available", MetricUnit::Count),
                    ],
                )
                .rows(workload_rows)
                .priority(20)
                .sorted_by_health()
                // Workloads are bounded and durable. Pods below are not
                // tracked — they are replaced constantly and run to thousands.
                .tracked()
                .empty_text("No workloads in scope."),
                Section::new(
                    "pods",
                    "Pods needing attention",
                    vec![
                        ColumnSpec::text("name", "Pod").primary(),
                        ColumnSpec::text("namespace", "Namespace"),
                        ColumnSpec::text("reason", "Reason"),
                        ColumnSpec::text("ready", "Ready"),
                        ColumnSpec::metric("restarts", "Restarts", MetricUnit::Count),
                        ColumnSpec::text("node", "Node"),
                    ],
                )
                .rows(pod_rows)
                .priority(5)
                .sorted_by_health()
                .empty_text("Every pod is running and ready."),
                Section::new(
                    "events",
                    "Warning events",
                    vec![
                        ColumnSpec::text("object", "Object").primary(),
                        ColumnSpec::text("reason", "Reason"),
                        ColumnSpec::text("message", "Message"),
                        ColumnSpec::metric("count", "Count", MetricUnit::Count),
                        ColumnSpec::timestamp("last_seen", "Last seen"),
                    ],
                )
                .rows(event_rows)
                .priority(30)
                .empty_text("No warning events."),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Warning events become log lines. Pod stdout is deliberately not tailed:
    /// a cluster has thousands of pods and following them all would be a
    /// different feature with a different cost model.
    /// Replica counts and per-container resource requests and limits.
    ///
    /// This is the scaling question in its most literal form. Whether a
    /// workload can absorb more load is decided by three numbers — replicas,
    /// requests, limits — and the answer to "should we scale up" is different
    /// depending on which of them is the binding one. A deployment at 3/3 with
    /// no limits set is not the same problem as one at 3/3 being OOM-killed,
    /// and the metrics look similar until you read this.
    async fn config(&self) -> Result<Vec<crate::configsource::ConfigDocument>> {
        use crate::configsource::ConfigDocument;

        let deployments: Api<Deployment> = self.namespaced();
        let statefulsets: Api<StatefulSet> = self.namespaced();
        let params = self.params();
        let (deploy_list, sts_list) =
            tokio::join!(deployments.list(&params), statefulsets.list(&params));

        let mut map = serde_json::Map::new();

        if let Ok(list) = deploy_list {
            for item in &list.items {
                let name = item.metadata.name.clone().unwrap_or_default();
                let spec = item.spec.as_ref();
                map.insert(
                    format!("Deployment/{name}"),
                    serde_json::json!({
                        "replicas": spec.and_then(|s| s.replicas),
                        "strategy": spec.and_then(|s| s.strategy.as_ref()).and_then(|s| s.type_.clone()),
                        "containers": spec
                            .and_then(|s| s.template.spec.as_ref())
                            .map(|p| container_resources(&p.containers)),
                    }),
                );
            }
        }

        if let Ok(list) = sts_list {
            for item in &list.items {
                let name = item.metadata.name.clone().unwrap_or_default();
                let spec = item.spec.as_ref();
                map.insert(
                    format!("StatefulSet/{name}"),
                    serde_json::json!({
                        "replicas": spec.and_then(|s| s.replicas),
                        "containers": spec
                            .map(|s| container_resources(&s.template.spec.as_ref()
                                .map(|p| p.containers.clone())
                                .unwrap_or_default())),
                    }),
                );
            }
        }

        Ok(vec![ConfigDocument::live(
            &self.profile_id,
            "Workload sizing",
            "apps/v1 specs",
            &serde_json::Value::Object(map),
        )])
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let events: Api<Event> = self.namespaced();
        let list = match events.list(&self.params()).await {
            Ok(list) => list,
            Err(_) => return Ok(Vec::new()),
        };

        let mut out: Vec<LogEvent> = list
            .items
            .iter()
            .filter(|e| e.type_.as_deref() == Some("Warning"))
            .filter_map(|event| {
                let at = event_time(event)?;
                if at <= since {
                    return None;
                }
                let object = event
                    .involved_object
                    .name
                    .clone()
                    .unwrap_or_else(|| "cluster".to_string());
                let reason = event.reason.clone().unwrap_or_default();
                let message = event.message.clone().unwrap_or_default();

                Some(
                    LogEvent::new(
                        &self.profile_id,
                        &self.profile_name,
                        ServiceKind::Kubernetes,
                        &object,
                        LogLevel::Warn,
                        format!("{reason}: {message}"),
                        at,
                    )
                    .stream(LogStream::Event),
                )
            })
            .collect();

        out.sort_by_key(|e| e.timestamp);
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }
}

fn workload_row(
    meta: kube::core::ObjectMeta,
    kind: &str,
    state: WorkloadState,
) -> Row {
    let name = meta.name.unwrap_or_default();
    let namespace = meta.namespace.unwrap_or_default();
    Row::new(
        format!("workload:{namespace}/{kind}/{name}"),
        &name,
        state.health,
    )
    .summary(format!("{kind} · {} ready", state.ready))
    .cell("name", CellValue::text(&name))
    .cell("namespace", CellValue::text(&namespace))
    .cell("type", CellValue::text(kind))
    .cell("ready", CellValue::text(&state.ready))
    .cell(
        "available",
        CellValue::gauge(state.available as f64, Some(state.desired.max(1) as f64)),
    )
}

/// The most recent timestamp an event carries.
///
/// `lastTimestamp` is empty on events from the newer events API, which only
/// populate `eventTime`, so both have to be consulted or recent events look
/// like they never happened.
///
/// k8s-openapi times are jiff, not chrono, and `Time` and `MicroTime` are
/// distinct wrapper types — so each is converted through its numeric accessors
/// rather than unified first. That also keeps jiff out of our dependency list.
fn event_time(event: &Event) -> Option<DateTime<Utc>> {
    fn convert(seconds: i64, subsec_nanos: i32) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(seconds, subsec_nanos.max(0) as u32)
    }

    event
        .last_timestamp
        .as_ref()
        .and_then(|t| convert(t.0.as_second(), t.0.subsec_nanosecond()))
        .or_else(|| {
            event
                .event_time
                .as_ref()
                .and_then(|t| convert(t.0.as_second(), t.0.subsec_nanosecond()))
        })
        .or_else(|| {
            event
                .first_timestamp
                .as_ref()
                .and_then(|t| convert(t.0.as_second(), t.0.subsec_nanosecond()))
        })
        .or_else(|| {
            event
                .metadata
                .creation_timestamp
                .as_ref()
                .and_then(|t| convert(t.0.as_second(), t.0.subsec_nanosecond()))
        })
}

/// Requests and limits per container, which is the whole of what sizing means
/// here. A container with neither is scheduled as best-effort and is the first
/// thing evicted when a node runs short — worth seeing as an explicit `null`
/// rather than an absent key.
fn container_resources(containers: &[k8s_openapi::api::core::v1::Container]) -> serde_json::Value {
    serde_json::Value::Object(
        containers
            .iter()
            .map(|c| {
                let resources = c.resources.as_ref();
                let quantity = |side: Option<&std::collections::BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>>, key: &str| {
                    side.and_then(|m| m.get(key)).map(|q| q.0.clone())
                };
                (
                    c.name.clone(),
                    serde_json::json!({
                        "image": c.image,
                        "requests": {
                            "cpu": quantity(resources.and_then(|r| r.requests.as_ref()), "cpu"),
                            "memory": quantity(resources.and_then(|r| r.requests.as_ref()), "memory"),
                        },
                        "limits": {
                            "cpu": quantity(resources.and_then(|r| r.limits.as_ref()), "cpu"),
                            "memory": quantity(resources.and_then(|r| r.limits.as_ref()), "memory"),
                        },
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateWaiting, ContainerStatus, NodeCondition, NodeSpec, NodeStatus,
        PodStatus,
    };

    fn node_with(conditions: Vec<(&str, &str)>, unschedulable: bool) -> Node {
        Node {
            spec: Some(NodeSpec {
                unschedulable: Some(unschedulable),
                ..Default::default()
            }),
            status: Some(NodeStatus {
                conditions: Some(
                    conditions
                        .into_iter()
                        .map(|(t, s)| NodeCondition {
                            type_: t.to_string(),
                            status: s.to_string(),
                            ..Default::default()
                        })
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn ready_node_is_healthy() {
        let (health, summary, tags) = node_state(&node_with(vec![("Ready", "True")], false));
        assert_eq!(health, Health::Healthy);
        assert_eq!(summary, "Ready");
        assert!(tags.is_empty());
    }

    #[test]
    fn not_ready_node_is_down() {
        let (health, _, _) = node_state(&node_with(vec![("Ready", "False")], false));
        assert_eq!(health, Health::Down);
    }

    #[test]
    fn pressure_conditions_are_read_inverted_from_ready() {
        // MemoryPressure=True is bad; Ready=True is good. Treating them the
        // same way is the obvious bug here, so it gets a test.
        let (health, summary, tags) = node_state(&node_with(
            vec![("Ready", "True"), ("MemoryPressure", "True"), ("DiskPressure", "False")],
            false,
        ));
        assert_eq!(health, Health::Degraded);
        assert!(summary.contains("MemoryPressure"), "{summary}");
        assert_eq!(tags, vec!["MemoryPressure"]);
    }

    #[test]
    fn cordoned_node_is_degraded_not_down() {
        let (health, summary, tags) = node_state(&node_with(vec![("Ready", "True")], true));
        assert_eq!(health, Health::Degraded);
        assert_eq!(summary, "Ready, cordoned");
        assert_eq!(tags, vec!["Cordoned"]);
    }

    #[test]
    fn roles_default_to_worker() {
        let labels = BTreeMap::new();
        assert_eq!(node_roles(&labels), vec!["worker"]);
    }

    #[test]
    fn roles_come_from_node_role_labels() {
        let mut labels = BTreeMap::new();
        labels.insert("node-role.kubernetes.io/control-plane".to_string(), String::new());
        labels.insert("node-role.kubernetes.io/etcd".to_string(), String::new());
        labels.insert("kubernetes.io/os".to_string(), "linux".to_string());
        assert_eq!(node_roles(&labels), vec!["control-plane", "etcd"]);
    }

    fn pod_with(phase: &str, statuses: Vec<ContainerStatus>) -> Pod {
        Pod {
            status: Some(PodStatus {
                phase: Some(phase.to_string()),
                container_statuses: Some(statuses),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn container(ready: bool, restarts: i32, waiting: Option<&str>) -> ContainerStatus {
        ContainerStatus {
            ready,
            restart_count: restarts,
            state: waiting.map(|reason| ContainerState {
                waiting: Some(ContainerStateWaiting {
                    reason: Some(reason.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn running_ready_pod_is_healthy() {
        let state = pod_state(&pod_with("Running", vec![container(true, 0, None)]), 5);
        assert_eq!(state.health, Health::Healthy);
        assert_eq!(state.ready, "1/1");
    }

    #[test]
    fn crash_looping_pod_reports_its_waiting_reason() {
        let state = pod_state(
            &pod_with("Running", vec![container(false, 12, Some("CrashLoopBackOff"))]),
            5,
        );
        assert_eq!(state.health, Health::Degraded);
        assert_eq!(state.reason, "CrashLoopBackOff");
        assert_eq!(state.restarts, 12);
    }

    #[test]
    fn restarts_below_the_threshold_do_not_degrade_a_ready_pod() {
        let state = pod_state(&pod_with("Running", vec![container(true, 2, None)]), 5);
        assert_eq!(state.health, Health::Healthy);
    }

    #[test]
    fn restarts_at_the_threshold_degrade_it() {
        let state = pod_state(&pod_with("Running", vec![container(true, 5, None)]), 5);
        assert_eq!(state.health, Health::Degraded);
    }

    #[test]
    fn failed_pod_is_down_and_succeeded_is_not() {
        assert_eq!(pod_state(&pod_with("Failed", vec![]), 5).health, Health::Down);
        // A completed Job pod is not a fault, and reporting it as one would
        // make every cluster with CronJobs permanently red.
        assert_eq!(
            pod_state(&pod_with("Succeeded", vec![]), 5).health,
            Health::Healthy
        );
    }

    #[test]
    fn scaled_to_zero_workload_is_healthy() {
        // Deliberately scaled down is not the same as unavailable.
        let state = workload_state(0, 0, 0);
        assert_eq!(state.health, Health::Healthy);
    }

    #[test]
    fn workload_with_no_available_replicas_is_down() {
        let state = workload_state(3, 0, 0);
        assert_eq!(state.health, Health::Down);
        assert_eq!(state.ready, "0/3");
    }

    #[test]
    fn partially_ready_workload_is_degraded() {
        let state = workload_state(3, 2, 2);
        assert_eq!(state.health, Health::Degraded);
        assert_eq!(state.ready, "2/3");
    }

    #[test]
    fn fully_ready_workload_is_healthy() {
        assert_eq!(workload_state(3, 3, 3).health, Health::Healthy);
    }
}
