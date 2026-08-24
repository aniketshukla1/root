//! Statistics for ranking which metrics actually moved.
//!
//! Modelled on Netdata's Metric Correlations, which compares a highlighted
//! window against a baseline **four times its length, immediately preceding**,
//! and scores each metric with a two-sample Kolmogorov–Smirnov test. Netdata
//! keeps a "volume" heuristic alongside it, deliberately, because KS is blind
//! to the case that matters most in practice: a metric that was flat at zero
//! and then switched on.
//!
//! Both are implemented here for the same reason. The previous approach — a
//! flat "changed by more than 15%" threshold on first-vs-last sample — is the
//! volume heuristic with none of the baseline discipline, and it fires on any
//! noisy series while missing a variance blow-up that never shifts the mean.

/// How much longer the baseline is than the highlight window.
pub const BASELINE_MULTIPLIER: i64 = 4;

/// Fewest samples each side needs before KS says anything meaningful.
const MIN_SAMPLES: usize = 4;

/// Critical-value coefficient for α = 0.05, from the asymptotic KS distribution.
const KS_ALPHA_05: f64 = 1.358;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MovementMethod {
    /// Distributional change — catches variance and shape shifts.
    Ks2,
    /// Percentage change in means — catches on/off transitions that KS misses.
    Volume,
}

#[derive(Debug, Clone, Copy)]
pub struct Movement {
    /// 0.0–1.0. For KS2 this is the D statistic; for volume it is the
    /// normalised magnitude of the change. Comparable enough to sort by.
    pub score: f64,
    /// Whether the change clears the α = 0.05 critical value (KS2), or the
    /// volume floor.
    pub significant: bool,
    pub method: MovementMethod,
    pub baseline_mean: f64,
    pub highlight_mean: f64,
    pub change_pct: f64,
}

/// Score one metric's change between a baseline and a highlight window.
///
/// Returns `None` when there is not enough data on either side to say anything
/// — silence is the honest answer, and a lane that reports "no movement" is
/// more useful than one that reports noise.
pub fn score_movement(baseline: &[f64], highlight: &[f64]) -> Option<Movement> {
    if baseline.is_empty() || highlight.is_empty() {
        return None;
    }

    let baseline_mean = mean(baseline);
    let highlight_mean = mean(highlight);
    let change_pct = if baseline_mean.abs() > f64::EPSILON {
        (highlight_mean - baseline_mean) / baseline_mean.abs() * 100.0
    } else if highlight_mean.abs() > f64::EPSILON {
        // Off to on. A percentage against zero is meaningless, so report the
        // transition as a large-but-finite move rather than an infinity that
        // formats as "NaN%" three layers up.
        1000.0
    } else {
        0.0
    };

    // A metric that switched on or off is the clearest signal there is, and KS
    // on a constant-zero baseline is degenerate. Take the volume reading.
    let switched = baseline_mean.abs() < f64::EPSILON || highlight_mean.abs() < f64::EPSILON;
    if switched && baseline_mean != highlight_mean {
        return Some(Movement {
            score: 1.0,
            significant: true,
            method: MovementMethod::Volume,
            baseline_mean,
            highlight_mean,
            change_pct,
        });
    }

    if baseline.len() >= MIN_SAMPLES && highlight.len() >= MIN_SAMPLES {
        let d = ks2_statistic(baseline, highlight);
        let n = baseline.len() as f64;
        let m = highlight.len() as f64;
        let critical = KS_ALPHA_05 * ((n + m) / (n * m)).sqrt();
        return Some(Movement {
            score: d,
            significant: d > critical,
            method: MovementMethod::Ks2,
            baseline_mean,
            highlight_mean,
            change_pct,
        });
    }

    // Too few samples for KS — fall back to volume with a floor that keeps
    // ordinary jitter out of the lane.
    let magnitude = (change_pct.abs() / 100.0).min(1.0);
    Some(Movement {
        score: magnitude,
        significant: change_pct.abs() >= 15.0,
        method: MovementMethod::Volume,
        baseline_mean,
        highlight_mean,
        change_pct,
    })
}

