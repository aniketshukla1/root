//! Tomcat monitor, via the manager's JMX proxy.
//!
//! `GET /manager/jmxproxy?qry=Catalina:type=ThreadPool,*` returns plain text:
//! a header line per matching bean, then `name: value` for each attribute.
//!
//! ```text
//! OK - Attributes for Catalina:type=ThreadPool,name="http-nio-8080"
//! maxThreads: 200
//! currentThreadCount: 25
//! currentThreadsBusy: 3
//! ```
//!
//! The manager also serves `/manager/status?XML=true`, which is the more
//! commonly enabled endpoint — and it is XML, which this build has no parser
//! for. jmxproxy is text, needs no dependency, and gives the exact attributes
//! rather than a rendering of them. It does need the `manager-jmx` role.
//!
//! Thread pools are read per connector and never summed, because summing them
//! is how the failure hides. A host serving http-nio-8080 and https-nio-8443
//! has two independent pools with independent limits: 200 busy of 400 total
//! looks like half capacity and can be one pool completely exhausted while the
//! other sits idle. Requests to the exhausted connector queue in the accept
//! backlog and time out; the average says everything is fine.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct TomcatCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// Percent of a connector's threads busy above which Tomcat is degraded.
    thread_warn: f64,
    profile_id: String,
    profile_name: String,
    previous: Mutex<Option<Sample>>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    requests: f64,
    errors: f64,
    at: DateTime<Utc>,
}

/// One JMX bean and its attributes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bean {
    /// The `name="..."` part of the object name, which is the connector.
    pub name: String,
    pub attributes: BTreeMap<String, String>,
}

impl Bean {
    pub fn number(&self, key: &str) -> Option<f64> {
        self.attributes.get(key).and_then(|v| v.parse::<f64>().ok())
    }

    /// Threads in use as a percentage of this connector's own ceiling.
    ///
    /// `maxThreads` of -1 means unlimited, which Tomcat does allow. A
    /// percentage of unlimited is not a number, so it is `None` rather than a
    /// division that would report infinity or zero.
    pub fn thread_use(&self) -> Option<f64> {
        let busy = self.number("currentThreadsBusy")?;
        let max = self.number("maxThreads")?;
        (max > 0.0).then(|| (busy / max) * 100.0)
    }
}

/// Pull the connector name out of a JMX object name.
///
/// `Catalina:type=ThreadPool,name="http-nio-8080"` → `http-nio-8080`. Falls
/// back to the whole object name so an unusual shape is still labelled with
/// something an operator can match against their config.
pub(crate) fn bean_name(object_name: &str) -> String {
    object_name
        .split_once("name=")
        .map(|(_, rest)| rest.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| object_name.trim().to_string())
}

/// Parse a jmxproxy response into its beans.
pub(crate) fn parse_jmx(body: &str) -> Vec<Bean> {
    let mut beans: Vec<Bean> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(object_name) = line.strip_prefix("OK - Attributes for ") {
            beans.push(Bean {
                name: bean_name(object_name),
                attributes: BTreeMap::new(),
            });
            continue;
        }
        // Anything before the first header — including an `Error - ...` line —
        // has no bean to belong to and is dropped rather than invented one.
        let Some(bean) = beans.last_mut() else {
            continue;
        };
        if let Some((key, value)) = line.split_once(':') {
            bean.attributes
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    beans
}

impl TomcatCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "Tomcat connections need the base URL, e.g. http://tomcat.internal:8080".into(),
            ));
        }
        // Accept the base, or a URL already pointing at the manager, so
        // pasting either out of a browser works.
        let base = raw
            .trim_end_matches("/jmxproxy")
            .trim_end_matches("/manager")
            .to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(TomcatCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            thread_warn: profile
                .options
                .get("threadWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(85.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
        })
    }

    async fn query(&self, qry: &str) -> std::result::Result<Vec<Bean>, String> {
        let url = format!("{}/manager/jmxproxy", self.base);
        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&url),
        };
        let response = request
            .query(&[("qry", qry)])
            .send()
            .await
            .map_err(|e| format!("{url}: {e}"))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(format!(
                "{url} returned {} — jmxproxy needs a user with the `manager-jmx` role",
                response.status()
            ));
        }
        if !response.status().is_success() {
            return Err(format!("{url} returned {}", response.status()));
        }

        Ok(parse_jmx(&response.text().await.unwrap_or_default()))
    }
}

