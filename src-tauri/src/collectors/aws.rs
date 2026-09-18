//! S3 and SQS, as one account's worth of storage and queues.
//!
//! Every other collector here connects to a thing and asks how it is. AWS is
//! not a thing, it is a hundred services behind one credential, so the shape
//! has to be different: one connection is an **account and region**, and the
//! individual buckets and queues are components inside it — the same model as
//! containers on a Docker host or exports on an NFS client.
//!
//! ## Why these two first
//!
//! Because they are the two where root's whole argument lands hardest.
//!
//! A bucket has a size that only ever grows and a lifecycle policy that
//! decides whether it ever stops. Neither number means much alone; together
//! they are "this grows 40 GB a day and nothing will ever delete any of it",
//! which is a bill nobody has read yet.
//!
//! A queue has a depth, a count in flight, and a redrive policy. Depth alone
//! is meaningless — a busy queue is supposed to have messages in it. Depth
//! with **nothing in flight** means no consumer is even trying, which is a
//! different and much worse thing. And a queue with no dead-letter target
//! retries a poison message until somebody notices, which is a config fact no
//! metric can show you.
//!
//! ## The measurement that is missing, and why it stays missing
//!
//! The number you actually want is the age of the oldest message, and SQS does
//! not expose it through `GetQueueAttributes` — it exists only as a CloudWatch
//! metric.
//!
//! There is an obvious workaround: peek at the head of the queue with
//! `ReceiveMessage` and read its `SentTimestamp`. It is not taken here,
//! because a receive increments each message's `ApproximateReceiveCount` even
//! at a visibility timeout of zero. A queue with `maxReceiveCount: 3` polled
//! every fifteen seconds would have its entire contents in the dead-letter
//! queue inside a minute — the monitoring tool causing the outage it is
//! watching for. A monitor may not change the thing it observes, and no metric
//! is worth that.
//!
//! So the age is left to CloudWatch, and until that exists this reports the
//! signals that cost nothing: depth, in flight, and whether anything is
//! consuming at all.
//!
//! ## What this does not do
//!
//! CloudWatch. Almost every AWS monitoring tool is a CloudWatch reader, and
//! for good reason — it is where the interesting numbers live for Lambda,
//! RDS, DynamoDB and everything else. It is also a metered API where a
//! careless poll loop is a line item, so it wants its own deliberate design
//! rather than being tacked on here.
//!
//! What this reads instead is the services' own APIs, which for S3 and SQS
//! carry the numbers that matter and cost nothing beyond a request.
//!
//! ## Counting objects
//!
//! There is no cheap "how big is this bucket" call. `ListObjectsV2` is
//! paginated and a bucket with ten million objects is ten thousand requests,
//! so this walks a bounded number of pages and says plainly when it stopped
//! rather than either hanging or quietly reporting a fraction as a total. A
//! partial figure presented as complete would make a capacity forecast that is
//! confidently wrong, which is worse than not having one.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

/// Pages of `ListObjectsV2` walked per bucket before giving up.
///
/// A thousand objects a page, so this is a million objects — past which the
/// answer is reported as a floor rather than a total. Chosen so a poll of a
/// large estate stays inside a few seconds.
const MAX_OBJECT_PAGES: usize = 20;

/// Buckets and queues enumerated before the listing is truncated.
const MAX_RESOURCES: usize = 200;

pub struct AwsCollector {
    s3: aws_sdk_s3::Client,
    sqs: aws_sdk_sqs::Client,
    profile_id: String,
    profile_name: String,
    /// Depth at which a queue is called backed up.
    queue_depth_warn: f64,
    /// Depth at which a queue with no consumer at all is called stalled.
    ///
    /// Lower than `queue_depth_warn`, because a queue nothing is reading is a
    /// worse state at a smaller number than a queue that is merely behind.
    queue_stalled_warn: f64,
    /// Bucket size, in bytes, worth warning about.
    bucket_bytes_warn: Option<f64>,
}

