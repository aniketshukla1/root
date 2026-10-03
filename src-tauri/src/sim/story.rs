//! What happens, and when.
//!
//! The demo is a loop of incidents on a fixed grid, each one a story with a
//! cause, a spread and a fix — the shapes real outages take, so every screen
//! that reasons about an incident has something true to reason about:
//!
//! - **A deploy goes wrong.** checkout-api ships an N+1 query. Its containers
//!   fail their health checks, the orders database runs out of connection
//!   slots under the retry storm, the load balancer loses the backend, and a
//!   fix deploy recovers it.
//! - **A consumer falls over.** A new analytics-ingest build is OOM-killed in a
//!   loop on the data cluster; its Kafka consumer group empties and lag climbs
//!   until a second deploy raises the limit.
//! - **Search runs hot.** A reindex drives one Elasticsearch node's heap into
//!   GC, the node drops out, the cluster goes yellow and the catalog — and
//!   with it the storefront — slows down.
//!
//! Between them the estate is calm but not still: benign deploys ship, a
//! smoke test occasionally flakes, and two capacity trends climb slowly enough
//! to be forecast rather than alerted on.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::noise::{envelope, hash, within};

/// One full turn of the loop.
pub const CYCLE_SECS: i64 = 12 * 60;
/// How soon after starting the demo the first live story begins.
pub const LEAD_SECS: i64 = 30;
/// Stories the replayed past contains before the first live one.
pub const PAST_STORIES: i64 = 2;
/// How much history is drawn at startup.
pub const BACKFILL_SECS: i64 = 2 * 60 * 60;
/// Quiet time kept after a story before another may begin.
const CALM_SECS: i64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Story {
    BadDeploy,
    ConsumerOom,
    SearchHeap,
}

impl Story {
    pub const ALL: [Story; 3] = [Story::BadDeploy, Story::ConsumerOom, Story::SearchHeap];

    pub fn id(self) -> &'static str {
        match self {
            Story::BadDeploy => "bad-deploy",
            Story::ConsumerOom => "consumer-oom",
            Story::SearchHeap => "search-heap",
        }
    }

    pub fn from_id(id: &str) -> Option<Story> {
        Story::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Says what kind of trouble, not what caused it — the cause is for the
    /// screens to find.
    pub fn title(self) -> &'static str {
        match self {
            Story::BadDeploy => "A deploy goes wrong",
            Story::ConsumerOom => "A consumer falls over",
            Story::SearchHeap => "Search runs hot",
        }
    }

    /// Seconds from start to fully recovered.
    pub fn duration(self) -> f64 {
        match self {
            Story::BadDeploy => 440.0,
            Story::ConsumerOom => 470.0,
            Story::SearchHeap => 420.0,
        }
    }

    /// What a presenter is told is happening. Neutral on purpose: the banner
    /// must not give away what the incident view is there to work out.
    fn phases(self) -> &'static [(f64, &'static str)] {
        match self {
            Story::BadDeploy => &[
                (0.0, "A deploy is building"),
                (70.0, "The deploy is rolling out"),
                (100.0, "Something is going wrong"),
                (190.0, "Incident in progress"),
                (290.0, "A fix is building"),
                (360.0, "The fix is rolling out"),
                (380.0, "Recovering"),
            ],
            Story::ConsumerOom => &[
                (0.0, "A deploy is building"),
                (60.0, "The deploy is rolling out"),
                (80.0, "Something is going wrong"),
                (150.0, "Incident in progress"),
                (250.0, "A fix is building"),
                (310.0, "Recovering"),
            ],
            Story::SearchHeap => &[
                (0.0, "A batch job is running"),
                (90.0, "Something is going wrong"),
                (170.0, "Incident in progress"),
                (250.0, "Recovering"),
            ],
        }
    }

    pub fn phase(self, tau: f64) -> &'static str {
        self.phases()
            .iter()
            .rev()
            .find(|(at, _)| tau >= *at)
            .map(|(_, label)| *label)
            .unwrap_or("Calm")
    }
}

/// One telling of one story.
#[derive(Debug, Clone, Copy)]
pub struct Instance {
    pub story: Story,
    pub start: DateTime<Utc>,
    /// How many of this story came before it. Versions are numbered from
    /// this, so every telling ships a new build rather than the same one —
    /// the change engine collapses a revision it has already seen.
    pub ordinal: u32,
    pub manual: bool,
}

