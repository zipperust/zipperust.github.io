//! Browser host: owns the game, blits the 1-bit framebuffer, accepts input,
//! and plays one-shot SFX through Web Audio.

mod sho_synth;

use std::collections::HashMap;

use wasm_bindgen::prelude::*;
use wasm_bindgen::Clamped;
use web_sys::AudioBuffer;
use web_sys::AudioContext;
use web_sys::AudioContextState;
use web_sys::GainNode;
use web_sys::CanvasRenderingContext2d;
use web_sys::ImageData;

use sho_synth::ShoInstrument;
use zipper_core::{
    decode_pda, decode_pdi, decode_pdt, decode_pft, introchord_from_json, port_version, Buttons,
    Framebuffer, Game, GameState, Input, PcmSample, SfxId, SynthEvent, DEFAULT_RANDOM_SEED,
    FRAMEBUFFER_BYTES, SCREEN_HEIGHT, SCREEN_WIDTH,
};

/// Expand packed 1-bit framebuffer into RGBA8888 for canvas ImageData.
/// When `inverted`, swap the light/dark palette (`playdate.display.setInverted`).
fn expand_1bit_to_rgba(fb: &Framebuffer, rgba: &mut [u8], inverted: bool) {
    debug_assert_eq!(rgba.len(), FRAMEBUFFER_BYTES * 8 * 4);
    let mut out = 0usize;
    for &byte in fb.pixels.iter() {
        for bit in (0..8).rev() {
            let black = (byte >> bit) & 1 == 1;
            let dark = if inverted { !black } else { black };
            if dark {
                // Ink: RGB(49, 47, 42)
                rgba[out] = 49;
                rgba[out + 1] = 47;
                rgba[out + 2] = 42;
            } else {
                // Paper: RGB(215, 212, 205)
                rgba[out] = 215;
                rgba[out + 1] = 212;
                rgba[out + 2] = 205;
            }
            rgba[out + 3] = 0xff;
            out += 4;
        }
    }
}

fn sfx_key(id: SfxId) -> &'static str {
    match id {
        SfxId::Select => "select",
        SfxId::Buzz => "buzz",
        SfxId::Slash => "slash",
        SfxId::Step => "step",
        SfxId::Swoosh => "swoosh",
        SfxId::Falldead => "falldead",
        SfxId::Falldead2 => "falldead2",
        SfxId::Falldead3 => "falldead3",
        SfxId::Falldead4 => "falldead4",
        SfxId::PlayerDeath => "playerdeath",
        SfxId::DeathMusic => "deathmusic",
        SfxId::Blood => "blood",
        SfxId::LifeDown => "lifedown",
        SfxId::LifeUp => "lifeup",
        SfxId::Clunk => "clunk",
        SfxId::Key => "key",
        SfxId::Click => "click",
        SfxId::Zzt => "zzt",
        SfxId::Warning => "warning",
        SfxId::Parry => "parry",
        SfxId::Shuriken => "shuriken",
        SfxId::SpiritDispel => "spiritdispel",
        SfxId::SpiritRevive => "spiritrevive",
        SfxId::Transition => "transition",
        SfxId::Transition2 => "transition2",
    }
}

#[wasm_bindgen]
pub struct ZipperApp {
    game: Game,
    rgba: Vec<u8>,
    buttons: Buttons,
    crank_degrees: f32,
    crank_delta: f32,
    crank_docked: bool,
    audio: Option<AudioContext>,
    /// Master SFX bus (`GainNode` → destination). Created with the context.
    master_gain: Option<GainNode>,
    /// Linear gain 0.0..=1.0 applied to every one-shot (host menu slider).
    volume: f32,
    /// Decoded PCM waiting for an `AudioContext` (autoplay policy: create ctx on gesture).
    pending_sfx: HashMap<String, PcmSample>,
    /// Decoded Web Audio buffers keyed by `soundm` name (`select`, `buzz`, …).
    sfx: HashMap<String, AudioBuffer>,
    /// One-shots that fired while the context was suspended (iOS background / lock).
    /// Replayed once [`ensure_audio`] brings the context back to `running`.
    held_sfx: Vec<SfxId>,
    /// Softsynth events held until AudioContext is running (introchord).
    held_synth: Vec<SynthEvent>,
    /// Playdate-ish sho instrument (created with the AudioContext).
    sho: Option<ShoInstrument>,
}

/// Default host SFX volume slider position (60% → perceptual mid via [`slider_to_gain`]).
const DEFAULT_SFX_VOLUME: f32 = 0.6;

/// Map a UI slider position `0.0..=1.0` to a linear amplitude for `GainNode`.
///
/// Human loudness is roughly logarithmic: a naive `gain = slider` wastes the
/// top half of the control (60% still sounds almost full). We use a −40 dB…0 dB
/// curve so mid positions feel useful; 0 stays hard mute.
fn slider_to_gain(slider: f32) -> f32 {
    let t = slider.clamp(0.0, 1.0);
    if t <= 0.0 {
        return 0.0;
    }
    // amplitude = 10^(dB/20); dB = -40 * (1 - t) → t=1 → 0 dB, t→0 → −40 dB.
    10f32.powf(-40.0 * (1.0 - t) / 20.0)
}

