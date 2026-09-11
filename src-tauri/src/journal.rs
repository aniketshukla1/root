//! The judgement record: what root said, and whether it was right.
//!
//! Everything else in this app is disposable. Metric history is a ring buffer,
//! logs are a ring buffer, incidents are derived fresh on every read, and all
//! of it is gone when the process stops — deliberately, because none of it is
//! worth a retention policy an operator has to manage.
//!
//! This is the exception. A record of which findings were right, which were
//! noise, and which problems root missed entirely is the only dataset here
//! that gets **more** valuable with age, and it is the only thing that can
//! answer the question every step of this product's roadmap depends on: is it
//! actually any good yet?
//!
//! ## Why a local file and not the estate's Prometheus
//!
//! A verdict is a document — the finding, the evidence under it, a free-text
//! note, which rules version produced it — and Prometheus stores `float64`
//! samples against label sets. Cramming a document into labels is a cardinality
//! explosion, and Prometheus expires data by design (15 days by default),
//! which is precisely backwards for the one dataset that must never expire.
//!
//! It is also monitored infrastructure. Storing root's own memory inside a
//! thing root watches means losing the record and the signal in the same
//! outage. A few hundred rows a year belongs in a file with no daemon.
//!
//! ## Append-only, and why that is not pedantry
//!
//! A verdict is never updated or deleted. Changing your mind writes a second
//! row; the first one stays. Two reasons:
//!
//! Someone will eventually want to know whether root got *better*, and that
//! question is unanswerable if the past has been edited to agree with the
//! present. And a rating that moved has its own information in it — a finding
//! people first called noise and later called correct is a finding whose
//! wording was wrong, not whose logic was.
//!
//! ## Snapshots, because findings drift
//!
//! Each verdict stores the whole finding as it was judged, plus a fingerprint
//! of it and the rules version that produced it. Without that, a rule reworded
//! next month silently rewrites history: you are left with a score attached to
//! text nobody can reconstruct, which is a number that cannot be interpreted
//! and therefore is not evidence of anything.
//!
//! ## Two tables, because one of them measures what the other cannot
//!
//! `verdicts` rates what root *said*. That is precision, and it is the easy
//! half.
//!
//! `recalls` asks, when something has actually broken, whether root called it.
//! That is recall, and no amount of thumbs on findings can produce it: if root
//! never mentioned the thing that broke, there was never a card to rate. A
//! product graded only on the times it spoke will look excellent right up
//! until the outage it had nothing to say about.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::error::Result;

/// Bumped whenever a rule's logic or wording changes, so a score can be
/// attributed to a version rather than averaged across two different products.
pub const RULES_VERSION: &str = "1";

/// Was it a real problem?
///
/// Four answers rather than a thumb, because "bad" collapses three unrelated
/// failures into one number and each is fixed somewhere different: a rule that
/// fires wrongly, a rule that fires on something true but unimportant, and a
/// rule that is right but arrived too late to be worth anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProblemVerdict {
    /// Correct, and worth knowing.
    Real,
    /// Not actually a problem — a false positive.
    NotAProblem,
    /// True, but already known. Correct and redundant.
    AlreadyKnew,
    /// Cannot tell. Recorded rather than skipped: a finding nobody can
    /// evaluate is itself a defect in how the finding is written.
    Unsure,
}

/// Was the suggested fix the right one?
///
/// Separate from the problem verdict because a single rating cannot say which
/// half failed, and the two are fixed in completely different places: a wrong
/// problem is a detection bug, a wrong fix with a right problem is a remedy
/// bug, and treating them as one number hides both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FixVerdict {
    /// Would run it.
    Right,
    /// Would not — wrong remedy, or would make things worse.
    Wrong,
    /// Right idea, wrong specifics (value, syntax, target).
    PartlyRight,
    Unsure,
}

