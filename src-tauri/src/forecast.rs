//! Where a metric is heading, and when it gets there.
//!
//! Fill level answers whether something is a problem now. Capacity planning
//! needs the other question — when does it become one — and that is the same
//! calculation for every metric that declares a limit. A disk filling, a
//! connection pool filling, an inode table filling, etcd's backend quota
//! filling: one derivation, applied wherever a limit exists, rather than
//! re-implemented per collector.
//!
//! The hard part is not the arithmetic. It is refusing to answer.
//!
//! A straight line through a metric's history will always produce a slope, and
//! that slope will always produce a date. On a disk that has been climbing
//! steadily for a week, the date is worth acting on. On CPU that oscillates
//! between 20% and 80% all day, the very same arithmetic yields a confident
//! "full in four hours" that is pure artefact — and a projection nobody can
//! trust is worse than none, because the first wrong one teaches people to
//! ignore the rest.
//!
//! So three things gate an answer, and all of them can withhold it:
//!
//! **Enough samples, over enough time.** Two readings a minute apart can be
//! extrapolated to next year, and the result means nothing. A projection needs
//! both a real number of points and a real span of clock time behind it.
//!
//! **A line that actually fits.** R² is how much of the movement the straight
//! line explains. A metric that sawtooths has a slope and almost no fit, and
//! that is exactly the case to stay quiet about. The fit travels with the
//! projection so the screen can show how much to believe it.
//!
//! **A direction that reaches the target.** A falling metric never reaches a
//! limit above it. That is `None`, not infinity, and not a very large number.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Readings below this are a sample, not a series.
const MIN_SAMPLES: usize = 8;

/// Clock time the samples must span. Ten readings in ten seconds describe a
/// moment, however many of them there are.
const MIN_WINDOW_SECS: f64 = 900.0;

/// Below this, the straight line explains so little of the movement that its
/// slope is noise wearing a trend's clothes.
const MIN_FIT: f64 = 0.25;

/// A metric's direction of travel, with the evidence for it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trend {
    /// Change per second, in the metric's own unit.
    pub per_second: f64,
    /// 0 to 1 — how much of the variance a straight line explains. Carried so
    /// the reader can weigh the projection rather than being handed a number
    /// with no provenance.
    pub fit: f64,
    /// Clock seconds the samples span.
    pub window_secs: f64,
    pub samples: usize,
    /// Seconds until the metric reaches its declared limit at this rate.
    /// Absent when it is not heading there, or when the trend is not
    /// trustworthy enough to extrapolate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds_to_limit: Option<f64>,
}

impl Trend {
    /// Whether the line explains enough of the movement to extrapolate from.
    pub fn is_reliable(&self) -> bool {
        self.fit >= MIN_FIT
    }

    /// Seconds until the metric is at or above `limit`.
    ///
    /// Ceiling semantics, which is what `warn_above` means everywhere else in
    /// the product. Three outcomes, and they are three different facts:
    ///
    /// - `Some(0)` — already at or past it. Not a negative time, which would
    ///   render as a date in the past for something happening now.
    /// - `Some(n)` — climbing, and this is when it arrives.
    /// - `None` — flat, falling, or too noisy to extrapolate. Never, rather
    ///   than a very large number somebody would read as a date.
    ///
    /// The first two were one branch until a test caught it: a metric over its
    /// limit and still rising was being reported as "never reaches", because
    /// the direction check could not tell *moving away from* apart from
    /// *already past*.
    pub fn seconds_to_ceiling(&self, current: f64, limit: f64) -> Option<f64> {
        // Before the fit gate, because this is not a projection. Whether a
        // metric is over its threshold right now is a reading of the present,
        // and it is true however badly a line fits the history behind it. The
        // gate belongs on the extrapolation only — a sawtoothing error rate
        // sitting at four times its limit was reporting nothing at all.
        if current >= limit {
            return Some(0.0);
        }
        if !self.is_reliable() {
            return None;
        }
        if self.per_second <= 0.0 {
            return None;
        }
        let seconds = (limit - current) / self.per_second;
        seconds.is_finite().then_some(seconds)
    }

    /// Seconds until the metric is at or below `floor`.
    ///
    /// The mirror of `seconds_to_ceiling`, for the measures where running out
    /// is running *down*: free bytes, available memory, healthy replicas.
    pub fn seconds_to_floor(&self, current: f64, floor: f64) -> Option<f64> {
        // Present tense first, for the reason `seconds_to_ceiling` gives.
        if current <= floor {
            return Some(0.0);
        }
        if !self.is_reliable() {
            return None;
        }
        if self.per_second >= 0.0 {
            return None;
        }
        let seconds = (floor - current) / self.per_second;
        seconds.is_finite().then_some(seconds)
    }
}

/// One reading, in the shape `history` keeps them.
pub trait Reading {
    fn at(&self) -> DateTime<Utc>;
    fn value(&self) -> f64;
}