/// Accept `{ introchord: number[] }`, bare `number[]`, or a JSON string array.
fn parse_introchord_js(config: &JsValue) -> Result<Vec<u8>, JsValue> {
    if let Some(s) = config.as_string() {
        return introchord_from_json(&s).map_err(|e| JsValue::from_str(&e));
    }
    let arr = if js_sys::Array::is_array(config) {
        js_sys::Array::from(config)
    } else {
        let key = JsValue::from_str("introchord");
        let v = js_sys::Reflect::get(config, &key)?;
        if v.is_undefined() || v.is_null() {
            return Err(JsValue::from_str("loadIntroMusic: missing introchord"));
        }
        if let Some(s) = v.as_string() {
            return introchord_from_json(&s).map_err(|e| JsValue::from_str(&e));
        }
        js_sys::Array::from(&v)
    };
    let mut notes = Vec::with_capacity(arr.length() as usize);
    for i in 0..arr.length() {
        let n = arr
            .get(i)
            .as_f64()
            .ok_or_else(|| JsValue::from_str("loadIntroMusic: note must be number"))?;
        if !(0.0..=127.0).contains(&n) {
            return Err(JsValue::from_str("loadIntroMusic: note out of range"));
        }
        notes.push(n as u8);
    }
    if notes.is_empty() {
        return Err(JsValue::from_str("loadIntroMusic: empty introchord"));
    }
    Ok(notes)
}

#[wasm_bindgen]
impl ZipperApp {
    /// Create the app.
    ///
    /// `seed` mirrors Lua `random_seed`: pass Playdate epoch seconds
    /// (`Date.now()/1000 - 946684800`) for a fresh run, or omit for the
    /// deterministic test seed. Death restart keeps this seed (Lua-faithful).
    #[wasm_bindgen(constructor)]
    pub fn new(seed: Option<u32>) -> Result<ZipperApp, JsValue> {
        console_error_panic_hook::set_once();
        // Prove which wasm the browser loaded (no on-canvas stamp).
        web_sys::console::info_1(&JsValue::from_str(&format!(
            "zipper {}",
            port_version()
        )));
        let random_seed = seed.unwrap_or(DEFAULT_RANDOM_SEED);
        Ok(ZipperApp {
            game: Game::new_with_seed(random_seed),
            rgba: vec![0; (SCREEN_WIDTH * SCREEN_HEIGHT * 4) as usize],
            buttons: Buttons::default(),
            crank_degrees: 0.0,
            crank_delta: 0.0,
            crank_docked: true,
            audio: None,
            master_gain: None,
            volume: DEFAULT_SFX_VOLUME,
            pending_sfx: HashMap::new(),
            sfx: HashMap::new(),
            held_sfx: Vec::new(),
            held_synth: Vec::new(),
            sho: None,
        })
    }