impl Instance {
    pub fn end(&self) -> DateTime<Utc> {
        self.start + Duration::milliseconds((self.story.duration() * 1000.0) as i64)
    }

    pub fn tau(&self, t: DateTime<Utc>) -> f64 {
        (t - self.start).num_milliseconds() as f64 / 1000.0
    }

    pub fn at(&self, secs: f64) -> DateTime<Utc> {
        self.start + Duration::milliseconds((secs * 1000.0) as i64)
    }
}

/// Everything the stories are doing to the estate at one moment.
///
/// Each field is something one collector can see. A story never reaches into
/// a snapshot directly; it sets these, and each kind reads the ones that would
/// show up in what its real collector reads.
#[derive(Debug, Clone, Default)]
pub struct Effects {
    // -- a deploy goes wrong
    pub n_plus_one: f64,
    pub db_saturation: f64,
    pub checkout_errors: f64,
    pub checkout_unhealthy: [bool; 2],
    pub checkout_crashed: bool,
    pub checkout_503: bool,
    /// The load balancer's view of the two checkout servers.
    pub checkout_lb_down: [bool; 2],
    pub edge_queue: f64,
    // -- a consumer falls over
    pub ingest_crashloop: f64,
    pub ingest_ready: u8,
    pub ingest_members: u8,
    pub ingest_lag: f64,
    // -- search runs hot
    pub es_heap: f64,
    pub es_node_out: bool,
    pub search_slow: f64,
    pub catalog_unhealthy: bool,
    pub catalog_lb_down: bool,
}

