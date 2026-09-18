//! Jenkins collector: job results, build queue depth, executor/node health,
//! and recent build history.
//!
//! Console output is reachable through [`DetailQuery::JenkinsBuildLog`] rather
//! than pushed into the shared log stream — forty lines per failed build there
//! buried everything else.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use super::{elapsed_ms, http_client, Collector, CollectorAction, DetailQuery};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, RowAction, Section, ServiceKind,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct JobsResponse {
    #[serde(default)]
    jobs: Vec<Job>,
}

#[derive(Debug, Clone, Deserialize)]
struct Job {
    name: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    buildable: Option<bool>,
    #[serde(default, rename = "lastBuild")]
    last_build: Option<Build>,
    /// Recent build history, newest first. Feeds the build-history strip, which
    /// is what distinguishes a consistently-broken job from a flaky one — the
    /// single most useful thing to see on a Jenkins dashboard.
    #[serde(default)]
    builds: Vec<Build>,
    /// Present when the entry is a folder rather than a job.
    #[serde(default)]
    jobs: Vec<Job>,
}

#[derive(Debug, Clone, Deserialize)]
struct Build {
    number: i64,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    building: Option<bool>,
    /// Milliseconds since the epoch.
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    duration: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct QueueResponse {
    #[serde(default)]
    items: Vec<QueueItem>,
}

#[derive(Debug, Deserialize)]
struct QueueItem {
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    stuck: Option<bool>,
    #[serde(default)]
    task: Option<QueueTask>,
}

#[derive(Debug, Deserialize)]
struct QueueTask {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComputerResponse {
    #[serde(default)]
    computer: Vec<Computer>,
    #[serde(default, rename = "busyExecutors")]
    busy_executors: Option<i64>,
    #[serde(default, rename = "totalExecutors")]
    total_executors: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Computer {
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(default)]
    offline: Option<bool>,
    #[serde(default)]
    idle: Option<bool>,
    #[serde(default, rename = "numExecutors")]
    num_executors: Option<i64>,
    #[serde(default, rename = "offlineCauseReason")]
    offline_cause_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Crumb {
    crumb: String,
    #[serde(rename = "crumbRequestField")]
    crumb_request_field: String,
}

pub struct JenkinsCollector {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    token: Option<String>,
    job_filter: Option<String>,
    profile_id: String,
    profile_name: String,
    log_limit: usize,
}

impl JenkinsCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let base_url = profile.endpoint.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(AppError::Config(
                "Jenkins connections need a base URL, e.g. https://jenkins.internal".to_string(),
            ));
        }
        Ok(JenkinsCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            base_url,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            token: secret,
            job_filter: profile.options.string("jobFilter"),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            log_limit: profile.options.log_limit(),
        })
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.token) {
            (Some(user), Some(token)) => req.basic_auth(user, Some(token)),
            (Some(user), None) => req.basic_auth(user, None::<&str>),
            _ => req,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.authed(self.client.get(&url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Collector(format!(
                "Jenkins returned {status} for {path}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        Ok(resp.json::<T>().await?)
    }

    async fn get_text(&self, path: &str) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.authed(self.client.get(&url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::Collector(format!(
                "Jenkins returned {status} for {path}"
            )));
        }
        Ok(resp.text().await?)
    }

    /// Flatten one level of folders so jobs inside a folder still show up.
    async fn fetch_jobs(&self) -> Result<Vec<Job>> {
        const BUILD_FIELDS: &str = "number,result,building,timestamp,duration";
        // `{0,20}` is Jenkins' range specifier — the most recent 20 builds.
        let job_fields = format!(
            "name,url,color,buildable,lastBuild[{BUILD_FIELDS}],builds[{BUILD_FIELDS}]{{0,20}}"
        );
        let tree = format!("jobs[{job_fields},jobs[{job_fields}]]");
        let resp: JobsResponse = self
            .get_json(&format!("/api/json?tree={}", urlencode(&tree)))
            .await?;

        let mut flat = Vec::new();
        for job in resp.jobs {
            if job.jobs.is_empty() {
                flat.push(job);
            } else {
                let folder = job.name.clone();
                for mut child in job.jobs {
                    child.name = format!("{folder}/{}", child.name);
                    flat.push(child);
                }
            }
        }

        if let Some(filter) = &self.job_filter {
            let needle = filter.to_ascii_lowercase();
            flat.retain(|j| j.name.to_ascii_lowercase().contains(&needle));
        }
        Ok(flat)
    }

    /// Jenkins encodes job status in the ball color, with an `_anime` suffix
    /// while a build is in flight.
    fn job_health(job: &Job) -> (Health, String) {
        let color = job.color.clone().unwrap_or_else(|| "notbuilt".to_string());
        let building = color.ends_with("_anime");
        let base = color.trim_end_matches("_anime");

        let (health, label) = match base {
            "blue" | "green" => (Health::Healthy, "last build succeeded"),
            "yellow" => (Health::Degraded, "last build unstable"),
            "red" => (Health::Down, "last build failed"),
            "aborted" => (Health::Degraded, "last build aborted"),
            "disabled" => (Health::Unknown, "disabled"),
            "notbuilt" => (Health::Unknown, "never built"),
            _ => (Health::Unknown, "unknown state"),
        };

        let mut detail = label.to_string();
        if building {
            detail.push_str(" · building now");
        }
        if let Some(build) = &job.last_build {
            detail = format!("#{} — {detail}", build.number);
        }
        (health, detail)
    }
}

#[async_trait]
impl Collector for JenkinsCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Jenkins
    }

    // Health is decided as a chain of `else if`s naming one condition each,
    // and several arms legitimately return the same `Health`. The chain mirrors
    // the `summary` chain below it so the two read in the same order and a
    // reader can see which condition produced which sentence; collapsing the
    // arms with `||` would satisfy the lint and break that correspondence.
    #[allow(clippy::if_same_then_else)]
    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let jobs = match self.fetch_jobs().await {
            Ok(j) => j,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Jenkins,
                    e.to_string(),
                    elapsed_ms(start),
                ))
            }
        };

        // Queue and node data are useful but not essential; a failure there
        // should not blank out the job list.
        let queue: QueueResponse = self
            .get_json("/queue/api/json?tree=items[why,stuck,task[name]]")
            .await
            .unwrap_or(QueueResponse { items: Vec::new() });
        let computers: ComputerResponse = self
            .get_json(
                "/computer/api/json?tree=computer[displayName,offline,idle,numExecutors,\
                 offlineCauseReason],busyExecutors,totalExecutors",
            )
            .await
            .unwrap_or(ComputerResponse {
                computer: Vec::new(),
                busy_executors: None,
                total_executors: None,
            });

        let mut failing = 0usize;
        let mut unstable = 0usize;
        let mut building = 0usize;

        // --- Jobs
        let job_rows: Vec<Row> = jobs
            .iter()
            .map(|job| {
                let (health, detail) = Self::job_health(job);
                match health {
                    Health::Down => failing += 1,
                    Health::Degraded => unstable += 1,
                    _ => {}
                }
                let in_flight = job.color.as_deref().is_some_and(|c| c.ends_with("_anime"));
                if in_flight {
                    building += 1;
                }

                let last = job.last_build.as_ref();
                let recent_failures = job
                    .builds
                    .iter()
                    .filter(|b| b.result.as_deref() == Some("FAILURE"))
                    .count();

                Row::new(&job.name, &job.name, health)
                    .summary(detail)
                    .cell("job", CellValue::text(&job.name))
                    .cell(
                        "result",
                        CellValue::text(if in_flight {
                            "building".to_string()
                        } else {
                            last.and_then(|b| b.result.clone())
                                .unwrap_or_else(|| "never built".to_string())
                        }),
                    )
                    .cell(
                        "build",
                        last.map(|b| CellValue::number(b.number as f64))
                            .unwrap_or(CellValue::Null),
                    )
                    .cell(
                        "duration",
                        last.and_then(|b| b.duration)
                            .filter(|d| *d > 0)
                            .map(|d| CellValue::duration(d as f64 / 1000.0))
                            .unwrap_or(CellValue::Null),
                    )
                    .cell(
                        "last_run",
                        last.and_then(build_finished_at)
                            .map(CellValue::timestamp)
                            .unwrap_or(CellValue::Null),
                    )
                    .cell(
                        "recent_failures",
                        CellValue::number(recent_failures as f64),
                    )
                    .action(RowAction::new(
                        "trigger_jenkins_build",
                        "Build now",
                        serde_json::json!({ "job": job.name }),
                    ))
                    // Only a purpose-built view reads this; the generic table
                    // ignores it entirely.
                    .extra(serde_json::json!({
                        "builds": job.builds.iter().map(|b| serde_json::json!({
                            "number": b.number,
                            "result": b.result,
                            "building": b.building.unwrap_or(false),
                            "durationMs": b.duration,
                            "finishedAt": build_finished_at(b).map(|t| t.to_rfc3339()),
                        })).collect::<Vec<_>>(),
                        "buildable": job.buildable.unwrap_or(true),
                    }))
            })
            .collect();

        // --- Agents
        let offline_nodes = computers
            .computer
            .iter()
            .filter(|c| c.offline == Some(true))
            .count();

        let agent_rows: Vec<Row> = computers
            .computer
            .iter()
            .map(|node| {
                let offline = node.offline == Some(true);
                let status = if offline {
                    "offline"
                } else if node.idle == Some(true) {
                    "idle"
                } else {
                    "busy"
                };
                let reason = node
                    .offline_cause_reason
                    .clone()
                    .filter(|r| !r.is_empty() && offline);

                Row::new(
                    format!("node:{}", node.display_name),
                    &node.display_name,
                    if offline { Health::Down } else { Health::Healthy },
                )
                .summary(reason.clone().unwrap_or_else(|| format!("online · {status}")))
                .cell("agent", CellValue::text(&node.display_name))
                .cell("status", CellValue::text(status))
                .cell(
                    "executors",
                    node.num_executors
                        .map(|n| CellValue::number(n as f64))
                        .unwrap_or(CellValue::Null),
                )
                .cell("reason", CellValue::maybe_text(reason))
            })
            .collect();

        // --- Queue. Surfacing *why* something is queued is the whole signal:
        // "waiting for an executor" and "blocked by an upstream lock" need very
        // different responses.
        let stuck_in_queue = queue.items.iter().filter(|i| i.stuck == Some(true)).count();
        let queue_rows: Vec<Row> = queue
            .items
            .iter()
            .map(|item| {
                let name = item
                    .task
                    .as_ref()
                    .and_then(|t| t.name.clone())
                    .unwrap_or_else(|| "<unknown task>".to_string());
                let stuck = item.stuck == Some(true);
                let why = item
                    .why
                    .clone()
                    .filter(|w| !w.is_empty())
                    .unwrap_or_else(|| "waiting in the build queue".to_string());

                Row::new(
                    format!("queue:{name}"),
                    &name,
                    if stuck { Health::Degraded } else { Health::Unknown },
                )
                .summary(why.clone())
                .cell("task", CellValue::text(&name))
                .cell("why", CellValue::text(why))
                .cell("stuck", CellValue::text(if stuck { "stuck" } else { "waiting" }))
            })
            .collect();

        let mut sections = vec![Section::new(
            "jobs",
            "Jobs",
            vec![
                ColumnSpec::text("job", "Job").primary(),
                ColumnSpec::text("result", "Last result"),
                // The build number advancing is this job having shipped.
                ColumnSpec::metric("build", "Build", MetricUnit::Count).revision(),
                ColumnSpec::duration("duration", "Duration"),
                ColumnSpec::timestamp("last_run", "Finished"),
                ColumnSpec::metric("recent_failures", "Failures / 20", MetricUnit::Count)
                    .secondary(),
            ],
        )
        .rows(job_rows)
        .priority(10)
        .empty_text("No jobs are visible to this credential.")
        // Jobs are a stable, bounded list; build duration and failure rate per
        // job are worth a trend. The queue is not tracked — it is transient.
        .tracked()
        .sorted_by_health()];

        // The queue only matters when something is in it, and it matters a lot
        // then — so it outranks the inventory sections.
        if !queue_rows.is_empty() {
            sections.push(
                Section::new(
                    "queue",
                    "Build queue",
                    vec![
                        ColumnSpec::text("task", "Task").primary(),
                        ColumnSpec::text("stuck", "State"),
                        ColumnSpec::text("why", "Waiting for"),
                    ],
                )
                .rows(queue_rows)
                .priority(5)
                .sorted_by_health(),
            );
        }

        sections.push(
            Section::new(
                "agents",
                "Agents",
                vec![
                    ColumnSpec::text("agent", "Agent").primary(),
                    ColumnSpec::text("status", "Status"),
                    ColumnSpec::metric("executors", "Executors", MetricUnit::Count),
                    ColumnSpec::text("reason", "Offline reason"),
                ],
            )
            .rows(agent_rows)
            .priority(20)
            .empty_text("No agents are configured.")
            .tracked()
            .sorted_by_health(),
        );

        // Jenkins itself is reachable, so `Down` is reserved for connectivity.
        // Failing jobs and offline agents make the service degraded.
        let health = if failing > 0 || offline_nodes > 0 || stuck_in_queue > 0 {
            Health::Degraded
        } else if unstable > 0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let metrics = vec![
            Metric::new("jobs", "Jobs", jobs.len() as f64, MetricUnit::Count),
            Metric::new("failing", "Failing", failing as f64, MetricUnit::Count).warn_above(0.0),
            Metric::new("unstable", "Unstable", unstable as f64, MetricUnit::Count).warn_above(0.0),
            Metric::new("building", "Building", building as f64, MetricUnit::Count),
            Metric::new(
                "queue",
                "Queued",
                queue.items.len() as f64,
                MetricUnit::Count,
            )
            .warn_above(10.0),
            Metric::new(
                "stuck",
                "Stuck in queue",
                stuck_in_queue as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "executors_busy",
                "Busy executors",
                computers.busy_executors.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "executors_total",
                "Total executors",
                computers.total_executors.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "nodes_offline",
                "Offline nodes",
                offline_nodes as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
        ];

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Jenkins,
            health,
            summary: format!(
                "{} jobs — {failing} failing, {unstable} unstable, {} queued",
                jobs.len(),
                queue.items.len()
            ),
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(start),
            error: None,
        })
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let jobs = self.fetch_jobs().await?;
        let mut events = Vec::new();

        for job in &jobs {
            let Some(build) = &job.last_build else { continue };
            // Only report builds that finished after the watermark. In-flight
            // builds are reported once they complete.
            if build.building == Some(true) {
                continue;
            }
            let Some(finished) = build_finished_at(build) else {
                continue;
            };
            if finished <= since {
                continue;
            }

            let result = build.result.clone().unwrap_or_else(|| "UNKNOWN".to_string());
            let level = match result.as_str() {
                "SUCCESS" => LogLevel::Info,
                "UNSTABLE" | "ABORTED" => LogLevel::Warn,
                "FAILURE" => LogLevel::Error,
                _ => LogLevel::Info,
            };

            events.push(LogEvent::new(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Jenkins,
                &job.name,
                level,
                format!(
                    "Build #{} finished: {result}{}",
                    build.number,
                    build
                        .duration
                        .filter(|d| *d > 0)
                        .map(|d| format!(" in {:.1}s", d as f64 / 1000.0))
                        .unwrap_or_default()
                ),
                finished,
            )
            // One discrete line per finished build — not runtime output.
            .stream(LogStream::Event));

            if events.len() >= limit.max(self.log_limit) {
                break;
            }
        }

        events.sort_by_key(|e| e.timestamp);
        events.truncate(limit);
        Ok(events)
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        match query {
            DetailQuery::JenkinsBuildLog {
                job,
                build,
                tail_lines,
            } => {
                let path = job_path(job)
                    .ok_or_else(|| AppError::Collector(format!("invalid job name `{job}`")))?;
                let build_ref = build
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "lastBuild".to_string());
                let console = self.get_text(&format!("{path}/{build_ref}/consoleText")).await?;
                let lines: Vec<&str> = console.lines().collect();
                let take = tail_lines.unwrap_or(200).clamp(1, 5000);
                let tail: Vec<&str> = lines
                    .iter()
                    .skip(lines.len().saturating_sub(take))
                    .copied()
                    .collect();
                Ok(serde_json::json!({
                    "job": job,
                    "build": build_ref,
                    "totalLines": lines.len(),
                    "console": tail.join("\n"),
                }))
            }
            _ => Err(AppError::UnsupportedAction("Jenkins".to_string())),
        }
    }

    async fn run_action(&self, action: &CollectorAction) -> Result<String> {
        match action {
            CollectorAction::TriggerJenkinsBuild { job } => {
                let path = job_path(job)
                    .ok_or_else(|| AppError::Collector(format!("invalid job name `{job}`")))?;
                let url = format!("{}{path}/build", self.base_url);

                let mut req = self.authed(self.client.post(&url));
                // Jenkins rejects POSTs without a CSRF crumb unless the
                // protection is disabled, in which case this lookup 404s and we
                // proceed without one.
                if let Ok(crumb) = self
                    .get_json::<Crumb>("/crumbIssuer/api/json")
                    .await
                {
                    req = req.header(crumb.crumb_request_field, crumb.crumb);
                }

                let resp = req.send().await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AppError::Collector(format!(
                        "Jenkins refused the build trigger ({status}): {}",
                        body.chars().take(200).collect::<String>()
                    )));
                }
                Ok(format!("Queued a build of `{job}`."))
            }
            _ => Err(AppError::UnsupportedAction("Jenkins".to_string())),
        }
    }
}

