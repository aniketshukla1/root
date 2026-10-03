//! Where root tells people.
//!
//! Until this module root could only tell whoever was looking at it: a toast in
//! the window, a badge on the sidebar. Monitoring that needs somebody watching
//! the monitor is a dashboard, and the failure that matters most is the one at
//! three in the morning.
//!
//! ## Channels and rules
//!
//! A channel is somewhere a message can go — a Slack or Teams channel, a
//! Discord channel, a PagerDuty service, or any URL that takes a JSON POST —
//! and the rules that decide which alerts it hears about: how severe, which
//! environments, which teams. The rules read the same ownership metadata the
//! Services screen shows, so "production only, payments team" is a setting
//! rather than a routing table somebody has to keep in step.
//!
//! ## A delay is an escalation
//!
//! A channel with a delay only hears about an alert that has gone that long
//! *unacknowledged*. That one setting is the whole of on-call escalation: chat
//! at once, PagerDuty after five minutes if nobody has said they have it. An
//! acknowledgement before the delay means the second channel never hears of it.
//!
//! ## The ledger
//!
//! Each channel is told about each occurrence of a condition once, and told
//! again when it resolves. The ledger is the record of what was told — keyed on
//! the channel and the condition's fingerprint, and holding the occurrence's
//! `first_seen` so a condition that resolves and fires again is a new message
//! rather than a silent repeat. It holds the alert as it was told, too, so a
//! resolution can still be described after the alert store has forgotten it.
//!
//! Delivery is diffed, not driven by events: every few seconds the loop
//! compares the alert store with the ledger and sends the difference. A missed
//! event cannot leave a channel out of step, an unreachable channel catches up
//! when it comes back, and nothing in the poll path waits on Slack.
//!
//! The ledger lives in memory. A restart during an incident tells the channels
//! again — the honest failure, since root cannot know who was told by a process
//! that no longer exists. PagerDuty deduplicates on the fingerprint, so the one
//! channel that pages is not paged twice.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::model::{Alert, AlertSeverity};
use crate::state::AppState;

/// How often the ledger is compared with the alert store.
const TICK: Duration = Duration::from_secs(10);

/// How long an alert must have been firing before anyone is told.
///
/// Alertmanager's `group_wait`, for the same two reasons: alerts that fire
/// together — a host going down takes its containers with it — arrive as one
/// message instead of eleven, and a condition that clears itself inside half a
/// minute was never worth waking anyone for.
const GROUP_WAIT_SECS: i64 = 30;

/// Alerts listed in one chat message. Past this the message says how many more.
const MESSAGE_CAP: usize = 10;

/// Deliveries remembered for the log in Settings.
const LOG_CAP: usize = 200;

const PAGERDUTY_EVENTS: &str = "https://events.pagerduty.com/v2/enqueue";

// ------------------------------------------------------------------- config

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelKind {
    #[serde(rename = "slack")]
    Slack,
    #[serde(rename = "teams")]
    Teams,
    #[serde(rename = "discord")]
    Discord,
    #[serde(rename = "pagerduty")]
    PagerDuty,
    #[serde(rename = "webhook")]
    Webhook,
    /// A banner on the machine running the desktop app. No URL, no key: the
    /// system's notification centre is the target.
    #[serde(rename = "desktop")]
    Desktop,
}

impl ChannelKind {
    pub fn label(self) -> &'static str {
        match self {
            ChannelKind::Slack => "Slack",
            ChannelKind::Teams => "Microsoft Teams",
            ChannelKind::Discord => "Discord",
            ChannelKind::PagerDuty => "PagerDuty",
            ChannelKind::Webhook => "Webhook",
            ChannelKind::Desktop => "Desktop",
        }
    }

    /// Whether this kind is reached through a URL or key kept in the keychain.
    pub fn needs_target(self) -> bool {
        self != ChannelKind::Desktop
    }

    /// PagerDuty opens an incident on a trigger and keeps it open until told
    /// otherwise, so for it a resolution is not a courtesy that can be turned
    /// off: without one, every page stays open forever.
    pub fn must_resolve(self) -> bool {
        self == ChannelKind::PagerDuty
    }

    /// Reject a target that is plainly the wrong thing before it is stored,
    /// rather than at three in the morning when the first alert tries it.
    pub fn validate(self, target: &str) -> Result<()> {
        let target = target.trim();
        if self == ChannelKind::Desktop {
            return Ok(());
        }
        if self == ChannelKind::PagerDuty {
            return if target.len() == 32 && target.chars().all(|c| c.is_ascii_alphanumeric()) {
                Ok(())
            } else {
                Err(AppError::Rejected(
                    "a PagerDuty integration key is 32 letters and digits — the Events API v2 \
                     key from the service's Integrations tab, not an API token"
                        .into(),
                ))
            };
        }

        let url = url::Url::parse(target)
            .map_err(|_| AppError::Rejected(format!("{} needs a webhook URL", self.label())))?;
        let plain_http_allowed = self == ChannelKind::Webhook;
        if url.scheme() != "https" && !(plain_http_allowed && url.scheme() == "http") {
            return Err(AppError::Rejected(format!(
                "{} webhook URLs start with https://",
                self.label()
            )));
        }
        let host = url.host_str().unwrap_or_default();
        match self {
            ChannelKind::Slack if !host.ends_with("slack.com") => Err(AppError::Rejected(
                "a Slack incoming webhook URL starts https://hooks.slack.com/".into(),
            )),
            ChannelKind::Discord
                if !(host.ends_with("discord.com") || host.ends_with("discordapp.com")) =>
            {
                Err(AppError::Rejected(
                    "a Discord webhook URL starts https://discord.com/api/webhooks/".into(),
                ))
            }
            _ => Ok(()),
        }
    }

    /// Enough of a target to recognise it by, and not enough to use it: the
    /// host and the last four characters. Stored in the config file, which is
    /// why it is not more.
    pub fn hint(self, target: &str) -> String {
        let target = target.trim();
        let tail: String = target
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        match url::Url::parse(target)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
        {
            Some(host) => format!("{host}/…{tail}"),
            None => format!("••••{tail}"),
        }
    }
}

/// One place root can tell, and what it should hear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyChannel {
    /// Empty on a channel being created; assigned when it is saved.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The least severe alert this channel hears about.
    #[serde(default = "default_min_severity")]
    pub min_severity: AlertSeverity,
    /// Environments it hears about. Empty means all of them.
    #[serde(default)]
    pub environments: Vec<String>,
    /// Teams it hears about, by name. Empty means every team, and connections
    /// with no team at all.
    #[serde(default)]
    pub teams: Vec<String>,
    /// Minutes an alert must stay unacknowledged before this channel hears of
    /// it. Zero is at once.
    #[serde(default)]
    pub delay_minutes: u32,
    /// Whether it is told when an alert clears. Always, for PagerDuty.
    #[serde(default = "default_true")]
    pub send_resolved: bool,
    /// See [`ChannelKind::hint`]. Set by root when a target is saved.
    #[serde(default)]
    pub target_hint: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_min_severity() -> AlertSeverity {
    AlertSeverity::Warning
}

impl NotifyChannel {
    /// The keychain entry holding the URL or key. Never in the config file:
    /// anyone holding a webhook URL can post as the channel.
    pub fn secret_key(&self) -> String {
        secret_key(&self.id)
    }

    fn resolves(&self) -> bool {
        self.send_resolved || self.kind.must_resolve()
    }
}

pub fn secret_key(channel_id: &str) -> String {
    format!("notify:{channel_id}")
}

/// Where the channels live in `AppConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSettings {
    #[serde(default)]
    pub channels: Vec<NotifyChannel>,
}

// ---------------------------------------------------------------- ownership

/// What a connection's ownership metadata says, as the rules need it.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub environment: String,
    pub team: Option<String>,
    pub owner: Option<String>,
    pub runbook_url: Option<String>,
}

/// The same reading of `metadata.operations` the frontend makes in
/// `ownershipFor`, so a rule and the Services screen never disagree about
/// which team a connection belongs to.
pub fn meta_of(config: &AppConfig) -> HashMap<String, Meta> {
    config
        .profiles
        .iter()
        .map(|profile| {
            let operations = profile.metadata.get("operations");
            let field = |key: &str| {
                operations
                    .and_then(|o| o.get(key))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            };
            let environment = field("environment")
                .filter(|e| {
                    matches!(
                        e.as_str(),
                        "production" | "staging" | "development" | "shared"
                    )
                })
                .unwrap_or_else(|| "shared".into());
            (
                profile.id.clone(),
                Meta {
                    environment,
                    team: field("team"),
                    owner: field("owner"),
                    runbook_url: field("runbookUrl"),
                },
            )
        })
        .collect()
}

