//! Level meters in the state snapshot (`GetP Appl JaSt`).
//!
//! UC's handler for the state reply (`dspusbdevice.dll` 0x1800346c0) keeps a running maximum of
//! slots 0..=19 (levels, linear 0..1) and a running minimum of slots 20..=25 (gain reduction,
//! linear, 1.0 = none). Slot 44 is the flag word. The mic itself reports instantaneous values.
//!
//! Which slot belongs to which signal is *inferred* from a Revelator Dynamic capture (speaking
//! with nothing playing): slots 8 and 9 are equal (a mono mic sent to both sides), 14..=19 equal
//! them with the mixes at unity (Main, Stream Mix A, Stream Mix B, left/right each), slots 6 and
//! 7 move with the voice 3 dB apart, and 0..=5 and 12/13 stayed at 0 with no playback or reverb.
//! The playback / virtual / reverb slots are therefore a guess until checked with audio playing
//! (`revelator-cli meters all`).

use crate::device::State;

/// Mic input level (before the DSP chain; UC's first level slot).
pub const MIC_IN: &[usize] = &[6];
/// Mic after the DSP chain, as sent to the mixes.
pub const MIC_MIX: &[usize] = &[8, 9];
/// Playback, Virtual A, Virtual B returns (left, right) - inferred.
pub const PLAYBACK: &[usize] = &[0, 1];
pub const VIRTUAL_A: &[usize] = &[2, 3];
pub const VIRTUAL_B: &[usize] = &[4, 5];
/// Reverb return (left, right) - inferred.
pub const REVERB: &[usize] = &[12, 13];
/// Output buses (left, right).
pub const OUT_MAIN: &[usize] = &[14, 15];
pub const OUT_MIXA: &[usize] = &[16, 17];
pub const OUT_MIXB: &[usize] = &[18, 19];
/// Gain-reduction slots (linear, 1.0 = no reduction).
pub const REDUCTION: &[usize] = &[20, 21, 22, 23, 24, 25];

/// Lowest level reported, dBFS.
pub const FLOOR_DB: f32 = -96.0;
/// Largest gain reduction reported, dB.
pub const MAX_REDUCTION_DB: f32 = 40.0;

/// Largest finite, non-negative value among `slots` (`None` if the state has none of them).
pub fn peak(st: &State, slots: &[usize]) -> Option<f32> {
    slots
        .iter()
        .filter_map(|&i| st.slot(i))
        .filter(|v| v.is_finite() && *v >= 0.0)
        .reduce(f32::max)
}

/// Linear level to dBFS, floored at [`FLOOR_DB`].
pub fn level_db(lin: f32) -> f32 {
    if lin <= 0.0 || !lin.is_finite() {
        return FLOOR_DB;
    }
    (20.0 * lin.log10()).clamp(FLOOR_DB, 0.0)
}

/// Strongest gain reduction among [`REDUCTION`], in dB (>= 0).
pub fn reduction_db(st: &State) -> Option<f32> {
    let min = REDUCTION
        .iter()
        .filter_map(|&i| st.slot(i))
        .filter(|v| v.is_finite() && *v > 0.0)
        .reduce(f32::min)?;
    Some((-20.0 * min.min(1.0).log10()).clamp(0.0, MAX_REDUCTION_DB))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(set: &[(usize, f32)]) -> State {
        let mut v = vec![0.0; 53];
        for s in REDUCTION {
            v[*s] = 1.0;
        }
        for (i, x) in set {
            v[*i] = *x;
        }
        State::from_floats(v)
    }

    #[test]
    fn levels_in_db() {
        let st = state(&[(8, 0.175), (9, 0.175), (14, 0.5), (15, 0.25)]);
        assert!((level_db(peak(&st, MIC_MIX).unwrap()) + 15.14).abs() < 0.05);
        assert!((level_db(peak(&st, OUT_MAIN).unwrap()) + 6.02).abs() < 0.05);
        assert_eq!(level_db(0.0), FLOOR_DB);
        assert_eq!(level_db(f32::NAN), FLOOR_DB);
        assert_eq!(level_db(3.0), 0.0); // clamped at full scale
        assert_eq!(peak(&state(&[]), PLAYBACK), Some(0.0));
        assert_eq!(peak(&State::from_floats(vec![0.0; 3]), MIC_MIX), None);
    }

    #[test]
    fn reduction() {
        assert_eq!(reduction_db(&state(&[])), Some(0.0));
        let st = state(&[(20, 0.5), (21, 0.9)]);
        assert!((reduction_db(&st).unwrap() - 6.02).abs() < 0.05);
        assert_eq!(reduction_db(&state(&[(22, 1e-9)])), Some(MAX_REDUCTION_DB));
        // NaN / negative values are ignored
        assert_eq!(
            reduction_db(&state(&[(20, f32::NAN), (21, -1.0)])),
            Some(0.0)
        );
    }
}