/// Two-sample Kolmogorov–Smirnov D: the largest gap between the two empirical
/// CDFs.
///
/// Walks both sorted samples together rather than evaluating one CDF at every
/// point of the other, so it is O(n log n) for the sort and O(n + m) after.
pub fn ks2_statistic(a: &[f64], b: &[f64]) -> f64 {
    let mut x: Vec<f64> = a.to_vec();
    let mut y: Vec<f64> = b.to_vec();
    x.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
    y.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));

    let (n, m) = (x.len(), y.len());
    if n == 0 || m == 0 {
        return 0.0;
    }

    let (mut i, mut j) = (0usize, 0usize);
    let mut d: f64 = 0.0;

    while i < n && j < m {
        let value = x[i].min(y[j]);
        // Advance past every tie, so both CDFs step at the same value together
        // and the gap is measured after the step, not mid-step.
        while i < n && x[i] <= value {
            i += 1;
        }
        while j < m && y[j] <= value {
            j += 1;
        }
        let gap = (i as f64 / n as f64) - (j as f64 / m as f64);
        d = d.max(gap.abs());
    }

    d
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_samples_have_no_distance() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(ks2_statistic(&a, &a), 0.0);
    }

    #[test]
    fn fully_disjoint_samples_have_maximum_distance() {
        let a = [1.0, 2.0, 3.0];
        let b = [10.0, 11.0, 12.0];
        assert!((ks2_statistic(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ks_is_symmetric() {
        let a = [1.0, 3.0, 5.0, 7.0, 9.0];
        let b = [2.0, 3.0, 4.0, 8.0, 20.0];
        assert!((ks2_statistic(&a, &b) - ks2_statistic(&b, &a)).abs() < 1e-12);
    }

    #[test]
    fn catches_a_variance_change_that_leaves_the_mean_alone() {
        // The exact case a percentage-change threshold cannot see: same mean,
        // wildly different spread.
        let calm = [50.0, 50.0, 49.0, 51.0, 50.0, 50.0, 49.0, 51.0];
        let wild = [10.0, 90.0, 5.0, 95.0, 20.0, 80.0, 0.0, 100.0];

        let movement = score_movement(&calm, &wild).unwrap();
        assert_eq!(movement.method, MovementMethod::Ks2);
        assert!(movement.significant, "score {}", movement.score);
        assert!(
            movement.change_pct.abs() < 15.0,
            "the mean barely moved: {}",
            movement.change_pct
        );
    }

    #[test]
    fn steady_noise_is_not_significant() {
        let before = [10.0, 10.4, 9.8, 10.1, 9.9, 10.2, 10.0, 9.7];
        let after = [10.1, 9.9, 10.3, 10.0, 9.8, 10.2, 10.1, 9.9];
        let movement = score_movement(&before, &after).unwrap();
        assert!(!movement.significant, "score {}", movement.score);
    }

    #[test]
    fn a_metric_switching_on_is_always_significant() {
        let off = [0.0, 0.0, 0.0, 0.0];
        let on = [120.0, 340.0, 900.0, 450.0];
        let movement = score_movement(&off, &on).unwrap();
        assert_eq!(movement.method, MovementMethod::Volume);
        assert!(movement.significant);
        assert!(movement.change_pct.is_finite(), "must not be inf or NaN");
    }

    #[test]
    fn a_metric_switching_off_is_always_significant() {
        let on = [500.0, 480.0, 520.0, 495.0];
        let off = [0.0, 0.0, 0.0, 0.0];
        let movement = score_movement(&on, &off).unwrap();
        assert!(movement.significant);
        assert!((movement.change_pct + 100.0).abs() < 1e-9);
    }

    #[test]
    fn falls_back_to_volume_when_samples_are_scarce() {
        let before = [10.0, 10.0];
        let after = [40.0, 40.0];
        let movement = score_movement(&before, &after).unwrap();
        assert_eq!(movement.method, MovementMethod::Volume);
        assert!(movement.significant);
        assert!((movement.change_pct - 300.0).abs() < 1e-9);
    }

    #[test]
    fn empty_windows_report_nothing_rather_than_zero() {
        assert!(score_movement(&[], &[1.0]).is_none());
        assert!(score_movement(&[1.0], &[]).is_none());
    }

    #[test]
    fn a_sustained_level_shift_is_significant() {
        let before = [20.0, 21.0, 19.0, 20.5, 20.0, 19.5, 20.2, 20.1];
        let after = [80.0, 82.0, 79.0, 81.0, 80.5, 79.5, 80.2, 81.1];
        let movement = score_movement(&before, &after).unwrap();
        assert_eq!(movement.method, MovementMethod::Ks2);
        assert!(movement.significant);
        assert!((movement.score - 1.0).abs() < 1e-9, "disjoint ranges");
    }
}