/// Where a finding came from.
///
/// Rules and the model fail differently — a rule never invents a setting but
/// only finds what somebody encoded; the model reaches further and can be
/// confidently wrong — so a score that mixes them describes neither. Kept as
/// a string rather than an enum so a rule id (`redis.maxmemory-noeviction`)
/// travels with it and a per-rule score is a `GROUP BY`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provenance {
    /// A deterministic rule, named.
    Rule { id: String },
    /// The assistant, unprompted by any rule.
    Model { model: String },
    /// The assistant restating a rule's finding. Scored apart from both,
    /// because counting it as a model win would credit the model for the
    /// rule's work.
    ModelCitingRule { model: String, rule: String },
}

impl Provenance {
    /// The column value: `rule:<id>`, `model:<name>`, `model+rule:<id>`.
    pub fn tag(&self) -> String {
        match self {
            Provenance::Rule { id } => format!("rule:{id}"),
            Provenance::Model { .. } => "model".to_string(),
            Provenance::ModelCitingRule { rule, .. } => format!("model+rule:{rule}"),
        }
    }

    /// The coarse bucket a scorecard groups by.
    pub fn family(&self) -> &'static str {
        match self {
            Provenance::Rule { .. } => "rule",
            Provenance::Model { .. } => "model",
            Provenance::ModelCitingRule { .. } => "model+rule",
        }
    }
}

/// One judgement, as written.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub finding_id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub provenance: Provenance,
    /// The finding exactly as it was judged. Stored whole so a score can still
    /// be read back after the rule that produced it has been rewritten.
    pub snapshot: serde_json::Value,
    pub problem: ProblemVerdict,
    /// Absent when the finding carried no suggestion to judge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<FixVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One judgement as it comes back out, with what the store added.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedVerdict {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub finding_id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub provenance: String,
    pub family: String,
    pub fingerprint: String,
    pub rules_version: String,
    pub snapshot: serde_json::Value,
    pub problem: ProblemVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<FixVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Did root see this coming?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Called {
    /// Root reported it before it broke.
    Yes,
    /// Root had nothing to say. The miss — the measurement that ratings on
    /// findings structurally cannot produce.
    No,
    /// Root said something related but not the thing that happened.
    Partly,
}