#[async_trait]
impl Collector for TomcatCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Tomcat
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let pools = match self.query("Catalina:type=ThreadPool,*").await {
            Ok(pools) => pools,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Tomcat,
                    message,
                    elapsed_ms(start),
                ));
            }
        };

        if pools.is_empty() {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Tomcat,
                format!(
                    "{}/manager/jmxproxy answered with no thread pools — check the manager \
                     webapp is deployed and the URL points at Tomcat's root",
                    self.base
                ),
                elapsed_ms(start),
            ));
        }

        // Request counters are a nicety; a build that will not serve them
        // degrades the poll rather than failing it.
        let processors = self
            .query("Catalina:type=GlobalRequestProcessor,*")
            .await
            .unwrap_or_default();
        let latency = elapsed_ms(start);

        let requests: f64 = processors.iter().filter_map(|p| p.number("requestCount")).sum();
        let errors: f64 = processors.iter().filter_map(|p| p.number("errorCount")).sum();

        let now = Utc::now();
        let mut previous = self.previous.lock().await;
        let rates = previous.and_then(|last| {
            let seconds = (now - last.at).num_milliseconds() as f64 / 1000.0;
            // A counter that went backwards is a restart, not a negative rate.
            if seconds <= 0.0 || requests < last.requests {
                return None;
            }
            Some((
                (requests - last.requests) / seconds,
                (errors - last.errors) / seconds,
            ))
        });
        *previous = Some(Sample {
            requests,
            errors,
            at: now,
        });
        drop(previous);

        // Per connector, never summed — see the module doc.
        let worst = pools
            .iter()
            .filter_map(|pool| pool.thread_use().map(|use_| (pool, use_)))
            .max_by(|a, b| a.1.total_cmp(&b.1));

        let exhausted = worst.is_some_and(|(_, use_)| use_ >= 100.0);
        let stretched = worst.is_some_and(|(_, use_)| use_ >= self.thread_warn);

        let health = if exhausted {
            Health::Down
        } else if stretched {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = match worst {
            Some((pool, _)) if exhausted => format!(
                "{} has no free threads — every one of {:.0} is busy",
                pool.name,
                pool.number("maxThreads").unwrap_or(0.0)
            ),
            Some((pool, use_)) if stretched => {
                format!("{} at {use_:.0}% of its thread limit", pool.name)
            }
            Some((_, _)) => match rates {
                Some((rps, _)) => format!(
                    "{} connector{} · {rps:.1} req/sec",
                    pools.len(),
                    if pools.len() == 1 { "" } else { "s" }
                ),
                None => format!(
                    "{} connector{}",
                    pools.len(),
                    if pools.len() == 1 { "" } else { "s" }
                ),
            },
            // Every pool reported an unlimited maxThreads. Real, and there is
            // no saturation to report.
            None => format!("{} connectors, none with a thread limit", pools.len()),
        };

        let mut metrics = Vec::new();
        if let Some((pool, use_)) = worst {
            let label = format!("Busiest connector ({})", pool.name);
            metrics.push(
                Metric::new("threads_busiest", &label, use_, MetricUnit::Percent)
                    .warn_above(self.thread_warn),
            );
        }
        metrics.push(Metric::new(
            "threads_busy",
            "Threads busy",
            pools.iter().filter_map(|p| p.number("currentThreadsBusy")).sum(),
            MetricUnit::Count,
        ));
        metrics.push(Metric::new("requests_total", "Requests", requests, MetricUnit::Count));
        metrics.push(
            Metric::new("errors_total", "Errors", errors, MetricUnit::Count).warn_above(0.0),
        );
        if let Some((rps, eps)) = rates {
            metrics.push(Metric::new("request_rate", "Requests/sec", rps, MetricUnit::PerSecond));
            metrics.push(
                Metric::new("error_rate", "Errors/sec", eps, MetricUnit::PerSecond)
                    .warn_above(0.0),
            );
        }
        let bytes_sent: f64 = processors.iter().filter_map(|p| p.number("bytesSent")).sum();
        metrics.push(Metric::new("bytes_sent", "Bytes sent", bytes_sent, MetricUnit::Bytes));

        let pool_rows: Vec<Row> = pools
            .iter()
            .map(|pool| {
                let use_ = pool.thread_use();
                let state = match use_ {
                    Some(pct) if pct >= 100.0 => Health::Down,
                    Some(pct) if pct >= self.thread_warn => Health::Degraded,
                    Some(_) => Health::Healthy,
                    // No limit configured: nothing to be saturated against.
                    None => Health::Unknown,
                };
                Row::new(format!("pool:{}", pool.name), pool.name.clone(), state)
                    .summary(match use_ {
                        Some(pct) => format!("{pct:.0}% of this connector's thread limit"),
                        None => "no thread limit configured".to_string(),
                    })
                    .cell("connector", CellValue::text(pool.name.clone()))
                    .cell("state", CellValue::status(state))
                    .cell(
                        "busy",
                        CellValue::gauge_warn(
                            pool.number("currentThreadsBusy").unwrap_or(0.0),
                            pool.number("maxThreads").filter(|m| *m > 0.0),
                            pool.number("maxThreads").unwrap_or(0.0) * (self.thread_warn / 100.0),
                        ),
                    )
                    .cell(
                        "threads",
                        CellValue::number(pool.number("currentThreadCount").unwrap_or(0.0)),
                    )
                    .cell(
                        "connections",
                        CellValue::number(pool.number("connectionCount").unwrap_or(0.0)),
                    )
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Tomcat,
            health,
            summary,
            metrics,
            sections: vec![Section::new(
                "connectors",
                "Connectors",
                vec![
                    ColumnSpec::text("connector", "Connector").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::gauge("busy", "Threads busy", MetricUnit::Count),
                    ColumnSpec::metric("threads", "Threads started", MetricUnit::Count),
                    ColumnSpec::metric("connections", "Connections", MetricUnit::Count),
                ],
            )
            .rows(pool_rows)
            .tracked()
            .empty_text("Tomcat reported no connectors.")
            .priority(10)],
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// catalina.out is a file on the host. jmxproxy serves attributes, not a
    /// journal.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_CONNECTORS: &str = "OK - Attributes for Catalina:type=ThreadPool,name=\"http-nio-8080\"\n\
                                  modelerType: org.apache.tomcat.util.net.NioEndpoint\n\
                                  maxThreads: 200\n\
                                  currentThreadCount: 200\n\
                                  currentThreadsBusy: 200\n\
                                  connectionCount: 412\n\
                                  OK - Attributes for Catalina:type=ThreadPool,name=\"https-nio-8443\"\n\
                                  maxThreads: 200\n\
                                  currentThreadCount: 10\n\
                                  currentThreadsBusy: 2\n\
                                  connectionCount: 6\n";

    #[test]
    fn splits_a_response_into_beans() {
        let beans = parse_jmx(TWO_CONNECTORS);
        assert_eq!(beans.len(), 2);
        assert_eq!(beans[0].name, "http-nio-8080");
        assert_eq!(beans[1].name, "https-nio-8443");
        assert_eq!(beans[0].number("maxThreads"), Some(200.0));
    }

    #[test]
    fn reads_the_connector_name_out_of_the_object_name() {
        assert_eq!(
            bean_name("Catalina:type=ThreadPool,name=\"http-nio-8080\""),
            "http-nio-8080"
        );
    }

    /// An unusual object name still gets a label an operator can match against
    /// their config, rather than an empty cell.
    #[test]
    fn an_object_name_without_a_name_attribute_falls_back_to_itself() {
        assert_eq!(bean_name("Catalina:type=Server"), "Catalina:type=Server");
    }

    /// The whole reason pools are not summed. Across both connectors this is
    /// 202 of 400 busy — barely half — while one of them has nothing left.
    #[test]
    fn one_exhausted_connector_is_invisible_in_the_total() {
        let beans = parse_jmx(TWO_CONNECTORS);
        let total_busy: f64 = beans.iter().filter_map(|b| b.number("currentThreadsBusy")).sum();
        let total_max: f64 = beans.iter().filter_map(|b| b.number("maxThreads")).sum();
        assert_eq!((total_busy / total_max) * 100.0, 50.5);

        assert_eq!(beans[0].thread_use(), Some(100.0));
        assert_eq!(beans[1].thread_use(), Some(1.0));
    }

    /// Tomcat permits an unlimited pool, and a percentage of unlimited is not
    /// a number.
    #[test]
    fn an_unlimited_pool_has_no_saturation() {
        let beans = parse_jmx(
            "OK - Attributes for Catalina:type=ThreadPool,name=\"ajp\"\n\
             maxThreads: -1\n\
             currentThreadsBusy: 4\n",
        );
        assert_eq!(beans[0].thread_use(), None);
    }

    /// jmxproxy prefixes failures with `Error -`, and there is no bean for
    /// those lines to belong to.
    #[test]
    fn an_error_response_yields_no_beans() {
        assert!(parse_jmx("Error - java.lang.Exception: No MBeans found").is_empty());
    }

    #[test]
    fn an_empty_response_is_not_a_panic() {
        assert!(parse_jmx("").is_empty());
    }

    /// Attribute values contain colons — object names and class names both do
    /// — so only the first one separates.
    #[test]
    fn a_value_may_contain_a_colon() {
        let beans = parse_jmx(
            "OK - Attributes for Catalina:type=ThreadPool,name=\"a\"\n\
             modelerType: org.apache.tomcat.util.net.NioEndpoint\n",
        );
        assert_eq!(
            beans[0].attributes.get("modelerType").map(String::as_str),
            Some("org.apache.tomcat.util.net.NioEndpoint")
        );
    }
}