fn matches(channel: &NotifyChannel, alert: &Alert, meta: &HashMap<String, Meta>) -> bool {
    if alert.severity < channel.min_severity {
        return false;
    }
    let fallback = Meta {
        environment: "shared".into(),
        ..Meta::default()
    };
    let meta = meta.get(&alert.profile_id).unwrap_or(&fallback);
    if !channel.environments.is_empty() && !channel.environments.contains(&meta.environment) {
        return false;
    }
    if !channel.teams.is_empty() {
        let Some(team) = &meta.team else { return false };
        if !channel.teams.iter().any(|t| t.eq_ignore_ascii_case(team)) {
            return false;
        }
    }
    true
}

// ------------------------------------------------------------------- ledger

/// What one channel has been told about one condition.
#[derive(Debug, Clone)]
struct Told {
    /// The occurrence it was told about.
    first_seen: DateTime<Utc>,
    /// When it was told.
    at: DateTime<Utc>,
    /// The alert as it was told — see the module notes.
    alert: Alert,
    acknowledged_told: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEvent {
    Firing,
    Resolved,
    Acknowledged,
    Test,
}

/// One attempt to tell a channel something, kept for the log in Settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub kind: ChannelKind,
    pub event: DeliveryEvent,
    /// How many alerts it carried.
    pub alerts: usize,
    /// The first alert's title, or what a test said.
    pub summary: String,
    pub ok: bool,
    /// The last HTTP status seen, when the request got that far.
    pub status: Option<u16>,
    pub error: Option<String>,
    pub attempts: u32,
    pub latency_ms: u64,
    pub at: DateTime<Utc>,
}

/// Everything the loop remembers between ticks.
#[derive(Default)]
pub struct Notifier {
    /// Keyed on (channel id, fingerprint).
    told: HashMap<(String, String), Told>,
    /// Channels that failed, and when each may be tried again. A Slack outage
    /// must not become a request every ten seconds.
    backoff: HashMap<String, (DateTime<Utc>, u32)>,
    log: VecDeque<Delivery>,
}

impl Notifier {
    pub fn deliveries(&self, take: usize) -> Vec<Delivery> {
        self.log.iter().rev().take(take).cloned().collect()
    }

    fn record(&mut self, delivery: Delivery) {
        if self.log.len() == LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(delivery);
    }

    fn forget_channel(&mut self, channel_id: &str) {
        self.told.retain(|(channel, _), _| channel != channel_id);
        self.backoff.remove(channel_id);
    }

    fn failed(&mut self, channel_id: &str, now: DateTime<Utc>) {
        let failures = self.backoff.get(channel_id).map_or(0, |(_, n)| *n) + 1;
        // 30s, 1m, 2m, 4m … capped at ten minutes.
        let wait = (30i64 << (failures - 1).min(5)).min(600);
        self.backoff.insert(
            channel_id.to_string(),
            (now + ChronoDuration::seconds(wait), failures),
        );
    }

    fn waiting(&self, channel_id: &str, now: DateTime<Utc>) -> bool {
        self.backoff
            .get(channel_id)
            .is_some_and(|(until, _)| *until > now)
    }
}

/// What one tick owes one channel.
#[derive(Debug, Default)]
pub struct Plan {
    pub firing: Vec<Alert>,
    /// Told alerts that have cleared, with when they first fired.
    pub resolved: Vec<Alert>,
    /// Told alerts somebody has acknowledged since.
    pub acknowledged: Vec<Alert>,
    /// Ledger entries to drop without a message: cleared on a channel that
    /// does not want resolutions.
    pub dropped: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.firing.is_empty()
            && self.resolved.is_empty()
            && self.acknowledged.is_empty()
            && self.dropped.is_empty()
    }
}

/// The difference between what is true and what this channel has been told.
fn plan(
    channel: &NotifyChannel,
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
    told: &HashMap<(String, String), Told>,
    now: DateTime<Utc>,
) -> Plan {
    let mut plan = Plan::default();
    let wait = ChronoDuration::seconds(GROUP_WAIT_SECS)
        .max(ChronoDuration::minutes(i64::from(channel.delay_minutes)));
    let by_fingerprint: HashMap<&str, &Alert> =
        alerts.iter().map(|a| (a.fingerprint.as_str(), a)).collect();

    for alert in alerts.iter().filter(|a| a.active) {
        let key = (channel.id.clone(), alert.fingerprint.clone());
        let already = told
            .get(&key)
            .is_some_and(|t| t.first_seen == alert.first_seen);
        if already || !matches(channel, alert, meta) || now - alert.first_seen < wait {
            continue;
        }
        // An escalation is for what nobody has picked up.
        if channel.delay_minutes > 0 && alert.acknowledged {
            continue;
        }
        plan.firing.push(alert.clone());
    }

    for ((channel_id, fingerprint), entry) in told {
        if channel_id != &channel.id {
            continue;
        }
        match by_fingerprint.get(fingerprint.as_str()) {
            // Still the occurrence it was told about, and still firing.
            Some(alert) if alert.active && alert.first_seen == entry.first_seen => {
                if alert.acknowledged && !entry.acknowledged_told {
                    plan.acknowledged.push((*alert).clone());
                }
            }
            // Fired again since: the new occurrence is in `firing` and replaces
            // this entry. The one it replaces did clear in between.
            Some(alert) if alert.active => {}
            // Cleared, or gone from the store entirely.
            found => {
                if channel.resolves() {
                    let mut cleared = found.map_or_else(|| entry.alert.clone(), |a| (*a).clone());
                    cleared.active = false;
                    if found.is_none() {
                        cleared.last_seen = now;
                    }
                    plan.resolved.push(cleared);
                } else {
                    plan.dropped.push(fingerprint.clone());
                }
            }
        }
    }

    // Worst first, then oldest: the order an on-call reads a list in.
    plan.firing.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.first_seen.cmp(&b.first_seen))
    });
    plan
}

// ------------------------------------------------------------------- render

fn severity_word(severity: AlertSeverity) -> &'static str {
    match severity {
        AlertSeverity::Critical => "critical",
        AlertSeverity::Warning => "warning",
        AlertSeverity::Info => "info",
    }
}

/// The app's own status colours, so a message looks like the screen it came from.
fn severity_hex(event: DeliveryEvent, severity: AlertSeverity) -> &'static str {
    match (event, severity) {
        (DeliveryEvent::Resolved, _) => "#5fc48c",
        (DeliveryEvent::Test, _) => "#86a8cc",
        (_, AlertSeverity::Critical) => "#e0705a",
        (_, AlertSeverity::Warning) => "#d9a84e",
        (_, AlertSeverity::Info) => "#80839a",
    }
}

fn span(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let mins = (to - from).num_minutes().max(0);
    match mins {
        0 => "under a minute".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 48 * 60 => match m % 60 {
            0 => format!("{}h", m / 60),
            r => format!("{}h {r}m", m / 60),
        },
        m => format!("{}d", m / (24 * 60)),
    }
}

/// "orders-api / worker"
fn subject(alert: &Alert) -> String {
    match &alert.component {
        Some(component) if !component.is_empty() => format!("{} / {component}", alert.profile_name),
        _ => alert.profile_name.clone(),
    }
}

/// "critical · team payments · production · firing 14m"
fn context_line(
    alert: &Alert,
    meta: Option<&Meta>,
    event: DeliveryEvent,
    now: DateTime<Utc>,
) -> String {
    let mut parts = vec![severity_word(alert.severity).to_string()];
    if let Some(meta) = meta {
        if let Some(team) = &meta.team {
            parts.push(format!("team {team}"));
        }
        parts.push(meta.environment.clone());
    }
    parts.push(match event {
        DeliveryEvent::Resolved => {
            format!("cleared after {}", span(alert.first_seen, alert.last_seen))
        }
        _ => format!("since {} UTC", alert.first_seen.format("%H:%M")),
    });
    if event == DeliveryEvent::Firing {
        parts.push(format!("for {}", span(alert.first_seen, now)));
    }
    parts.join(" · ")
}

fn headline(event: DeliveryEvent, alerts: &[Alert]) -> String {
    let n = alerts.len();
    match (event, alerts.first()) {
        (DeliveryEvent::Test, _) => "root test notification".into(),
        (DeliveryEvent::Firing, Some(a)) if n == 1 => format!("{} — {}", subject(a), a.title),
        (DeliveryEvent::Resolved, Some(a)) if n == 1 => {
            format!("Resolved: {} — {}", subject(a), a.title)
        }
        (DeliveryEvent::Acknowledged, Some(a)) if n == 1 => {
            format!("Acknowledged: {} — {}", subject(a), a.title)
        }
        (DeliveryEvent::Firing, _) => format!("{n} alerts firing"),
        (DeliveryEvent::Resolved, _) => format!("{n} alerts resolved"),
        (DeliveryEvent::Acknowledged, _) => format!("{n} alerts acknowledged"),
    }
}