impl Reading for crate::history::Point {
    fn at(&self) -> DateTime<Utc> {
        self.t
    }
    fn value(&self) -> f64 {
        self.v
    }
}

/// Fit a line to the readings and report where it points.
///
/// `limit` is the declared threshold the metric is measured against — the same
/// `warn_above` the charts draw — so "reaches its limit" here means the same
/// thing it means everywhere else in the product.
pub fn trend<R: Reading>(readings: &[R], limit: Option<f64>) -> Option<Trend> {
    if readings.len() < MIN_SAMPLES {
        return None;
    }

    let first = readings.first()?.at();
    let last = readings.last()?.at();
    let window_secs = (last - first).num_milliseconds() as f64 / 1000.0;
    if window_secs < MIN_WINDOW_SECS {
        return None;
    }

    // Seconds since the first reading, so the numbers stay small and the
    // regression does not lose precision on epoch-sized timestamps.
    let xs: Vec<f64> = readings
        .iter()
        .map(|r| (r.at() - first).num_milliseconds() as f64 / 1000.0)
        .collect();
    let ys: Vec<f64> = readings.iter().map(Reading::value).collect();

    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;

    let mut sxy = 0.0;
    let mut sxx = 0.0;
    for (x, y) in xs.iter().zip(&ys) {
        sxy += (x - mean_x) * (y - mean_y);
        sxx += (x - mean_x).powi(2);
    }
    // Every reading at the same instant. No slope exists.
    if sxx <= f64::EPSILON {
        return None;
    }

    let per_second = sxy / sxx;
    let intercept = mean_y - per_second * mean_x;

    // R²: how much of the movement the line accounts for.
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for (x, y) in xs.iter().zip(&ys) {
        let predicted = intercept + per_second * x;
        ss_res += (y - predicted).powi(2);
        ss_tot += (y - mean_y).powi(2);
    }
    // A perfectly flat metric has no variance to explain. The line describes
    // it exactly, which is a fit of 1 — and a slope of zero, so nothing will
    // be projected from it anyway.
    let fit = if ss_tot <= f64::EPSILON {
        1.0
    } else {
        (1.0 - ss_res / ss_tot).clamp(0.0, 1.0)
    };

    let mut out = Trend {
        per_second,
        fit,
        window_secs,
        samples: readings.len(),
        seconds_to_limit: None,
    };
    if let (Some(limit), Some(current)) = (limit, ys.last().copied()) {
        out.seconds_to_limit = out.seconds_to_ceiling(current, limit);
    }
    Some(out)
}

#[cfg(test)]
mod tests {

    /// A metric can be over its limit and too noisy to fit at the same time,
    /// and those are different facts. Reporting nothing because the line is
    /// poor hides a breach that is happening right now.
    #[test]
    fn a_noisy_metric_already_over_its_limit_still_reports_no_time_left() {
        // Sawtoothing well above a limit of 2.0 — no line fits it, and it is
        // breaching regardless.
        let points = series(40, Duration::minutes(30), |i| if i % 2 == 0 { 4.0 } else { 12.0 });
        let trend = trend(&points, Some(2.0)).expect("a fit exists, however poor");
        assert!(!trend.is_reliable(), "fit was {}", trend.fit);
        assert_eq!(trend.seconds_to_limit, Some(0.0));
    }

    use super::*;
    use chrono::Duration;

    struct Sample(DateTime<Utc>, f64);
    impl Reading for Sample {
        fn at(&self) -> DateTime<Utc> {
            self.0
        }
        fn value(&self) -> f64 {
            self.1
        }
    }

    fn base() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    /// `count` readings, `step` apart, following `f`.
    fn series(count: usize, step: Duration, f: impl Fn(usize) -> f64) -> Vec<Sample> {
        (0..count)
            .map(|i| Sample(base() + step * (i as i32), f(i)))
            .collect()
    }

    /// A disk climbing a steady gigabyte an hour, with a limit ten hours out.
    #[test]
    fn a_steady_climb_projects_to_its_limit() {
        // 24 readings, 30 minutes apart: half a gigabyte each step.
        let points = series(24, Duration::minutes(30), |i| 100.0 + i as f64 * 0.5);
        let trend = trend(&points, Some(160.0)).expect("a trend");

        assert!(trend.fit > 0.999, "a straight line fits a straight line");
        // 0.5 units per 1800s.
        assert!((trend.per_second - 0.5 / 1800.0).abs() < 1e-9);

        let hours = trend.seconds_to_limit.expect("heading there") / 3600.0;
        // Latest is 111.5; 48.5 to go at 1/hour.
        assert!((hours - 48.5).abs() < 0.1, "{hours}");
    }

    /// The case the module exists to refuse. CPU sawtoothing between 20 and 80
    /// has a slope, and extrapolating it would invent a deadline.
    #[test]
    fn an_oscillating_metric_is_not_extrapolated() {
        let points = series(40, Duration::minutes(30), |i| {
            if i % 2 == 0 {
                20.0
            } else {
                80.0
            }
        });
        let trend = trend(&points, Some(100.0)).expect("a trend exists");
        assert!(trend.fit < MIN_FIT, "fit was {}", trend.fit);
        assert_eq!(
            trend.seconds_to_limit, None,
            "a sawtooth must not produce a date",
        );
    }