/// `folder/job` becomes `/job/folder/job/job`, which is how Jenkins nests URLs.
fn job_path(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let segments: Vec<String> = name
        .split('/')
        .filter(|s| !s.is_empty())
        .map(urlencode)
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(format!("/job/{}", segments.join("/job/")))
}

fn build_finished_at(build: &Build) -> Option<DateTime<Utc>> {
    let started_ms = build.timestamp?;
    let duration_ms = build.duration.unwrap_or(0).max(0);
    Utc.timestamp_millis_opt(started_ms + duration_ms).single()
}

/// Minimal percent-encoding for path and query segments. Jenkins job names may
/// contain spaces and other characters that must not be sent raw.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b'[' | b']' | b',' => out.push(*byte as char), // Jenkins `tree=` syntax
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_nested_job_paths() {
        assert_eq!(job_path("build").unwrap(), "/job/build");
        assert_eq!(job_path("team/api").unwrap(), "/job/team/job/api");
        assert_eq!(job_path("my job").unwrap(), "/job/my%20job");
        assert!(job_path("").is_none());
    }

    #[test]
    fn maps_ball_colors_to_health() {
        let job = |color: &str| Job {
            name: "j".into(),
            color: Some(color.to_string()),
            buildable: Some(true),
            last_build: None,
            builds: Vec::new(),
            jobs: Vec::new(),
        };
        assert_eq!(JenkinsCollector::job_health(&job("blue")).0, Health::Healthy);
        assert_eq!(JenkinsCollector::job_health(&job("red")).0, Health::Down);
        assert_eq!(JenkinsCollector::job_health(&job("yellow")).0, Health::Degraded);
        // `_anime` means a build is running; the underlying colour still rules.
        assert_eq!(
            JenkinsCollector::job_health(&job("blue_anime")).0,
            Health::Healthy
        );
    }
}