const TEST_TEXT: &str =
    "If you can read this, alerts that match this channel's rules will arrive here.";

/// The request bodies one delivery makes. One for every chat channel; one per
/// alert for PagerDuty, whose unit is the incident rather than the message.
fn render(
    channel: &NotifyChannel,
    target: &str,
    event: DeliveryEvent,
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
    now: DateTime<Utc>,
) -> Vec<(String, Value)> {
    let shown = &alerts[..alerts.len().min(MESSAGE_CAP)];
    let more = alerts.len().saturating_sub(MESSAGE_CAP);
    let title = headline(event, alerts);

    match channel.kind {
        // Shown through the notification centre by `deliver_desktop`.
        ChannelKind::Desktop => Vec::new(),
        ChannelKind::Slack => {
            let emoji = |a: &Alert| match (event, a.severity) {
                (DeliveryEvent::Resolved, _) => ":large_green_circle:",
                (DeliveryEvent::Acknowledged, _) => ":eyes:",
                (_, AlertSeverity::Critical) => ":red_circle:",
                (_, AlertSeverity::Warning) => ":large_orange_circle:",
                (_, AlertSeverity::Info) => ":large_blue_circle:",
            };
            let mut attachments: Vec<Value> = shown
                .iter()
                .map(|a| {
                    let m = meta.get(&a.profile_id);
                    let mut context = context_line(a, m, event, now);
                    if let Some(url) = m.and_then(|m| m.runbook_url.as_ref()) {
                        context.push_str(&format!(" · <{url}|runbook>"));
                    }
                    json!({
                        "color": severity_hex(event, a.severity),
                        "blocks": [
                            {
                                "type": "section",
                                "text": {
                                    "type": "mrkdwn",
                                    "text": format!(
                                        "{} *{}* — {}\n{}",
                                        emoji(a),
                                        subject(a),
                                        a.title,
                                        if a.detail.is_empty() { String::new() } else { format!("_{}_", a.detail) },
                                    ),
                                },
                            },
                            { "type": "context", "elements": [{ "type": "mrkdwn", "text": context }] },
                        ],
                    })
                })
                .collect();
            if event == DeliveryEvent::Test {
                attachments.push(json!({
                    "color": severity_hex(event, AlertSeverity::Info),
                    "blocks": [{ "type": "section", "text": { "type": "mrkdwn", "text": format!(":white_check_mark: *root test notification*\n{TEST_TEXT}") } }],
                }));
            }
            if more > 0 {
                attachments.push(json!({ "blocks": [{ "type": "context", "elements": [{ "type": "mrkdwn", "text": format!("and {more} more") }] }] }));
            }
            vec![(
                target.to_string(),
                json!({ "text": title, "attachments": attachments }),
            )]
        }

        ChannelKind::Teams => {
            // An Adaptive Card in a message envelope: what a Workflows
            // ("post to a channel when a webhook request is received") URL
            // expects, and what the retiring Office 365 connector URLs accept.
            let color = |a: &Alert| match (event, a.severity) {
                (DeliveryEvent::Resolved, _) => "Good",
                (_, AlertSeverity::Critical) => "Attention",
                (_, AlertSeverity::Warning) => "Warning",
                _ => "Accent",
            };
            let mut body = vec![json!({
                "type": "TextBlock",
                "text": title,
                "weight": "Bolder",
                "size": "Medium",
                "wrap": true,
            })];
            if event == DeliveryEvent::Test {
                body.push(json!({ "type": "TextBlock", "text": TEST_TEXT, "wrap": true, "isSubtle": true }));
            }
            for a in shown {
                let m = meta.get(&a.profile_id);
                body.push(json!({
                    "type": "Container",
                    "separator": true,
                    "items": [
                        { "type": "TextBlock", "text": format!("**{}** — {}", subject(a), a.title), "wrap": true, "color": color(a) },
                        { "type": "TextBlock", "text": a.detail, "wrap": true, "isSubtle": true, "spacing": "Small" },
                        { "type": "TextBlock", "text": context_line(a, m, event, now), "wrap": true, "isSubtle": true, "size": "Small", "spacing": "Small" },
                    ],
                }));
            }
            if more > 0 {
                body.push(json!({ "type": "TextBlock", "text": format!("and {more} more"), "isSubtle": true }));
            }
            let actions: Vec<Value> = shown
                .first()
                .filter(|_| shown.len() == 1)
                .and_then(|a| meta.get(&a.profile_id))
                .and_then(|m| m.runbook_url.as_ref())
                .map(|url| {
                    vec![json!({ "type": "Action.OpenUrl", "title": "Runbook", "url": url })]
                })
                .unwrap_or_default();
            vec![(
                target.to_string(),
                json!({
                    "type": "message",
                    "attachments": [{
                        "contentType": "application/vnd.microsoft.card.adaptive",
                        "contentUrl": null,
                        "content": {
                            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                            "type": "AdaptiveCard",
                            "version": "1.4",
                            "body": body,
                            "actions": actions,
                        },
                    }],
                }),
            )]
        }

        ChannelKind::Discord => {
            let hex = |s: &str| i64::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0);
            let mut embeds: Vec<Value> = shown
                .iter()
                .map(|a| {
                    let m = meta.get(&a.profile_id);
                    let mut embed = json!({
                        "title": format!("{} — {}", subject(a), a.title),
                        "description": a.detail,
                        "color": hex(severity_hex(event, a.severity)),
                        "footer": { "text": context_line(a, m, event, now) },
                    });
                    // Absent rather than null: Discord validates what it is given.
                    if let Some(url) = m.and_then(|m| m.runbook_url.clone()) {
                        embed["url"] = json!(url);
                    }
                    embed
                })
                .collect();
            if event == DeliveryEvent::Test {
                embeds.push(json!({ "title": "root test notification", "description": TEST_TEXT, "color": hex(severity_hex(event, AlertSeverity::Info)) }));
            }
            let content = if more > 0 {
                format!("{title} (and {more} more)")
            } else {
                title
            };
            vec![(
                target.to_string(),
                json!({ "username": "root", "content": content, "embeds": embeds }),
            )]
        }

        ChannelKind::Webhook => {
            let items: Vec<Value> = alerts
                .iter()
                .map(|a| {
                    let m = meta.get(&a.profile_id);
                    json!({
                        "id": a.id,
                        "fingerprint": a.fingerprint,
                        "connectionId": a.profile_id,
                        "connection": a.profile_name,
                        "kind": a.kind,
                        "severity": a.severity,
                        "title": a.title,
                        "detail": a.detail,
                        "component": a.component,
                        "firstSeen": a.first_seen,
                        "lastSeen": a.last_seen,
                        "active": a.active,
                        "acknowledged": a.acknowledged,
                        "team": m.and_then(|m| m.team.clone()),
                        "owner": m.and_then(|m| m.owner.clone()),
                        "environment": m.map(|m| m.environment.clone()),
                        "runbookUrl": m.and_then(|m| m.runbook_url.clone()),
                    })
                })
                .collect();
            vec![(
                target.to_string(),
                json!({
                    "version": 1,
                    "source": "root",
                    "event": event,
                    "channel": channel.name,
                    "summary": title,
                    "sentAt": now,
                    "alerts": items,
                }),
            )]
        }

        ChannelKind::PagerDuty => {
            let action = match event {
                DeliveryEvent::Firing | DeliveryEvent::Test => "trigger",
                DeliveryEvent::Resolved => "resolve",
                DeliveryEvent::Acknowledged => "acknowledge",
            };
            alerts
                .iter()
                .map(|a| {
                    let m = meta.get(&a.profile_id);
                    let pd_severity = match a.severity {
                        AlertSeverity::Critical => "critical",
                        AlertSeverity::Warning => "warning",
                        AlertSeverity::Info => "info",
                    };
                    let mut summary = format!("{} — {}", subject(a), a.title);
                    summary.truncate(1000);
                    let mut body = json!({
                        "routing_key": target.trim(),
                        "event_action": action,
                        // The fingerprint, so the same condition is one
                        // incident however many times root is restarted.
                        "dedup_key": format!("root:{}", a.fingerprint),
                        "client": "root",
                    });
                    if action == "trigger" {
                        let mut payload = json!({
                            "summary": summary,
                            "source": a.profile_name,
                            "severity": pd_severity,
                            "class": a.kind,
                            "timestamp": a.first_seen,
                            "custom_details": {
                                "detail": a.detail,
                                "environment": m.map(|m| m.environment.clone()),
                                "owner": m.and_then(|m| m.owner.clone()),
                                "runbook": m.and_then(|m| m.runbook_url.clone()),
                            },
                        });
                        // The Events API wants these strings or absent, not null.
                        if let Some(component) = &a.component {
                            payload["component"] = json!(component);
                        }
                        if let Some(team) = m.and_then(|m| m.team.clone()) {
                            payload["group"] = json!(team);
                        }
                        body["payload"] = payload;
                        if let Some(url) = m.and_then(|m| m.runbook_url.clone()) {
                            body["links"] = json!([{ "href": url, "text": "Runbook" }]);
                        }
                    }
                    (PAGERDUTY_EVENTS.to_string(), body)
                })
                .collect()
        }
    }
}