/// One incident, and whether root called it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recall {
    pub incident_id: String,
    pub headline: String,
    pub profile_ids: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub called: Called,
    /// Which finding called it, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedRecall {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub incident_id: String,
    pub headline: String,
    pub profile_ids: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub called: Called,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How root is doing, by the only two measures that matter.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scorecard {
    pub by_family: Vec<FamilyScore>,
    pub by_rule: Vec<RuleScore>,
    pub recall: RecallScore,
    pub rules_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyScore {
    pub family: String,
    pub judged: i64,
    pub real: i64,
    pub not_a_problem: i64,
    pub already_knew: i64,
    pub unsure: i64,
    pub fix_right: i64,
    pub fix_wrong: i64,
    pub fix_partly: i64,
}

impl FamilyScore {
    /// Share of judged findings that were real problems.
    ///
    /// `AlreadyKnew` counts as correct: root was right, and being second to
    /// know is a usefulness problem, not an accuracy one. `Unsure` is excluded
    /// from the denominator rather than counted against — grading a rule on
    /// findings nobody could evaluate measures the rater, not the rule.
    pub fn precision(&self) -> Option<f64> {
        let decided = self.real + self.not_a_problem + self.already_knew;
        (decided > 0).then(|| (self.real + self.already_knew) as f64 / decided as f64)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScore {
    pub rule: String,
    pub judged: i64,
    pub real: i64,
    pub not_a_problem: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallScore {
    pub incidents: i64,
    pub called: i64,
    pub missed: i64,
    pub partly: i64,
}

impl RecallScore {
    /// Share of real incidents root saw coming. `Partly` counts as a half.
    pub fn recall(&self) -> Option<f64> {
        (self.incidents > 0)
            .then(|| (self.called as f64 + self.partly as f64 * 0.5) / self.incidents as f64)
    }
}

/// The judgement store.
pub struct Journal {
    pool: SqlitePool,
}

impl Journal {
    /// Open (creating if needed) the journal beside the config file.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // The record must survive the kind of stop that loses everything
            // else — a crash, a force quit, a laptop lid. WAL plus a real
            // fsync is the point of keeping it here rather than in memory.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;

        migrate(&pool).await?;
        Ok(Journal { pool })
    }

    /// An in-memory journal, for tests.
    pub async fn ephemeral() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        migrate(&pool).await?;
        Ok(Journal { pool })
    }

    /// Record a judgement. Always an insert — see the module note on why there
    /// is no update.
    pub async fn record_verdict(&self, verdict: &Verdict) -> Result<i64> {
        let fingerprint = fingerprint(&verdict.snapshot);
        let row = sqlx::query(
            "INSERT INTO verdicts (
                 at, finding_id, profile_id, profile_name, provenance, family,
                 fingerprint, rules_version, snapshot, problem, fix, note
             ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) RETURNING id",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&verdict.finding_id)
        .bind(&verdict.profile_id)
        .bind(&verdict.profile_name)
        .bind(verdict.provenance.tag())
        .bind(verdict.provenance.family())
        .bind(&fingerprint)
        .bind(RULES_VERSION)
        .bind(serde_json::to_string(&verdict.snapshot)?)
        .bind(serde_json::to_string(&verdict.problem)?)
        .bind(
            verdict
                .fix
                .map(|f| serde_json::to_string(&f))
                .transpose()?,
        )
        .bind(verdict.note.as_deref())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>("id")?)
    }

    /// Record whether root called an incident.
    pub async fn record_recall(&self, recall: &Recall) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO recalls (
                 at, incident_id, headline, profile_ids, started_at,
                 called, finding_id, note
             ) VALUES (?,?,?,?,?,?,?,?) RETURNING id",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&recall.incident_id)
        .bind(&recall.headline)
        .bind(serde_json::to_string(&recall.profile_ids)?)
        .bind(recall.started_at.to_rfc3339())
        .bind(serde_json::to_string(&recall.called)?)
        .bind(recall.finding_id.as_deref())
        .bind(recall.note.as_deref())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>("id")?)
    }

    /// The latest judgement for each finding, newest first.
    ///
    /// "Latest" rather than "the" because a finding can be judged more than
    /// once; the history is all still there, and this is what the UI needs in
    /// order to show a finding as already rated.
    pub async fn verdicts(&self, limit: i64) -> Result<Vec<RecordedVerdict>> {
        let rows = sqlx::query(
            "SELECT * FROM verdicts
             WHERE id IN (SELECT MAX(id) FROM verdicts GROUP BY finding_id)
             ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 2000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(read_verdict).collect()
    }

    /// Every judgement ever written for one finding, oldest first — including
    /// the ones that were later changed.
    pub async fn verdict_history(&self, finding_id: &str) -> Result<Vec<RecordedVerdict>> {
        let rows = sqlx::query("SELECT * FROM verdicts WHERE finding_id = ? ORDER BY id")
            .bind(finding_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(read_verdict).collect()
    }

    pub async fn recalls(&self, limit: i64) -> Result<Vec<RecordedRecall>> {
        let rows = sqlx::query(
            "SELECT * FROM recalls
             WHERE id IN (SELECT MAX(id) FROM recalls GROUP BY incident_id)
             ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 2000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(read_recall).collect()
    }

    /// Incident ids already judged, so the UI does not ask twice.
    pub async fn judged_incidents(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT DISTINCT incident_id FROM recalls")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<String, _>("incident_id")?))
            .collect()
    }

    /// Precision by provenance and by rule, and recall from the incident side.
    pub async fn scorecard(&self) -> Result<Scorecard> {
        // Only the latest judgement per finding counts toward a score. The
        // superseded ones stay on disk — they are the record of a mind
        // changing — but counting them would let one much-revisited finding
        // outweigh fifty settled ones.
        //
        // The `WHERE id IN (SELECT MAX(id) ...)` is written out in each query
        // rather than interpolated: sqlx refuses non-static SQL by default,
        // and that is the correct default even when the fragment is a
        // constant — the moment it is `format!`-able, somebody formats a
        // variable into it.
        let family_rows = sqlx::query(
            "SELECT family,
                    COUNT(*) AS judged,
                    SUM(problem = '\"real\"')           AS real_count,
                    SUM(problem = '\"notAProblem\"')    AS not_a_problem,
                    SUM(problem = '\"alreadyKnew\"')    AS already_knew,
                    SUM(problem = '\"unsure\"')         AS unsure,
                    SUM(fix = '\"right\"')              AS fix_right,
                    SUM(fix = '\"wrong\"')              AS fix_wrong,
                    SUM(fix = '\"partlyRight\"')        AS fix_partly
             FROM verdicts
             WHERE id IN (SELECT MAX(id) FROM verdicts GROUP BY finding_id)
             GROUP BY family ORDER BY family",
        )
        .fetch_all(&self.pool)
        .await?;

        let by_family = family_rows
            .iter()
            .map(|r| {
                Ok(FamilyScore {
                    family: r.try_get("family")?,
                    judged: r.try_get("judged")?,
                    real: r.try_get::<Option<i64>, _>("real_count")?.unwrap_or(0),
                    not_a_problem: r.try_get::<Option<i64>, _>("not_a_problem")?.unwrap_or(0),
                    already_knew: r.try_get::<Option<i64>, _>("already_knew")?.unwrap_or(0),
                    unsure: r.try_get::<Option<i64>, _>("unsure")?.unwrap_or(0),
                    fix_right: r.try_get::<Option<i64>, _>("fix_right")?.unwrap_or(0),
                    fix_wrong: r.try_get::<Option<i64>, _>("fix_wrong")?.unwrap_or(0),
                    fix_partly: r.try_get::<Option<i64>, _>("fix_partly")?.unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Per-rule, because "rules are 80% precise" is not actionable and
        // "this one rule is 30% precise" is: it names the thing to fix.
        let rule_rows = sqlx::query(
            "SELECT provenance,
                    COUNT(*) AS judged,
                    SUM(problem = '\"real\"')        AS real_count,
                    SUM(problem = '\"notAProblem\"') AS not_a_problem
             FROM verdicts
             WHERE id IN (SELECT MAX(id) FROM verdicts GROUP BY finding_id)
               AND family = 'rule'
             GROUP BY provenance
             ORDER BY not_a_problem DESC, judged DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        let by_rule = rule_rows
            .iter()
            .map(|r| {
                Ok(RuleScore {
                    rule: r
                        .try_get::<String, _>("provenance")?
                        .trim_start_matches("rule:")
                        .to_string(),
                    judged: r.try_get("judged")?,
                    real: r.try_get::<Option<i64>, _>("real_count")?.unwrap_or(0),
                    not_a_problem: r.try_get::<Option<i64>, _>("not_a_problem")?.unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let recall_row = sqlx::query(
            "SELECT COUNT(*) AS incidents,
                    SUM(called = '\"yes\"')    AS called,
                    SUM(called = '\"no\"')     AS missed,
                    SUM(called = '\"partly\"') AS partly
             FROM recalls
             WHERE id IN (SELECT MAX(id) FROM recalls GROUP BY incident_id)",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(Scorecard {
            by_family,
            by_rule,
            recall: RecallScore {
                incidents: recall_row.try_get("incidents")?,
                called: recall_row.try_get::<Option<i64>, _>("called")?.unwrap_or(0),
                missed: recall_row.try_get::<Option<i64>, _>("missed")?.unwrap_or(0),
                partly: recall_row.try_get::<Option<i64>, _>("partly")?.unwrap_or(0),
            },
            rules_version: RULES_VERSION.to_string(),
        })
    }
}

/// Schema, applied on every open.
///
/// `IF NOT EXISTS` rather than a migration framework: there is one table set,
/// it is append-only, and the day a column needs to change the honest move is
/// a new column with a default, not a rewrite of history.
async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS verdicts (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            at            TEXT NOT NULL,
            finding_id    TEXT NOT NULL,
            profile_id    TEXT NOT NULL,
            profile_name  TEXT NOT NULL,
            provenance    TEXT NOT NULL,
            family        TEXT NOT NULL,
            fingerprint   TEXT NOT NULL,
            rules_version TEXT NOT NULL,
            snapshot      TEXT NOT NULL,
            problem       TEXT NOT NULL,
            fix           TEXT,
            note          TEXT
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS verdicts_finding ON verdicts (finding_id)")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS recalls (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            at           TEXT NOT NULL,
            incident_id  TEXT NOT NULL,
            headline     TEXT NOT NULL,
            profile_ids  TEXT NOT NULL,
            started_at   TEXT NOT NULL,
            called       TEXT NOT NULL,
            finding_id   TEXT,
            note         TEXT
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS recalls_incident ON recalls (incident_id)")
        .execute(pool)
        .await?;

    Ok(())
}

/// A stable hash of the judged content.
///
/// Lets a later reader ask "is this the same finding I rated, or has the rule
/// been reworded underneath me" without diffing two JSON blobs by eye.
fn fingerprint(snapshot: &serde_json::Value) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    // Serialised through serde_json::Value, whose object representation is
    // key-ordered, so the same content hashes the same regardless of how the
    // caller happened to build it.
    snapshot.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn read_verdict(row: &sqlx::sqlite::SqliteRow) -> Result<RecordedVerdict> {
    Ok(RecordedVerdict {
        id: row.try_get("id")?,
        at: parse_time(row.try_get::<String, _>("at")?),
        finding_id: row.try_get("finding_id")?,
        profile_id: row.try_get("profile_id")?,
        profile_name: row.try_get("profile_name")?,
        provenance: row.try_get("provenance")?,
        family: row.try_get("family")?,
        fingerprint: row.try_get("fingerprint")?,
        rules_version: row.try_get("rules_version")?,
        snapshot: serde_json::from_str(&row.try_get::<String, _>("snapshot")?)
            .unwrap_or(serde_json::Value::Null),
        problem: serde_json::from_str(&row.try_get::<String, _>("problem")?)
            .unwrap_or(ProblemVerdict::Unsure),
        fix: row
            .try_get::<Option<String>, _>("fix")?
            .and_then(|s| serde_json::from_str(&s).ok()),
        note: row.try_get("note")?,
    })
}

fn read_recall(row: &sqlx::sqlite::SqliteRow) -> Result<RecordedRecall> {
    Ok(RecordedRecall {
        id: row.try_get("id")?,
        at: parse_time(row.try_get::<String, _>("at")?),
        incident_id: row.try_get("incident_id")?,
        headline: row.try_get("headline")?,
        profile_ids: serde_json::from_str(&row.try_get::<String, _>("profile_ids")?)
            .unwrap_or_default(),
        started_at: parse_time(row.try_get::<String, _>("started_at")?),
        called: serde_json::from_str(&row.try_get::<String, _>("called")?)
            .unwrap_or(Called::Partly),
        finding_id: row.try_get("finding_id")?,
        note: row.try_get("note")?,
    })
}

fn parse_time(raw: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(title: &str) -> serde_json::Value {
        serde_json::json!({ "title": title, "severity": "now" })
    }

    fn verdict(finding: &str, problem: ProblemVerdict) -> Verdict {
        Verdict {
            finding_id: finding.to_string(),
            profile_id: "p1".into(),
            profile_name: "session-cache".into(),
            provenance: Provenance::Rule {
                id: "redis.maxmemory-noeviction".into(),
            },
            snapshot: snapshot("Writes will start failing"),
            problem,
            fix: Some(FixVerdict::Right),
            note: None,
        }
    }

    #[tokio::test]
    async fn a_verdict_survives_being_written_and_read() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();

        let out = journal.verdicts(10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].problem, ProblemVerdict::Real);
        assert_eq!(out[0].fix, Some(FixVerdict::Right));
        assert_eq!(out[0].provenance, "rule:redis.maxmemory-noeviction");
        assert_eq!(out[0].family, "rule");
        assert_eq!(out[0].rules_version, RULES_VERSION);
        assert_eq!(out[0].snapshot["title"], "Writes will start failing");
    }

    /// Changing your mind writes a row; it does not erase one. Without this
    /// the question "did root get better" is unanswerable, because the past
    /// has been edited to agree with the present.
    #[tokio::test]
    async fn a_second_judgement_supersedes_without_erasing() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::NotAProblem))
            .await
            .unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();

        let latest = journal.verdicts(10).await.unwrap();
        assert_eq!(latest.len(), 1, "one row per finding in the current view");
        assert_eq!(latest[0].problem, ProblemVerdict::Real);

        let history = journal.verdict_history("f1").await.unwrap();
        assert_eq!(history.len(), 2, "both are still on disk");
        assert_eq!(history[0].problem, ProblemVerdict::NotAProblem);
        assert_eq!(history[1].problem, ProblemVerdict::Real);
    }

    /// The score must not let one much-revisited finding outweigh fifty
    /// settled ones.
    #[tokio::test]
    async fn only_the_latest_judgement_counts_toward_a_score() {
        let journal = Journal::ephemeral().await.unwrap();
        for _ in 0..5 {
            journal
                .record_verdict(&verdict("f1", ProblemVerdict::NotAProblem))
                .await
                .unwrap();
        }
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();

        let card = journal.scorecard().await.unwrap();
        assert_eq!(card.by_family.len(), 1);
        assert_eq!(card.by_family[0].judged, 1);
        assert_eq!(card.by_family[0].real, 1);
        assert_eq!(card.by_family[0].not_a_problem, 0);
    }

    /// Rules and the model fail differently, so a score that mixes them
    /// describes neither.
    #[tokio::test]
    async fn precision_is_reported_per_family() {
        let journal = Journal::ephemeral().await.unwrap();

        journal
            .record_verdict(&verdict("r1", ProblemVerdict::Real))
            .await
            .unwrap();
        journal
            .record_verdict(&verdict("r2", ProblemVerdict::Real))
            .await
            .unwrap();

        let mut model = verdict("m1", ProblemVerdict::NotAProblem);
        model.provenance = Provenance::Model {
            model: "claude-opus-5".into(),
        };
        journal.record_verdict(&model).await.unwrap();

        let card = journal.scorecard().await.unwrap();
        let rules = card.by_family.iter().find(|f| f.family == "rule").unwrap();
        let models = card.by_family.iter().find(|f| f.family == "model").unwrap();

        assert_eq!(rules.precision(), Some(1.0));
        assert_eq!(models.precision(), Some(0.0));
    }

    /// Root was right; being second to know is a usefulness problem, not an
    /// accuracy one.
    #[tokio::test]
    async fn already_knowing_counts_as_correct() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::AlreadyKnew))
            .await
            .unwrap();

        let card = journal.scorecard().await.unwrap();
        assert_eq!(card.by_family[0].precision(), Some(1.0));
    }

    /// Grading a rule on findings nobody could evaluate measures the rater.
    #[tokio::test]
    async fn unsure_is_recorded_but_left_out_of_the_denominator() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();
        journal
            .record_verdict(&verdict("f2", ProblemVerdict::Unsure))
            .await
            .unwrap();

        let card = journal.scorecard().await.unwrap();
        let family = &card.by_family[0];
        assert_eq!(family.judged, 2, "both are recorded");
        assert_eq!(family.unsure, 1);
        assert_eq!(family.precision(), Some(1.0), "one of one decided");
    }

    /// The measurement no amount of thumbs on findings can produce.
    #[tokio::test]
    async fn recall_comes_from_the_incident_side() {
        let journal = Journal::ephemeral().await.unwrap();
        for (id, called) in [
            ("i1", Called::Yes),
            ("i2", Called::No),
            ("i3", Called::No),
            ("i4", Called::Partly),
        ] {
            journal
                .record_recall(&Recall {
                    incident_id: id.into(),
                    headline: "orders-api down".into(),
                    profile_ids: vec!["p1".into()],
                    started_at: Utc::now(),
                    called,
                    finding_id: None,
                    note: None,
                })
                .await
                .unwrap();
        }

        let card = journal.scorecard().await.unwrap();
        assert_eq!(card.recall.incidents, 4);
        assert_eq!(card.recall.called, 1);
        assert_eq!(card.recall.missed, 2);
        assert_eq!(card.recall.partly, 1);
        // 1 called + half of 1 partly, over 4.
        assert_eq!(card.recall.recall(), Some(0.375));
    }

    /// "Rules are 80% precise" names nothing to fix. "This rule is 0% precise"
    /// names the thing.
    #[tokio::test]
    async fn the_scorecard_names_the_worst_rule() {
        let journal = Journal::ephemeral().await.unwrap();

        let mut noisy = verdict("n1", ProblemVerdict::NotAProblem);
        noisy.provenance = Provenance::Rule {
            id: "workload.single-replica".into(),
        };
        journal.record_verdict(&noisy).await.unwrap();
        let mut noisy2 = verdict("n2", ProblemVerdict::NotAProblem);
        noisy2.provenance = Provenance::Rule {
            id: "workload.single-replica".into(),
        };
        journal.record_verdict(&noisy2).await.unwrap();

        journal
            .record_verdict(&verdict("g1", ProblemVerdict::Real))
            .await
            .unwrap();

        let card = journal.scorecard().await.unwrap();
        assert_eq!(card.by_rule[0].rule, "workload.single-replica");
        assert_eq!(card.by_rule[0].not_a_problem, 2);
        assert_eq!(card.by_rule[0].real, 0);
    }

    /// A finding reworded after being rated must be detectable as different,
    /// or the score is attached to text nobody can reconstruct.
    #[tokio::test]
    async fn a_reworded_finding_has_a_different_fingerprint() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();

        let mut reworded = verdict("f1", ProblemVerdict::Real);
        reworded.snapshot = snapshot("Writes will start failing soon");
        journal.record_verdict(&reworded).await.unwrap();

        let history = journal.verdict_history("f1").await.unwrap();
        assert_ne!(history[0].fingerprint, history[1].fingerprint);
    }

    #[tokio::test]
    async fn identical_content_fingerprints_identically() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::Real))
            .await
            .unwrap();
        journal
            .record_verdict(&verdict("f1", ProblemVerdict::NotAProblem))
            .await
            .unwrap();

        let history = journal.verdict_history("f1").await.unwrap();
        assert_eq!(history[0].fingerprint, history[1].fingerprint);
    }

    #[tokio::test]
    async fn an_empty_journal_scores_nothing_rather_than_zero() {
        let journal = Journal::ephemeral().await.unwrap();
        let card = journal.scorecard().await.unwrap();
        assert!(card.by_family.is_empty());
        assert_eq!(card.recall.incidents, 0);
        // Not Some(0.0) — no data is not a bad score.
        assert_eq!(card.recall.recall(), None);
    }

    #[tokio::test]
    async fn incidents_already_judged_are_not_asked_about_twice() {
        let journal = Journal::ephemeral().await.unwrap();
        journal
            .record_recall(&Recall {
                incident_id: "i1".into(),
                headline: "x".into(),
                profile_ids: vec![],
                started_at: Utc::now(),
                called: Called::Yes,
                finding_id: None,
                note: None,
            })
            .await
            .unwrap();

        assert_eq!(journal.judged_incidents().await.unwrap(), vec!["i1"]);
    }

    /// The whole reason this is not a ring buffer.
    ///
    /// Set `ROOT_JOURNAL_KEEP` to leave the file behind for inspection with
    /// the sqlite3 CLI — a test that passes proves the code round-trips, and
    /// opening the file proves there is actually a database there.
    #[tokio::test]
    async fn the_record_survives_the_process_that_wrote_it() {
        let dir = match std::env::var("ROOT_JOURNAL_KEEP") {
            Ok(path) => std::path::PathBuf::from(path),
            Err(_) => std::env::temp_dir().join(format!("root-journal-{}", uuid::Uuid::new_v4())),
        };
        let path = dir.join("journal.db");

        {
            let journal = Journal::open(&path).await.unwrap();
            journal
                .record_verdict(&verdict("f1", ProblemVerdict::Real))
                .await
                .unwrap();
        } // dropped: pool closed, as it would be on quit

        let reopened = Journal::open(&path).await.unwrap();
        let out = reopened.verdicts(10).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].problem, ProblemVerdict::Real);
        assert_eq!(out[0].snapshot["title"], "Writes will start failing");

        if std::env::var("ROOT_JOURNAL_KEEP").is_err() {
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
