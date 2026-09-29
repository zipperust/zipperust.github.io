//! Playdate-ish sho instrument: 8 sawtooth voices + ADSR + amp LFO.
//!
//! Matches `Globals.lua` synth loop (not a GM bagpipe / recorded sample).

use js_sys::Math;
use wasm_bindgen::JsValue;
use web_sys::{AudioContext, GainNode, OscillatorNode, OscillatorType};

const VOICE_COUNT: usize = 8;
const ATTACK: f64 = 1.0;
const DECAY: f64 = 2.0;
const SUSTAIN: f64 = 0.8;
const RELEASE: f64 = 0.5;
const VOICE_VOL: f64 = 0.1;

/// One sustained MIDI note (Playdate `playMIDINote` with no length).
struct Voice {
    osc: OscillatorNode,
    /// ADSR envelope → voice bus → master.
    env: GainNode,
    /// Kept alive so the Web Audio graph is not GC'd.
    #[allow(dead_code)]
    lfo_osc: OscillatorNode,
    #[allow(dead_code)]
    voice_bus: GainNode,
    #[allow(dead_code)]
    lfo_depth: GainNode,
    active: bool,
    #[allow(dead_code)]
    note: u8,
    started_at: f64,
}

/// Shared sho instrument wired to the host master bus.
pub struct ShoInstrument {
    voices: Vec<Voice>,
    /// Frequency multiplier from port easter-egg pitch bend (`2^(semitones/12)`).
    bend_factor: f64,
}

impl ShoInstrument {
    pub fn new(ctx: &AudioContext, master: &GainNode) -> Result<Self, JsValue> {
        let mut voices = Vec::with_capacity(VOICE_COUNT);
        for _ in 0..VOICE_COUNT {
            voices.push(Voice::new(ctx, master)?);
        }
        Ok(Self {
            voices,
            bend_factor: 1.0,
        })
    }

    /// Apply global pitch bend (semitones). Updates active voice frequencies;
    /// future [`Self::note_on`] uses the same factor. `0` = authored pitch.
    pub fn set_pitch_bend_semitones(
        &mut self,
        ctx: &AudioContext,
        semitones: f32,
    ) -> Result<(), JsValue> {
        let factor = (2f64).powf(f64::from(semitones) / 12.0);
        if (factor - self.bend_factor).abs() < 1e-9 {
            return Ok(());
        }
        self.bend_factor = factor;
        let now = ctx.current_time();
        for v in &mut self.voices {
            if v.active {
                let hz = midi_to_hz(v.note) * self.bend_factor;
                v.osc.frequency().set_value_at_time(hz as f32, now)?;
            }
        }
        Ok(())
    }

    pub fn note_on(&mut self, ctx: &AudioContext, midi: u8) -> Result<(), JsValue> {
        let now = ctx.current_time();
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .or_else(|| {
                // Steal oldest active voice.
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.active)
                    .min_by(|(_, a), (_, b)| {
                        a.started_at
                            .partial_cmp(&b.started_at)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i)
            });
        let Some(i) = slot else {
            return Ok(());
        };
        if self.voices[i].active {
            self.voices[i].note_off(ctx)?;
        }
        self.voices[i].note_on(ctx, midi, now, self.bend_factor)?;
        Ok(())
    }

    /// Release the active voice playing `midi` (sequence note-off).
    pub fn note_off_midi(&mut self, ctx: &AudioContext, midi: u8) -> Result<(), JsValue> {
        for v in &mut self.voices {
            if v.active && v.note == midi {
                v.note_off(ctx)?;
                break;
            }
        }
        Ok(())
    }

    pub fn all_notes_off(&mut self, ctx: &AudioContext) -> Result<(), JsValue> {
        for v in &mut self.voices {
            if v.active {
                v.note_off(ctx)?;
            }
        }
        // Intro / victory cut — drop any leftover bend so the next chord is centered.
        self.bend_factor = 1.0;
        Ok(())
    }
}