// ----------------------------------------------------------------- delivery

/// POST one body, retrying what is worth retrying.
///
/// A timeout, a refused connection, a 429 or a 5xx is the far side having a
/// bad moment, and gets two more tries a second and three seconds apart. A
/// 4xx is root sending something wrong — a revoked webhook, a bad key — and
/// trying again cannot fix it.
async fn post(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> (std::result::Result<u16, String>, Option<u16>, u32) {
    let mut last_status = None;
    let mut last_error = String::new();
    for attempt in 1..=3u32 {
        match client.post(url).json(body).send().await {
            Ok(response) => {
                let status = response.status();
                last_status = Some(status.as_u16());
                if status.is_success() {
                    return (Ok(status.as_u16()), last_status, attempt);
                }
                let text = response.text().await.unwrap_or_default();
                let text: String = text.chars().take(200).collect();
                last_error = format!(
                    "{status}{}",
                    if text.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", text.trim())
                    }
                );
                if !(status.is_server_error() || status.as_u16() == 429) {
                    return (Err(last_error), last_status, attempt);
                }
            }
            Err(e) => {
                last_error = if e.is_timeout() {
                    "timed out".into()
                } else {
                    e.to_string()
                };
            }
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(if attempt == 1 { 1 } else { 3 })).await;
        }
    }
    (Err(last_error), last_status, 3)
}

/// Send one event to one channel. Returns the log record and, per alert, whether it got there.
async fn deliver(
    client: &reqwest::Client,
    desktop: Option<&tauri::AppHandle>,
    channel: &NotifyChannel,
    target: &str,
    event: DeliveryEvent,
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
) -> (Delivery, Vec<bool>) {
    if channel.kind == ChannelKind::Desktop {
        return deliver_desktop(desktop, channel, event, alerts, meta);
    }
    let now = Utc::now();
    let started = Instant::now();
    let requests = render(channel, target, event, alerts, meta, now);
    let per_alert = channel.kind == ChannelKind::PagerDuty;

    let mut outcomes = Vec::with_capacity(requests.len());
    let mut error = None;
    let mut status = None;
    let mut attempts = 0;
    for (url, body) in &requests {
        let (result, seen, tries) = post(client, url, body).await;
        attempts = attempts.max(tries);
        status = seen.or(status);
        match result {
            Ok(_) => outcomes.push(true),
            Err(e) => {
                error.get_or_insert(e);
                outcomes.push(false);
            }
        }
    }
    // A chat message carries every alert or none of them.
    let delivered = if per_alert {
        outcomes
    } else {
        vec![outcomes.first().copied().unwrap_or(false); alerts.len().max(1)]
    };

    let delivery = Delivery {
        id: uuid::Uuid::new_v4().to_string(),
        channel_id: channel.id.clone(),
        channel_name: channel.name.clone(),
        kind: channel.kind,
        event,
        alerts: if event == DeliveryEvent::Test {
            0
        } else {
            alerts.len()
        },
        summary: headline(event, alerts),
        ok: error.is_none(),
        status,
        error,
        attempts,
        latency_ms: started.elapsed().as_millis() as u64,
        at: now,
    };
    (delivery, delivered)
}

/// What a desktop banner says: a title and at most a few lines.
///
/// The notification centre shows a line of title and two or three of body,
/// and truncates the rest without a word — so this decides what survives
/// rather than leaving it to the cut.
fn banner(
    event: DeliveryEvent,
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
    now: DateTime<Utc>,
) -> (String, String) {
    let title: String = headline(event, alerts).chars().take(120).collect();
    let body = match (event, alerts) {
        (DeliveryEvent::Test, _) => TEST_TEXT.to_string(),
        (_, [one]) => {
            let detail: String = one.detail.chars().take(160).collect();
            let context = context_line(one, meta.get(&one.profile_id), event, now);
            if detail.is_empty() {
                context
            } else {
                format!("{detail}\n{context}")
            }
        }
        (_, many) => {
            let mut lines: Vec<String> = many
                .iter()
                .take(4)
                .map(|a| format!("{} — {}", subject(a), a.title))
                .collect();
            if many.len() > 4 {
                lines.push(format!("and {} more", many.len() - 4));
            }
            lines.join("\n")
        }
    };
    (title, body)
}

/// A banner, through the system's notification centre. Only the desktop app
/// has one; anywhere else this says so in the delivery log instead.
fn deliver_desktop(
    app: Option<&tauri::AppHandle>,
    channel: &NotifyChannel,
    event: DeliveryEvent,
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
) -> (Delivery, Vec<bool>) {
    let now = Utc::now();
    let started = Instant::now();
    let (title, body) = banner(event, alerts, meta, now);
    let result = match app {
        Some(app) => {
            use tauri_plugin_notification::NotificationExt;
            app.notification()
                .builder()
                .title(&title)
                .body(&body)
                .show()
                .map_err(|e| e.to_string())
        }
        None => Err("desktop banners only show in the root desktop app".to_string()),
    };
    let ok = result.is_ok();
    let delivery = Delivery {
        id: uuid::Uuid::new_v4().to_string(),
        channel_id: channel.id.clone(),
        channel_name: channel.name.clone(),
        kind: channel.kind,
        event,
        alerts: if event == DeliveryEvent::Test {
            0
        } else {
            alerts.len()
        },
        summary: headline(event, alerts),
        ok,
        status: None,
        error: result.err(),
        attempts: 1,
        latency_ms: started.elapsed().as_millis() as u64,
        at: now,
    };
    (delivery, vec![ok; alerts.len().max(1)])
}

/// The desktop app's handle, when this process is the desktop app.
async fn desktop(state: &Arc<AppState>) -> Option<tauri::AppHandle> {
    match &*state.events.read().await {
        crate::events::Events::Tauri(handle) => Some(handle.clone()),
        _ => None,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("root/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Start the loop. Called once at startup by the desktop app and the server alike.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        let client = client();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            sweep(&state, &client).await;
        }
    });
}

/// One comparison of the alert store with the ledger, and the sends it implies.
pub async fn sweep(state: &Arc<AppState>, client: &reqwest::Client) {
    let (plans, meta) = plans(state, Utc::now()).await;
    if plans.is_empty() {
        return;
    }
    let mut targets = HashMap::new();
    for (channel, _) in &plans {
        if !channel.kind.needs_target() {
            continue;
        }
        if let Ok(Some(target)) = crate::secrets::get_secret(&channel.secret_key()).await {
            targets.insert(channel.id.clone(), target);
        }
    }
    send(state, client, plans, &targets, &meta).await;
}

/// Every alert except the live demo's.
///
/// An invented outage must never reach a real channel. The demo plays one
/// every twelve minutes; routed like the rest, each would page whoever is on
/// call for a checkout service that does not exist.
async fn real_alerts(state: &Arc<AppState>) -> Vec<Alert> {
    let simulated: std::collections::HashSet<String> = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .filter(|p| p.simulated)
        .map(|p| p.id.clone())
        .collect();
    let mut alerts = state.alerts.read().await.all();
    alerts.retain(|a| !simulated.contains(&a.profile_id));
    alerts
}

/// What every enabled channel is owed right now, with the bookkeeping that
/// needs no message already done.
async fn plans(
    state: &Arc<AppState>,
    now: DateTime<Utc>,
) -> (Vec<(NotifyChannel, Plan)>, HashMap<String, Meta>) {
    let (channels, meta) = {
        let config = state.config.read().await;
        (
            config
                .notifications
                .channels
                .iter()
                .filter(|c| c.enabled)
                .cloned()
                .collect::<Vec<_>>(),
            meta_of(&config),
        )
    };
    let alerts = real_alerts(state).await;
    let mut notifier = state.notifier.lock().await;

    // A channel deleted or switched off takes its ledger with it.
    notifier
        .told
        .retain(|(channel, _), _| channels.iter().any(|c| &c.id == channel));

    let mut owed = Vec::new();
    for channel in channels {
        if notifier.waiting(&channel.id, now) {
            continue;
        }
        let plan = plan(&channel, &alerts, &meta, &notifier.told, now);
        for fingerprint in &plan.dropped {
            notifier
                .told
                .remove(&(channel.id.clone(), fingerprint.clone()));
        }
        // Chat channels are not told about acknowledgements — a thread of
        // "acked" messages is noise — but the ledger notes them so the
        // question is not asked again every ten seconds.
        if channel.kind != ChannelKind::PagerDuty {
            for alert in &plan.acknowledged {
                if let Some(entry) = notifier
                    .told
                    .get_mut(&(channel.id.clone(), alert.fingerprint.clone()))
                {
                    entry.acknowledged_told = true;
                }
            }
        }
        let sends = !plan.firing.is_empty()
            || !plan.resolved.is_empty()
            || (channel.kind == ChannelKind::PagerDuty && !plan.acknowledged.is_empty());
        if sends {
            owed.push((channel, plan));
        }
    }
    (owed, meta)
}

