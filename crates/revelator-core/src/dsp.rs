//! Pure DSP helpers: biquad coefficients (RBJ cookbook), frequency-response evaluation for
//! drawing EQ curves, and static gain curves for compressor / gate / limiter.

use std::f64::consts::PI;

pub fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

pub fn lin_to_db(lin: f64) -> f64 {
    20.0 * lin.max(1e-12).log10()
}

/// Normalized biquad (a0 == 1): `y = b0 x + b1 x1 + b2 x2 - a1 y1 - a2 y2`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    pub const IDENTITY: Biquad = Biquad {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Peaking (bell) EQ.
    pub fn peaking(fs: f64, f0: f64, gain_db: f64, q: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(
            1.0 + al * a,
            -2.0 * c,
            1.0 - al * a,
            1.0 + al / a,
            -2.0 * c,
            1.0 - al / a,
        )
    }

    fn shelf_alpha(a: f64, s: f64, slope: f64) -> f64 {
        s / 2.0 * ((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt()
    }

    /// Low shelf (slope S = 1).
    pub fn low_shelf(fs: f64, f0: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let t = 2.0 * a.sqrt() * Self::shelf_alpha(a, s, 1.0);
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * c + t),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - t),
            (a + 1.0) + (a - 1.0) * c + t,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - t,
        )
    }

    /// High shelf (slope S = 1).
    pub fn high_shelf(fs: f64, f0: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let t = 2.0 * a.sqrt() * Self::shelf_alpha(a, s, 1.0);
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * c + t),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - t),
            (a + 1.0) - (a - 1.0) * c + t,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - t,
        )
    }

    /// Second-order high-pass with the given Q.
    pub fn highpass(fs: f64, f0: f64, q: f64) -> Self {
        let w = 2.0 * PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }

    /// First-order high-pass (bilinear transform), stored in biquad form.
    pub fn highpass_1st(fs: f64, f0: f64) -> Self {
        let k = (PI * f0 / fs).tan();
        let b0 = 1.0 / (1.0 + k);
        Self {
            b0,
            b1: -b0,
            b2: 0.0,
            a1: (k - 1.0) / (1.0 + k),
            a2: 0.0,
        }
    }

    /// Complex response H(e^{jw}) as (re, im).
    pub fn response(&self, fs: f64, freq: f64) -> (f64, f64) {
        let w = 2.0 * PI * freq / fs;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let den = dr * dr + di * di;
        ((nr * dr + ni * di) / den, (ni * dr - nr * di) / den)
    }

    /// Magnitude in dB at `freq`.
    pub fn magnitude_db(&self, fs: f64, freq: f64) -> f64 {
        let (re, im) = self.response(fs, freq);
        10.0 * (re * re + im * im).max(1e-24).log10()
    }
}

/// Butterworth high-pass cascade. `slope_db_oct` is rounded to a multiple of 6 dB/oct
/// (6, 12, 18, 24, ... up to 48).
pub fn highpass_cascade(fs: f64, f0: f64, slope_db_oct: u32) -> Vec<Biquad> {
    let order = (slope_db_oct / 6).clamp(1, 8) as usize;
    let mut v = Vec::new();
    for k in 0..order / 2 {
        let q = 1.0 / (2.0 * (PI * (2 * k + 1) as f64 / (2.0 * order as f64)).sin());
        v.push(Biquad::highpass(fs, f0, q));
    }
    if order % 2 == 1 {
        v.push(Biquad::highpass_1st(fs, f0));
    }
    v
}

/// EQ band shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqBand {
    pub kind: BandKind,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
}

impl EqBand {
    pub fn biquad(&self, fs: f64) -> Biquad {
        if !self.enabled {
            return Biquad::IDENTITY;
        }
        match self.kind {
            BandKind::Peak => Biquad::peaking(fs, self.freq, self.gain_db, self.q),
            BandKind::LowShelf => Biquad::low_shelf(fs, self.freq, self.gain_db),
            BandKind::HighShelf => Biquad::high_shelf(fs, self.freq, self.gain_db),
        }
    }
}

/// Combined magnitude response in dB of an optional HPF (freq, slope dB/oct) plus EQ bands,
/// evaluated at each of `freqs`.
pub fn eq_response_db(
    bands: &[EqBand],
    hpf: Option<(f64, u32)>,
    freqs: &[f64],
    fs: f64,
) -> Vec<f64> {
    let mut chain: Vec<Biquad> = bands.iter().map(|b| b.biquad(fs)).collect();
    if let Some((f, slope)) = hpf {
        chain.extend(highpass_cascade(fs, f, slope));
    }
    freqs
        .iter()
        .map(|&f| chain.iter().map(|b| b.magnitude_db(fs, f)).sum())
        .collect()
}

/// `n` log-spaced frequencies from `lo` to `hi` inclusive.
pub fn log_freqs(n: usize, lo: f64, hi: f64) -> Vec<f64> {
    if n < 2 {
        return vec![lo];
    }
    (0..n)
        .map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64))
        .collect()
}