impl Effects {
    fn calm() -> Self {
        Effects {
            ingest_ready: 3,
            ingest_members: 3,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Deploy {
    pub at: DateTime<Utc>,
    pub service: &'static str,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildResult {
    Success,
    Failure,
}

#[derive(Debug, Clone)]
pub struct Build {
    pub job: &'static str,
    pub number: i64,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub result: BuildResult,
    /// What the console says it did.
    pub what: String,
}

impl Build {
    pub fn building(&self, t: DateTime<Utc>) -> bool {
        self.start <= t && t < self.end
    }
}

/// The jobs the CI server has, with the first build number each starts from.
pub const JOBS: &[(&str, i64)] = &[
    ("deploy-checkout-api", 812),
    ("deploy-catalog-api", 1290),
    ("deploy-web", 2044),
    ("deploy-analytics-ingest", 211),
    ("reindex-catalog", 96),
    ("e2e-smoke", 5130),
];

/// Versions before anything in the demo shipped.
const BASE_VERSIONS: &[(&str, &str)] = &[
    ("checkout-api", "2.40.3"),
    ("catalog-api", "3.18.0"),
    ("web", "4.12.0"),
    ("analytics-ingest", "1.33.2"),
];

/// The demo's settings, as the simulation reads them.
#[derive(Debug, Clone)]
pub struct Script {
    pub started_at: DateTime<Utc>,
    pub seed: u64,
    pub triggers: Vec<(DateTime<Utc>, Story)>,
}

/// One moment, with everything about it worked out once.
///
/// A snapshot asks for versions, builds and effects many times over, and each
/// of those walks every telling since the demo began. The walk is done here
/// once and the snapshot reads from the result.
#[derive(Debug, Clone)]
pub struct Moment {
    pub t: DateTime<Utc>,
    pub seed: u64,
    /// Where the replayed past begins.
    pub epoch: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub instances: Vec<Instance>,
    pub builds: Vec<Build>,
    pub deploys: Vec<Deploy>,
    pub effects: Effects,
    pub active: Option<Instance>,
}

impl Script {
    pub fn epoch(&self) -> DateTime<Utc> {
        self.started_at - Duration::seconds(BACKFILL_SECS)
    }

    fn cycle_base(&self, k: i64) -> DateTime<Utc> {
        self.started_at + Duration::seconds(LEAD_SECS + k * CYCLE_SECS)
    }

    pub fn moment(&self, t: DateTime<Utc>) -> Moment {
        let instances = self.instances(t);
        let builds = self.builds_from(&instances, t);
        let deploys = deploys_from(&builds, t);
        let effects = effects_at(&instances, t);
        let active = active_at(&instances, t);
        Moment {
            t,
            seed: self.seed,
            epoch: self.epoch(),
            started_at: self.started_at,
            instances,
            builds,
            deploys,
            effects,
            active,
        }
    }

    /// A moment for reading effects only, over tellings already worked out.
    ///
    /// For callers that step through many moments of one window — a log
    /// window, a trace search — and need what the stories are doing at each,
    /// not the builds and deploys, which they take from one full moment.
    pub fn light_moment(&self, instances: &[Instance], t: DateTime<Utc>) -> Moment {
        Moment {
            t,
            seed: self.seed,
            epoch: self.epoch(),
            started_at: self.started_at,
            instances: Vec::new(),
            builds: Vec::new(),
            deploys: Vec::new(),
            effects: effects_at(instances, t),
            active: active_at(instances, t),
        }
    }

    /// Every telling that has started by `until`, oldest first.
    pub fn instances(&self, until: DateTime<Utc>) -> Vec<Instance> {
        let mut manual: Vec<Instance> = self
            .triggers
            .iter()
            .filter(|(at, _)| *at <= until)
            .map(|(at, story)| Instance {
                story: *story,
                start: *at,
                ordinal: 0,
                manual: true,
            })
            .collect();
        manual.sort_by_key(|i| i.start);

        let mut all: Vec<Instance> = Vec::new();
        let mut k = -PAST_STORIES;
        loop {
            let start = self.cycle_base(k);
            if start > until {
                break;
            }
            let story = Story::ALL[k.rem_euclid(Story::ALL.len() as i64) as usize];
            // A story on the grid gives way to one started by hand: it would
            // begin during it, or straight after it with no calm between.
            let displaced = manual
                .iter()
                .any(|m| start >= m.start && start < m.end() + Duration::seconds(CALM_SECS));
            if !displaced {
                all.push(Instance {
                    story,
                    start,
                    ordinal: 0,
                    manual: false,
                });
            }
            k += 1;
        }
        all.extend(manual);
        all.sort_by_key(|i| i.start);

        let mut seen = [0u32; 3];
        for instance in &mut all {
            let slot = Story::ALL.iter().position(|s| *s == instance.story).unwrap_or(0);
            instance.ordinal = seen[slot];
            seen[slot] += 1;
        }
        all
    }

    /// The telling playing at `t`, if one is.
    pub fn active(&self, t: DateTime<Utc>) -> Option<Instance> {
        active_at(&self.instances(t), t)
    }

    /// The next telling to start after `t`.
    pub fn next(&self, t: DateTime<Utc>) -> Option<Instance> {
        let horizon = t + Duration::seconds(CYCLE_SECS * 2);
        self.instances(horizon).into_iter().find(|i| i.start > t)
    }

    /// When a story asked for now should begin: now, unless one is already
    /// playing, in which case straight after it. Two incidents on top of each
    /// other teach nothing about either.
    pub fn trigger_time(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self.active(now) {
            Some(playing) => playing.end() + Duration::seconds(20),
            None => now,
        }
    }

    /// Every build started by `until`, numbered per job, oldest first.
    fn builds_from(&self, instances: &[Instance], until: DateTime<Utc>) -> Vec<Build> {
        type Raw = (&'static str, DateTime<Utc>, f64, BuildResult, String);
        let mut raw: Vec<Raw> = Vec::new();

        // Benign work on the grid, reaching back over the whole replayed past.
        let first = -(BACKFILL_SECS / CYCLE_SECS) - 1;
        let mut k = first;
        loop {
            let base = self.cycle_base(k);
            if base > until {
                break;
            }
            if k.rem_euclid(2) == 0 {
                raw.push((
                    "deploy-catalog-api",
                    base + Duration::seconds(495),
                    75.0,
                    BuildResult::Success,
                    format!("Deployed catalog-api 3.{}.0", 19 + (k - first) / 2),
                ));
            }
            if k.rem_euclid(3) == 1 {
                raw.push((
                    "deploy-web",
                    base + Duration::seconds(555),
                    75.0,
                    BuildResult::Success,
                    format!("Deployed web 4.{}.0", 13 + (k - first) / 3),
                ));
            }
            // A minute long, twice a cycle: the second run lands just after a
            // bad deploy's recovery, so a smoke test failed by the outage goes
            // green with the estate rather than lingering as its own incident.
            for (slot, offset) in [(0u64, 200i64), (1, 400)] {
                let flaky = hash(&[self.seed, k as u64, slot, 0xE2E]).is_multiple_of(6);
                raw.push((
                    "e2e-smoke",
                    base + Duration::seconds(offset),
                    60.0,
                    if flaky { BuildResult::Failure } else { BuildResult::Success },
                    if flaky {
                        "Smoke suite: 1 of 38 failed (search suggestions timed out)".to_string()
                    } else {
                        "Smoke suite: 38 of 38 passed".to_string()
                    },
                ));
            }
            k += 1;
        }

        // Each story's job last ran a day before the replay begins, so no
        // job reads "never built" before its story first comes round.
        let before = self.epoch() - Duration::days(1);
        for (job, what) in [
            ("deploy-checkout-api", "Deployed checkout-api 2.40.3"),
            ("deploy-analytics-ingest", "Deployed analytics-ingest 1.33.2"),
            ("reindex-catalog", "Reindexed products-v7: 2,138,977 documents"),
        ] {
            raw.push((job, before, 80.0, BuildResult::Success, what.to_string()));
        }

        // The stories' own builds.
        for instance in instances {
            let n = instance.ordinal;
            match instance.story {
                Story::BadDeploy => {
                    raw.push((
                        "deploy-checkout-api",
                        instance.start,
                        70.0,
                        BuildResult::Success,
                        format!("Deployed checkout-api 2.{}.0", 41 + n),
                    ));
                    raw.push((
                        "deploy-checkout-api",
                        instance.at(290.0),
                        70.0,
                        BuildResult::Success,
                        format!("Deployed checkout-api 2.{}.1", 41 + n),
                    ));
                }
                Story::ConsumerOom => {
                    raw.push((
                        "deploy-analytics-ingest",
                        instance.start,
                        60.0,
                        BuildResult::Success,
                        format!("Deployed analytics-ingest 1.{}.0", 34 + n),
                    ));
                    raw.push((
                        "deploy-analytics-ingest",
                        instance.at(250.0),
                        60.0,
                        BuildResult::Success,
                        format!("Deployed analytics-ingest 1.{}.1", 34 + n),
                    ));
                }
                Story::SearchHeap => {
                    raw.push((
                        "reindex-catalog",
                        instance.start,
                        300.0,
                        BuildResult::Success,
                        "Reindexed products-v7: 2,140,512 documents".to_string(),
                    ));
                }
            }
        }

        raw.retain(|(_, start, ..)| *start <= until);
        raw.sort_by_key(|(_, start, ..)| *start);

        let mut counters: Vec<(&str, i64)> = JOBS.to_vec();
        let mut out = Vec::with_capacity(raw.len());
        for (job, start, secs, mut result, mut what) in raw {
            let Some(slot) = counters.iter_mut().find(|(j, _)| *j == job) else {
                continue;
            };
            let number = slot.1;
            slot.1 += 1;
            // A smoke run while a bad checkout build is live fails on
            // checkout, whatever the dice said — the test doing its job.
            if job == "e2e-smoke" {
                let mid = start + Duration::milliseconds((secs * 500.0) as i64);
                if effects_at(instances, mid).checkout_errors > 0.3 {
                    result = BuildResult::Failure;
                    what = "Smoke suite: 3 of 38 failed (checkout flow timed out after 30s)"
                        .to_string();
                }
            }
            out.push(Build {
                job,
                number,
                start,
                end: start + Duration::milliseconds((secs * 1000.0) as i64),
                result,
                what,
            });
        }
        out
    }
}

fn active_at(instances: &[Instance], t: DateTime<Utc>) -> Option<Instance> {
    instances
        .iter()
        .rev()
        .find(|i| i.start <= t && t < i.end())
        .copied()
}

/// Every deploy finished by `until`, oldest first. A deploy is a successful
/// deploy job; its version is the last word of what the job says it did.
fn deploys_from(builds: &[Build], until: DateTime<Utc>) -> Vec<Deploy> {
    let mut out: Vec<Deploy> = builds
        .iter()
        .filter(|b| b.end <= until && b.result == BuildResult::Success)
        .filter_map(|b| {
            let service = match b.job {
                "deploy-checkout-api" => "checkout-api",
                "deploy-catalog-api" => "catalog-api",
                "deploy-web" => "web",
                "deploy-analytics-ingest" => "analytics-ingest",
                _ => return None,
            };
            Some(Deploy {
                at: b.end,
                service,
                version: b.what.rsplit(' ').next().unwrap_or("").to_string(),
            })
        })
        .collect();
    out.sort_by_key(|d| d.at);
    out
}

/// What the telling playing at `t` is doing to the estate.
pub(crate) fn effects_at(instances: &[Instance], t: DateTime<Utc>) -> Effects {
    let mut out = Effects::calm();
    let Some(instance) = active_at(instances, t) else {
        return out;
    };
    let tau = instance.tau(t);
    match instance.story {
        Story::BadDeploy => {
            out.n_plus_one = envelope(tau, 85.0, 50.0, 365.0, 50.0);
            out.db_saturation = envelope(tau, 120.0, 110.0, 370.0, 70.0);
            out.checkout_errors = envelope(tau, 150.0, 60.0, 360.0, 40.0);
            out.checkout_unhealthy = [
                within(tau, 100.0, 360.0),
                within(tau, 125.0, 240.0) || within(tau, 262.0, 375.0),
            ];
            out.checkout_crashed = within(tau, 240.0, 262.0);
            out.checkout_503 = within(tau, 190.0, 345.0);
            out.checkout_lb_down = [
                within(tau, 190.0, 345.0),
                within(tau, 215.0, 345.0) || out.checkout_crashed,
            ];
            out.edge_queue = envelope(tau, 160.0, 40.0, 350.0, 40.0);
        }
        Story::ConsumerOom => {
            out.ingest_crashloop = envelope(tau, 75.0, 30.0, 300.0, 20.0);
            out.ingest_ready = if within(tau, 85.0, 140.0) {
                1
            } else if within(tau, 140.0, 320.0) {
                0
            } else if within(tau, 320.0, 335.0) {
                2
            } else {
                3
            };
            out.ingest_members = if within(tau, 95.0, 150.0) {
                1
            } else if within(tau, 150.0, 325.0) {
                0
            } else {
                3
            };
            out.ingest_lag = envelope(tau, 90.0, 220.0, 320.0, 120.0);
        }
        Story::SearchHeap => {
            out.es_heap = envelope(tau, 20.0, 130.0, 250.0, 60.0);
            out.es_node_out = within(tau, 170.0, 250.0);
            out.search_slow = envelope(tau, 165.0, 50.0, 260.0, 60.0);
            out.catalog_unhealthy = within(tau, 172.0, 255.0);
            out.catalog_lb_down = within(tau, 180.0, 255.0);
        }
    }
    out
}

impl Moment {
    pub fn secs(&self) -> f64 {
        self.t.timestamp_millis() as f64 / 1000.0
    }

    /// The version of `service` running at `t` (or at an earlier moment, for
    /// a replica that has not rolled yet).
    pub fn version_at(&self, service: &str, t: DateTime<Utc>) -> String {
        self.deploys
            .iter()
            .rev()
            .find(|d| d.service == service && d.at <= t)
            .map(|d| d.version.clone())
            .or_else(|| {
                BASE_VERSIONS
                    .iter()
                    .find(|(s, _)| *s == service)
                    .map(|(_, v)| (*v).to_string())
            })
            .unwrap_or_else(|| "1.0.0".to_string())
    }

    pub fn deployed_at(&self, service: &str, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.deploys
            .iter()
            .rev()
            .find(|d| d.service == service && d.at <= t)
            .map(|d| d.at)
    }

    /// When a container last started, how often it has crashed, and since
    /// when it has been down if it is down right now.
    ///
    /// A deploy replaces a service's containers — the second replica a few
    /// seconds after the first, as a rolling update does — and a crash
    /// restarts one.
    pub fn container_life(
        &self,
        service: &str,
        replica: usize,
        born: DateTime<Utc>,
    ) -> (DateTime<Utc>, u32, Option<DateTime<Utc>>) {
        let stagger = Duration::seconds(15 * replica as i64);
        let mut started = born;
        if let Some(at) = self.deployed_at(service, self.t - stagger) {
            started = started.max(at + stagger);
        }
        let mut restarts = 0u32;
        let mut exited_at = None;
        if service == "checkout-api" && replica == 1 {
            for instance in self.instances.iter().filter(|i| i.story == Story::BadDeploy) {
                let crash = instance.at(240.0);
                let back = instance.at(262.0);
                if crash <= self.t {
                    if self.t < back {
                        exited_at = Some(crash);
                    } else {
                        restarts += 1;
                        started = started.max(back);
                    }
                }
            }
        }
        (started, restarts, exited_at)
    }

    /// The latest build of a job started by now, and the one before it that
    /// finished — Jenkins colours a job by the last *completed* build.
    pub fn job_builds(&self, job: &str) -> Vec<&Build> {
        self.builds.iter().filter(|b| b.job == job).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Script {
        Script {
            started_at: DateTime::parse_from_rfc3339("2026-10-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            seed: 7,
            triggers: Vec::new(),
        }
    }

    #[test]
    fn the_first_live_story_is_the_deploy_and_it_starts_soon() {
        let s = script();
        let next = s.next(s.started_at).expect("a story is coming");
        assert_eq!(next.story, Story::BadDeploy);
        assert_eq!((next.start - s.started_at).num_seconds(), LEAD_SECS);
    }

    #[test]
    fn the_past_holds_the_other_two_stories() {
        let s = script();
        let past: Vec<Story> = s.instances(s.started_at).into_iter().map(|i| i.story).collect();
        assert_eq!(past, vec![Story::ConsumerOom, Story::SearchHeap]);
    }

    #[test]
    fn every_telling_ships_a_new_version() {
        let s = script();
        let m = s.moment(s.started_at + Duration::hours(2));
        let versions: Vec<String> = m
            .deploys
            .iter()
            .filter(|d| d.service == "checkout-api")
            .map(|d| d.version.clone())
            .collect();
        let mut unique = versions.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(versions.len(), unique.len(), "{versions:?}");
        assert!(versions.len() >= 4, "{versions:?}");
    }

    #[test]
    fn a_story_started_by_hand_waits_for_the_one_playing() {
        let mut s = script();
        let during = s.started_at + Duration::seconds(LEAD_SECS + 100);
        let playing = s.active(during).expect("the deploy story is playing");
        let at = s.trigger_time(during);
        assert!(at >= playing.end());
        s.triggers.push((at, Story::SearchHeap));
        let all = s.instances(at + Duration::hours(1));
        for pair in all.windows(2) {
            assert!(pair[1].start >= pair[0].end(), "{pair:?}");
        }
    }

    #[test]
    fn the_estate_recovers_after_each_story() {
        let s = script();
        for instance in s.instances(s.started_at + Duration::hours(1)) {
            let e = s.moment(instance.end() + Duration::seconds(5)).effects;
            assert_eq!(e.n_plus_one, 0.0);
            assert!(!e.checkout_503 && !e.es_node_out);
            assert_eq!(e.ingest_ready, 3);
        }
    }

    #[test]
    fn build_numbers_climb_per_job() {
        let s = script();
        let m = s.moment(s.started_at + Duration::hours(1));
        for (job, first) in JOBS {
            let numbers: Vec<i64> = m.builds.iter().filter(|b| b.job == *job).map(|b| b.number).collect();
            for (i, n) in numbers.iter().enumerate() {
                assert_eq!(*n, first + i as i64);
            }
        }
    }

    #[test]
    fn the_bad_build_fails_the_smoke_test() {
        let s = script();
        let m = s.moment(s.started_at + Duration::hours(1));
        let bad = m.instances.iter().find(|i| i.story == Story::BadDeploy && !i.manual).unwrap();
        let during = m
            .builds
            .iter()
            .filter(|b| b.job == "e2e-smoke")
            .find(|b| b.start >= bad.at(150.0) && b.start <= bad.at(300.0));
        if let Some(build) = during {
            assert_eq!(build.result, BuildResult::Failure);
        }
    }
}