impl AwsCollector {
    pub async fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let region = profile
            .options
            .string("region")
            .unwrap_or_else(|| "us-east-1".to_string());

        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region));

        // An endpoint means an emulator or a private gateway. Empty means real
        // AWS, resolved the normal way.
        let endpoint = profile.endpoint.trim();
        if !endpoint.is_empty() {
            loader = loader.endpoint_url(endpoint);
        }

        // Explicit credentials when the profile carries them; otherwise the
        // standard chain — environment, profile, instance role — because on
        // an EC2 host or in a pod the right answer is to have no credentials
        // configured at all and let the role do it.
        // Half-configured is a mistake, not a fallback. Silently dropping to
        // the default chain when only one half is filled in produces "no
        // credentials found" against a profile that visibly has a secret in
        // it, and the operator goes looking in the wrong place.
        match (profile.username.as_deref(), secret.as_deref()) {
            (Some(key), None) if !key.trim().is_empty() => {
                return Err(AppError::Collector(
                    "an access key id is set but no secret access key — add the secret, or \
                     clear the key id to use the default credential chain"
                        .to_string(),
                ));
            }
            (None, Some(_)) => {
                return Err(AppError::Collector(
                    "a secret access key is set but no access key id — add the key id, or \
                     clear the secret to use the default credential chain"
                        .to_string(),
                ));
            }
            _ => {}
        }

        if let (Some(access_key), Some(secret_key)) = (profile.username.as_deref(), secret.as_deref())
        {
            loader = loader.credentials_provider(aws_sdk_s3::config::Credentials::new(
                access_key,
                secret_key,
                None,
                None,
                "root-profile",
            ));
        }

        let shared = loader.load().await;

        // Path-style addressing, because every emulator serves one host and
        // virtual-hosted style would resolve `bucket.localhost` and fail.
        let s3 = aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::config::Builder::from(&shared)
                .force_path_style(!endpoint.is_empty())
                .build(),
        );

        Ok(AwsCollector {
            s3,
            sqs: aws_sdk_sqs::Client::new(&shared),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            queue_depth_warn: profile.options.u64("queueDepthWarn").unwrap_or(1000) as f64,
            queue_stalled_warn: profile.options.u64("queueStalledWarn").unwrap_or(1) as f64,
            bucket_bytes_warn: profile
                .options
                .u64("bucketBytesWarn")
                .map(|bytes| bytes as f64),
        })
    }
}

/// One bucket, as far as a bounded walk could tell.
struct Bucket {
    name: String,
    objects: u64,
    bytes: u64,
    /// True when the walk hit its page ceiling, so the figures are a floor.
    truncated: bool,
    /// Whether anything will ever delete from this bucket.
    lifecycle_rules: usize,
    versioning: bool,
    error: Option<String>,
}

/// One queue and what it says about whether anything is draining it.
struct Queue {
    name: String,
    visible: f64,
    in_flight: f64,
    delayed: f64,
    /// A queue with no dead-letter target retries a poison message forever.
    has_dlq: bool,
    is_dlq: bool,
    error: Option<String>,
}

impl AwsCollector {
    async fn buckets(&self) -> Result<Vec<Bucket>> {
        let listed = self
            .s3
            .list_buckets()
            .send()
            .await
            .map_err(|e| AppError::Collector(format!("could not list buckets: {}", sdk_error(&e))))?;

        let mut out = Vec::new();
        for bucket in listed.buckets().iter().take(MAX_RESOURCES) {
            let Some(name) = bucket.name() else { continue };
            out.push(self.bucket(name).await);
        }
        Ok(out)
    }