/// Static compressor curve: output level (dB) for an input level (dB), with a soft knee of
/// `knee_db` width (0 = hard knee).
pub fn comp_out_db(in_db: f64, threshold: f64, ratio: f64, knee_db: f64) -> f64 {
    let slope = 1.0 / ratio.max(1.0) - 1.0;
    let d = in_db - threshold;
    if knee_db > 0.0 && d.abs() <= knee_db / 2.0 {
        let x = d + knee_db / 2.0;
        in_db + slope * x * x / (2.0 * knee_db)
    } else if d > 0.0 {
        in_db + slope * d
    } else {
        in_db
    }
}

/// Static gate / downward-expander curve. `range_db` is the (negative) maximum attenuation.
/// Hard gate: below threshold the signal is lowered by the full range. Expander: 2:1 below
/// threshold, limited to the range.
pub fn gate_out_db(in_db: f64, threshold: f64, range_db: f64, expander: bool) -> f64 {
    if in_db >= threshold {
        return in_db;
    }
    let max_atten = range_db.abs();
    let atten = if expander {
        (threshold - in_db).min(max_atten)
    } else {
        max_atten
    };
    in_db - atten
}

/// Brick-wall limiter curve.
pub fn limiter_out_db(in_db: f64, threshold: f64) -> f64 {
    in_db.min(threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f64 = 48000.0;

    #[test]
    fn peaking_gain_at_center() {
        for g in [-12.0, 6.0, 15.0] {
            let b = Biquad::peaking(FS, 1000.0, g, 1.5);
            assert!((b.magnitude_db(FS, 1000.0) - g).abs() < 1e-6);
            assert!(b.magnitude_db(FS, 20.0).abs() < 0.2);
        }
        let flat = Biquad::peaking(FS, 1000.0, 0.0, 1.0);
        assert!(flat.magnitude_db(FS, 333.0).abs() < 1e-9);
    }

    #[test]
    fn shelves_reach_asymptotes() {
        let l = Biquad::low_shelf(FS, 200.0, 9.0);
        assert!((l.magnitude_db(FS, 20.0) - 9.0).abs() < 0.2);
        assert!((l.magnitude_db(FS, 200.0) - 4.5).abs() < 0.05); // half gain at f0
        assert!(l.magnitude_db(FS, 15000.0).abs() < 0.2);
        let h = Biquad::high_shelf(FS, 5000.0, -6.0);
        assert!((h.magnitude_db(FS, 20000.0) + 6.0).abs() < 0.5);
        assert!(h.magnitude_db(FS, 50.0).abs() < 0.1);
    }

    #[test]
    fn hpf_is_minus_3db_at_cutoff() {
        for slope in [6, 12, 18, 24] {
            let c = highpass_cascade(FS, 100.0, slope);
            let m: f64 = c.iter().map(|b| b.magnitude_db(FS, 100.0)).sum();
            assert!((m + 3.0103).abs() < 0.05, "slope {slope}: {m}");
            let low: f64 = c.iter().map(|b| b.magnitude_db(FS, 25.0)).sum();
            // two octaves below: about -slope*2 dB
            assert!(low < -(slope as f64) * 1.6, "slope {slope}: {low}");
            let hi: f64 = c.iter().map(|b| b.magnitude_db(FS, 10000.0)).sum();
            assert!(hi.abs() < 0.05);
        }
    }

    #[test]
    fn eq_curve_sums_bands() {
        let bands = [
            EqBand {
                kind: BandKind::Peak,
                freq: 1000.0,
                gain_db: 6.0,
                q: 1.0,
                enabled: true,
            },
            EqBand {
                kind: BandKind::Peak,
                freq: 1000.0,
                gain_db: 6.0,
                q: 1.0,
                enabled: false,
            },
        ];
        let r = eq_response_db(&bands, None, &[1000.0], FS);
        assert!((r[0] - 6.0).abs() < 1e-6);
        let f = log_freqs(5, 20.0, 20000.0);
        assert_eq!(f.len(), 5);
        assert!((f[4] - 20000.0).abs() < 1e-6);
        assert!((f[2] - 632.455).abs() < 0.01);
    }

    #[test]
    fn compressor_curve() {
        assert_eq!(comp_out_db(-30.0, -20.0, 4.0, 0.0), -30.0);
        assert!((comp_out_db(0.0, -20.0, 4.0, 0.0) - -15.0).abs() < 1e-9);
        // soft knee is continuous at its edges
        let (t, k) = (-20.0, 10.0);
        assert!((comp_out_db(t - k / 2.0, t, 4.0, k) - (t - k / 2.0)).abs() < 1e-9);
        let hard = comp_out_db(t + k / 2.0, t, 4.0, 0.0);
        assert!((comp_out_db(t + k / 2.0, t, 4.0, k) - hard).abs() < 1e-9);
    }

    #[test]
    fn gate_and_limiter() {
        assert_eq!(gate_out_db(-10.0, -40.0, -30.0, false), -10.0);
        assert_eq!(gate_out_db(-50.0, -40.0, -30.0, false), -80.0);
        assert_eq!(gate_out_db(-45.0, -40.0, -30.0, true), -50.0);
        assert_eq!(gate_out_db(-90.0, -40.0, -30.0, true), -120.0);
        assert_eq!(limiter_out_db(3.0, -1.0), -1.0);
        assert!((lin_to_db(db_to_lin(-12.0)) + 12.0).abs() < 1e-9);
    }
}