    /// Two readings a minute apart can be extrapolated to next year.
    #[test]
    fn too_few_readings_produce_nothing() {
        let points = series(3, Duration::hours(1), |i| i as f64);
        assert!(trend(&points, Some(100.0)).is_none());
    }

    /// Ten readings in ten seconds describe a moment, not a direction.
    #[test]
    fn a_window_that_is_too_short_produces_nothing() {
        let points = series(30, Duration::seconds(1), |i| i as f64);
        assert!(
            trend(&points, Some(1000.0)).is_none(),
            "30 seconds of history is not a capacity plan",
        );
    }

    /// A falling metric never reaches a limit above it. Absent, not infinite.
    #[test]
    fn a_falling_metric_has_no_time_to_a_limit_above_it() {
        let points = series(24, Duration::minutes(30), |i| 500.0 - i as f64 * 2.0);
        let trend = trend(&points, Some(900.0)).expect("a trend");
        assert!(trend.per_second < 0.0);
        assert_eq!(trend.seconds_to_limit, None);
    }

    /// And a falling metric heading toward a floor does reach it.
    #[test]
    fn a_falling_metric_reaches_a_target_below_it() {
        let points = series(24, Duration::minutes(30), |i| 500.0 - i as f64 * 2.0);
        let trend = trend(&points, None).expect("a trend");
        let seconds = trend.seconds_to_floor(454.0, 0.0).expect("heading to zero");
        assert!((seconds / 3600.0 - 113.5).abs() < 0.5, "{}", seconds / 3600.0);
    }

    /// A metric already past its limit is at zero, not at a negative time.
    #[test]
    fn a_metric_already_over_the_limit_reports_no_time_left() {
        let points = series(24, Duration::minutes(30), |i| 90.0 + i as f64 * 0.5);
        let trend = trend(&points, Some(95.0)).expect("a trend");
        assert_eq!(
            trend.seconds_to_limit,
            Some(0.0),
            "it is happening now, not in the past",
        );
    }

    /// A flat metric fits perfectly and goes nowhere. Both are true, and the
    /// second is what stops it being projected.
    #[test]
    fn a_flat_metric_has_a_perfect_fit_and_no_destination() {
        let points = series(24, Duration::minutes(30), |_| 42.0);
        let trend = trend(&points, Some(100.0)).expect("a trend");
        assert_eq!(trend.fit, 1.0);
        assert_eq!(trend.per_second, 0.0);
        assert_eq!(trend.seconds_to_limit, None);
    }

    /// No limit declared means nothing to count down to — but the direction is
    /// still worth knowing.
    #[test]
    fn a_metric_with_no_limit_still_has_a_direction() {
        let points = series(24, Duration::minutes(30), |i| 10.0 + i as f64);
        let trend = trend(&points, None).expect("a trend");
        assert!(trend.per_second > 0.0);
        assert_eq!(trend.seconds_to_limit, None);
    }

    /// Noise around a real climb should still be believed — the discrimination
    /// is against metrics with no trend, not against imperfect ones.
    #[test]
    fn a_noisy_but_real_climb_still_projects() {
        let points = series(40, Duration::minutes(30), |i| {
            let wobble = if i % 3 == 0 { 1.5 } else { -1.0 };
            50.0 + i as f64 * 0.8 + wobble
        });
        let trend = trend(&points, Some(120.0)).expect("a trend");
        assert!(trend.fit > MIN_FIT, "fit was {}", trend.fit);
        assert!(trend.seconds_to_limit.is_some());
    }

    /// The floor's own already-there case, so the mirror is not left untested.
    #[test]
    fn a_metric_already_below_a_floor_reports_no_time_left() {
        let points = series(24, Duration::minutes(30), |i| 50.0 - i as f64);
        let trend = trend(&points, None).expect("a trend");
        assert_eq!(trend.seconds_to_floor(10.0, 20.0), Some(0.0));
    }

    /// A rising metric never falls to a floor beneath it.
    #[test]
    fn a_rising_metric_has_no_time_to_a_floor_below_it() {
        let points = series(24, Duration::minutes(30), |i| 10.0 + i as f64);
        let trend = trend(&points, None).expect("a trend");
        assert_eq!(trend.seconds_to_floor(33.0, 0.0), None);
    }

    #[test]
    fn the_window_and_sample_count_travel_with_the_answer() {
        let points = series(24, Duration::minutes(30), |i| i as f64);
        let trend = trend(&points, None).expect("a trend");
        assert_eq!(trend.samples, 24);
        // 23 gaps of half an hour.
        assert!((trend.window_secs - 23.0 * 1800.0).abs() < 1.0);
    }
}
