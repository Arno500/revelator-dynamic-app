//! Short plain-language explanations shown as tooltips, keyed by GUI parameter id.

pub fn tip(id: &str) -> Option<&'static str> {
    Some(match id {
        // ---- microphone
        "mic.gain" => "Preamp gain: how much the mic signal is amplified before anything else. Raise it until your normal speaking level peaks around -12 dB on the meter.",
        "mic.hp_volume" => "Volume of the headphone output on the mic.",
        "mic.monitor_blend" => "What you hear in the headphones: 0 % is only your own mic (zero latency), 100 % is only the computer's playback. In between mixes both.",
        "mic.preset_button_mode" => "How the mic's preset button behaves: Single steps through slots one by one, Double needs a double press, Multi picks the slot by how many times you press.",
        "mic.mute_button_mode" => "What the mic's mute button does: mutes the headphones, or mutes the microphone.",
        "mic.mute" => "Silences the microphone.",

        // ---- high-pass
        "dsp.hpf.on" => "Turns the high-pass filter on. It removes low rumble, desk thumps and breath pops from your voice.",
        "dsp.hpf.freq" => "Cut-off frequency: everything below it is removed. 80-120 Hz suits most voices; raise it for more rumble reduction (a higher value also thins the voice).",

        // ---- gate / expander
        "dsp.gate.on" => "Turns the gate on. A gate lowers the signal when you are not speaking, so keyboard noise, fans and room sound disappear between phrases.",
        "dsp.gate.expander" => "Expander mode: instead of cutting the sound completely below the threshold, it lowers it gradually. More natural, less chopped-off words.",
        "dsp.gate.threshold" => "Level below which the gate closes. Set it just above your background noise but below your quietest speech.",
        "dsp.gate.range" => "How far the gate lowers the sound when closed. -80 dB is near silence; -20 dB only reduces the noise a little.",
        "dsp.gate.attack" => "How fast the gate opens when you start speaking. Short (1-3 ms) keeps the first syllable.",
        "dsp.gate.release" => "How long the gate takes to close after you stop. Longer (200+ ms) sounds smoother and keeps word endings and breaths.",
        "dsp.gate.keyfilter" => "Key filter: the gate decides when to open by listening only above this frequency. Raise it so low thumps and rumble do not open the gate. 40 Hz or less means off.",
        "dsp.gate.keylisten" => "Lets you hear the filtered signal the gate listens to, to tune the key filter. Switch off again afterwards: it changes what you send.",

        // ---- compressor (all models)
        "dsp.comp.on" => "Turns the compressor on. A compressor evens out your voice: loud parts are turned down so quiet parts and loud parts sit closer together.",
        "dsp.comp.model" => "Compressor flavour. Standard: clean and precise. Tube: smooth and warm. FET: fast and punchy, the classic 'radio' sound.",
        "dsp.comp.keyfilter" => "Key filter: the compressor reacts only to sound above this frequency, so bass thumps do not make it squash your voice. 40 Hz or less means off.",
        "dsp.comp.keylisten" => "Lets you hear the filtered signal the compressor listens to, to tune the key filter. Switch off again afterwards.",
        // standard
        "dsp.comp.threshold" => "Level above which compression starts. Lower means more of your speech gets compressed.",
        "dsp.comp.ratio" => "How strongly levels above the threshold are reduced. 2:1 is gentle, 4:1 is firm, 10:1 and more is limiting.",
        "dsp.comp.attack" => "How fast the compressor reacts to a loud sound. Slower lets the start of words through (more punch); faster is smoother and safer.",
        "dsp.comp.release" => "How fast the compressor lets go after a loud sound. Too short can sound pumping, too long can dull the next words.",
        "dsp.comp.gain" => "Makeup gain: boosts the signal after compression to bring the volume back up.",
        "dsp.comp.softknee" => "Soft knee: compression eases in around the threshold instead of switching on abruptly. Sounds more natural.",
        "dsp.comp.auto" => "Automatic attack and release: the compressor adapts its timing to your voice. Overrides the Attack and Release settings.",
        // tube
        "dsp.comp.tube.mode" => "Compress: normal gentle compression. Limit: much firmer, catches peaks like a limiter.",
        "dsp.comp.tube.gain" => "Output gain of the tube compressor. Raise it to make up for the volume lost by compression.",
        "dsp.comp.tube.peak" => "Peak reduction: how much compression is applied. More means a more compressed, denser sound.",
        // FET
        "dsp.comp.fet.input" => "Input drive: how hard the signal hits the compressor. Higher means more compression and more character.",
        "dsp.comp.fet.output" => "Output level after compression. Lower it if the drive makes the voice too loud.",
        "dsp.comp.fet.attack" => "How fast the compressor reacts. Very fast values give a tight, punchy sound; slower lets more transient through.",
        "dsp.comp.fet.release" => "How fast the compressor lets go. Shorter feels more aggressive, longer feels smoother.",
        "dsp.comp.fet.ratio" => "Compression ratio: 4:1 is moderate, 20:1 is heavy. 'All buttons' is the extreme, aggressive setting of the classic hardware.",

        // ---- equalizer
        "dsp.eq.on" => "Turns the equalizer on. An EQ makes some frequencies louder or quieter to shape the tone of your voice.",
        "dsp.eq.model" => "EQ flavour. Parametric: four fully adjustable bands. Passive and Vintage: simpler, with fixed frequencies and a classic analog character.",
        "dsp.eq.swap" => "Changes the order of the chain: normally the compressor comes first and the EQ after it. Swapping puts the EQ before the compressor.",
        "dsp.eq.low.on" | "dsp.eq.lowmid.on" | "dsp.eq.himid.on" | "dsp.eq.high.on" => "Turns this EQ band on or off.",
        "dsp.eq.low.shelf" | "dsp.eq.high.shelf" => "Peak: the band bumps or dips around its frequency. Shelf: it raises or lowers everything below (low band) or above (high band) its frequency.",
        "dsp.eq.passive.lowfreq" => "Frequency of the low boost and cut.",
        "dsp.eq.passive.lowboost" => "Boosts the low end around the chosen frequency. Adds body and warmth.",
        "dsp.eq.passive.lowatten" => "Cuts the low end around the chosen frequency. Reduces boominess. Boost and cut together give the classic 'tight but full' bass.",
        "dsp.eq.passive.highfreq" => "Frequency of the high boost.",
        "dsp.eq.passive.highboost" => "Boosts the highs around the chosen frequency. Adds presence and air.",
        "dsp.eq.passive.bandwidth" => "How wide the high boost is. Low is a narrow peak, high is a broad lift.",
        "dsp.eq.passive.attenfreq" => "Frequency above which the highs are cut.",
        "dsp.eq.passive.highatten" => "How much the highs are cut. Tames harshness and hiss.",
        "dsp.eq.vintage.lowfreq" => "Frequency of the low band.",
        "dsp.eq.vintage.lowgain" => "Boost or cut of the low band. Controls body and warmth.",
        "dsp.eq.vintage.lowmidfreq" => "Frequency of the low-mid band.",
        "dsp.eq.vintage.lowmidgain" => "Boost or cut of the low-mid band. Cutting here removes a boxy, muddy sound.",
        "dsp.eq.vintage.himidfreq" => "Frequency of the high-mid band.",
        "dsp.eq.vintage.himidgain" => "Boost or cut of the high-mid band. Boosting adds presence and clarity.",
        "dsp.eq.vintage.highgain" => "Boost or cut of the high band. Boosting adds air; cutting softens harshness.",

        // ---- limiter
        "dsp.limiter.on" => "Turns the limiter on. A limiter is a safety net: it stops the signal from ever going louder than the threshold, to avoid clipping.",
        "dsp.limiter.threshold" => "Maximum level allowed. -3 to -1 dB leaves a little headroom.",

        // ---- reverb
        "dsp.reverb.on" => "Turns the reverb effect on. It adds the sound of a room around your voice.",
        "dsp.reverb.size" => "Size of the simulated room: small is a tight room, large is a hall with a long tail.",
        "dsp.reverb.mix" => "Balance between your dry voice (0) and the reverb sound (1).",
        "dsp.reverb.hpf" => "Removes low frequencies from the reverb so it does not sound muddy. 0 means off.",
        "dsp.reverb.predelay" => "Delay before the reverb starts. A longer pre-delay keeps your voice clear in front of the reverb.",
        "mixer.mic.bypass" => "Turns the whole Fat Channel (gate, compressor, EQ, limiter) off without losing its settings. Universal Control's Bypass.",
        "mixer.mic.dspamount" => "How much of the Fat Channel processing is applied, 1 = full (Universal Control's DSP Amount, sent as 1 by default).",
        "mixer.mic.dawpost" => "Revelator manual, Settings: on = USB recordings are processed by the Fat Channel as you hear them; off = raw mic on USB while the headphones stay processed (UC's \"DAW Send Post DSP\").",
        "mixer.mic.reverb" => "How much of your mic is sent into the reverb. At the minimum you hear no reverb. You hear it through the Reverb strip in the Mixer tab, which must be raised too.",

        // ---- voice FX
        "dsp.voicefx.on" => "Turns the voice effect on.",
        "dsp.voicefx.model" => "Which voice effect is used.",
        "dsp.voicefx.mix" => "Balance between your normal voice (0) and the effect (1).",
        "dsp.voicefx.doubler.lows" => "How much of the low frequencies go into the doubled voice.",
        "dsp.voicefx.doubler.width" => "How far apart the doubled voices are spread, for a wider, thicker sound.",
        "dsp.voicefx.detune.amount" => "How far the copy is detuned from your voice. Small amounts thicken the voice; large amounts sound out of tune.",
        "dsp.voicefx.vocoder.volume" => "Volume of the vocoder effect.",
        "dsp.voicefx.vocoder.carrier" => "Which synthetic tone shapes the robotic voice: each carrier has a different character.",
        "dsp.voicefx.vocoder.carrierfreq" => "Pitch of the vocoder tone. Lower is deeper, higher is more squeaky.",
        "dsp.voicefx.ringmod.freq" => "Frequency of the ring modulator. Low values give a tremolo-like wobble, high values a metallic, robotic sound.",
        "dsp.voicefx.ringmod.sub" => "Adds a second carrier to the ring modulator for a richer, more complex sound.",
        "dsp.voicefx.ringmod.subfreq" => "Frequency of the second carrier.",
        "dsp.voicefx.ringmod.dist" => "Adds distortion to the effect for a harsher sound.",
        "dsp.voicefx.ringmod.volume" => "Volume of the ring modulator effect.",
        "dsp.voicefx.robot.pitch" => "Pitch of the robot voice.",
        "dsp.voicefx.robot.regen" => "How much of the effect is fed back into itself. More gives a longer, more metallic ring.",
        "dsp.voicefx.robot.damping" => "Softens the highs of the effect. More damping sounds duller and warmer.",
        "dsp.voicefx.robot.dist" => "Adds distortion to the effect.",
        "dsp.voicefx.robot.volume" => "Volume of the robot effect.",
        "dsp.voicefx.echo.time" => "Time between your voice and its echo.",
        "dsp.voicefx.echo.feedback" => "How many times the echo repeats. Higher means a longer trail of repeats.",
        _ => return None,
    })
}

/// Tooltip for the Freq / Gain / Q boxes of a parametric EQ band.
pub fn eq_field_tip(field: &str) -> &'static str {
    match field {
        "freq" => "Centre frequency of this band: the part of the sound it affects.",
        "gain" => "Boost (+) or cut (-) applied at the band's frequency.",
        _ => "Q: how narrow the band is. Low is a broad, gentle change, high is a narrow, precise one.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::all_specs;

    #[test]
    fn every_dsp_control_has_a_tip() {
        let missing: Vec<_> = all_specs()
            .filter(|s| s.id.starts_with("dsp."))
            .filter(|s| tip(&s.id).is_none())
            .map(|s| s.id.clone())
            .collect();
        // Per-band EQ frequency/gain/Q have their own shared tips (see `eq_field_tip`).
        let missing: Vec<_> = missing
            .into_iter()
            .filter(|id| {
                !(id.starts_with("dsp.eq.")
                    && (id.ends_with(".freq") || id.ends_with(".gain") || id.ends_with(".q"))
                    && !id.contains("passive")
                    && !id.contains("vintage"))
            })
            .collect();
        assert!(missing.is_empty(), "no tooltip for: {missing:?}");
    }
}