    async fn bucket(&self, name: &str) -> Bucket {
        let mut objects = 0u64;
        let mut bytes = 0u64;
        let mut truncated = false;
        let mut token: Option<String> = None;
        let mut error = None;

        for page in 0..MAX_OBJECT_PAGES {
            let mut request = self.s3.list_objects_v2().bucket(name);
            if let Some(token) = &token {
                request = request.continuation_token(token);
            }

            match request.send().await {
                Ok(response) => {
                    for object in response.contents() {
                        objects += 1;
                        bytes += object.size().unwrap_or(0).max(0) as u64;
                    }
                    match response.next_continuation_token() {
                        Some(next) => {
                            token = Some(next.to_string());
                            if page + 1 == MAX_OBJECT_PAGES {
                                truncated = true;
                            }
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    error = Some(sdk_error(&e));
                    break;
                }
            }
        }

        // Absent is the interesting case for both of these, so a failure to
        // read them is reported rather than defaulted: "no lifecycle rules"
        // and "we could not tell" lead to different actions.
        let lifecycle_rules = match self
            .s3
            .get_bucket_lifecycle_configuration()
            .bucket(name)
            .send()
            .await
        {
            Ok(config) => config.rules().len(),
            // AWS returns an error rather than an empty list when no
            // configuration exists, which is the commonest case in the world.
            Err(_) => 0,
        };

        let versioning = self
            .s3
            .get_bucket_versioning()
            .bucket(name)
            .send()
            .await
            .ok()
            .and_then(|v| v.status().map(|s| s.as_str() == "Enabled"))
            .unwrap_or(false);

        Bucket {
            name: name.to_string(),
            objects,
            bytes,
            truncated,
            lifecycle_rules,
            versioning,
            error,
        }
    }

    async fn queues(&self) -> Result<Vec<Queue>> {
        let listed = self
            .sqs
            .list_queues()
            .send()
            .await
            .map_err(|e| AppError::Collector(format!("could not list queues: {}", sdk_error(&e))))?;

        let mut out = Vec::new();
        for url in listed.queue_urls().iter().take(MAX_RESOURCES) {
            out.push(self.queue(url).await);
        }
        Ok(out)
    }

    async fn queue(&self, url: &str) -> Queue {
        let name = url.rsplit('/').next().unwrap_or(url).to_string();
        let is_dlq = name.ends_with("-dlq") || name.ends_with("-dead-letter");

        let attributes = self
            .sqs
            .get_queue_attributes()
            .queue_url(url)
            .attribute_names(aws_sdk_sqs::types::QueueAttributeName::All)
            .send()
            .await;

        let Ok(attributes) = attributes else {
            return Queue {
                name,
                visible: 0.0,
                in_flight: 0.0,
                delayed: 0.0,
                has_dlq: false,
                is_dlq,
                error: attributes.err().map(|e| sdk_error(&e)),
            };
        };

        let read = |key: aws_sdk_sqs::types::QueueAttributeName| -> f64 {
            attributes
                .attributes()
                .and_then(|a| a.get(&key))
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0)
        };

        use aws_sdk_sqs::types::QueueAttributeName as Attr;
        let visible = read(Attr::ApproximateNumberOfMessages);

        Queue {
            name,
            visible,
            in_flight: read(Attr::ApproximateNumberOfMessagesNotVisible),
            delayed: read(Attr::ApproximateNumberOfMessagesDelayed),
            has_dlq: attributes
                .attributes()
                .and_then(|a| a.get(&Attr::RedrivePolicy))
                .is_some_and(|policy| policy.contains("deadLetterTargetArn")),
            is_dlq,
            error: None,
        }
    }
}

#[async_trait]
impl Collector for AwsCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Aws
    }

    // Health is decided as a chain of `else if`s naming one condition each,
    // and several arms legitimately return the same `Health`. The chain mirrors
    // the `summary` chain below it so the two read in the same order and a
    // reader can see which condition produced which sentence; collapsing the
    // arms with `||` would satisfy the lint and break that correspondence.
    #[allow(clippy::if_same_then_else)]
    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let (buckets, queues) = tokio::join!(self.buckets(), self.queues());

        // Unreachable is Down with a summary, not an error — an error is for
        // a fault in this process, and an endpoint that will not answer is a
        // fact about the estate.
        let (buckets, queues) = match (buckets, queues) {
            (Ok(buckets), Ok(queues)) => (buckets, queues),
            (Err(e), _) | (_, Err(e)) => {
                return Ok(HealthSnapshot {
                    profile_id: self.profile_id.clone(),
                    profile_name: self.profile_name.clone(),
                    kind: ServiceKind::Aws,
                    health: Health::Down,
                    summary: e.to_string(),
                    metrics: Vec::new(),
                    sections: Vec::new(),
                    checked_at: Utc::now(),
                    latency_ms: elapsed_ms(start),
                    error: None,
                });
            }
        };

        let latency = elapsed_ms(start);

        let total_bytes: f64 = buckets.iter().map(|b| b.bytes as f64).sum();
        let total_objects: f64 = buckets.iter().map(|b| b.objects as f64).sum();
        // Dead-letter queues are excluded from the backlog figure: messages
        // sitting in a DLQ are already-failed work waiting to be looked at,
        // and counting them as backlog would make a queue read as behind when
        // what it actually is, is finished and unhappy.
        let backlog: f64 = queues
            .iter()
            .filter(|q| !q.is_dlq)
            .map(|q| q.visible)
            .sum();
        // Messages waiting with nothing in flight: no consumer is even
        // trying. Worse than a deep queue that is moving, and visible without
        // the age metric SQS will not give us.
        let unattended: f64 = queues
            .iter()
            .filter(|q| !q.is_dlq && q.in_flight == 0.0)
            .map(|q| q.visible)
            .sum();
        let dlq_depth: f64 = queues.iter().filter(|q| q.is_dlq).map(|q| q.visible).sum();

        let unreachable = buckets.iter().filter(|b| b.error.is_some()).count()
            + queues.iter().filter(|q| q.error.is_some()).count();

        let health = if unreachable > 0 {
            Health::Degraded
        } else if unattended >= self.queue_stalled_warn || backlog >= self.queue_depth_warn {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = format!(
            "{} bucket{} · {} queue{}{}",
            buckets.len(),
            if buckets.len() == 1 { "" } else { "s" },
            queues.len(),
            if queues.len() == 1 { "" } else { "s" },
            if backlog > 0.0 {
                format!(" · {backlog:.0} queued")
            } else {
                String::new()
            },
        );

        let mut metrics = vec![
            Metric::new("buckets", "Buckets", buckets.len() as f64, MetricUnit::Count),
            Metric::new("queues", "Queues", queues.len() as f64, MetricUnit::Count),
            Metric::new("objects", "Objects", total_objects, MetricUnit::Count),
            Metric::new("stored_bytes", "Stored", total_bytes, MetricUnit::Bytes),
            Metric::new("queue_backlog", "Queued messages", backlog, MetricUnit::Count)
                .warn_above(self.queue_depth_warn),
            Metric::new(
                "unattended_messages",
                "Waiting, nothing consuming",
                unattended,
                MetricUnit::Count,
            )
            .warn_above(self.queue_stalled_warn),
        ];
        if let Some(limit) = self.bucket_bytes_warn {
            metrics[3] = Metric::new("stored_bytes", "Stored", total_bytes, MetricUnit::Bytes)
                .warn_above(limit);
        }
        if dlq_depth > 0.0 {
            // Any depth at all is worth seeing, so the threshold is zero
            // rather than a guess at how many failures are acceptable.
            metrics.push(
                Metric::new("dlq_depth", "In dead-letter queues", dlq_depth, MetricUnit::Count)
                    .warn_above(0.0),
            );
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Aws,
            health,
            summary,
            metrics,
            sections: vec![bucket_section(&buckets), queue_section(&queues, self)],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Bucket and queue configuration — the half that says whether the numbers
    /// above are a problem.
    async fn config(&self) -> Result<Vec<crate::configsource::ConfigDocument>> {
        use crate::configsource::ConfigDocument;

        let (buckets, queues) = tokio::join!(self.buckets(), self.queues());
        let mut map = serde_json::Map::new();

        if let Ok(buckets) = buckets {
            let mut entries = serde_json::Map::new();
            for bucket in &buckets {
                entries.insert(
                    bucket.name.clone(),
                    serde_json::json!({
                        "lifecycleRules": bucket.lifecycle_rules,
                        "versioning": bucket.versioning,
                        "objects": bucket.objects,
                        "bytes": bucket.bytes,
                        "countTruncated": bucket.truncated,
                    }),
                );
            }
            map.insert("buckets".to_string(), serde_json::Value::Object(entries));
        }

        if let Ok(queues) = queues {
            let mut entries = serde_json::Map::new();
            for queue in &queues {
                entries.insert(
                    queue.name.clone(),
                    serde_json::json!({
                        "hasDeadLetterQueue": queue.has_dlq,
                        "isDeadLetterQueue": queue.is_dlq,
                        "visible": queue.visible,
                        "inFlight": queue.in_flight,
                    }),
                );
            }
            map.insert("queues".to_string(), serde_json::Value::Object(entries));
        }

        Ok(vec![ConfigDocument::live(
            &self.profile_id,
            "Storage and queue configuration",
            "s3/sqs attributes",
            &serde_json::Value::Object(map),
        )])
    }

    /// S3 and SQS have no log stream of their own — that is CloudTrail, which
    /// is a different service and a different design.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

fn bucket_section(buckets: &[Bucket]) -> Section {
    Section::new(
        "buckets",
        "Buckets",
        vec![
            ColumnSpec::text("name", "Bucket").primary(),
            ColumnSpec::status("state", "State"),
            ColumnSpec::metric("bytes", "Size", MetricUnit::Bytes),
            ColumnSpec::metric("objects", "Objects", MetricUnit::Count),
            ColumnSpec::text("lifecycle", "Lifecycle"),
            ColumnSpec::text("versioning", "Versioning"),
        ],
    )
    .rows(
        buckets
            .iter()
            .map(|bucket| {
                let state = if bucket.error.is_some() {
                    Health::Down
                } else {
                    Health::Healthy
                };

                let summary = match &bucket.error {
                    Some(e) => e.clone(),
                    None if bucket.truncated => format!(
                        "at least {} objects — the walk stopped at its page limit, so this is a \
                         floor rather than a total",
                        bucket.objects
                    ),
                    None if bucket.lifecycle_rules == 0 => {
                        "no lifecycle rules — nothing here is ever deleted".to_string()
                    }
                    None => format!("{} lifecycle rule(s)", bucket.lifecycle_rules),
                };

                Row::new(format!("bucket:{}", bucket.name), bucket.name.clone(), state)
                    .summary(summary)
                    .cell("name", CellValue::text(bucket.name.clone()))
                    .cell("state", CellValue::status(state))
                    .cell("bytes", CellValue::number(bucket.bytes as f64))
                    .cell("objects", CellValue::number(bucket.objects as f64))
                    .cell(
                        "lifecycle",
                        CellValue::text(if bucket.lifecycle_rules == 0 {
                            "none".to_string()
                        } else {
                            format!("{} rule(s)", bucket.lifecycle_rules)
                        }),
                    )
                    .cell(
                        "versioning",
                        CellValue::text(if bucket.versioning { "on" } else { "off" }),
                    )
            })
            .collect(),
    )
    // Tracked, so each bucket gets its own series: the growth that matters is
    // one bucket's, and a total across an account hides which one is moving.
    .tracked()
    .priority(10)
}

fn queue_section(queues: &[Queue], collector: &AwsCollector) -> Section {
    Section::new(
        "queues",
        "Queues",
        vec![
            ColumnSpec::text("name", "Queue").primary(),
            ColumnSpec::status("state", "State"),
            ColumnSpec::gauge("visible", "Waiting", MetricUnit::Count),
            ColumnSpec::metric("in_flight", "In flight", MetricUnit::Count),
            ColumnSpec::metric("delayed", "Delayed", MetricUnit::Count),
            ColumnSpec::text("dlq", "Dead letters"),
        ],
    )
    .rows(
        queues
            .iter()
            .map(|queue| {
                // Depth alone says nothing — a busy queue is supposed to have
                // messages in it. Messages waiting with nothing in flight is
                // what says no consumer is running.
                let unattended = queue.visible > 0.0 && queue.in_flight == 0.0;
                let state = if queue.error.is_some() {
                    Health::Down
                } else if (unattended && queue.visible >= collector.queue_stalled_warn && !queue.is_dlq)
                    || (queue.is_dlq && queue.visible > 0.0)
                {
                    Health::Degraded
                } else {
                    Health::Healthy
                };

                let summary = match &queue.error {
                    Some(e) => e.clone(),
                    None if queue.is_dlq && queue.visible > 0.0 => format!(
                        "{} message(s) failed their way in here and nobody has looked",
                        queue.visible
                    ),
                    None if unattended && !queue.is_dlq => format!(
                        "{:.0} waiting and nothing in flight — no consumer is reading this",
                        queue.visible
                    ),
                    None if !queue.has_dlq && !queue.is_dlq => {
                        "no dead-letter queue — a poison message retries forever".to_string()
                    }
                    None => format!("{:.0} waiting", queue.visible),
                };

                Row::new(format!("queue:{}", queue.name), queue.name.clone(), state)
                    .summary(summary)
                    .cell("name", CellValue::text(queue.name.clone()))
                    .cell("state", CellValue::status(state))
                    .cell(
                        "visible",
                        CellValue::gauge_warn(
                            queue.visible,
                            Some(collector.queue_depth_warn.max(queue.visible)),
                            collector.queue_depth_warn,
                        ),
                    )
                    .cell("in_flight", CellValue::number(queue.in_flight))
                    .cell("delayed", CellValue::number(queue.delayed))
                    .cell(
                        "dlq",
                        CellValue::text(match (queue.is_dlq, queue.has_dlq) {
                            (true, _) => "is one".to_string(),
                            (false, true) => "configured".to_string(),
                            (false, false) => "none".to_string(),
                        }),
                    )
            })
            .collect(),
    )
    .tracked()
    .priority(9)
}

/// The useful line out of an SDK error.
///
/// The `Display` of an SDK error is a wrapper naming the operation; the reason
/// is one level down in the source, and without unwrapping it every failure
/// reads as "service error" whatever actually happened.
fn sdk_error<E: std::error::Error>(error: &aws_sdk_s3::error::SdkError<E>) -> String
where
    aws_sdk_s3::error::SdkError<E>: std::error::Error,
{
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        message = inner.to_string();
        source = inner.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(name: &str, visible: f64, in_flight: f64, has_dlq: bool) -> Queue {
        Queue {
            name: name.to_string(),
            visible,
            in_flight,
            delayed: 0.0,
            has_dlq,
            is_dlq: name.ends_with("-dlq"),
            error: None,
        }
    }

    /// Depth alone is meaningless: a busy queue is supposed to have messages
    /// in it. Nothing in flight is what says no consumer is running.
    #[test]
    fn a_deep_but_moving_queue_is_not_the_same_as_an_unattended_one() {
        let busy = queue("orders", 5000.0, 120.0, true);
        let unattended = queue("orders", 40.0, 0.0, true);

        assert!(busy.in_flight > 0.0, "something is reading a busy queue");
        assert_eq!(
            unattended.in_flight, 0.0,
            "a shallow queue with no consumer is the worse state"
        );
    }

    /// A dead-letter queue holds already-failed work. Counting it as backlog
    /// makes a finished-and-unhappy queue read as behind.
    #[test]
    fn dead_letter_queues_are_not_counted_as_backlog() {
        let queues = [
            queue("orders", 10.0, 5.0, true),
            queue("orders-dlq", 7.0, 0.0, false),
        ];

        let backlog: f64 = queues.iter().filter(|q| !q.is_dlq).map(|q| q.visible).sum();
        let dlq: f64 = queues.iter().filter(|q| q.is_dlq).map(|q| q.visible).sum();

        assert_eq!(backlog, 10.0);
        assert_eq!(dlq, 7.0);
    }

    #[test]
    fn a_dlq_is_recognised_by_its_name() {
        assert!(queue("jobs-dlq", 0.0, 0.0, false).is_dlq);
        assert!(!queue("jobs", 0.0, 0.0, false).is_dlq);
    }

    /// A partial count presented as a total would make a capacity forecast
    /// that is confidently wrong, which is worse than not having one.
    #[test]
    fn a_truncated_walk_says_so_rather_than_reporting_a_fraction_as_a_total() {
        let bucket = Bucket {
            name: "big".into(),
            objects: 20_000,
            bytes: 1_000_000,
            truncated: true,
            lifecycle_rules: 0,
            versioning: false,
            error: None,
        };
        let section = bucket_section(&[bucket]);
        let summary = &section.rows[0].summary;
        assert!(summary.contains("at least"), "{summary}");
        assert!(summary.contains("floor"), "{summary}");
    }

    /// The commonest real misconfiguration in S3, and one no metric shows.
    #[test]
    fn a_bucket_with_no_lifecycle_rules_says_nothing_is_ever_deleted() {
        let bucket = Bucket {
            name: "uploads".into(),
            objects: 10,
            bytes: 100,
            truncated: false,
            lifecycle_rules: 0,
            versioning: false,
            error: None,
        };
        let section = bucket_section(&[bucket]);
        assert!(
            section.rows[0].summary.contains("ever deleted"),
            "{}",
            section.rows[0].summary
        );
    }

    #[test]
    fn a_bucket_that_could_not_be_read_is_down_with_its_reason() {
        let bucket = Bucket {
            name: "denied".into(),
            objects: 0,
            bytes: 0,
            truncated: false,
            lifecycle_rules: 0,
            versioning: false,
            error: Some("access denied".into()),
        };
        let section = bucket_section(&[bucket]);
        assert_eq!(section.rows[0].health, Health::Down);
        assert_eq!(section.rows[0].summary, "access denied");
    }
}