    /// Master SFX volume **slider** position in `0.0..=1.0` (host menu). Default `0.6`.
    /// Applied to the bus as a perceptual (−40 dB…0 dB) gain, not 1:1 amplitude.
    #[wasm_bindgen(js_name = setVolume)]
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(gain) = &self.master_gain {
            gain.gain().set_value(slider_to_gain(self.volume));
        }
    }

    /// Current master SFX **slider** position (`0.0..=1.0`), not linear amplitude.
    #[wasm_bindgen]
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Play a loaded one-shot by bank name (`select`, `buzz`, …).
    /// Used for host-menu volume preview (aiming slide = `select`).
    #[wasm_bindgen(js_name = playSfx)]
    pub fn play_sfx(&mut self, name: &str) -> Result<(), JsValue> {
        let _ = self.ensure_audio();
        if !self.audio_is_running() {
            return Ok(());
        }
        let Some(ctx) = self.audio.as_ref() else {
            return Ok(());
        };
        let Some(gain) = self.master_gain.as_ref() else {
            return Ok(());
        };
        let Some(buffer) = self.sfx.get(name) else {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "sfx missing from bank: {name}"
            )));
            return Ok(());
        };
        let src = ctx.create_buffer_source()?;
        src.set_buffer(Some(buffer));
        src.connect_with_audio_node(gain)?;
        src.start()?;
        Ok(())
    }

    /// Current run seed (`main.lua` `random_seed`).
    #[wasm_bindgen(js_name = randomSeed)]
    pub fn random_seed(&self) -> u32 {
        self.game.random_seed()
    }

    /// Mid-run save JSON (`utils.lua` datastore `"save"`). Host writes `localStorage`.
    #[wasm_bindgen(js_name = exportSave)]
    pub fn export_save(&self) -> String {
        self.game.export_save()
    }

    /// `check_for_save` — restore from JSON. Returns whether resume applied.
    #[wasm_bindgen(js_name = applySave)]
    pub fn apply_save(&mut self, json: &str) -> bool {
        let ok = self.game.apply_save_json(json);
        if ok {
            self.refresh_if_boot();
        }
        ok
    }

    /// `gameWillTerminate` — snapshot or delete JSON; `None` → leave storage alone.
    #[wasm_bindgen(js_name = takeTerminateSave)]
    pub fn take_terminate_save(&mut self) -> Option<String> {
        self.game.game_will_terminate()
    }

    /// Force `deletesave()` and return the cleared blob JSON.
    #[wasm_bindgen(js_name = deleteSave)]
    pub fn delete_save(&mut self) -> String {
        self.game.delete_save();
        self.game.export_save()
    }

    /// Drain dirty save JSON after exit / delete (host frame loop).
    #[wasm_bindgen(js_name = takeSaveFlush)]
    pub fn take_save_flush(&mut self) -> Option<String> {
        self.game.take_save_flush()
    }

    /// True when [`Self::take_save_flush`] would return `Some`.
    #[wasm_bindgen(js_name = saveNeedsFlush)]
    pub fn save_needs_flush(&self) -> bool {
        self.game.save_needs_flush()
    }

    /// Local high-score board JSON (Catalog stand-in). Host writes `zipper.hs.v1`.
    #[wasm_bindgen(js_name = exportHighscores)]
    pub fn export_highscores(&self) -> String {
        self.game.export_highscores()
    }

    /// Boot: replace board from `localStorage` JSON.
    #[wasm_bindgen(js_name = applyHighscores)]
    pub fn apply_highscores(&mut self, json: &str) {
        self.game.apply_highscores_json(json);
    }

    /// Drain dirty high-score JSON after a win submit.
    #[wasm_bindgen(js_name = takeHsFlush)]
    pub fn take_hs_flush(&mut self) -> Option<String> {
        self.game.take_hs_flush()
    }

    /// True when [`Self::take_hs_flush`] would return `Some`.
    #[wasm_bindgen(js_name = hsNeedsFlush)]
    pub fn hs_needs_flush(&self) -> bool {
        self.game.hs_needs_flush()
    }

    /// Load outdoor `worldmap.bin` (`ZMAP`). Required before play — not baked into wasm.
    #[wasm_bindgen(js_name = loadWorldmap)]
    pub fn load_worldmap(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.game
            .load_worldmap(data)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.refresh_if_boot();
        Ok(())
    }

    /// True after a successful [`Self::load_worldmap`].
    #[wasm_bindgen(js_name = worldmapLoaded)]
    pub fn worldmap_loaded(&self) -> bool {
        self.game.worldmap_loaded()
    }

    /// Install dialog scripts from `script.lua` / `script.luac` (array of string arrays).
    /// Creative prose is not baked into wasm — load from the user’s pdx at runtime.
    #[wasm_bindgen(js_name = loadDialogs)]
    pub fn load_dialogs(&mut self, scripts: JsValue) -> Result<(), JsValue> {
        let outer = js_sys::Array::from(&scripts);
        let mut out = Vec::with_capacity(outer.length() as usize);
        for i in 0..outer.length() {
            let inner = js_sys::Array::from(&outer.get(i));
            let mut lines = Vec::with_capacity(inner.length() as usize);
            for j in 0..inner.length() {
                let v = inner.get(j);
                lines.push(v.as_string().unwrap_or_default());
            }
            out.push(lines);
        }
        zipper_core::dialog_scripts::load_dialogs(out);
        Ok(())
    }

    /// True after a non-empty [`Self::load_dialogs`].
    #[wasm_bindgen(js_name = dialogsLoaded)]
    pub fn dialogs_loaded(&self) -> bool {
        zipper_core::dialog_scripts::dialogs_loaded()
    }

    /// Install `introchord` from a JS `{ introchord: number[] }` or bare `number[]`.
    /// Notes are authored content — not baked into wasm (demo stages JSON).
    #[wasm_bindgen(js_name = loadIntroMusic)]
    pub fn load_intro_music(&mut self, config: JsValue) -> Result<(), JsValue> {
        let notes = parse_introchord_js(&config)?;
        self.game.load_intro_music(&notes);
        Ok(())
    }

    /// Extract + install `introchord` from Playdate `Globals.luac` bytes (Rust LuaT scan).
    #[wasm_bindgen(js_name = loadIntroMusicFromLuac)]
    pub fn load_intro_music_from_luac(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.game
            .load_intro_music_from_globals_luac(data)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// True after a non-empty introchord load.
    #[wasm_bindgen(js_name = introMusicLoaded)]
    pub fn intro_music_loaded(&self) -> bool {
        self.game.intro_music_loaded()
    }

    /// Extract + install ending-credit prose from the user's Playdate `main.luac`.
    /// Authored content — not baked into wasm; the engine keeps only the ticks.
    #[wasm_bindgen(js_name = loadCreditsFromLuac)]
    pub fn load_credits_from_luac(&mut self, data: &[u8]) -> Result<(), JsValue> {
        self.game
            .load_credits_from_main_luac(data)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// True after a non-empty credits load.
    #[wasm_bindgen(js_name = creditsLoaded)]
    pub fn credits_loaded(&self) -> bool {
        self.game.credits_loaded()
    }

    /// Install a MIDI sequence by name. Currently only `"sho"` (`Sounds/Sho.mid`).
    #[wasm_bindgen(js_name = loadMidi)]
    pub fn load_midi(&mut self, name: &str, data: &[u8]) -> Result<(), JsValue> {
        if name != "sho" {
            return Err(JsValue::from_str(&format!(
                "loadMidi: unknown name {name:?} (only \"sho\")"
            )));
        }
        self.game
            .load_sho_midi(data)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// True after a successful `loadMidi("sho", …)`.
    #[wasm_bindgen(js_name = shoMidiLoaded)]
    pub fn sho_midi_loaded(&self) -> bool {
        self.game.sho_midi_loaded()
    }

    /// Forward the URL fragment to wasm. Some builds may return optional menu
    /// HTML to inject; the default build always returns `None`.
    #[wasm_bindgen(js_name = applyUrlFragment)]
    pub fn apply_url_fragment(&mut self, hash: &str) -> Option<String> {
        #[cfg(feature = "god")]
        {
            return god_apply_url_fragment(&mut self.game, hash);
        }
        #[cfg(not(feature = "god"))]
        {
            let _ = hash;
            None
        }
    }

    /// True when optional fragment-enabled tools are active for this session.
    #[wasm_bindgen(js_name = devToolsActive)]
    pub fn dev_tools_active(&self) -> bool {
        #[cfg(feature = "god")]
        {
            self.game.god_mode()
        }
        #[cfg(not(feature = "god"))]
        {
            false
        }
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = setInvulnerable)]
    pub fn set_invulnerable(&mut self, on: bool) {
        self.game.set_invulnerable(on);
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = invulnerable)]
    pub fn invulnerable(&self) -> bool {
        self.game.invulnerable()
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = setCheatKey)]
    pub fn set_cheat_key(&mut self, on: bool) {
        self.game.set_cheat_key(on);
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = cheatKey)]
    pub fn cheat_key(&self) -> bool {
        self.game.cheat_key()
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = playerTile)]
    pub fn player_tile(&self) -> Vec<i32> {
        let (x, y) = self.game.player_tile();
        vec![x, y]
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = teleportTo)]
    pub fn teleport_to(&mut self, x: i32, y: i32) -> bool {
        self.game.god_teleport(x, y)
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = forceWin)]
    pub fn force_win(&mut self) {
        self.game.force_win_for_test();
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = chestPos)]
    pub fn chest_pos(&self) -> Option<Vec<i32>> {
        self.game.chest_pos().map(|(x, y)| vec![x, y])
    }

    #[cfg(feature = "god")]
    #[wasm_bindgen(js_name = takeChestSpawn)]
    pub fn take_chest_spawn(&mut self) -> Option<Vec<i32>> {
        self.game.take_chest_spawn().map(|(x, y)| vec![x, y])
    }

    /// Dismiss the title card early. Returns true if boot was skipped.
    #[wasm_bindgen(js_name = skipBoot)]
    pub fn skip_boot(&mut self) -> bool {
        self.game.skip_boot()
    }

    /// Playdate system-menu **seppuku** — pierce self-kill, then restart prompt.
    ///
    /// Queues `playerdeath` outside [`Game::update`]. The next `frame` clears
    /// `sfx_queue` at the start of `update`, so we must flush here or the scream
    /// is silently dropped (combat deaths queue *inside* `update` and are fine).
    #[wasm_bindgen(js_name = seppuku)]
    pub fn seppuku(&mut self) {
        self.game.seppuku();
        let _ = self.flush_sfx();
    }

    /// Playdate system-menu **delete save** — clear mid-run resume + dialog
    /// counters (in-RAM), restart at outdoor start. Host flushes `localStorage`.
    #[wasm_bindgen(js_name = deleteSaveMenu)]
    pub fn delete_save_menu(&mut self) {
        self.game.delete_save_menu();
    }

    #[wasm_bindgen(js_name = screenWidth)]
    pub fn screen_width(&self) -> u32 {
        SCREEN_WIDTH
    }

    #[wasm_bindgen(js_name = screenHeight)]
    pub fn screen_height(&self) -> u32 {
        SCREEN_HEIGHT
    }

    /// Port stamp baked into the wasm (`PORT_VERSION`, e.g. `v0.8f`).
    #[wasm_bindgen(js_name = portVersion)]
    pub fn port_version(&self) -> String {
        port_version().to_string()
    }

    /// Ensure `AudioContext` exists and is running (call from a user gesture).
    /// Also materializes any `.pda` samples that were loaded before unlock.
    ///
    /// Safari suspends the context after background / screen lock; call this again
    /// on `visibilitychange` / `pageshow` / the next tap so SFX keep working.
    #[wasm_bindgen(js_name = ensureAudio)]
    pub fn ensure_audio(&mut self) -> Result<(), JsValue> {
        if self.audio.is_none() {
            let ctx = AudioContext::new()?;
            let gain = ctx.create_gain()?;
            gain.gain().set_value(slider_to_gain(self.volume));
            gain.connect_with_audio_node(&ctx.destination())?;
            let sho = ShoInstrument::new(&ctx, &gain)?;
            self.master_gain = Some(gain);
            self.sho = Some(sho);
            self.audio = Some(ctx);
        }
        if let Some(ctx) = &self.audio {
            if ctx.state() != AudioContextState::Running {
                let _ = ctx.resume();
            }
        }
        self.materialize_pending_sfx()?;
        // resume() may already be running on desktop; drain anything held for the
        // async iOS case on the same call when possible.
        self.flush_held_sfx_if_running()?;
        self.flush_held_synth_if_running()?;
        Ok(())
    }

    /// Page went to background / screen locked: drop queued one-shots so we do not
    /// dump a backlog of swooshes when Safari resumes the context later.
    #[wasm_bindgen(js_name = onAudioSuspend)]
    pub fn on_audio_suspend(&mut self) {
        self.held_sfx.clear();
        self.held_synth.clear();
        if let Some(ctx) = &self.audio {
            let _ = ctx.suspend();
        }
    }

    fn audio_is_running(&self) -> bool {
        matches!(
            self.audio.as_ref().map(AudioContext::state),
            Some(AudioContextState::Running)
        )
    }

    fn flush_held_sfx_if_running(&mut self) -> Result<(), JsValue> {
        if self.held_sfx.is_empty() || !self.audio_is_running() {
            return Ok(());
        }
        let queued: Vec<SfxId> = self.held_sfx.drain(..).collect();
        self.play_sfx_ids(&queued)
    }

    fn flush_synth(&mut self) -> Result<(), JsValue> {
        let queued = self.game.take_synth();
        if !queued.is_empty() {
            const HOLD_CAP: usize = 16;
            self.held_synth.extend(queued);
            if self.held_synth.len() > HOLD_CAP {
                let skip = self.held_synth.len() - HOLD_CAP;
                self.held_synth.drain(0..skip);
            }
        }
        if self.held_synth.is_empty() {
            return Ok(());
        }
        let _ = self.ensure_audio();
        if !self.audio_is_running() {
            return Ok(());
        }
        self.flush_held_synth_if_running()
    }

    fn flush_held_synth_if_running(&mut self) -> Result<(), JsValue> {
        if self.held_synth.is_empty() || !self.audio_is_running() {
            return Ok(());
        }
        let queued: Vec<SynthEvent> = self.held_synth.drain(..).collect();
        self.play_synth_events(&queued)
    }

    fn play_synth_events(&mut self, events: &[SynthEvent]) -> Result<(), JsValue> {
        if self.audio.is_none() || self.master_gain.is_none() {
            return Ok(());
        }
        if self.sho.is_none() {
            let sho = {
                let ctx = self.audio.as_ref().unwrap();
                let gain = self.master_gain.as_ref().unwrap();
                ShoInstrument::new(ctx, gain)?
            };
            self.sho = Some(sho);
        }
        let ctx = self.audio.as_ref().unwrap();
        let sho = self.sho.as_mut().unwrap();
        for ev in events {
            match *ev {
                SynthEvent::NoteOn(n) => sho.note_on(ctx, n)?,
                SynthEvent::NoteOff(n) => sho.note_off_midi(ctx, n)?,
                SynthEvent::AllNotesOff => sho.all_notes_off(ctx)?,
            }
        }
        Ok(())
    }

    /// Sync core `sho_pitch_bend` onto the Web Audio instrument (port easter egg).
    fn apply_sho_pitch_bend(&mut self) -> Result<(), JsValue> {
        let bend = self.game.sho_pitch_bend();
        let Some(ctx) = self.audio.as_ref() else {
            return Ok(());
        };
        if ctx.state() != AudioContextState::Running {
            return Ok(());
        }
        // Lazily create the instrument if victory is bent before the first NoteOn flush.
        if self.sho.is_none() {
            if bend == 0.0 {
                return Ok(());
            }
            let Some(gain) = self.master_gain.as_ref() else {
                return Ok(());
            };
            self.sho = Some(ShoInstrument::new(ctx, gain)?);
        }
        let sho = self.sho.as_mut().unwrap();
        sho.set_pitch_bend_semitones(ctx, bend)
    }

    fn play_sfx_ids(&mut self, ids: &[SfxId]) -> Result<(), JsValue> {
        let Some(ctx) = self.audio.as_ref() else {
            return Ok(());
        };
        let Some(gain) = self.master_gain.as_ref() else {
            return Ok(());
        };
        for id in ids {
            let key = sfx_key(*id);
            let Some(buffer) = self.sfx.get(key) else {
                web_sys::console::warn_1(
                    &JsValue::from_str(&format!("sfx missing from bank: {key}")),
                );
                continue;
            };
            let src = ctx.create_buffer_source()?;
            src.set_buffer(Some(buffer));
            src.connect_with_audio_node(gain)?;
            src.start()?;
        }
        Ok(())
    }

    /// How many SFX buffers are ready to play (tests / debug HUD).
    #[wasm_bindgen(js_name = sfxReadyCount)]
    pub fn sfx_ready_count(&self) -> usize {
        self.sfx.len()
    }

    /// How many decoded `.pda` samples are still waiting for AudioContext unlock.
    #[wasm_bindgen(js_name = sfxPendingCount)]
    pub fn sfx_pending_count(&self) -> usize {
        self.pending_sfx.len()
    }

    /// Load a raw `.pdi` as a named bitmap.
    /// Names: `playerface`, `ninjaface`, `swordface`, `pikeface`, `twinstepface`,
    /// `kingface`, `spiritface`, `monkface`, `faceblood`, `key`, `card`, …
    #[wasm_bindgen(js_name = loadPdi)]
    pub fn load_pdi(&mut self, name: &str, data: &[u8]) -> Result<(), JsValue> {
        let bmp = decode_pdi(data).map_err(|e| JsValue::from_str(&e.to_string()))?;
        match name {
            "playerface" => {
                self.game.assets.dialog_faces[0] = Some(bmp.clone());
                self.game.assets.player_face = Some(bmp);
            }
            "swordface" => {
                self.game.assets.dialog_faces[1] = Some(bmp.clone());
                self.game.assets.dialog_faces[7] = Some(bmp); // Lua dup
            }
            "pikeface" => self.game.assets.dialog_faces[2] = Some(bmp),
            "ninjaface" => {
                self.game.assets.dialog_faces[3] = Some(bmp.clone());
                self.game.assets.ninja_face = Some(bmp);
            }
            "twinstepface" => self.game.assets.dialog_faces[4] = Some(bmp),
            "kingface" => self.game.assets.dialog_faces[5] = Some(bmp),
            "spiritface" => self.game.assets.dialog_faces[6] = Some(bmp),
            "monkface" => self.game.assets.dialog_faces[8] = Some(bmp),
            "faceblood" => self.game.assets.faceblood = Some(bmp),
            "key" => self.game.assets.key = Some(bmp),
            "chest" => self.game.assets.chest = Some(bmp),
            "card" => self.game.assets.card = Some(bmp),
            "passicon" => self.game.assets.passicon = Some(bmp),
            "moveicon" => self.game.assets.moveicon = Some(bmp),
            "centerdot" => self.game.assets.centerdot = Some(bmp),
            "killicon" => self.game.assets.killicon = Some(bmp),
            "stabicon" => self.game.assets.stabicon = Some(bmp),
            "hereblood" => self.game.assets.hereblood = Some(bmp),
            "movebar" => self.game.assets.movebar = Some(bmp),
            "lifebar" => self.game.assets.lifebar = Some(bmp),
            "readyword" => self.game.assets.readyword = Some(bmp),
            "pressx" => self.game.assets.pressx = Some(bmp),
            "barmatte" => self.game.assets.barmatte = Some(bmp),
            "readysegwalk" => self.game.assets.readysegwalk = Some(bmp),
            "readysegwalk_0" => self.game.assets.readysegwalk_0 = Some(bmp),
            "readysegwalk_1" => self.game.assets.readysegwalk_1 = Some(bmp),
            "readysegkill" => self.game.assets.readysegkill = Some(bmp),
            "readysegghost1" => self.game.assets.readysegghost1 = Some(bmp),
            "readysegghost2" => self.game.assets.readysegghost2 = Some(bmp),
            "restart" => self.game.assets.restart = Some(bmp),
            "bennett" => self.game.assets.bennett = Some(bmp),
            "endbg" => self.game.assets.endbg = Some(bmp),
            _ => {
                return Err(JsValue::from_str(&format!(
                    "unknown pdi name: {name}"
                )));
            }
        }
        self.refresh_if_boot();
        Ok(())
    }

    /// Load a raw `.pdt` image table.
    /// Names: `tiles`, `ninja`, `player`, `enemy`, `pikeman`, `piketip`, `twinstep`,
    /// `parry`, `shuriken`, `king`, `demon` (Spirit `spiritTable`), `smoke`, `smoke2`,
    /// `espray1`…`5`, `floorspray`, `trail`, `dripsC/N/S/E/W`, `isospray`,
    /// `exiticon`, `hourglass`, `*_ghost`.
    #[wasm_bindgen(js_name = loadPdt)]
    pub fn load_pdt(&mut self, name: &str, data: &[u8]) -> Result<(), JsValue> {
        let table = decode_pdt(data).map_err(|e| JsValue::from_str(&e.to_string()))?;
        match name {
            "tiles" | "tileset" => self.game.assets.tiles = Some(table),
            "ninja" => self.game.assets.ninja = Some(table),
            "player" => self.game.assets.player = Some(table),
            "enemy" => self.game.assets.enemy = Some(table),
            "pikeman" => self.game.assets.pikeman = Some(table),
            "piketip" => self.game.assets.piketip = Some(table),
            "twinstep" => self.game.assets.twinstep = Some(table),
            "parry" => self.game.assets.parry = Some(table),
            "shuriken" => self.game.assets.shuriken = Some(table),
            "king" => self.game.assets.king = Some(table),
            // Lua `spiritTable = Images/demon` — host loads as `demon`.
            "demon" | "spirit" => self.game.assets.spirit = Some(table),
            // Spirit-room `door` class (`Images/door` / `doorTable`).
            "door" => self.game.assets.spirit_door = Some(table),
            "leftdoor" => self.game.assets.leftdoor = Some(table),
            "rightdoor" => self.game.assets.rightdoor = Some(table),
            "smoke" => self.game.assets.smoke = Some(table),
            "smoke2" => self.game.assets.smoke2 = Some(table),
            "espray1" => self.game.assets.espray[0] = Some(table),
            "espray2" => self.game.assets.espray[1] = Some(table),
            "espray3" => self.game.assets.espray[2] = Some(table),
            "espray4" => self.game.assets.espray[3] = Some(table),
            "espray5" => self.game.assets.espray[4] = Some(table),
            "floorspray" => self.game.assets.floorspray = Some(table),
            "trail" => self.game.assets.trail = Some(table),
            "dripsC" => self.game.assets.drips[0] = Some(table),
            "dripsN" => self.game.assets.drips[1] = Some(table),
            "dripsS" => self.game.assets.drips[2] = Some(table),
            "dripsE" => self.game.assets.drips[3] = Some(table),
            "dripsW" => self.game.assets.drips[4] = Some(table),
            "isospray" => self.game.assets.isospray = Some(table),
            "exiticon" => self.game.assets.exiticon = Some(table),
            "hourglass" => self.game.assets.hourglass = Some(table),
            "dialogbg" => self.game.assets.dialogbg = Some(table),
            "continue" => self.game.assets.continue_bar = Some(table),
            "zip" => self.game.assets.zip = Some(table),
            "enemy_ghost" => self.game.assets.enemy_ghost = Some(table),
            "ninja_ghost" => self.game.assets.ninja_ghost = Some(table),
            "pikeman_ghost" => self.game.assets.pikeman_ghost = Some(table),
            "twinstep_ghost" => self.game.assets.twinstep_ghost = Some(table),
            "crankhint" => self.game.assets.crankhint = Some(table),
            "highscore" => self.game.assets.highscore = Some(table),
            "wipe" => self.game.assets.wipe = Some(table),
            _ => {
                return Err(JsValue::from_str(&format!(
                    "unknown demo pdt name: {name}"
                )));
            }
        }
        self.refresh_if_boot();
        Ok(())
    }

    /// Load a raw `.pft` font.
    /// Names: `headerwhite` (`Fonts/headerwhite` — lifebar LIFE label).
    #[wasm_bindgen(js_name = loadPft)]
    pub fn load_pft(&mut self, name: &str, data: &[u8]) -> Result<(), JsValue> {
        let font = decode_pft(data).map_err(|e| JsValue::from_str(&e.to_string()))?;
        match name {
            "headerwhite" => self.game.assets.headerwhite = Some(font),
            "monoblack" => self.game.assets.monoblack = Some(font),
            _ => {
                return Err(JsValue::from_str(&format!(
                    "unknown demo pft name: {name}"
                )));
            }
        }
        self.refresh_if_boot();
        Ok(())
    }

    /// Load a `.pda` sample into the Web Audio bank (`soundm` names: `select`, `buzz`, …).
    ///
    /// Decodes immediately. If the AudioContext is not unlocked yet (autoplay),
    /// PCM is kept in `pending_sfx` and uploaded on the next [`ensure_audio`].
    #[wasm_bindgen(js_name = loadPda)]
    pub fn load_pda(&mut self, name: &str, data: &[u8]) -> Result<(), JsValue> {
        let pcm = decode_pda(data).map_err(|e| JsValue::from_str(&e.to_string()))?;
        if self.audio.is_some() {
            self.upload_sfx(name, &pcm)?;
        } else {
            self.pending_sfx.insert(name.to_string(), pcm);
        }
        Ok(())
    }

    fn materialize_pending_sfx(&mut self) -> Result<(), JsValue> {
        if self.audio.is_none() || self.pending_sfx.is_empty() {
            return Ok(());
        }
        let pending: Vec<(String, PcmSample)> = self.pending_sfx.drain().collect();
        for (name, pcm) in pending {
            self.upload_sfx(&name, &pcm)?;
        }
        Ok(())
    }

    fn upload_sfx(&mut self, name: &str, pcm: &PcmSample) -> Result<(), JsValue> {
        let ctx = self
            .audio
            .as_ref()
            .ok_or_else(|| JsValue::from_str("no audio"))?;
        let frames = pcm.frame_count() as u32;
        if frames == 0 {
            return Ok(());
        }
        let buffer = ctx.create_buffer(1, frames, pcm.sample_rate as f32)?;
        let floats = pcm.to_f32_mono();
        // Length must match the buffer; truncate/pad defensively.
        let mut channel = floats;
        channel.resize(frames as usize, 0.0);
        buffer.copy_to_channel(&channel, 0)?;
        self.sfx.insert(name.to_string(), buffer);
        Ok(())
    }

    fn refresh_if_boot(&mut self) {
        if matches!(self.game.state, GameState::Boot) {
            let input = Input {
                buttons: self.buttons,
                crank_degrees: self.crank_degrees,
                crank_delta: 0.0,
                crank_docked: self.crank_docked,
            };
            self.game.update(0.0, input);
            let _ = self.game.take_sfx();
        }
    }

    #[wasm_bindgen(js_name = setButton)]
    pub fn set_button(&mut self, name: &str, down: bool) {
        match name {
            "left" => self.buttons.left = down,
            "right" => self.buttons.right = down,
            "up" => self.buttons.up = down,
            "down" => self.buttons.down = down,
            "a" => self.buttons.a = down,
            "b" => self.buttons.b = down,
            "menu" => self.buttons.menu = down,
            _ => {}
        }
        // Unlock audio on first input (autoplay policy).
        if down {
            let _ = self.ensure_audio();
        }
    }

    #[wasm_bindgen(js_name = setCrank)]
    pub fn set_crank(&mut self, degrees: f32, docked: bool) {
        let d = degrees.rem_euclid(360.0);
        let prev = self.crank_degrees;
        let mut delta = d - prev;
        if delta > 180.0 {
            delta -= 360.0;
        } else if delta < -180.0 {
            delta += 360.0;
        }
        self.crank_delta = delta;
        self.crank_degrees = d;
        self.crank_docked = docked;
    }

    pub fn frame(&mut self, ctx: &CanvasRenderingContext2d, dt: f32) -> Result<(), JsValue> {
        let input = Input {
            buttons: self.buttons,
            crank_degrees: self.crank_degrees,
            crank_delta: self.crank_delta,
            crank_docked: self.crank_docked,
        };
        self.crank_delta = 0.0;

        self.game.update(dt, input);
        self.flush_sfx()?;
        self.flush_synth()?;
        // Port easter egg: victory sho crank pitch bend (sticky; not a SynthEvent).
        self.apply_sho_pitch_bend()?;
        // iOS resume() is async: keep retrying held one-shots once the context
        // flips back to running (often a frame or two after the unlock gesture).
        if !self.held_sfx.is_empty() || !self.held_synth.is_empty() {
            if let Some(ctx) = &self.audio {
                if ctx.state() != AudioContextState::Running {
                    let _ = ctx.resume();
                }
            }
            let _ = self.flush_held_sfx_if_running();
            let _ = self.flush_held_synth_if_running();
        }
        expand_1bit_to_rgba(&self.game.fb, &mut self.rgba, self.game.screen_inverted());

        let image_data = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(&self.rgba),
            SCREEN_WIDTH,
            SCREEN_HEIGHT,
        )?;
        ctx.put_image_data(&image_data, 0.0, 0.0)?;
        Ok(())
    }

    fn flush_sfx(&mut self) -> Result<(), JsValue> {
        let queued = self.game.take_sfx();
        if !queued.is_empty() {
            // Cap so a long suspended stretch cannot accumulate an endless backlog.
            const HOLD_CAP: usize = 12;
            self.held_sfx.extend(queued);
            if self.held_sfx.len() > HOLD_CAP {
                let skip = self.held_sfx.len() - HOLD_CAP;
                self.held_sfx.drain(0..skip);
            }
        }
        if self.held_sfx.is_empty() {
            return Ok(());
        }
        // Kill SFX (falldead*) often fire on the same gesture that unlocks audio —
        // materialize pending buffers first so the scream isn't silently dropped.
        // After iOS background, resume() is async: keep holding until state is running.
        let _ = self.ensure_audio();
        if !self.audio_is_running() {
            return Ok(());
        }
        self.flush_held_sfx_if_running()
    }

    #[wasm_bindgen(js_name = gameState)]
    pub fn game_state(&self) -> String {
        format!("{:?}", self.game.state)
    }

    /// Host mobile crank-swipe tip: true when the in-game `crankhint` would show
    /// while aiming (`movebar:updatehint` gates + Aiming state).
    #[wasm_bindgen(js_name = shouldShowCrankHint)]
    pub fn should_show_crank_hint(&self) -> bool {
        matches!(self.game.state, GameState::Aiming) && self.game.should_show_crankhint()
    }

    /// Host mobile: clear Down-pad backdrop blur while chest `(145,176)` is visible
    /// in the current outdoor room (pad overlaps that tile).
    #[wasm_bindgen(js_name = shouldClearTouchDownBlur)]
    pub fn should_clear_touch_down_blur(&self) -> bool {
        self.game.should_clear_touch_down_blur()
    }
}