/// Make the sends, and write down what got there.
async fn send(
    state: &Arc<AppState>,
    client: &reqwest::Client,
    plans: Vec<(NotifyChannel, Plan)>,
    targets: &HashMap<String, String>,
    meta: &HashMap<String, Meta>,
) {
    let handle = desktop(state).await;
    for (channel, plan) in plans {
        let now = Utc::now();
        let none = String::new();
        let found = if channel.kind.needs_target() {
            targets.get(&channel.id)
        } else {
            Some(&none)
        };
        let Some(target) = found else {
            let mut notifier = state.notifier.lock().await;
            notifier.failed(&channel.id, now);
            notifier.record(Delivery {
                id: uuid::Uuid::new_v4().to_string(),
                channel_id: channel.id.clone(),
                channel_name: channel.name.clone(),
                kind: channel.kind,
                event: DeliveryEvent::Firing,
                alerts: plan.firing.len(),
                summary: headline(DeliveryEvent::Firing, &plan.firing),
                ok: false,
                status: None,
                error: Some(
                    "no webhook URL or key is stored for this channel — edit it and add one".into(),
                ),
                attempts: 0,
                latency_ms: 0,
                at: now,
            });
            continue;
        };

        let acknowledged = if channel.kind == ChannelKind::PagerDuty {
            plan.acknowledged
        } else {
            Vec::new()
        };
        let batches = [
            (DeliveryEvent::Firing, plan.firing),
            (DeliveryEvent::Resolved, plan.resolved),
            (DeliveryEvent::Acknowledged, acknowledged),
        ];
        for (event, alerts) in batches {
            if alerts.is_empty() {
                continue;
            }
            let (delivery, delivered) = deliver(
                client,
                handle.as_ref(),
                &channel,
                target,
                event,
                &alerts,
                meta,
            )
            .await;
            let mut notifier = state.notifier.lock().await;
            for (alert, ok) in alerts.iter().zip(delivered) {
                if !ok {
                    continue;
                }
                let key = (channel.id.clone(), alert.fingerprint.clone());
                match event {
                    DeliveryEvent::Firing => {
                        notifier.told.insert(
                            key,
                            Told {
                                first_seen: alert.first_seen,
                                at: now,
                                alert: alert.clone(),
                                acknowledged_told: alert.acknowledged,
                            },
                        );
                    }
                    DeliveryEvent::Resolved => {
                        notifier.told.remove(&key);
                    }
                    DeliveryEvent::Acknowledged => {
                        if let Some(entry) = notifier.told.get_mut(&key) {
                            entry.acknowledged_told = true;
                        }
                    }
                    DeliveryEvent::Test => {}
                }
            }
            if delivery.ok {
                notifier.backoff.remove(&channel.id);
            } else {
                notifier.failed(&channel.id, now);
            }
            notifier.record(delivery);
        }
    }
}

// ------------------------------------------------------------------ notices

/// Where one alert stands with one channel, for the Alerts screen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub channel_id: String,
    pub channel_name: String,
    pub kind: ChannelKind,
    pub status: NoticeStatus,
    /// When it was told, when it will be, or when the next attempt is.
    pub at: DateTime<Utc>,
    pub delay_minutes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeStatus {
    /// Told about this occurrence.
    Sent,
    /// Will be told at `at` — for a delayed channel, unless somebody
    /// acknowledges it first. That clause is the reason this exists: the
    /// person reading the Alerts screen is the one who can stop the page.
    Due,
    /// The last attempt failed; the next is at `at`.
    Retrying,
    /// A delayed channel that will not hear of it, because it was
    /// acknowledged in time.
    Held,
}

/// Every active alert's standing with every channel whose rules it matches,
/// keyed by alert id. Computed from the same ledger and rules the loop uses,
/// so the screen cannot promise a page the loop will not send.
pub async fn notices(state: &Arc<AppState>) -> HashMap<String, Vec<Notice>> {
    let (channels, meta) = {
        let config = state.config.read().await;
        (
            config
                .notifications
                .channels
                .iter()
                .filter(|c| c.enabled)
                .cloned()
                .collect::<Vec<_>>(),
            meta_of(&config),
        )
    };
    if channels.is_empty() {
        return HashMap::new();
    }
    let alerts = real_alerts(state).await;
    let notifier = state.notifier.lock().await;
    notices_from(&channels, &alerts, &meta, &notifier, Utc::now())
}

fn notices_from(
    channels: &[NotifyChannel],
    alerts: &[Alert],
    meta: &HashMap<String, Meta>,
    notifier: &Notifier,
    now: DateTime<Utc>,
) -> HashMap<String, Vec<Notice>> {
    let mut out: HashMap<String, Vec<Notice>> = HashMap::new();
    for alert in alerts.iter().filter(|a| a.active) {
        for channel in channels.iter().filter(|c| matches(c, alert, meta)) {
            let wait = ChronoDuration::seconds(GROUP_WAIT_SECS)
                .max(ChronoDuration::minutes(i64::from(channel.delay_minutes)));
            let due = alert.first_seen + wait;
            let told = notifier
                .told
                .get(&(channel.id.clone(), alert.fingerprint.clone()))
                .filter(|t| t.first_seen == alert.first_seen);
            let (status, at) = match (told, notifier.backoff.get(&channel.id)) {
                (Some(told), _) => (NoticeStatus::Sent, told.at),
                (None, _) if channel.delay_minutes > 0 && alert.acknowledged => {
                    (NoticeStatus::Held, due)
                }
                (None, Some((until, _))) if *until > now && due <= now => {
                    (NoticeStatus::Retrying, *until)
                }
                (None, _) => (NoticeStatus::Due, due),
            };
            out.entry(alert.id.clone()).or_default().push(Notice {
                channel_id: channel.id.clone(),
                channel_name: channel.name.clone(),
                kind: channel.kind,
                status,
                at,
                delay_minutes: channel.delay_minutes,
            });
        }
    }
    // Soonest first within an alert: what was told, then what is coming.
    for list in out.values_mut() {
        list.sort_by_key(|n| (n.delay_minutes, n.at));
    }
    out
}

// ----------------------------------------------------------------- commands

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsView {
    pub channels: Vec<NotifyChannel>,
    pub deliveries: Vec<Delivery>,
    /// Every target lives in the keychain; without one nothing can be saved.
    pub keychain_available: bool,
    /// Teams named in ownership metadata, for the rule editor to offer.
    pub teams: Vec<String>,
}

pub async fn view(state: &Arc<AppState>) -> NotificationsView {
    let (channels, teams) = {
        let config = state.config.read().await;
        let mut teams: Vec<String> = meta_of(&config)
            .into_values()
            .filter_map(|m| m.team)
            .collect();
        teams.sort_by_key(|t| t.to_lowercase());
        teams.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        (config.notifications.channels.clone(), teams)
    };
    NotificationsView {
        channels,
        deliveries: state.notifier.lock().await.deliveries(50),
        keychain_available: crate::secrets::store_available(),
        teams,
    }
}

