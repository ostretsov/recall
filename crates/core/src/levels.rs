//! Audio level meters.

/// Lowest level shown on the meter; anything below is drawn as silence.
pub const FLOOR_DB: f64 = -60.0;

/// RMS level of S16 samples in dBFS (-inf for silence or no samples).
pub fn rms_db(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    10.0 * (sum / samples.len() as f64).log10()
}

/// Per-channel RMS levels of interleaved S16 samples.
pub fn channel_rms_db(interleaved: &[i16], channels: usize) -> Vec<f64> {
    (0..channels)
        .map(|c| {
            let ch: Vec<i16> = interleaved
                .iter()
                .skip(c)
                .step_by(channels)
                .copied()
                .collect();
            rms_db(&ch)
        })
        .collect()
}

/// Position on the meter, 0 (at or below the floor) to 1 (0 dBFS), linear in dB.
pub fn fraction(db: f64) -> f64 {
    if db.is_nan() {
        return 0.0;
    }
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_values() {
        assert_eq!(rms_db(&[]), f64::NEG_INFINITY);
        assert_eq!(rms_db(&[0, 0]), f64::NEG_INFINITY);
        // Full-scale square wave: 0 dBFS.
        assert!(rms_db(&[-32768, -32768]).abs() < 1e-9);
        // Half amplitude: about -6 dB.
        assert!((rms_db(&[16384, -16384]) + 6.02).abs() < 0.01);
    }

    #[test]
    fn channels() {
        let v = channel_rms_db(&[16384, 0, -16384, 0], 2);
        assert!((v[0] + 6.02).abs() < 0.01);
        assert_eq!(v[1], f64::NEG_INFINITY);
    }

    #[test]
    fn scale_mapping() {
        assert_eq!(fraction(0.0), 1.0);
        assert_eq!(fraction(-30.0), 0.5);
        assert_eq!(fraction(-60.0), 0.0);
        assert_eq!(fraction(f64::NEG_INFINITY), 0.0);
        assert_eq!(fraction(f64::NAN), 0.0);
        assert_eq!(fraction(3.0), 1.0);
    }
}