impl Default for ZipperApp {
    fn default() -> Self {
        Self::new(None).expect("ZipperApp::new")
    }
}

/// God-build fragment parse + menu HTML (`feature = "god"` only).
#[cfg(feature = "god")]
fn god_apply_url_fragment(game: &mut Game, hash: &str) -> Option<String> {
    if !fragment_requests_god(hash) {
        return None;
    }
    game.set_god_mode(true);
    Some(include_str!("god_menu.html").to_string())
}

#[cfg(feature = "god")]
fn fragment_requests_god(hash: &str) -> bool {
    let raw = hash.trim().trim_start_matches('#').to_ascii_lowercase();
    if raw.is_empty() {
        return false;
    }
    raw.split(|c| c == '&' || c == ',')
        .flat_map(|p| p.split('/'))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .any(|p| p == "god" || p == "cheat=god" || p == "mode=god")
}

#[cfg(test)]
mod volume_curve_tests {
    use super::slider_to_gain;

    #[test]
    fn slider_zero_is_mute() {
        assert_eq!(slider_to_gain(0.0), 0.0);
    }

    #[test]
    fn slider_full_is_unity() {
        let g = slider_to_gain(1.0);
        assert!((g - 1.0).abs() < 1e-5, "gain={g}");
    }

    #[test]
    fn mid_slider_is_much_quieter_than_linear() {
        // 60% linear would be 0.6; −40 dB curve → 10^(-16/20) ≈ 0.158.
        let g = slider_to_gain(0.6);
        assert!(g > 0.10 && g < 0.25, "expected perceptual mid, got {g}");
        assert!(g < 0.6 * 0.5, "must be well below naive linear 0.6");
    }
}