/// Create or update a channel. `target` is the webhook URL or integration key;
/// `None` or empty keeps the one already stored.
pub async fn save(
    state: &Arc<AppState>,
    mut channel: NotifyChannel,
    target: Option<String>,
) -> Result<NotificationsView> {
    channel.name = channel.name.trim().to_string();
    if channel.name.is_empty() {
        return Err(AppError::Rejected("give the channel a name".into()));
    }
    if channel.name.chars().count() > 60 {
        return Err(AppError::Rejected(
            "keep the name under 60 characters".into(),
        ));
    }
    if channel.delay_minutes > 24 * 60 {
        return Err(AppError::Rejected(
            "a delay longer than a day is not an escalation".into(),
        ));
    }
    channel.environments.retain(|e| {
        matches!(
            e.as_str(),
            "production" | "staging" | "development" | "shared"
        )
    });
    channel.environments.sort();
    channel.environments.dedup();
    channel.teams = channel
        .teams
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    channel.teams.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

    let creating = channel.id.is_empty()
        || !state
            .config
            .read()
            .await
            .notifications
            .channels
            .iter()
            .any(|c| c.id == channel.id);
    if channel.id.is_empty() {
        channel.id = uuid::Uuid::new_v4().to_string();
    }

    let target = target
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if !channel.kind.needs_target() {
        if desktop(state).await.is_none() {
            return Err(AppError::Rejected(
                "desktop banners only show in the root desktop app — this copy of root is a server"
                    .into(),
            ));
        }
        channel.target_hint = None;
    }
    match &target {
        _ if !channel.kind.needs_target() => {}
        Some(target) => {
            channel.kind.validate(target)?;
            crate::secrets::set_secret(&channel.secret_key(), target).await?;
            channel.target_hint = Some(channel.kind.hint(target));
        }
        None if creating => {
            return Err(AppError::Rejected(match channel.kind {
                ChannelKind::PagerDuty => {
                    "paste the service's Events API v2 integration key".into()
                }
                kind => format!("paste the {} webhook URL", kind.label()),
            }));
        }
        None => {
            // Keep the hint the stored target was saved with.
            channel.target_hint = state
                .config
                .read()
                .await
                .notifications
                .channels
                .iter()
                .find(|c| c.id == channel.id)
                .and_then(|c| c.target_hint.clone());
        }
    }

    {
        let mut config = state.config.write().await;
        let channels = &mut config.notifications.channels;
        match channels.iter_mut().find(|c| c.id == channel.id) {
            Some(existing) => {
                if existing.kind != channel.kind && target.is_none() && channel.kind.needs_target()
                {
                    return Err(AppError::Rejected(
                        "a different kind of channel needs its own URL or key".into(),
                    ));
                }
                *existing = channel;
            }
            None => channels.push(channel),
        }
    }
    state.save_config().await?;
    Ok(view(state).await)
}

pub async fn delete(state: &Arc<AppState>, channel_id: &str) -> Result<NotificationsView> {
    {
        let mut config = state.config.write().await;
        config.notifications.channels.retain(|c| c.id != channel_id);
    }
    state.save_config().await?;
    crate::secrets::delete_secret(&secret_key(channel_id)).await?;
    state.notifier.lock().await.forget_channel(channel_id);
    Ok(view(state).await)
}

