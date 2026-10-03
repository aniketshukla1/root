//! Deterministic randomness, and the shapes the stories are drawn with.
//!
//! Nothing in the simulation keeps state between polls. Every number is a
//! function of the seed, a name and a moment, so the same moment always
//! produces the same reading — which is what lets two hours of history be
//! drawn at startup and then continue seamlessly into the live polls, and lets
//! a restart redraw exactly what was on screen before it.

/// splitmix64's finaliser: a cheap, well-mixed 64-bit hash step.
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Several values folded into one hash.
pub fn hash(parts: &[u64]) -> u64 {
    parts
        .iter()
        .fold(0x005E_ED0F_A0C1_u64, |acc, part| mix(acc ^ part.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
}

/// A stable number for a name, so a series can be keyed by what it measures.
pub fn key(name: &str) -> u64 {
    // FNV-1a: stable across builds and platforms, unlike `DefaultHasher`.
    name.bytes().fold(0xCBF2_9CE4_8422_2325_u64, |acc, b| {
        (acc ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

/// Uniform in [0, 1).
pub fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Uniform in [-1, 1).
pub fn signed(h: u64) -> f64 {
    unit(h) * 2.0 - 1.0
}

pub fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Value noise in [-1, 1]: random at each multiple of `period`, eased in
/// between. Smooth enough to read as a real metric wandering rather than as
/// static, which is what white noise on a chart looks like.
pub fn smooth(seed: u64, name: u64, secs: f64, period: f64) -> f64 {
    let x = secs / period;
    let i = x.floor();
    let f = x - i;
    let a = signed(hash(&[seed, name, i as i64 as u64]));
    let b = signed(hash(&[seed, name, (i + 1.0) as i64 as u64]));
    a + (b - a) * smoothstep(f)
}

/// A slow drift with a faster tremor on top — the texture of most gauges.
pub fn wander(seed: u64, name: u64, secs: f64) -> f64 {
    0.7 * smooth(seed, name, secs, 540.0) + 0.3 * smooth(seed, name ^ 0xA5A5, secs, 40.0)
}

/// One sample's worth of jitter, independent per poll.
pub fn jitter(seed: u64, name: u64, secs: f64) -> f64 {
    signed(hash(&[seed, name ^ 0x7177, secs.floor() as i64 as u64]))
}

/// 0 before `start`, easing up to 1 over `rise` seconds, held until `end`,
/// easing back to 0 over `fall`. Every story effect is one of these.
pub fn envelope(tau: f64, start: f64, rise: f64, end: f64, fall: f64) -> f64 {
    if tau < start {
        0.0
    } else if tau < start + rise {
        smoothstep((tau - start) / rise.max(1e-6))
    } else if tau < end {
        1.0
    } else if tau < end + fall {
        1.0 - smoothstep((tau - end) / fall.max(1e-6))
    } else {
        0.0
    }
}

/// Inside `[from, to)`.
pub fn within(tau: f64, from: f64, to: f64) -> bool {
    tau >= from && tau < to
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_moment_gives_the_same_number() {
        assert_eq!(smooth(7, key("cpu"), 1234.5, 60.0), smooth(7, key("cpu"), 1234.5, 60.0));
        assert_ne!(smooth(7, key("cpu"), 1234.5, 60.0), smooth(8, key("cpu"), 1234.5, 60.0));
    }

    #[test]
    fn noise_stays_in_range_and_moves_smoothly() {
        let mut previous = smooth(1, 2, 0.0, 30.0);
        for step in 1..2000 {
            let value = smooth(1, 2, step as f64, 30.0);
            assert!((-1.0..=1.0).contains(&value));
            // One second of a 30-second period cannot jump far.
            assert!((value - previous).abs() < 0.2, "jumped at {step}");
            previous = value;
        }
    }

    #[test]
    fn an_envelope_rises_holds_and_falls() {
        assert_eq!(envelope(5.0, 10.0, 10.0, 30.0, 10.0), 0.0);
        assert!((envelope(15.0, 10.0, 10.0, 30.0, 10.0) - 0.5).abs() < 1e-9);
        assert_eq!(envelope(25.0, 10.0, 10.0, 30.0, 10.0), 1.0);
        assert!((envelope(35.0, 10.0, 10.0, 30.0, 10.0) - 0.5).abs() < 1e-9);
        assert_eq!(envelope(45.0, 10.0, 10.0, 30.0, 10.0), 0.0);
    }
}