impl Voice {
    fn new(ctx: &AudioContext, master: &GainNode) -> Result<Self, JsValue> {
        let osc = ctx.create_oscillator()?;
        osc.set_type(OscillatorType::Sawtooth);

        let env = ctx.create_gain()?;
        env.gain().set_value(0.0);

        // Amp LFO: sine → depth gain → env output? Playdate: synth amplitude mod.
        // Graph: osc → env → lfo_depth (static? ) actually:
        //   osc → env_gain → master
        //   lfo_osc → lfo_depth (scale) → env_gain.gain (AudioParam)
        // But Web Audio can't easily do center+depth on gain param without
        // ConstantSource. Approximate: osc → env → master, and multiply by a
        // second gain driven by LFO centered via offset ConstantSource.
        // Simpler parity approximation used here:
        //   osc → env → voice_bus → master
        //   lfo → (depth) → voice_bus.gain  with voice_bus starting at center.
        let voice_bus = ctx.create_gain()?;
        // Center 0.8 — LFO adds ±0.2 via audio-rate modulation of this gain.
        voice_bus.gain().set_value(0.8);

        let lfo_osc = ctx.create_oscillator()?;
        lfo_osc.set_type(OscillatorType::Sine);
        let rate = Math::random() * 0.6 + 0.4;
        lfo_osc.frequency().set_value(rate as f32);
        // Phase: delay start slightly using start(when) offset via currentTime + phase.
        let phase = Math::random() * 4.0;

        let lfo_depth = ctx.create_gain()?;
        lfo_depth.gain().set_value(0.2); // depth

        // Wire: osc → env → voice_bus → master
        osc.connect_with_audio_node(&env)?;
        env.connect_with_audio_node(&voice_bus)?;
        voice_bus.connect_with_audio_node(master)?;
        // LFO → depth → voice_bus.gain (audio-rate)
        lfo_osc.connect_with_audio_node(&lfo_depth)?;
        lfo_depth.connect_with_audio_param(&voice_bus.gain())?;

        // Start free-running; keep silent via env=0 until note_on.
        let t0 = ctx.current_time() + phase * 0.01;
        osc.start_with_when(t0)?;
        lfo_osc.start_with_when(t0)?;

        Ok(Self {
            osc,
            env,
            lfo_osc,
            voice_bus,
            lfo_depth,
            active: false,
            note: 0,
            started_at: 0.0,
        })
    }

    fn note_on(
        &mut self,
        ctx: &AudioContext,
        midi: u8,
        now: f64,
        bend_factor: f64,
    ) -> Result<(), JsValue> {
        let hz = midi_to_hz(midi) * bend_factor;
        self.osc.frequency().set_value(hz as f32);
        let g = self.env.gain();
        g.cancel_scheduled_values(now)?;
        g.set_value_at_time(0.0, now)?;
        // Attack → VOICE_VOL
        g.linear_ramp_to_value_at_time(VOICE_VOL as f32, now + ATTACK)?;
        // Decay → sustain * VOICE_VOL
        g.linear_ramp_to_value_at_time(
            (SUSTAIN * VOICE_VOL) as f32,
            now + ATTACK + DECAY,
        )?;
        self.active = true;
        self.note = midi;
        self.started_at = now;
        let _ = ctx; // reserved
        Ok(())
    }

    fn note_off(&mut self, ctx: &AudioContext) -> Result<(), JsValue> {
        let now = ctx.current_time();
        let g = self.env.gain();
        g.cancel_scheduled_values(now)?;
        let cur = g.value() as f64;
        g.set_value_at_time(cur as f32, now)?;
        g.linear_ramp_to_value_at_time(0.0, now + RELEASE)?;
        self.active = false;
        Ok(())
    }
}

/// Equal temperament — Playdate `playMIDINote` (A4=440). Not Globals `note()`.
pub fn midi_to_hz(n: u8) -> f64 {
    440.0 * (2f64).powf((n as f64 - 69.0) / 12.0)
}