/// Send a test now, whatever the rules say, and say what happened.
///
/// For PagerDuty a test is a real incident, so it is a low-urgency one that
/// is resolved again straight away — an operator testing at their desk should
/// not page whoever is on call.
pub async fn test(state: &Arc<AppState>, channel_id: &str) -> Result<Delivery> {
    let channel = state
        .config
        .read()
        .await
        .notifications
        .channels
        .iter()
        .find(|c| c.id == channel_id)
        .cloned()
        .ok_or_else(|| AppError::Rejected("that channel no longer exists".into()))?;
    let target = if channel.kind.needs_target() {
        crate::secrets::get_secret(&channel.secret_key())
            .await?
            .ok_or_else(|| {
                AppError::Rejected("no webhook URL or key is stored for this channel".into())
            })?
    } else {
        String::new()
    };
    let handle = desktop(state).await;

    let now = Utc::now();
    let probe = Alert {
        id: "root-test".into(),
        fingerprint: format!("test:{}", channel.id),
        profile_id: String::new(),
        profile_name: "root".into(),
        kind: crate::model::ServiceKind::HttpEndpoint,
        severity: AlertSeverity::Info,
        title: "test notification".into(),
        detail: TEST_TEXT.into(),
        component: None,
        first_seen: now,
        last_seen: now,
        active: true,
        acknowledged: false,
    };
    let client = client();
    let meta = HashMap::new();
    let alerts: Vec<Alert> = if channel.kind == ChannelKind::PagerDuty {
        vec![probe.clone()]
    } else {
        Vec::new()
    };
    let (delivery, _) = deliver(
        &client,
        handle.as_ref(),
        &channel,
        &target,
        DeliveryEvent::Test,
        &alerts,
        &meta,
    )
    .await;
    if channel.kind == ChannelKind::PagerDuty && delivery.ok {
        let mut cleared = probe;
        cleared.active = false;
        let _ = deliver(
            &client,
            handle.as_ref(),
            &channel,
            &target,
            DeliveryEvent::Resolved,
            &[cleared],
            &meta,
        )
        .await;
    }
    state.notifier.lock().await.record(delivery.clone());
    Ok(delivery)
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionProfile, ProfileOptions};
    use crate::model::ServiceKind;

    fn alert(
        fp: &str,
        profile: &str,
        severity: AlertSeverity,
        age_secs: i64,
        now: DateTime<Utc>,
    ) -> Alert {
        Alert {
            id: format!("id-{fp}"),
            fingerprint: fp.into(),
            profile_id: profile.into(),
            profile_name: format!("{profile}-name"),
            kind: ServiceKind::Docker,
            severity,
            title: format!("{fp} title"),
            detail: format!("{fp} detail"),
            component: Some("worker".into()),
            first_seen: now - ChronoDuration::seconds(age_secs),
            last_seen: now,
            active: true,
            acknowledged: false,
        }
    }

    fn channel(kind: ChannelKind) -> NotifyChannel {
        NotifyChannel {
            id: "c1".into(),
            name: "ops".into(),
            kind,
            enabled: true,
            min_severity: AlertSeverity::Warning,
            environments: vec![],
            teams: vec![],
            delay_minutes: 0,
            send_resolved: true,
            target_hint: None,
        }
    }

    fn meta(entries: &[(&str, &str, Option<&str>)]) -> HashMap<String, Meta> {
        entries
            .iter()
            .map(|(id, env, team)| {
                (
                    id.to_string(),
                    Meta {
                        environment: env.to_string(),
                        team: team.map(String::from),
                        owner: None,
                        runbook_url: Some("https://runbooks.example/orders".into()),
                    },
                )
            })
            .collect()
    }

    fn told(entries: &[(&str, &Alert)]) -> HashMap<(String, String), Told> {
        entries
            .iter()
            .map(|(channel, alert)| {
                (
                    (channel.to_string(), alert.fingerprint.clone()),
                    Told {
                        first_seen: alert.first_seen,
                        at: alert.first_seen + ChronoDuration::seconds(GROUP_WAIT_SECS),
                        alert: (*alert).clone(),
                        acknowledged_told: false,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_banner_says_what_fits() {
        let now = Utc::now();
        let one = alert("down", "p1", AlertSeverity::Critical, 60, now);
        let meta = meta(&[("p1", "production", Some("payments"))]);
        let (title, body) = banner(
            DeliveryEvent::Firing,
            std::slice::from_ref(&one),
            &meta,
            now,
        );
        assert_eq!(title, "p1-name / worker — down title");
        assert!(
            body.starts_with("down detail\ncritical · team payments · production"),
            "{body}"
        );

        let many: Vec<Alert> = (0..6)
            .map(|i| alert(&format!("f{i}"), "p1", AlertSeverity::Warning, 60, now))
            .collect();
        let (title, body) = banner(DeliveryEvent::Firing, &many, &meta, now);
        assert_eq!(title, "6 alerts firing");
        assert_eq!(
            body.lines().count(),
            5,
            "four alerts and how many more: {body}"
        );
        assert!(body.ends_with("and 2 more"));
    }

    #[tokio::test]
    async fn a_desktop_channel_outside_the_app_says_so_in_the_log() {
        let mut desk = channel(ChannelKind::Desktop);
        desk.id = "desk".into();
        let state = estate(desk);
        state.alerts.write().await.reconcile(
            "p1",
            vec![alert("down", "p1", AlertSeverity::Critical, 0, Utc::now())],
        );
        let later = Utc::now() + ChronoDuration::seconds(45);
        let (owed, meta) = plans(&state, later).await;
        assert_eq!(owed.len(), 1, "a desktop channel is planned like any other");
        send(&state, &client(), owed, &HashMap::new(), &meta).await;
        let log = state.notifier.lock().await.deliveries(5);
        assert!(!log[0].ok, "{log:?}");
        assert!(
            log[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("desktop app"),
            "{log:?}"
        );
        assert!(ChannelKind::Desktop.validate("").is_ok(), "no URL to check");
        assert!(!ChannelKind::Desktop.needs_target());
    }

    #[test]
    fn the_screen_is_told_what_the_loop_will_do() {
        let now = Utc::now();
        let mut chat = channel(ChannelKind::Slack);
        chat.id = "chat".into();
        let mut pager = channel(ChannelKind::PagerDuty);
        pager.id = "pager".into();
        pager.delay_minutes = 5;
        let mut quiet = channel(ChannelKind::Teams);
        quiet.id = "quiet".into();
        quiet.min_severity = AlertSeverity::Critical;

        let told_one = alert("told", "p1", AlertSeverity::Warning, 120, now);
        let mut acked = alert("acked", "p1", AlertSeverity::Warning, 120, now);
        acked.acknowledged = true;
        let notifier = Notifier {
            told: told(&[("chat", &told_one)]),
            backoff: HashMap::from([("chat".to_string(), (now + ChronoDuration::minutes(1), 1))]),
            ..Notifier::default()
        };

        let notices = notices_from(
            &[chat, pager, quiet],
            &[told_one.clone(), acked.clone()],
            &HashMap::new(),
            &notifier,
            now,
        );

        let told_notices = &notices[&told_one.id];
        assert_eq!(
            told_notices.len(),
            2,
            "a warning is not for the critical-only channel: {told_notices:?}"
        );
        assert_eq!(told_notices[0].status, NoticeStatus::Sent);
        assert_eq!(told_notices[1].status, NoticeStatus::Due);
        assert_eq!(
            told_notices[1].at,
            told_one.first_seen + ChronoDuration::minutes(5),
            "pages five minutes after it fired"
        );

        let acked_notices = &notices[&acked.id];
        assert_eq!(
            acked_notices[0].status,
            NoticeStatus::Retrying,
            "never told, and the channel is backing off"
        );
        assert_eq!(
            acked_notices[1].status,
            NoticeStatus::Held,
            "acknowledged inside the delay: the page will not go"
        );
    }

    #[test]
    fn nothing_is_said_inside_the_group_wait() {
        let now = Utc::now();
        let fresh = alert("a", "p1", AlertSeverity::Critical, 5, now);
        let settled = alert("b", "p1", AlertSeverity::Critical, 45, now);
        let plan = plan(
            &channel(ChannelKind::Slack),
            &[fresh, settled],
            &HashMap::new(),
            &HashMap::new(),
            now,
        );
        let fired: Vec<_> = plan.firing.iter().map(|a| a.fingerprint.as_str()).collect();
        assert_eq!(
            fired,
            ["b"],
            "a five-second-old alert waits for the ones that fire with it"
        );
    }

    #[test]
    fn rules_filter_by_severity_environment_and_team() {
        let now = Utc::now();
        let alerts = [
            alert("prod-pay-crit", "p1", AlertSeverity::Critical, 60, now),
            alert("prod-pay-info", "p1", AlertSeverity::Info, 60, now),
            alert("stage-pay-crit", "p2", AlertSeverity::Critical, 60, now),
            alert("prod-search-crit", "p3", AlertSeverity::Critical, 60, now),
            alert("prod-nobody-crit", "p4", AlertSeverity::Critical, 60, now),
        ];
        let meta = meta(&[
            ("p1", "production", Some("Payments")),
            ("p2", "staging", Some("payments")),
            ("p3", "production", Some("search")),
            ("p4", "production", None),
        ]);
        let mut rules = channel(ChannelKind::Slack);
        rules.environments = vec!["production".into()];
        rules.teams = vec!["payments".into()];

        let plan = plan(&rules, &alerts, &meta, &HashMap::new(), now);
        let fired: Vec<_> = plan.firing.iter().map(|a| a.fingerprint.as_str()).collect();
        assert_eq!(
            fired,
            ["prod-pay-crit"],
            "team names match case-insensitively; a connection with no team is not on any team"
        );
    }

    #[test]
    fn a_delay_hears_only_what_nobody_acknowledged() {
        let now = Utc::now();
        let mut picked_up = alert("acked", "p1", AlertSeverity::Critical, 600, now);
        picked_up.acknowledged = true;
        let ignored = alert("ignored", "p1", AlertSeverity::Critical, 600, now);
        let recent = alert("recent", "p1", AlertSeverity::Critical, 120, now);
        let mut pager = channel(ChannelKind::PagerDuty);
        pager.delay_minutes = 5;

        let plan = plan(
            &pager,
            &[picked_up, ignored, recent],
            &HashMap::new(),
            &HashMap::new(),
            now,
        );
        let fired: Vec<_> = plan.firing.iter().map(|a| a.fingerprint.as_str()).collect();
        assert_eq!(
            fired,
            ["ignored"],
            "acknowledged in time, or not yet five minutes old: neither pages"
        );
    }

    #[test]
    fn a_told_alert_is_not_told_again_but_its_clearing_is() {
        let now = Utc::now();
        let still = alert("still", "p1", AlertSeverity::Critical, 600, now);
        let mut cleared = alert("cleared", "p1", AlertSeverity::Critical, 600, now);
        let gone = alert("gone", "p1", AlertSeverity::Warning, 600, now);
        let ledger = told(&[("c1", &still), ("c1", &cleared), ("c1", &gone)]);
        cleared.active = false;

        let plan = plan(
            &channel(ChannelKind::Slack),
            &[still, cleared],
            &HashMap::new(),
            &ledger,
            now,
        );
        assert!(plan.firing.is_empty(), "{:?}", plan.firing);
        let mut resolved: Vec<_> = plan
            .resolved
            .iter()
            .map(|a| a.fingerprint.as_str())
            .collect();
        resolved.sort();
        assert_eq!(
            resolved,
            ["cleared", "gone"],
            "an alert the store has forgotten is resolved from the ledger's copy"
        );
        assert!(plan.resolved.iter().all(|a| !a.active));
    }

    #[test]
    fn firing_again_is_a_new_message() {
        let now = Utc::now();
        let earlier = alert("flap", "p1", AlertSeverity::Critical, 3600, now);
        let ledger = told(&[("c1", &earlier)]);
        let again = alert("flap", "p1", AlertSeverity::Critical, 60, now);

        let plan = plan(
            &channel(ChannelKind::Slack),
            &[again],
            &HashMap::new(),
            &ledger,
            now,
        );
        assert_eq!(
            plan.firing.len(),
            1,
            "a new occurrence of a told condition is told"
        );
        assert!(plan.resolved.is_empty());
    }

    #[test]
    fn resolutions_can_be_declined_except_by_pagerduty() {
        let now = Utc::now();
        let mut cleared = alert("x", "p1", AlertSeverity::Critical, 600, now);
        let ledger = told(&[("c1", &cleared)]);
        cleared.active = false;

        let mut quiet = channel(ChannelKind::Slack);
        quiet.send_resolved = false;
        let plan_quiet = plan(&quiet, &[cleared.clone()], &HashMap::new(), &ledger, now);
        assert!(plan_quiet.resolved.is_empty());
        assert_eq!(plan_quiet.dropped, ["x"], "the entry is still let go");

        let mut pager = channel(ChannelKind::PagerDuty);
        pager.send_resolved = false;
        let plan_pager = plan(&pager, &[cleared], &HashMap::new(), &ledger, now);
        assert_eq!(
            plan_pager.resolved.len(),
            1,
            "an open PagerDuty incident must be closed"
        );
    }

    #[test]
    fn worst_first_then_oldest() {
        let now = Utc::now();
        let alerts = [
            alert("warn-old", "p1", AlertSeverity::Warning, 900, now),
            alert("crit-new", "p1", AlertSeverity::Critical, 60, now),
            alert("crit-old", "p1", AlertSeverity::Critical, 600, now),
        ];
        let plan = plan(
            &channel(ChannelKind::Slack),
            &alerts,
            &HashMap::new(),
            &HashMap::new(),
            now,
        );
        let order: Vec<_> = plan.firing.iter().map(|a| a.fingerprint.as_str()).collect();
        assert_eq!(order, ["crit-old", "crit-new", "warn-old"]);
    }

    #[test]
    fn pagerduty_gets_one_event_per_alert_keyed_on_the_fingerprint() {
        let now = Utc::now();
        let alerts = [
            alert("f1", "p1", AlertSeverity::Critical, 60, now),
            alert("f2", "p1", AlertSeverity::Warning, 60, now),
        ];
        let meta = meta(&[("p1", "production", Some("payments"))]);
        let key = "0123456789abcdef0123456789abcdef";
        let bodies = render(
            &channel(ChannelKind::PagerDuty),
            key,
            DeliveryEvent::Firing,
            &alerts,
            &meta,
            now,
        );
        assert_eq!(bodies.len(), 2);
        let (url, first) = &bodies[0];
        assert_eq!(url, PAGERDUTY_EVENTS);
        assert_eq!(first["routing_key"], key);
        assert_eq!(first["event_action"], "trigger");
        assert_eq!(first["dedup_key"], "root:f1");
        assert_eq!(first["payload"]["severity"], "critical");
        assert_eq!(first["payload"]["group"], "payments");
        assert_eq!(first["links"][0]["href"], "https://runbooks.example/orders");

        let resolved = render(
            &channel(ChannelKind::PagerDuty),
            key,
            DeliveryEvent::Resolved,
            &alerts[..1],
            &meta,
            now,
        );
        assert_eq!(resolved[0].1["event_action"], "resolve");
        assert!(
            resolved[0].1.get("payload").is_none(),
            "a resolve carries only the key"
        );
    }

    #[test]
    fn chat_messages_carry_the_batch_and_say_how_many_were_left_out() {
        let now = Utc::now();
        let alerts: Vec<Alert> = (0..12)
            .map(|i| alert(&format!("f{i}"), "p1", AlertSeverity::Critical, 60, now))
            .collect();
        let url = "https://hooks.slack.com/services/T0/B0/xyz";
        let slack = render(
            &channel(ChannelKind::Slack),
            url,
            DeliveryEvent::Firing,
            &alerts,
            &HashMap::new(),
            now,
        );
        assert_eq!(slack.len(), 1);
        assert_eq!(slack[0].1["text"], "12 alerts firing");
        let attachments = slack[0].1["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), MESSAGE_CAP + 1);
        assert!(attachments
            .last()
            .unwrap()
            .to_string()
            .contains("and 2 more"));
        assert_eq!(attachments[0]["color"], "#e0705a");

        let teams = render(
            &channel(ChannelKind::Teams),
            url,
            DeliveryEvent::Firing,
            &alerts[..1],
            &HashMap::new(),
            now,
        );
        let card = &teams[0].1["attachments"][0];
        assert_eq!(
            card["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(card["content"]["type"], "AdaptiveCard");
        assert!(card["content"]["body"][0]["text"]
            .as_str()
            .unwrap()
            .contains("p1-name / worker — f0 title"));

        let discord = render(
            &channel(ChannelKind::Discord),
            url,
            DeliveryEvent::Resolved,
            &alerts[..1],
            &HashMap::new(),
            now,
        );
        assert_eq!(
            discord[0].1["embeds"][0]["color"], 0x5fc48c,
            "a resolution is green whatever its severity was"
        );

        let hook = render(
            &channel(ChannelKind::Webhook),
            url,
            DeliveryEvent::Firing,
            &alerts[..2],
            &HashMap::new(),
            now,
        );
        assert_eq!(hook[0].1["event"], "firing");
        assert_eq!(
            hook[0].1["alerts"].as_array().unwrap().len(),
            2,
            "a webhook gets every alert, not a capped list"
        );
    }

    #[test]
    fn targets_are_checked_before_they_are_stored() {
        assert!(ChannelKind::Slack
            .validate("https://hooks.slack.com/services/T0/B0/xyz")
            .is_ok());
        assert!(ChannelKind::Slack
            .validate("https://discord.com/api/webhooks/1/x")
            .is_err());
        assert!(ChannelKind::Teams
            .validate("https://prod-11.westus.logic.azure.com:443/workflows/abc")
            .is_ok());
        assert!(ChannelKind::Teams
            .validate("http://insecure.example/hook")
            .is_err());
        assert!(ChannelKind::Discord
            .validate("https://discord.com/api/webhooks/1/x")
            .is_ok());
        assert!(
            ChannelKind::Webhook
                .validate("http://10.0.0.5:8080/alerts")
                .is_ok(),
            "an internal receiver may be plain http"
        );
        assert!(ChannelKind::PagerDuty
            .validate("0123456789abcdef0123456789abcdef")
            .is_ok());
        assert!(ChannelKind::PagerDuty.validate("u+abc-short").is_err());
        assert_eq!(
            ChannelKind::Slack.hint("https://hooks.slack.com/services/T0/B0/xyz1"),
            "hooks.slack.com/…xyz1"
        );
        assert_eq!(
            ChannelKind::PagerDuty.hint("0123456789abcdef0123456789abcdef"),
            "••••cdef"
        );
    }

    #[test]
    fn ownership_is_read_the_way_the_services_screen_reads_it() {
        let mut metadata = ProfileOptions::default();
        metadata.set(
            "operations",
            json!({ "team": " Payments ", "environment": "production", "runbookUrl": "https://rb" }),
        );
        let mut odd = ProfileOptions::default();
        odd.set("operations", json!({ "environment": "prod" }));
        let config = AppConfig {
            profiles: vec![profile("p1", metadata), profile("p2", odd)],
            ..AppConfig::default()
        };
        let meta = meta_of(&config);
        assert_eq!(meta["p1"].team.as_deref(), Some("Payments"));
        assert_eq!(meta["p1"].environment, "production");
        assert_eq!(
            meta["p2"].environment, "shared",
            "an unknown environment reads as shared, as it does on screen"
        );
    }

    /// A receiver on a loopback port that hands every body it is sent to the test.
    async fn receiver(status: u16) -> (String, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let app = Router::new()
            .route(
                "/hook",
                post(
                    |State((tx, status)): State<(
                        tokio::sync::mpsc::UnboundedSender<Value>,
                        u16,
                    )>,
                     Json(body): Json<Value>| async move {
                        let _ = tx.send(body);
                        StatusCode::from_u16(status).unwrap()
                    },
                ),
            )
            .with_state((tx, status));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, rx)
    }

    fn estate(channel: NotifyChannel) -> Arc<AppState> {
        let mut metadata = ProfileOptions::default();
        metadata.set(
            "operations",
            json!({ "team": "payments", "environment": "production" }),
        );
        let mut config = AppConfig {
            profiles: vec![profile("p1", metadata)],
            ..AppConfig::default()
        };
        config.notifications.channels.push(channel);
        Arc::new(AppState::new(
            config,
            std::path::PathBuf::from("/tmp/root-notify-test.json"),
        ))
    }

    #[tokio::test]
    async fn an_alert_is_told_once_and_its_clearing_is_told_once() {
        let (url, mut inbox) = receiver(200).await;
        let state = estate(channel(ChannelKind::Webhook));
        let targets = HashMap::from([("c1".to_string(), url)]);
        let client = client();
        let raised = alert("down", "p1", AlertSeverity::Critical, 0, Utc::now());
        state.alerts.write().await.reconcile("p1", vec![raised]);

        // Inside the group wait: nothing owed.
        let (owed, _) = plans(&state, Utc::now()).await;
        assert!(
            owed.is_empty(),
            "a fresh alert waits for the ones that fire with it"
        );

        let later = Utc::now() + ChronoDuration::seconds(45);
        let (owed, meta) = plans(&state, later).await;
        send(&state, &client, owed, &targets, &meta).await;
        let body = inbox.try_recv().expect("the firing message arrived");
        assert_eq!(body["event"], "firing");
        assert_eq!(body["alerts"][0]["fingerprint"], "down");
        assert_eq!(
            body["alerts"][0]["team"], "payments",
            "ownership travels with the alert"
        );

        // Told once: the next tick owes nothing.
        let (owed, _) = plans(&state, later + ChronoDuration::seconds(10)).await;
        assert!(owed.is_empty(), "{owed:?}");

        // It clears; the clearing is told, once.
        state.alerts.write().await.reconcile("p1", vec![]);
        let (owed, meta) = plans(&state, later + ChronoDuration::seconds(20)).await;
        send(&state, &client, owed, &targets, &meta).await;
        let body = inbox.try_recv().expect("the resolution arrived");
        assert_eq!(body["event"], "resolved");
        assert_eq!(body["alerts"][0]["active"], false);
        let (owed, _) = plans(&state, later + ChronoDuration::seconds(30)).await;
        assert!(owed.is_empty(), "and the ledger has let it go");

        let log = state.notifier.lock().await.deliveries(10);
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|d| d.ok && d.status == Some(200)), "{log:?}");
    }

    #[tokio::test]
    async fn a_channel_that_refuses_is_retried_later_not_every_tick() {
        let (url, mut inbox) = receiver(404).await;
        let state = estate(channel(ChannelKind::Webhook));
        let targets = HashMap::from([("c1".to_string(), url)]);
        let client = client();
        state.alerts.write().await.reconcile(
            "p1",
            vec![alert("down", "p1", AlertSeverity::Critical, 0, Utc::now())],
        );

        let later = Utc::now() + ChronoDuration::seconds(45);
        let (owed, meta) = plans(&state, later).await;
        send(&state, &client, owed, &targets, &meta).await;
        assert!(inbox.try_recv().is_ok(), "it was tried");
        assert!(
            inbox.try_recv().is_err(),
            "a 404 is not retried — trying again cannot fix it"
        );

        let log = state.notifier.lock().await.deliveries(10);
        assert!(!log[0].ok && log[0].status == Some(404), "{log:?}");

        // Backing off: the next tick does not try again…
        let (owed, _) = plans(&state, Utc::now() + ChronoDuration::seconds(1)).await;
        assert!(
            owed.is_empty(),
            "a failing channel waits before it is tried again"
        );
        // …but the alert is still owed once the wait is over, because it was never told.
        let (owed, _) = plans(&state, Utc::now() + ChronoDuration::seconds(60)).await;
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].1.firing.len(), 1);
    }

    fn profile(id: &str, metadata: ProfileOptions) -> ConnectionProfile {
        serde_json::from_value::<ConnectionProfile>(json!({
            "id": id,
            "name": id,
            "kind": "docker",
            "endpoint": "",
        }))
        .map(|mut p| {
            p.metadata = metadata;
            p
        })
        .unwrap()
    }
}
