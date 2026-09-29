//! Host-independent Zipper game logic.
//!
//! Rules and map data come from the 1.10 bytecode / decompiled Lua.
//! Prefer matching `selector.lua`, `samurai.lua`, `main.lua`, `Globals.lua`
//! over inventing alternate schemes.

pub mod bitmap;
pub mod dialog;
pub mod dialog_scripts;
pub mod ending;
pub mod framebuffer;
pub mod game;
pub mod highscores;
pub mod iso;
pub mod level;
pub mod luac;
pub mod midi;
pub mod pathfinding;
pub mod pda;
pub mod pdi;
pub mod pft;
pub mod save;
pub mod text;
pub mod version;
pub mod worldmap;

pub use bitmap::{Bitmap, DrawMode, ImageTable};
pub use framebuffer::{
    draw_line, Framebuffer, FRAMEBUFFER_BYTES, SCREEN_HEIGHT, SCREEN_WIDTH,
};
pub use game::{
    Buttons, DemoAssets, Game, GameState, Input, SfxId, SynthEvent, DEFAULT_RANDOM_SEED,
};
pub use luac::{
    credits_from_main_luac, introchord_from_globals_luac, introchord_from_json, introchord_json_path,
    LuacError,
};
pub use midi::{parse_smf, MidiError, MidiEv, MidiSequence};
pub use iso::{
    actor_blit_pos, grid_to_screen, isosprite_frame_index, selector_icon_blit_pos, tile_blit_pos,
};
pub use level::{load_start_room, Room, RoomId};
pub use pda::{decode_pda, PdaError, PcmSample};
pub use pdi::{decode_cell, decode_pdi, decode_pdt, DecodeError};
pub use pft::{decode_pft, Font, Glyph};
pub use highscores::{HighScoreBoard, HighScoreEntry, HS_DRAW_MAX, HS_PLAYER_NAME, HS_STORE_MAX};
pub use save::{DeadEnemySave, SavedGame, SAVE_VERSION};
pub use version::stamp as port_version;
pub use worldmap::{Facing, WorldMap, WorldMapError, START_PLAY};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::viewport_around;
    use crate::worldmap::{START_CAMERA, START_TITLE};

    fn test_worldmap_bytes() -> Vec<u8> {
        std::fs::read(WorldMap::data_file_path()).expect("data/worldmap.bin")
    }

    /// `Game::new` + runtime map + dialog corpus (neither is baked into the crate).
    fn test_game() -> Game {
        crate::dialog_scripts::load_test_dialogs();
        let mut g = Game::new();
        g.load_worldmap(&test_worldmap_bytes()).expect("load worldmap");
        g
    }

    fn test_game_seed(seed: u32) -> Game {
        crate::dialog_scripts::load_test_dialogs();
        let mut g = Game::new_with_seed(seed);
        g.load_worldmap(&test_worldmap_bytes()).expect("load worldmap");
        g
    }

    #[test]
    fn framebuffer_set_get() {
        let mut fb = Framebuffer::default();
        assert!(!fb.get_pixel(0, 0));
        fb.set_pixel(0, 0, true);
        assert!(fb.get_pixel(0, 0));
        fb.set_pixel(7, 0, true);
        assert!(fb.get_pixel(7, 0));
        assert_eq!(fb.pixels[0], 0b1000_0001);
    }

    #[test]
    fn game_starts_without_baked_worldmap() {
        let g = Game::new();
        assert!(!g.worldmap_loaded());
        assert_eq!(g.world.width, 0);
    }

    #[test]
    fn load_worldmap_installs_start_room() {
        let mut g = Game::new();
        g.load_worldmap(&test_worldmap_bytes()).unwrap();
        assert!(g.worldmap_loaded());
        assert_eq!(g.world.width, 256);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert!(g.world.selector_can_step(START_PLAY.0, START_PLAY.1));
    }

    #[test]
    fn game_boots_then_aims() {
        let mut g = test_game();
        assert_eq!(g.state, GameState::Boot);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.016, input);
        assert_eq!(g.state, GameState::Aiming);
        assert_eq!(g.player_x, START_PLAY.0);
        assert_eq!(g.player_y, START_PLAY.1);
        assert!(g.intro_seen());
        assert!(!g.screen_inverted());
    }

    /// Host splash idle timeout is 2s → Lua Title (white intro), not Aiming.
    #[test]
    fn boot_idle_2s_enters_title() {
        let mut g = test_game();
        assert_eq!(g.state, GameState::Boot);
        let input = Input::default();
        g.update(1.9, input);
        assert_eq!(g.state, GameState::Boot, "still on card before 2s");
        g.update(0.2, input);
        assert_eq!(g.state, GameState::Title);
        assert_eq!((g.player_x, g.player_y), START_TITLE);
        assert!(g.screen_inverted());
        assert!(g.intro_seen());
    }

    /// D-pad / B dismisses the splash card straight into the first room.
    #[test]
    fn boot_skips_on_any_game_button() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.up = true;
        g.update(0.016, input);
        assert_eq!(g.state, GameState::Aiming);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert!(g.intro_seen());
        assert!(!g.screen_inverted());
    }

    #[test]
    fn boot_click_skips_intro_to_aiming() {
        let mut g = test_game();
        assert!(g.skip_boot());
        assert_eq!(g.state, GameState::Aiming);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert!(g.intro_seen());
        assert!(!g.screen_inverted());
        assert!(!g.skip_boot(), "second call is a no-op");
    }

    #[test]
    fn title_to_intro_after_10_ticks() {
        let mut g = test_game();
        assert!(g.skip_boot()); // land in Aiming so we can force Title path cleanly
        // Re-enter Title via idle Boot path: new game.
        let mut g = test_game();
        g.update(2.1, Input::default());
        assert_eq!(g.state, GameState::Title);
        // 11 × 0.05s → ticks go 1..11; at ticks>10 enter Intro.
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.state, GameState::Intro);
        assert_eq!((g.player_x, g.player_y), START_TITLE);
    }

    #[test]
    fn intro_walks_six_steps_to_start() {
        let mut g = test_game();
        g.update(2.1, Input::default());
        assert_eq!(g.state, GameState::Title);
        // Title hold: 11 ticks → Intro.
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.state, GameState::Intro);
        // Title teleport spent 1 (249); 6 intro walks → (109,182), life 243.
        for _ in 0..60 {
            g.update(0.05, Input::default());
        }
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert_eq!(g.life, 243);
        // Still Intro until ticks==60 after land; land tick resets counter on step.
        // After the 6th step intro_ticks was reset to 0; need 60 more for endintro.
        assert_eq!(g.state, GameState::Intro);
        for _ in 0..60 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.state, GameState::Aiming);
        assert!(!g.screen_inverted());
    }

    #[test]
    fn intro_skip_with_a_after_tick_1() {
        let mut g = test_game();
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.state, GameState::Intro);
        // First Intro tick: A must not skip (ticks > 1 required).
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Intro);
        // Release then press again after ticks > 1.
        g.update(0.05, Input::default());
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        assert!(!g.screen_inverted());
    }

    #[test]
    fn endintro_queues_click() {
        let mut g = test_game();
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        // Advance past first Intro tick, then skip.
        g.update(0.05, Input::default());
        g.update(0.05, Input::default());
        let _ = g.take_sfx();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        let sfx = g.take_sfx();
        assert!(sfx.contains(&SfxId::Click), "expected Click in {sfx:?}");
        assert!(g.zip_anim_active(), "zip oneshot starts on endintro");
    }

    fn load_test_introchord(g: &mut Game) {
        let text = std::fs::read_to_string(crate::introchord_json_path())
            .expect("data/introchord.json");
        let notes = crate::introchord_from_json(&text).expect("parse introchord");
        g.load_intro_music(&notes);
    }

    /// Six intro walks queue the six chord NoteOns — not Step PDAs.
    #[test]
    fn intro_queues_chord_note_ons() {
        let mut g = test_game();
        load_test_introchord(&mut g);
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.state, GameState::Intro);
        let mut notes = Vec::new();
        for _ in 0..60 {
            g.update(0.05, Input::default());
            for ev in g.take_synth() {
                if let SynthEvent::NoteOn(n) = ev {
                    notes.push(n);
                }
            }
            let sfx = g.take_sfx();
            assert!(
                !sfx.contains(&SfxId::Step),
                "intro walk must not play Step PDA"
            );
        }
        assert_eq!(notes, vec![100, 93, 98, 95, 96, 102]);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
    }

    /// Mid-intro skip: AllNotesOff then Click.
    #[test]
    fn intro_skip_queues_all_notes_off_then_click() {
        let mut g = test_game();
        load_test_introchord(&mut g);
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        // First step so at least one note is held.
        for _ in 0..10 {
            g.update(0.05, Input::default());
        }
        let _ = g.take_synth();
        let _ = g.take_sfx();
        // ticks > 1, then A skip.
        g.update(0.05, Input::default());
        g.update(0.05, Input::default());
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        let synth = g.take_synth();
        assert!(
            synth.contains(&SynthEvent::AllNotesOff),
            "expected AllNotesOff in {synth:?}"
        );
        let sfx = g.take_sfx();
        assert!(sfx.contains(&SfxId::Click), "expected Click in {sfx:?}");
    }

    /// Without chord loaded, intro stays silent (no synth events).
    #[test]
    fn intro_silent_without_chord() {
        let mut g = test_game();
        assert!(!g.intro_music_loaded());
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        for _ in 0..60 {
            g.update(0.05, Input::default());
            assert!(g.take_synth().is_empty());
        }
    }

    /// Tiny SMF fixture (same as `midi::tests::tiny_smf` shape) for victory player.
    fn tiny_victory_smf() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&480u16.to_be_bytes());
        let mut t0 = Vec::new();
        t0.push(0x00);
        t0.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // 500000
        t0.push(0x00);
        t0.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(t0.len() as u32).to_be_bytes());
        out.extend_from_slice(&t0);
        let mut t1 = Vec::new();
        t1.push(0x00);
        t1.extend_from_slice(&[0x90, 72, 0x64]);
        t1.push(0x83);
        t1.push(0x60); // delta 480
        t1.extend_from_slice(&[0x80, 72, 0x40]);
        t1.push(0x00);
        t1.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(t1.len() as u32).to_be_bytes());
        out.extend_from_slice(&t1);
        out
    }

    /// Tiny SMF whose final note (74) is still held at EOT (no NoteOff).
    /// Playdate releases the instrument voices when the sequence ends.
    fn tiny_victory_smf_held_note() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&480u16.to_be_bytes());
        let mut t0 = Vec::new();
        t0.push(0x00);
        t0.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]); // 500000
        t0.push(0x00);
        t0.extend_from_slice(&[0x90, 72, 0x64]); // NoteOn 72 @ tick 0
        t0.push(0x83);
        t0.push(0x60); // delta 480
        t0.extend_from_slice(&[0x80, 72, 0x40]); // NoteOff 72 @ tick 480
        t0.push(0x83);
        t0.push(0x60); // delta 480
        t0.extend_from_slice(&[0x90, 74, 0x64]); // NoteOn 74 @ tick 960 — held, no off
        t0.push(0x00);
        t0.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(t0.len() as u32).to_be_bytes());
        out.extend_from_slice(&t0);
        out
    }

    /// Port easter egg: crank bends sho pitch across the win cinema (sea pans +
    /// credits); dock eases to center; the bend must clear once the player is
    /// returned to play while `Sho.mid` still sounds.
    #[test]
    fn victory_crank_bends_pitch_only_during_win_cinema() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.load_sho_midi(&tiny_victory_smf()).expect("load");
        g.force_win_for_test();
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);
        assert!(!g.credits_active_for_test());

        // Sea pans (win cinema, credits not yet scrolling): crank bends.
        g.play_victory_music();
        g.update(0.01, Input::default());
        assert!(g.sho_playing_for_test());
        assert_eq!(g.sho_pitch_bend(), 0.0);
        let mut input = Input::default();
        input.crank_delta = 90.0;
        g.update(0.05, input);
        assert!(
            (g.sho_pitch_bend() - 3.0).abs() < 1e-4,
            "crank must bend during the sea pan, got {}",
            g.sho_pitch_bend()
        );

        // Advance to the credits scroller and re-cue the fixture note.
        win_steps(&mut g, 601);
        assert!(g.credits_active_for_test());
        g.play_victory_music();
        g.update(0.01, Input::default());
        assert!(g.sho_playing_for_test());
        input = Input::default();
        input.crank_delta = 90.0;
        g.update(0.05, input);
        assert!((g.sho_pitch_bend() - 3.0).abs() < 1e-4);

        // Clamp at ±12 (tiny SMF note lasts ~0.5s — keep dt small while still playing).
        input.crank_delta = 10_000.0;
        g.update(0.05, input);
        assert!(g.sho_playing_for_test(), "fixture note must still be sounding");
        assert!((g.sho_pitch_bend() - 12.0).abs() < 1e-4);

        // Docked crank springs toward center (0.25s × 8/s = 2 → 10 left).
        input.crank_delta = 0.0;
        input.crank_docked = true;
        g.update(0.25, input);
        assert!(g.sho_playing_for_test());
        assert!((g.sho_pitch_bend() - 10.0).abs() < 0.05);

        // Leaving the cinema (A → Score) clears the bend even though sho plays.
        input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Score);
        assert!(!g.credits_active_for_test());
        assert!(g.sho_playing_for_test(), "fixture note must still be sounding");
        input = Input::default();
        input.crank_delta = 90.0;
        g.update(0.05, input);
        assert_eq!(
            g.sho_pitch_bend(),
            0.0,
            "crank must not bend sho outside the win cinema"
        );

        // Sequence end zeros bend.
        for _ in 0..40 {
            g.update(0.1, Input::default());
            if !g.sho_playing_for_test() {
                break;
            }
        }
        assert!(!g.sho_playing_for_test());
        assert_eq!(g.sho_pitch_bend(), 0.0);
    }

    /// Victory sequence queues NoteOn then NoteOff over file tempo.
    #[test]
    fn victory_midi_queues_note_on_then_off() {
        let mut g = test_game();
        g.load_sho_midi(&tiny_victory_smf()).expect("load");
        assert!(g.sho_midi_loaded());
        g.play_victory_music();
        // First update: cut + t=0 NoteOn.
        g.update(0.01, Input::default());
        let synth = g.take_synth();
        assert!(
            synth.contains(&SynthEvent::AllNotesOff),
            "expected AllNotesOff cut in {synth:?}"
        );
        assert!(
            synth.contains(&SynthEvent::NoteOn(72)),
            "expected NoteOn(72) in {synth:?}"
        );
        // Before 0.5s — no NoteOff yet.
        g.update(0.2, Input::default());
        assert!(
            !g.take_synth().contains(&SynthEvent::NoteOff(72)),
            "too early for NoteOff"
        );
        // Past 0.5s — NoteOff.
        g.update(0.4, Input::default());
        let synth = g.take_synth();
        assert!(
            synth.contains(&SynthEvent::NoteOff(72)),
            "expected NoteOff(72) in {synth:?}"
        );
    }

    /// Sequence reaching EOT must release the instrument: a note still held at
    /// the end (or a lost NoteOff) must not sustain forever.
    #[test]
    fn victory_sequence_end_releases_held_note() {
        let mut g = test_game();
        g.load_sho_midi(&tiny_victory_smf_held_note()).expect("load");
        g.play_victory_music();
        let mut saw_held_on = false;
        let mut end_events = Vec::new();
        for _ in 0..200 {
            g.update(0.1, Input::default());
            let ev = g.take_synth();
            if ev.contains(&SynthEvent::NoteOn(74)) {
                saw_held_on = true;
            }
            if !g.sho_playing_for_test() {
                end_events = ev;
                break;
            }
        }
        assert!(saw_held_on, "held fixture note 74 should have sounded");
        assert!(!g.sho_playing_for_test());
        assert!(
            end_events.contains(&SynthEvent::AllNotesOff),
            "sequence end must release held voices: {end_events:?}"
        );
    }

    /// `shomusic` is a module global in Lua (`Globals.lua:361`); `startGame`
    /// (death / new game, `restart_at_start`) never stops it. Regression: the
    /// port used to clear the sequence on restart, truncating the melody and
    /// stranding whatever chord was held.
    #[test]
    fn victory_survives_death_restart() {
        let mut g = test_game();
        g.load_sho_midi(&tiny_victory_smf()).expect("load");
        g.play_victory_music();
        g.update(0.01, Input::default());
        let _ = g.take_synth();
        assert!(g.sho_playing_for_test());

        g.force_restart_at_start_for_test();
        g.update(0.01, Input::default());
        assert!(
            g.sho_playing_for_test(),
            "restart must not stop the global victory sequence"
        );
    }

    /// Room (54,26) entrance starts victory; backtracking / start camera do not.
    #[test]
    fn victory_entrance_at_54_26() {
        let mut g = test_game();
        g.load_sho_midi(&tiny_victory_smf()).expect("load");

        // Start outdoor camera is skipped.
        g.test_mark_room_entrance_at(crate::worldmap::START_CAMERA);
        g.update(0.05, Input::default());
        assert!(
            !g.take_synth().iter().any(|e| matches!(e, SynthEvent::NoteOn(_))),
            "start camera must not play victory"
        );

        // First visit to victory room.
        g.test_mark_room_entrance_at((54, 26));
        g.update(0.05, Input::default());
        let synth = g.take_synth();
        assert!(
            synth.contains(&SynthEvent::NoteOn(72)) || synth.contains(&SynthEvent::AllNotesOff),
            "victory entrance should start sequence: {synth:?}"
        );
        // Drain / stop.
        g.update(1.0, Input::default());
        let _ = g.take_synth();

        // Re-enter same camera → backtracking → silent.
        g.test_mark_room_entrance_at((54, 26));
        g.update(0.05, Input::default());
        assert!(
            !g.take_synth().iter().any(|e| matches!(e, SynthEvent::NoteOn(_))),
            "backtracking must not replay victory"
        );
    }

    /// No MIDI loaded → entrance at (54,26) stays silent.
    #[test]
    fn victory_silent_without_midi() {
        let mut g = test_game();
        assert!(!g.sho_midi_loaded());
        g.test_mark_room_entrance_at((54, 26));
        g.update(0.5, Input::default());
        assert!(g.take_synth().is_empty());
    }


    /// Zip logo is a `mapsprites` oneshot — cleared on room change (`initbackground`).
    #[test]
    fn zip_logo_clears_on_room_exit() {
        let mut g = test_game();
        g.update(2.1, Input::default());
        for _ in 0..11 {
            g.update(0.05, Input::default());
        }
        g.update(0.05, Input::default());
        g.update(0.05, Input::default());
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert!(g.zip_anim_active());

        // North door exit from the start room (same id path as door_exit test).
        let exit_id = g
            .world
            .exit_at(109, 170)
            .expect("start north door")
            .id;
        g.facing = Facing::North;
        g.force_door_exit_for_test(exit_id);
        assert_ne!(g.camera, START_CAMERA, "must have entered the next room");
        assert!(
            !g.zip_anim_active(),
            "zip logo must not survive initbackground / mapsprites wipe"
        );
    }

    #[test]
    fn death_restart_skips_white_intro() {
        let mut g = test_game();
        assert!(g.skip_boot());
        assert!(g.intro_seen());
        // Simulate GameOver → restart path used by eitherbutton.
        g.state = GameState::GameOver;
        // restart_at_start is private; exercise via GameOver button after dead_ticks > 5.
        // Force via update: set dead_ticks by advancing, then press A.
        // Simpler: skip_boot already set intro_seen; call update from GameOver.
        for _ in 0..10 {
            g.update(0.05, Input::default());
        }
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert!(g.intro_seen());
        assert!(!g.screen_inverted());
    }

    #[test]
    fn worldmap_has_walkable_start() {
        let m = WorldMap::from_path(WorldMap::data_file_path()).unwrap();
        let t = m.tile(START_PLAY.0, START_PLAY.1);
        assert!(WorldMap::is_walkable_tile(t), "start tile {t}");
        assert!(m.selector_can_step(START_PLAY.0, START_PLAY.1));
        assert!(m.exits.len() >= 100);
    }

    #[test]
    fn selector_extends_north_from_spawn() {
        let mut g = test_game();
        // leave boot
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.016, input);
        // release A
        input = Input::default();
        g.update(0.016, input);

        assert_eq!(g.state, GameState::Aiming);

        // extend cursor north several times (selector:trymove 0,-1)
        for _ in 0..5 {
            input = Input::default();
            input.buttons.up = true;
            g.update(0.016, input);
            input = Input::default();
            g.update(0.016, input);
        }

        assert_eq!(g.player_x, START_PLAY.0);
        assert_eq!(g.player_y, START_PLAY.1);
        assert_eq!(g.selector_tile(), (START_PLAY.0, START_PLAY.1 - 5));

        // commit
        input.buttons.a = true;
        g.update(0.016, input);

        assert_eq!(g.player_x, START_PLAY.0);
        assert_eq!(g.player_y, START_PLAY.1 - 5);
    }

    #[test]
    fn selector_move_queues_select_sfx() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        let _ = g.take_sfx();
        input = Input::default();
        g.update(0.05, input);

        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(g.take_sfx(), vec![SfxId::Select]);
    }

    #[test]
    fn selector_hold_repeats_after_four_20hz_ticks() {
        // main.lua: ButtonDown → immediate step; buttonsdown>=4 → repeat each tick.
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        // Press and hold up for one frame (edge → one step)
        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (START_PLAY.0, START_PLAY.1 - 1));

        // Hold through ticks where old v is 1,2,3 — no further moves
        for _ in 0..3 {
            g.update(0.05, input);
        }
        assert_eq!(
            g.selector_tile(),
            (START_PLAY.0, START_PLAY.1 - 1),
            "no repeat before buttonsdown reaches 4"
        );

        // Next 20 Hz tick: old v == 4 → first auto-repeat
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (START_PLAY.0, START_PLAY.1 - 2));

        // Subsequent ticks keep stepping every frame
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (START_PLAY.0, START_PLAY.1 - 3));
    }

    #[test]
    fn north_door_is_selectable() {
        let m = WorldMap::from_path(WorldMap::data_file_path()).unwrap();
        assert!(WorldMap::is_door_tile(m.tile(109, 170)));
        assert!(m.selector_can_step(109, 170));
        assert!(m.exit_at(109, 170).is_some());
    }

    /// Outdoor start matches Lua: empty `localenemies` (no combatants on screen).
    #[test]
    fn start_room_has_no_enemies() {
        let g = test_game();
        assert!(
            g.room.enemies.is_empty(),
            "start roomtiles have no combatants; got {:?}",
            g.room.enemies
        );
    }

    /// Walking through the first north door must spawn the fixed swordsman at
    /// (108,160) — and must not drag off-screen weight-tile enemies from a
    /// rectangular viewport into AI.
    #[test]
    fn door_exit_spawns_fixed_enemy_in_next_room() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert!(g.room.enemies.is_empty());

        // Zip north onto the door at (109, 170); player starts at (109, 182).
        let steps = START_PLAY.1 - 170;
        for _ in 0..steps {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!(g.selector_tile(), (109, 170));
        // `selector:moveByTile` sets onExit/blockedDir to the arrival facing.
        assert_eq!(
            g.selector_exit_facing(),
            Some(Facing::North),
            "tip on exit object must arm onExit for exiticon tip"
        );

        input.buttons.a = true;
        g.update(0.05, input);

        // Door arms `onexit` immediately, but `exitroom` waits for the walk anim
        // (`faststep` for a long zip) — camera must not jump on the commit frame.
        assert_eq!(
            g.camera, START_CAMERA,
            "camera must stay put until the player move anim finishes"
        );
        assert_eq!(
            (g.player_x, g.player_y),
            (109, 170),
            "player stays on the door tile while the move anim plays"
        );

        // `faststep` = 4 poses × 2 ticks ≈ 8 frames at 20 Hz; pad a little.
        input = Input::default();
        for _ in 0..20 {
            g.update(0.05, input);
            if g.camera == (108, 162) {
                break;
            }
        }

        assert_eq!(
            g.camera,
            (108, 162),
            "north exit should set camera to exit.nx/ny after anim"
        );
        let alive: Vec<_> = g.room.enemies.iter().filter(|e| e.alive).collect();
        assert_eq!(
            alive.len(),
            1,
            "first interior room has one fixed swordsman, got {alive:?}"
        );
        assert_eq!((alive[0].x, alive[0].y), (108, 160));
        // Off-screen weight cells (e.g. 109,150) must not be local / pathing.
        assert!(
            !g.room
                .enemies
                .iter()
                .any(|e| e.y <= 155 && e.alive),
            "enemies from culled/off-screen components must not be in localenemies"
        );
    }

    /// Lua computes `cullroomtiles` once per room. The port used to recompute the
    /// enemy path fence from the player's live tile every enemy phase; standing on
    /// an exit then re-opened the far side of the door and let enemies path into
    /// the next room. The fence must stay frozen at the populate tile.
    #[test]
    fn player_on_door_does_not_reopen_room_fence() {
        let mut g = test_game();
        assert!(g.skip_boot());

        // Start-room cull: this-room floor stays; floor past the north door is out.
        assert!(g.path_roomtile_contains_for_test(109, 171));
        assert!(g.path_roomtile_contains_for_test(109, 170));
        assert!(!g.path_roomtile_contains_for_test(109, 169));
        assert!(!g.path_roomtile_contains_for_test(109, 168));

        // Mid-turn: the player stands on the door while the exit anim plays.
        let exit_id = g.world.exit_at(109, 170).expect("north door").id;
        g.facing = Facing::North;
        g.arm_door_exit_for_test(exit_id);
        assert_eq!((g.player_x, g.player_y), (109, 170));

        // Rebuilding the graph (as `resolve_enemies` does each enemy phase) must
        // not widen the fence around the player's door tile.
        g.construct_graphs_for_test();
        assert!(
            !g.path_roomtile_contains_for_test(109, 169),
            "player on the door must not re-open the floor north of it"
        );
        assert!(
            !g.path_roomtile_contains_for_test(109, 168),
            "player on the door must not re-open the next room"
        );
        assert!(
            g.path_roomtile_contains_for_test(109, 171),
            "this room's floor south of the door must stay connected"
        );
    }

    #[test]
    fn viewport_contains_player() {
        let m = WorldMap::from_path(WorldMap::data_file_path()).unwrap();
        let room = viewport_around(&m, START_CAMERA, 8, 8);
        assert!(room.walkable(START_PLAY.0, START_PLAY.1));
    }

    #[test]
    fn framebuffer_size() {
        assert_eq!(FRAMEBUFFER_BYTES, 400 * 240 / 8);
        assert_eq!(FRAMEBUFFER_BYTES, 12_000);
    }

    #[test]
    fn port_version_stamp_shape() {
        let s = port_version();
        assert!(s.starts_with('v'), "stamp={s}");
        assert!(s.len() >= 4, "stamp={s}");
    }

    /// Stepping onto an NPC pad (GID 253) shows monk/grave dialog and heals once.
    #[test]
    fn npc_pad_shows_dialog_and_heals() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let npc = g.world.npc_at(145, 166).expect("monk npc in worldmap");
        assert_eq!(npc.dialog, "monk");
        assert_eq!(npc.heal, 20);

        // Stand one south of the monk and zip onto the pad.
        g.player_x = 145;
        g.player_y = 167;
        let life_before = 100;
        g.life = life_before;
        g.reload_room_terrain_for_test((145, 166), 16, 16);

        input.buttons.b = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (145, 167));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (145, 166));

        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!((g.player_x, g.player_y), (145, 166));
        assert_eq!(g.life, life_before - 1 + 20, "walk spends 1, monk heals 20");
        // `dialog:update` opens the queued line on the next frame.
        g.update(0.05, Input::default());
        assert_eq!(g.state, GameState::Dialog, "monk dialog should open");

        g.dismiss_dialog_for_test();
        assert_eq!(g.state, GameState::Aiming);

        // Second visit: no further heal / dialog.
        let life2 = g.life;
        input = Input::default();
        input.buttons.b = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.down = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        g.drain_enemy_phase_for_test(); // deferred kEnemyMove locks aim until done
        input = Input::default();
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming, "revisiting NPC must not re-show");
        assert_eq!(g.life, life2 - 2, "only walk blood spend, no second heal");
    }

    #[test]
    fn isosprite_north_idle_frame() {
        assert_eq!(isosprite_frame_index(Facing::North, 1, 16), 48);
    }

    /// Stab: tip stops one cell short; A kills the enemy beyond the tip.
    #[test]
    fn stab_kills_enemy_beyond_tip() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Enemy two tiles north of player → aim one north (tip), stab beyond.
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px, py - 2, Facing::South as u8));

        // Extend tip one step north (cannot step onto the enemy at py-2).
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        // Commit zip → stab kill, land on tip, enemy dead.
        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!(g.player_x, px);
        assert_eq!(g.player_y, py - 1);
        assert!(!g.room.enemies[0].alive, "stab should kill enemy beyond tip");
        // Surviving a stab: player still near spawn (restart would snap to START_PLAY
        // only if already there — use life drain + alive enemy absence as signal).
        assert_eq!(
            (g.player_x, g.player_y),
            (px, py - 1),
            "player must remain on tip after stab, not restart"
        );
    }

    /// Slash: zip past an enemy on an adjacent parallel cell.
    #[test]
    fn slash_kills_enemy_beside_path() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Enemy one east of the cell one north of player: zip north past them.
        // Path: (px, py-1), (px, py-2), (px, py-3). Side slash at second_y when
        // step reaches the cell after the enemy's row.
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px + 1, py - 2, Facing::West as u8));

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!(g.selector_tile(), (px, py - 3));

        input.buttons.a = true;
        g.update(0.05, input);

        // Lua slash: A only enters kPlayerMove; player stays put until ticks > 3.
        assert_eq!(
            (g.player_x, g.player_y),
            (px, py),
            "slash wind-up keeps player on start tile"
        );
        assert!(
            g.room.enemies[0].alive,
            "enemy still alive during slash wind-up"
        );
        assert!(
            g.pending_slash_active(),
            "slash commit should arm pending kPlayerMove"
        );

        // ticks 1..=3: still wind-up; tick 4 (ticks > 3) resolves kill+land+trail.
        g.advance_sim_ticks(3);
        assert!(
            g.room.enemies[0].alive,
            "kill must not fire before ticks > 3"
        );
        assert_eq!((g.player_x, g.player_y), (px, py));

        g.advance_sim_ticks(1);
        assert!(!g.pending_slash_active());
        assert_eq!((g.player_x, g.player_y), (px, py - 3));
        assert!(
            !g.room.enemies[0].alive,
            "slash should kill enemy beside the zip path"
        );
        // Same resolve frame: land + kill FX + floorsmoke (`moveToTile` → do_smoke).
        assert!(
            g.kill_fx_count() > 0,
            "enemy slashed anim starts on the land frame"
        );
        assert!(
            g.floorsmoke_active_count() >= 3,
            "trail smoke spawns on the same frame as land+kill"
        );
        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::Slash),
            "slash kill should queue slash SFX, got {sfx:?}"
        );
        assert!(
            sfx.iter().any(|s| matches!(
                s,
                SfxId::Falldead | SfxId::Falldead2 | SfxId::Falldead3 | SfxId::Falldead4
            )),
            "slash kill should queue falldead SFX, got {sfx:?}"
        );
        assert!(
            sfx.contains(&SfxId::Blood),
            "slash kill should queue blood SFX, got {sfx:?}"
        );
        assert!(
            g.floor_blood_count() > 0,
            "slash kill should leave floor blood"
        );
        assert_eq!(
            g.corpse_count(),
            1,
            "killed enemy must remain as a corpse in localenemies"
        );

        // After `slashed*` finishes, the body stays on the `dead` pose (Lua isosprite
        // holds the last frame; we keep `alive=false` and draw `enemy_corpse_pose`).
        input = Input::default();
        for _ in 0..40 {
            g.update(0.05, input);
            if g.kill_fx_count() == 0 {
                break;
            }
        }
        assert_eq!(g.kill_fx_count(), 0, "slashed anim should finish");
        assert_eq!(
            g.corpse_count(),
            1,
            "corpse must remain after death anim ends (not only blood)"
        );
        assert!(
            g.floor_blood_count() > 0,
            "floor blood stays in-room until exitroom"
        );
    }

    /// Outdoor zip must not move `cameratile` — smoke shares the pre-zip screen frame.
    #[test]
    fn zip_keeps_outdoor_camera() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let cam_before = g.camera;
        for _ in 0..5 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!(
            g.camera, cam_before,
            "outdoor zip must not recenter cameratile (smoke timing)"
        );
        assert_ne!(
            (g.player_x, g.player_y),
            START_PLAY,
            "player should have moved"
        );
    }

    /// Leaving a tile stamps a blood footprint (`samurai:trailBlood`).
    #[test]
    fn zip_stamps_trail_blood_on_departure_tile() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        assert_eq!(g.trail_stamp_count(), 0);

        // Aim one north and zip.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!((g.player_x, g.player_y), (px, py - 1));
        assert_eq!(
            g.trail_stamp_count(),
            1,
            "trailBlood stamps the tile the player left"
        );
        // Second zip leaves another stamp (after deferred enemy phase unlocks aim).
        g.drain_enemy_phase_for_test();
        input = Input::default();
        g.update(0.05, input);
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.trail_stamp_count(), 2);
    }

    /// Multi-step zip spawns floor smoke behind the landing tile (`smoker:do_smoke`).
    #[test]
    fn floorsmoke_spawns_on_multistep_zip() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!(g.selector_tile(), (START_PLAY.0, START_PLAY.1 - 3));

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(
            g.floorsmoke_active_count() >= 3,
            "do_smoke should place one puff per step behind the player"
        );
    }

    /// One-tile zip does not spawn smoke (`numsteps > 1` gate).
    #[test]
    fn floorsmoke_skips_single_step_zip() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!(
            g.floorsmoke_active_count(),
            0,
            "single-step zip must not spawn floorsmoke"
        );
        assert!(
            g.take_sfx().contains(&SfxId::Step),
            "walk zip with numsteps < 5 should play step"
        );
    }

    /// Long no-kill zip uses `swoosh` (`samurai:step` when numsteps >= 5).
    #[test]
    fn long_walk_zip_queues_swoosh() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.room.enemies.clear();
        for _ in 0..5 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        input.buttons.a = true;
        g.update(0.05, input);

        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::Swoosh),
            "walk zip with numsteps >= 5 should play swoosh, got {sfx:?}"
        );
        assert!(
            !sfx.contains(&SfxId::Slash),
            "no-kill walk must not play slash"
        );
    }

    /// After a slash kill lands, a second adjacent enemy must not revenge-kill
    /// on the same frame — deferred `kEnemyMove` burns the readybar first.
    #[test]
    fn kill_then_revenge_is_not_same_frame() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        assert!(g.room.walkable(px, py - 1));
        assert!(g.room.walkable(px, py - 2));
        assert!(g.room.walkable(px, py - 3));
        assert!(g.room.walkable(px + 1, py - 3));

        // Slash target beside the tip; second swordsman already adjacent to tip
        // so braintwo will kill once the deferred enemy phase finishes.
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px + 1,
            py - 2,
            Facing::West as u8,
        ));
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px + 1,
            py - 3,
            Facing::West as u8,
        ));

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!(g.selector_tile(), (px, py - 3));

        input.buttons.a = true;
        g.update(0.05, input);
        g.advance_sim_ticks(4); // slash wind-up → kill+land

        assert!(!g.room.enemies[0].alive, "slash should kill the path-side enemy");
        assert!(g.room.enemies[1].alive, "adjacent threat still alive on land frame");
        assert!(
            g.pending_enemy_active(),
            "land arms deferred kEnemyMove, does not braintwo yet"
        );
        assert!(
            !g.death_hold_active(),
            "player must survive the kill/land frame"
        );

        // First deferred tick is a step (not braintwo), even with a kill dialog open.
        g.advance_sim_ticks(1);
        assert!(
            !g.death_hold_active(),
            "first enemy-phase tick is a step, not braintwo"
        );
        g.drain_enemy_phase_for_test();
        assert!(
            g.death_hold_active(),
            "revenge kill should land after deferred enemy phase"
        );
        assert!(g.enemy_attack_fx_count() > 0);
    }

    /// Wrong zip with no kill: enemy in reach walks adjacent (never onto the
    /// player tile) and `braintwo` kills the player.
    #[test]
    fn enemy_reaches_and_kills_after_bad_zip() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Outdoor spawn has clear north lane; east may be blocked.
        assert!(g.room.walkable(px, py - 1), "need walkable path");
        assert!(g.room.walkable(px, py - 2), "need walkable approach");
        assert!(g.room.walkable(px, py - 3), "need walkable enemy cell");

        // Enemy three north of spawn. Zip one north → land at (px, py-1).
        // Manhattan to enemy = 2; budget = 1 walk segment → enemy steps to
        // (px, py-2) (adjacent, not onto player) → braintwo kills.
        // Tip must NOT be one short of the enemy (that would be a stab at py-2).
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px, py - 3, Facing::South as u8));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);

        // Land this frame; deferred kEnemyMove must not kill yet.
        assert_eq!((g.player_x, g.player_y), (px, py - 1));
        assert!(
            g.pending_enemy_active(),
            "walk zip should arm deferred kEnemyMove"
        );
        assert!(
            !g.death_hold_active(),
            "player must not die on the same frame as the zip land"
        );

        // 1-step walk readybar = extent + bonus walk → 2 step ticks, then braintwo.
        g.advance_sim_ticks(1);
        assert!(
            !g.death_hold_active(),
            "first enemy-phase tick is a step, not braintwo"
        );
        g.drain_enemy_phase_for_test();

        // Death holds on the landing tile with `slashed` + playerdeath before restart.
        assert_eq!(
            (g.player_x, g.player_y),
            (px, py - 1),
            "player should stay on death tile while slashed plays"
        );
        assert!(
            g.death_hold_active(),
            "death hold should start when the enemy kills the player"
        );
        assert!(
            g.enemy_attack_fx_count() > 0,
            "killer should play stab anim"
        );
        assert!(
            g.blood_spray_count() > 0,
            "player death should spawn bleed spray"
        );
        assert_eq!(
            g.drip_fx_count(),
            5,
            "sploosh should spawn dripsC + N/S/E/W"
        );
        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::PlayerDeath),
            "player death should queue playerdeath SFX, got {sfx:?}"
        );
        assert!(
            sfx.contains(&SfxId::Slash),
            "killer stab should queue slash SFX, got {sfx:?}"
        );
        assert!(
            sfx.contains(&SfxId::Blood),
            "player bleed should queue blood SFX, got {sfx:?}"
        );

        // Death dialog (1.3s delay) → A dismiss → GameOver restart prompt → any button.
        g.dismiss_dialog_for_test();
        assert_eq!(
            (g.player_x, g.player_y),
            START_PLAY,
            "player should restart after dismissing the death dialog and pressing a button"
        );
        assert_eq!(g.state, GameState::Aiming);
    }

    /// Off-axis approach: first `pf` hop is a same-tile 90° turn (costs a
    /// readybar segment); the next hop walks without changing facing.
    /// `pathfinding.lua` `pf` + `isosprite:moveToTile(..., false)`.
    #[test]
    fn enemy_turn_costs_a_move_before_walk() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        assert!(g.room.walkable(px, py - 1));
        assert!(g.room.walkable(px, py - 2));
        assert!(g.room.walkable(px, py - 3));

        g.room.enemies.clear();
        // Three north, facing east — must turn south before walking toward player.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 3,
            Facing::East as u8,
        ));

        g.run_enemy_phase_for_test(1);
        let e = &g.room.enemies[0];
        assert_eq!(
            (e.x, e.y),
            (px, py - 3),
            "first hop must be turn-in-place, not a walk"
        );
        assert_eq!(
            e.facing,
            Facing::South as u8,
            "first hop faces the approach axis (90° turn)"
        );
        assert!(
            !g.death_hold_active(),
            "still two tiles away after the turn"
        );

        g.run_enemy_phase_for_test(1);
        let e = &g.room.enemies[0];
        assert_eq!(
            (e.x, e.y),
            (px, py - 2),
            "second hop walks one cell south"
        );
        assert_eq!(
            e.facing,
            Facing::South as u8,
            "walk hop must not reface (moveToTile changeFacing=false)"
        );
    }

    /// Crank ±15° while aiming advances / rewinds ghosts; tip move resets.
    /// Cap = readybar length. Enemy sits off the zip axis so it is not on deathlist.
    #[test]
    fn crank_ghost_preview_steps_cap_rewind_reset() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // South of the player while we aim north — clear of slash adjacency / stab tip.
        let mut spot = None;
        for dy in [1, 2, 3] {
            if g.room.walkable(px, py + dy) {
                spot = Some((px, py + dy));
                break;
            }
        }
        let (ex, ey) = spot.expect("need a walkable tile south of player");
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            ex,
            ey,
            Facing::North as u8,
        ));
        g.sync_enemy_ghosts_for_test();

        // Aim three cells north (walk + auto walk insert → ≥2 segments).
        for _ in 0..3 {
            input = Input::default();
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }

        assert!(
            g.aim_plan_len_for_test() >= 2,
            "need a multi-segment aim for crank preview, got {}",
            g.aim_plan_len_for_test()
        );
        assert!(
            g.aim_plan_deathlist_empty_for_test(),
            "south enemy must not be on a north-aim deathlist"
        );
        assert_eq!(g.ghost_step_count(), 0);
        assert_eq!(g.state, GameState::Aiming);

        // +16° → one ghost step + zzt.
        g.crank_for_test(16.0);
        let sfx = g.take_sfx();
        assert!(
            sfx.iter().any(|s| *s == SfxId::Zzt),
            "crank step plays zzt, sfx={sfx:?}"
        );
        assert_eq!(g.ghost_step_count(), 1);
        assert!(
            g.ghost_tile_for_test(0).is_some() || g.player_outline_for_test(),
            "ghost step should outline player and/or show a ghost"
        );

        // Cap: crank past segment count should no-op further advances.
        let cap = g.aim_plan_len_for_test() as i32;
        for _ in 0..cap + 3 {
            g.crank_for_test(16.0);
        }
        assert!(
            g.ghost_step_count() <= cap,
            "ghostStep capped by readybar length"
        );

        // Rewind one.
        let before = g.ghost_step_count();
        g.crank_for_test(-16.0);
        assert_eq!(g.ghost_step_count(), before - 1);

        // Tip nudge → reset.
        input = Input::default();
        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(g.ghost_step_count(), 0);
        assert!(g.ghost_tile_for_test(0).is_none());
    }

    /// Zip onto a cell that leaves an enemy already adjacent: braintwo kills
    /// without needing a path step. Tip must not register a stab on them.
    #[test]
    fn adjacent_enemy_kills_without_extra_steps() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Enemy one north of the tip cell we'll land on, but NOT beyond the tip
        // on the zip axis. Zip east to (px+1, py); enemy at (px+1, py-1) is
        // adjacent (manhattan 1) without being a stab target (stab looks at
        // (px+2, py)).
        assert!(g.room.walkable(px + 1, py));
        assert!(g.room.walkable(px + 1, py - 1));
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px + 1, py - 1, Facing::South as u8));

        input.buttons.right = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px + 1, py));

        input.buttons.a = true;
        g.update(0.05, input);

        assert_eq!((g.player_x, g.player_y), (px + 1, py));
        assert!(
            g.pending_enemy_active(),
            "adjacent threat still waits for deferred braintwo"
        );
        assert!(!g.death_hold_active(), "no same-frame revenge kill");

        // Readybar = extent + bonus walk → drain steps, then braintwo.
        g.drain_enemy_phase_for_test();

        assert_eq!(
            (g.player_x, g.player_y),
            (px + 1, py),
            "player should stay on death tile while slashed plays"
        );
        assert!(
            g.enemy_attack_fx_count() > 0,
            "adjacent killer should play stab"
        );
        assert_eq!(g.drip_fx_count(), 5, "death should sploosh drips");
        assert!(
            g.blood_spray_count() > 0,
            "slash death should bleed-spray"
        );
        assert!(
            g.take_sfx().contains(&SfxId::PlayerDeath),
            "adjacent melee kill should queue playerdeath"
        );

        g.dismiss_dialog_for_test();
        assert_eq!(
            (g.player_x, g.player_y),
            START_PLAY,
            "landing adjacent to a living swordsman must kill then restart"
        );
    }

    /// Walk zips spend 1 blood (`samurai:step`); slash/stab zips do not.
    #[test]
    fn walk_zip_spends_blood_slash_does_not() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert_eq!(
            g.life, 249,
            "teleport spawn spends step(0) → maxBlood-1"
        );
        let life0 = g.life;

        // Pure walk north one step.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.life, life0 - 1, "walk zip spends 1 blood");
        g.drain_enemy_phase_for_test();

        let px = g.player_x;
        let py = g.player_y;
        let life1 = g.life;
        // Stab: enemy two north of tip one north.
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px, py - 2, Facing::South as u8));
        input = Input::default();
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(
            g.life, life1,
            "stab/slash zip must not spend blood (samurai:slash/stab)"
        );
        assert!(!g.room.enemies[0].alive);
    }

    /// Lua: corpses stay in `globalenemies` across `initbackground`; floor blood in
    /// `mapsprites` is wiped. Re-entering a room reattaches dead enemies as corpses.
    #[test]
    fn corpses_persist_across_room_reload_blood_clears() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.inject_global_enemy_for_test(Enemy::new(EnemyKind::Swordsman, px + 1, py - 2, Facing::West as u8));

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        input.buttons.a = true;
        g.update(0.05, input);
        // Slash resolve is deferred (`ticks > 3`); wait for kill+land.
        g.advance_sim_ticks(4);

        assert_eq!(g.corpse_count(), 1);
        assert!(g.floor_blood_count() > 0);
        assert!(
            g.global_has_corpse_at(px + 1, py - 2),
            "death must sync into globalenemies"
        );

        // Simulate `initbackground`: wipe mapsprites (blood) + kill FX, rebuild locals
        // from globals (`findlocalenemies` phase 1 — includes alive=false).
        input = Input::default();
        for _ in 0..40 {
            g.update(0.05, input);
            if g.kill_fx_count() == 0 {
                break;
            }
        }
        g.clear_room_blood_for_test();
        g.reload_locals_for_test(Facing::South);

        assert_eq!(
            g.corpse_count(),
            1,
            "dead enemy must reattach as a corpse when reloading the room"
        );
        assert_eq!(
            g.floor_blood_count(),
            0,
            "floor blood must not survive initbackground / room exit"
        );
        assert!(
            !g.room.enemies.iter().any(|e| e.alive),
            "corpse must not respawn as a living enemy"
        );
    }

    /// Floorspray from a kill blinds a living neighbor (`enemy:bleed` → `stun`).
    #[test]
    fn kill_blood_stuns_nearby_enemy() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Stab: tip at (px, py-1), victim beyond tip at (px, py-2).
        // On-axis kill_dir = South → floorspray includes (px+1, py-3).
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px, py - 2, Facing::South as u8));
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px + 1, py - 3, Facing::South as u8));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(!g.room.enemies[0].alive, "victim must die");
        assert!(g.room.enemies[1].alive, "neighbor must survive");
        // bleed sets stunned=2 on the kill frame; braintwo (later) decrements.
        assert_eq!(
            g.enemy_stunned(1),
            2,
            "neighbor should be blood-blinded at stun=2 before deferred braintwo"
        );
        assert!(
            g.has_blinded_enemies(),
            "first blind should set hasBlindedEnemies"
        );
        g.drain_enemy_phase_for_test();
        assert_eq!(
            g.enemy_stunned(1),
            1,
            "braintwo decrements stun once when the bar empties"
        );
        // Neighbor must not have closed the gap this phase (stunned skips step_enemies).
        assert_eq!(
            (g.room.enemies[1].x, g.room.enemies[1].y),
            (px + 1, py - 3),
            "stunned enemy must not path this phase"
        );
    }

    /// Sploosh drips stay on the last frame (`hideOnFinish = false`).
    #[test]
    fn death_sploosh_drips_persist() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 3,
            Facing::South as u8,
        ));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        g.drain_enemy_phase_for_test();
        assert_eq!(g.drip_fx_count(), 5);

        // Drip anim is 8 poses @ 10fps ≈ 16 ticks; wait well past finish.
        g.advance_sim_ticks(60);
        assert_eq!(
            g.drip_fx_count(),
            5,
            "sploosh drips must remain after the oneshot (hideOnFinish=false)"
        );
        assert!(!g.screen_inverted(), "no invert flash during death dialog");
    }

    /// Fallen `slashed` pose must stay after the oneshot ends (dialog must not idle-stand).
    #[test]
    fn death_pose_holds_through_dialog() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 3,
            Facing::South as u8,
        ));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        g.drain_enemy_phase_for_test();
        assert!(g.death_hold_active());
        assert!(g.player_anim_active(), "slashed anim starts on kill");

        // `PLAYER_SLASHED` is 9 poses @ 10fps ≈ 18 ticks; wait well past finish + dialog open.
        g.advance_sim_ticks(60);
        assert!(
            g.death_hold_active(),
            "still dead while dialog / pending restart"
        );
        assert!(
            g.player_anim_active(),
            "death pose must remain while dialog is up (not idle stand)"
        );
        assert_eq!(
            g.player_pose_1based(),
            11,
            "held pose should be final slashed cell"
        );
    }

    /// Death dialog tick 30 → deathmusic; A hide → Playing-dead → ticks>30 → GameOver.
    /// Combat kills keep blood > 0, so Lua needs A to dismiss then another button to restart.
    #[test]
    fn death_dialog_plays_deathmusic_then_game_over_prompt() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 3,
            Facing::South as u8,
        ));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        g.drain_enemy_phase_for_test();
        assert!(g.death_hold_active());

        // Open delay 1.3s + dialog ticks to 30 → deathmusic (`main.lua`).
        let mut heard_deathmusic = false;
        for _ in 0..80 {
            g.update(0.05, Input::default());
            if g.take_sfx().contains(&SfxId::DeathMusic) {
                heard_deathmusic = true;
                break;
            }
        }
        assert!(
            heard_deathmusic,
            "dialog tick 30 while dead should queue deathmusic"
        );
        assert_eq!(g.state, GameState::Dialog);
        assert!(!g.screen_inverted(), "no invert flash during dialog");

        // A dismiss with blood > 0 → Playing (dead), not GameOver yet.
        input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(
            g.state,
            GameState::Aiming,
            "dialog:hide with blood>0 returns to Playing while dead"
        );
        assert_eq!((g.player_x, g.player_y), (px, py - 1));

        // Playing + dead + ticks > 30 → GameOver + restart sprite (no invert; ticks already high).
        g.advance_sim_ticks(2);
        assert_eq!(g.state, GameState::GameOver);
        assert!(!g.screen_inverted(), "combat GameOver enters with ticks>30 → no flash");
        assert_eq!(g.drip_fx_count(), 5, "blood drips still visible on game over");

        // Second press restarts (`eitherbuttondown`).
        input = Input::default();
        input.buttons.b = true;
        g.update(0.05, input);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert_eq!(g.state, GameState::Aiming);
    }

    /// System-menu seppuku: pierce self-kill, no death dialog, then GameOver after ticks.
    #[test]
    fn seppuku_pierce_death_then_game_over() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        let life0 = g.life;
        assert!(life0 > 0);

        g.seppuku();
        assert!(g.death_hold_active(), "seppuku marks the player dead");
        assert_ne!(
            g.state,
            GameState::Dialog,
            "seppuku must not open playerKilled (Lua samurai:kill alone)"
        );
        assert_eq!(g.life, life0, "seppuku does not spend blood");
        // Host must drain *before* the next `update` — `update` clears the queue
        // at entry, so a wasm `frame` after a bare `seppuku()` would drop the SFX
        // (see ZipperApp::seppuku flush).
        assert!(
            g.take_sfx().contains(&SfxId::PlayerDeath),
            "seppuku plays playerdeath"
        );
        assert_eq!(g.drip_fx_count(), 5, "pierce path still splooshes drips");

        // Playing-dead + ticks > 30 → GameOver (same path as combat after dialog hide).
        g.advance_sim_ticks(32);
        assert_eq!(g.state, GameState::GameOver);

        input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert_eq!(g.state, GameState::Aiming);
        assert!(!g.death_hold_active());
    }

    /// `#god` blocks seppuku the same way it blocks combat death.
    #[test]
    fn seppuku_blocked_by_god_mode() {
        let mut g = test_game();
        g.set_god_mode(true);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.seppuku();
        assert!(!g.death_hold_active());
        assert_eq!(g.state, GameState::Aiming);
    }

    /// Boot seed is kept across death restart (`main.lua` does not re-roll).
    #[test]
    fn death_restart_keeps_random_seed() {
        use crate::level::{Enemy, EnemyKind};

        let seed = 42u32;
        let mut g = test_game_seed(seed);
        assert_eq!(g.random_seed(), seed);

        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(EnemyKind::Swordsman, px, py - 3, Facing::South as u8));

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        // Deferred kEnemyMove: land ≠ death; drain steps + braintwo.
        assert!(!g.death_hold_active());
        g.drain_enemy_phase_for_test();
        assert!(g.death_hold_active());

        g.dismiss_dialog_for_test();
        assert_eq!((g.player_x, g.player_y), START_PLAY);
        assert_eq!(
            g.random_seed(),
            seed,
            "death restart must keep the boot seed (Lua startGame does not re-roll)"
        );
    }

    /// Different boot seeds can change weight-tile enemy placement in room 2.
    #[test]
    fn different_seeds_can_shift_weight_spawns() {
        let a = test_game_seed(1);
        let b = test_game_seed(2);
        assert_ne!(a.random_seed(), b.random_seed());
        // Both still boot into the empty outdoor start.
        assert!(a.room.enemies.is_empty());
        assert!(b.room.enemies.is_empty());
    }

    /// Pikeman spawns raised; braintwo lowers tip onto body+facing when ahead is free.
    #[test]
    fn pikeman_lowers_tip_after_enemy_phase() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        // Outdoor north lane is clear. Body three north, facing south → tip on
        // (px, py-2). Tip→player manhattan is 2 so no pierce this turn.
        g.room.enemies.clear();
        let pike = Enemy::new(EnemyKind::Pikeman, px, py - 3, Facing::South as u8);
        assert!(pike.pike_up, "Lua init raises the pike");
        assert_eq!((pike.child_x, pike.child_y), (pike.x, pike.y));
        g.room.enemies.push(pike);

        g.run_enemy_phase_for_test(0);

        assert!(!g.room.enemies[0].pike_up, "braintwo lowers tip when ahead is free");
        assert_eq!(
            (g.room.enemies[0].child_x, g.room.enemies[0].child_y),
            (px, py - 2),
            "lowered tip sits on body + facing"
        );
        assert!(
            g.room.enemies[0].alive && !g.death_hold_active(),
            "tip two tiles away must not pierce yet"
        );
    }

    /// Lowered tip adjacent to the player with matching facing → pierce kill.
    #[test]
    fn pikeman_tip_pierce_kills_player() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Body two north; tip already on (px, py-1); facing south at the player.
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 2, Facing::South as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);

        g.run_enemy_phase_for_test(0);

        assert!(
            g.death_hold_active() || g.enemy_attack_fx_count() > 0,
            "tip adjacency + facing should pierce the player"
        );
    }

    /// `#god` draws `tile_x,tile_y` just left of the Life / key HUD.
    #[test]
    fn god_mode_draws_player_tile_coords() {
        let mut g = test_game_seed(840993252);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        // Snapshot the HUD band without #god (key may already be absent).
        g.player_x = 139;
        g.player_y = 69;
        g.reset_selector_for_test();
        g.update(0.05, Input::default());
        let mut before = [false; 24 * 7];
        for y in 0..7u32 {
            for x in 0..24u32 {
                before[(y * 24 + x) as usize] = g.fb.get_pixel(286 + x, 2 + y);
            }
        }

        g.set_god_mode(true);
        g.update(0.05, Input::default());
        let mut changed = 0u32;
        let mut light = 0u32;
        for y in 0..7u32 {
            for x in 0..24u32 {
                let now = g.fb.get_pixel(286 + x, 2 + y);
                if now != before[(y * 24 + x) as usize] {
                    changed += 1;
                }
                if !now {
                    light += 1;
                }
            }
        }
        assert!(
            changed > 20,
            "enabling #god must rewrite the coord band left of Life/key (changed={changed})"
        );
        assert!(
            light > 10,
            "coord digits must leave light pixels in the matte (light={light})"
        );
    }

    /// `#god` cheat: enemy pierce does not kill; walk does not spend blood;
    /// castle key is granted immediately (and kept across restart).
    #[test]
    fn god_mode_blocks_death_and_blood_spend() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        assert!(!g.has_key());
        g.set_god_mode(true);
        assert!(g.has_key(), "#god must grant chester.hasKey");
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let life0 = g.life;
        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 2, Facing::South as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);

        g.run_enemy_phase_for_test(0);
        assert!(!g.death_hold_active(), "god mode must ignore enemy kill");
        assert_eq!(g.enemy_attack_fx_count(), 0);

        // Walk zip would normally spend 1 blood.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.life, life0, "god mode must not spend blood on walk");
        assert!(g.has_key(), "key must still be held after play");
    }

    /// `#god` keeps the castle key across death restart (flag survives; key restored).
    #[test]
    fn god_mode_keeps_key_across_restart() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        assert!(g.has_key());

        // Seppuku is blocked while god is on — turn it off briefly to force a
        // death→GameOver→restart, then confirm god (still intended) re-grants.
        g.set_god_mode(false);
        // Turning god off does not strip an already-held key; clear via seppuku restart.
        g.seppuku();
        assert!(g.death_hold_active());
        g.dismiss_dialog_for_test();
        assert!(!g.has_key(), "normal restart clears the key");

        g.set_god_mode(true);
        assert!(g.has_key(), "re-enabling #god must grant the key again");
    }

    /// God-tools Key switch: invuln stays on when the key is cleared.
    #[test]
    fn god_key_switch_clears_without_losing_invuln() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        g.set_god_mode(true);
        assert!(g.invulnerable());
        assert!(g.has_key());
        assert!(g.cheat_key());

        g.set_cheat_key(false);
        assert!(!g.has_key());
        assert!(!g.cheat_key());
        assert!(g.invulnerable(), "clearing key must not drop invuln");

        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let life0 = g.life;
        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 2, Facing::South as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);

        g.run_enemy_phase_for_test(0);
        assert!(!g.death_hold_active(), "invuln must ignore enemy kill");

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.life, life0, "invuln must not spend blood on walk");

        // Chest pickup still works with key switch off.
        let chest = g.chest_pos().expect("run must roll a chest");
        g.set_chest_pos_for_test(chest);
        g.player_y = chest.1 + 1;
        g.player_x = chest.0;
        g.facing = Facing::North;
        g.reset_selector_for_test();
        input = Input::default();
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        for _ in 0..30 {
            if g.has_key() {
                break;
            }
            g.update(0.05, input);
        }
        assert!(g.has_key(), "chest pickup must still grant the key");
    }

    /// Invulnerability switch off allows blood spend while god tools stay unlocked.
    #[test]
    fn god_invuln_switch_off_spends_blood() {
        let mut g = test_game();
        g.set_god_mode(true);
        g.set_invulnerable(false);
        assert!(g.god_mode());
        assert!(!g.invulnerable());

        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let life0 = g.life;
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.life, life0 - 1, "invuln off must spend blood on walk");
    }

    #[test]
    fn god_teleport_moves_player_and_camera() {
        let mut g = test_game();
        assert!(!g.god_teleport(109, 170), "teleport requires god mode");
        g.set_god_mode(true);
        assert!(g.skip_boot());
        assert!(g.god_teleport(109, 170));
        assert_eq!((g.player_x, g.player_y), (109, 170));
        assert_ne!(
            g.camera, START_CAMERA,
            "north door tile should pick a room camera near the exit"
        );
        assert_eq!(g.player_tile(), (109, 170));
    }

    /// Cold `chester:reset` draws from `SpawnRng(random_seed)` (boot reseed),
    /// not the FX xorshift — otherwise every run lands on the same pad.
    #[test]
    fn chest_roll_depends_on_random_seed() {
        let a = test_game_seed(840993252);
        let b = test_game_seed(1);
        let c = test_game_seed(840993252);
        let chest_a = a.chest_pos().expect("seed 840993252 must roll a chest");
        let chest_b = b.chest_pos().expect("seed 1 must roll a chest");
        assert_ne!(
            chest_a, chest_b,
            "different seeds must be able to pick different chest pads"
        );
        assert_eq!(
            chest_a,
            c.chest_pos().unwrap(),
            "same cold seed must roll the same first chest"
        );
        // xoshiro256** cold roll for this seed (export-order pad index 3).
        assert_eq!(chest_a, (151, 129));
    }

    /// Death `startGame` → `chester:reset` uses the *live* RNG (advanced by
    /// room `randomseed(seed+cam)`), so the chest pad can move — Lua-hardcore.
    #[test]
    fn death_restart_can_move_chest() {
        let mut g = test_game_seed(840993252);
        let first = g.chest_pos().expect("first chest");
        // Boot roll notifies once; drain so death can fire a fresh notify.
        assert_eq!(g.take_chest_spawn(), Some(first));
        assert_eq!(g.take_chest_spawn(), None);
        // Simulate a room enter that reseeds/advances the live stream the way
        // `findlocalenemies` does after the first `chester:reset`.
        g.advance_run_rng_for_test();
        g.force_restart_at_start_for_test();
        assert!(!g.has_key(), "death restart clears the key");
        let second = g.chest_pos().expect("second chest");
        assert_ne!(
            first, second,
            "live-RNG chester:reset should be able to pick a new pad after the stream advances"
        );
        assert_eq!(
            g.take_chest_spawn(),
            Some(second),
            "death chester:reset must notify the host again"
        );
        assert_eq!(
            g.random_seed(),
            840993252,
            "random_seed itself must not change on death"
        );
    }

    /// Win-scene `initbackground` reseeds `run_rng` at each fixed pan camera
    /// (`main.lua` `kGameWinState` → `findlocalenemies` → `randomseed(seed+cam)`),
    /// so the *last* pan fixes the next `startGame`'s `chester:reset`. Two wins
    /// must therefore roll the same chest.
    #[test]
    fn win_restart_uses_fixed_win_camera_reseed() {
        fn cycle_win(g: &mut Game) -> (i32, i32) {
            g.force_win_for_test();
            drain_win_transition(g);
            // Walk the sea pans to the credits camera (187,17), the last reseed.
            win_steps(g, 601);
            assert_eq!(g.camera, (187, 17), "credits pan camera");
            let mut input = Input::default();
            input.buttons.a = true;
            g.update(0.05, input);
            assert_eq!(g.state, GameState::Score);
            for _ in 0..26 {
                g.update(0.05, Input::default());
            }
            let mut input = Input::default();
            input.buttons.a = true;
            g.update(0.05, input);
            assert_eq!(g.state, GameState::Aiming);
            g.chest_pos().expect("post-win chest")
        }

        let mut g = test_game_seed(840993252);
        assert!(g.skip_boot());
        let boot = g.chest_pos().expect("boot chest");
        let first_win = cycle_win(&mut g);
        let second_win = cycle_win(&mut g);
        assert_eq!(
            first_win, second_win,
            "post-win chest must be fixed by the deterministic win-camera reseed"
        );
        // The discovery is that it is stable, not merely that a chest exists.
        assert_ne!(boot, first_win, "boot chest may differ from the post-win chest");
    }

    /// Mobile Down-pad blur clear: only while SW chest `(145,176)` is present in-room.
    #[test]
    fn touch_down_blur_clear_only_for_sw_chest_room() {
        const SW: (i32, i32) = (145, 176);
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_chest_pos_for_test(SW);
        assert!(
            !g.should_clear_touch_down_blur(),
            "start room must not clear Down blur for SW chest"
        );

        // Enter the monk/chest outdoor room (north-door camera `(146,166)`).
        g.camera = (146, 166);
        g.player_x = 145;
        g.player_y = 170;
        g.reset_selector_for_test();
        assert!(
            g.should_clear_touch_down_blur(),
            "SW chest in its room must clear Down blur"
        );

        g.set_chest_pos_for_test((61, 103));
        assert!(
            !g.should_clear_touch_down_blur(),
            "different chest in SW room must keep Down blur"
        );

        g.set_chest_pos_for_test(SW);
        g.set_god_mode(true);
        g.set_cheat_key(true);
        assert!(
            !g.should_clear_touch_down_blur(),
            "key already taken must restore Down blur"
        );
    }

    /// Chest LCG must reach odd-index pads — regression for the old `seed | 1` bias.
    #[test]
    fn chest_roll_can_hit_odd_index_pads() {
        let pads = {
            let g = test_game_seed(1);
            g.world
                .chests
                .iter()
                .map(|c| (c.x, c.y))
                .collect::<Vec<_>>()
        };
        assert_eq!(pads.len(), 10);
        let mut hit_odd = false;
        let mut hit_even = false;
        // Consecutive epoch-like seeds + a few fixed ones.
        for seed in (0u32..200).chain([840993252, 0x5EED_2026, 0xDEAD_BEEF]) {
            let g = test_game_seed(seed);
            let pos = g.chest_pos().expect("chest");
            let idx = pads.iter().position(|&p| p == pos).expect("pad");
            if idx % 2 == 0 {
                hit_even = true;
            } else {
                hit_odd = true;
            }
            if hit_odd && hit_even {
                break;
            }
        }
        assert!(hit_even, "expected at least one even-index chest pad");
        assert!(hit_odd, "expected at least one odd-index chest pad");
    }

    /// Normal mode rolls one of the map chest pads; stepping on it grants the key.
    #[test]
    fn chest_pickup_grants_key_without_god() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert!(!g.has_key(), "fresh run must not start with the key");
        let chest = g.chest_pos().expect("chester:reset must pick a chest");
        assert!(
            g.world.chests.iter().any(|c| (c.x, c.y) == chest),
            "rolled chest {chest:?} must be one of the map pads"
        );

        // Place the player on the chest tile and resolve a zero-length "walk"
        // via finish path — zip onto the chest from an adjacent floor cell.
        let (cx, cy) = chest;
        // Prefer standing south of the chest and zipping north onto it.
        g.player_x = cx;
        g.player_y = cy + 1;
        g.facing = Facing::North;
        g.reset_selector_for_test();
        // Aim one step north onto the chest.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (cx, cy));
        let _ = g.take_sfx(); // discard aim/select SFX
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        // Short walk may resolve on the A frame (`finish_zip_resolve` → checkChest).
        let mut heard_key = g
            .take_sfx()
            .iter()
            .any(|s| matches!(s, crate::game::SfxId::Key));
        for _ in 0..30 {
            if g.has_key() {
                break;
            }
            g.update(0.05, input);
            heard_key |= g
                .take_sfx()
                .iter()
                .any(|s| matches!(s, crate::game::SfxId::Key));
        }
        assert!(
            g.has_key(),
            "stepping onto the chest tile must grant chester.hasKey"
        );
        assert!(heard_key, "pickup must queue soundm.key");
    }

    /// Embedded map ships the 10 outdoor chest pads from the Chests layer.
    #[test]
    fn worldmap_has_chest_pads() {
        let m = WorldMap::from_path(WorldMap::data_file_path()).unwrap();
        assert_eq!(m.chests.len(), 10, "Chests layer must export 10 pads");
    }

    /// Castle gate markers spawn as inanimate entrances and block the selector
    /// until the key slides them aside (`enemy:braintwo` inanimate branch).
    #[test]
    fn castle_gate_blocks_without_key_and_opens_with_key() {
        use crate::level::EnemyKind;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        // Approach camera that includes the south castle gates (54/55, 82).
        g.camera = (52, 85);
        g.player_x = 54;
        g.player_y = 84;
        g.facing = Facing::North;
        g.reload_room_terrain_for_test((52, 85), 16, 16);
        g.reload_locals_for_test(Facing::South);

        let gates: Vec<_> = g
            .room
            .enemies
            .iter()
            .filter(|e| e.kind.is_inanimate())
            .map(|e| (e.x, e.y, e.kind))
            .collect();
        assert!(
            gates.iter().any(|(x, y, k)| {
                *x == 54 && *y == 82 && matches!(k, EnemyKind::LeftSouthEntrance)
            }),
            "left south gate must spawn at (54,82), got {gates:?}"
        );
        assert!(
            gates.iter().any(|(x, y, k)| {
                *x == 55 && *y == 82 && matches!(k, EnemyKind::RightSouthEntrance)
            }),
            "right south gate must spawn at (55,82), got {gates:?}"
        );

        // Without the key, the gate tile blocks the selector.
        assert!(!g.has_key());
        g.reset_selector_for_test();
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_ne!(
            g.selector_tile(),
            (54, 82),
            "selector must not land on the locked gate"
        );

        // With the key, standing on the cell the gate faces slides it aside.
        g.set_god_mode(true);
        g.player_x = 54;
        g.player_y = 83;
        g.run_enemy_phase_for_test(0);
        let left = g
            .room
            .enemies
            .iter()
            .find(|e| matches!(e.kind, EnemyKind::LeftSouthEntrance))
            .expect("left gate");
        assert_eq!(
            (left.x, left.y),
            (53, 82),
            "left south gate slides west when player stands south of it"
        );
        assert!(
            g.room.enemy_at(54, 82).is_none(),
            "door tile must be clear after the gate opens"
        );
    }

    /// Castle exits are teleports: land on the destination exit tile with the
    /// destination camera (`samurai:exitroom` exitTeleport), not one step into void.
    #[test]
    fn castle_exit_teleports_into_interior() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        // South castle gate exit 568 → teleport 640 @ (23,35), camera (23,26) when
        // arriving from the south/west centers on the exit object.
        g.facing = Facing::North;
        g.force_door_exit_for_test(568);

        assert_eq!(
            (g.player_x, g.player_y),
            (23, 35),
            "teleport must land on destination exit tile"
        );
        assert_eq!(
            g.camera,
            (23, 26),
            "teleport must use destination exit camera (nx/ny for north approach)"
        );
        // Interior floors around the pad must be loaded (not a black void).
        assert!(
            g.world.selector_can_step(23, 34) || g.room.cell(23, 34).tile().is_some(),
            "castle interior tile north of pad must exist"
        );
        let painted = g
            .room
            .paint_order()
            .iter()
            .filter(|&&(x, y)| g.room.cell(x, y).tile().is_some())
            .count();
        assert!(
            painted > 20,
            "castle interior viewport must include floor tiles, got {painted}"
        );
        // Floors north of the pad are GIDs 258/259 — only present in unifiedtiles.
        let north = g.world.tile(23, 34);
        assert!(
            north >= 257,
            "castle interior floor GID expected >=257 (unifiedtiles), got {north}"
        );
    }

    /// `canBuzz`: blocked aim buzzes once until D-pad ButtonUp re-arms.
    #[test]
    fn can_buzz_gates_spam_until_dpad_release() {
        use crate::game::SfxId;
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        // Same boot path as other selector tests (avoid skip_boot's all-true prev).
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Living swordsman north — every Up press / hold-repeat is a buzz path.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 1,
            Facing::South as u8,
        ));

        let _ = g.take_sfx();
        input.buttons.up = true;
        g.update(0.05, input);
        let first = g.take_sfx();
        assert_eq!(
            first.iter().filter(|&&s| s == SfxId::Buzz).count(),
            1,
            "first blocked press plays one buzz, got {first:?}"
        );

        // Hold-repeat ticks (≥4 at 20 Hz) must not spam buzz while still held.
        for _ in 0..12 {
            g.update(0.05, input);
        }
        let held = g.take_sfx();
        assert!(
            !held.contains(&SfxId::Buzz),
            "held blocked aim must not re-buzz, got {held:?}"
        );

        // Release → ButtonUp re-arms canBuzz.
        input = Input::default();
        g.update(0.05, input);
        let _ = g.take_sfx();

        input.buttons.up = true;
        g.update(0.05, input);
        let again = g.take_sfx();
        assert_eq!(
            again.iter().filter(|&&s| s == SfxId::Buzz).count(),
            1,
            "after release, next blocked press buzzes again, got {again:?}"
        );
    }

    /// Selector cannot step onto a living lowered pike tip.
    #[test]
    fn selector_blocked_by_pike_tip() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 2, Facing::South as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        assert_eq!((pike.child_x, pike.child_y), (px, py - 1));
        g.room.enemies.push(pike);

        input.buttons.up = true;
        g.update(0.05, input);
        // Tip blocks (px, py-1) — selector must stay on player.
        assert_eq!(g.selector_tile(), (px, py));
    }

    /// `pikeBlockCheck` ahead: tip cell blocked → raise before hop.
    #[test]
    fn pike_block_raises_when_tip_ahead_blocked() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        assert!(g.skip_boot());
        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();

        // Pike south of player, facing south — tip ahead is (px, py+3) if body at py+2.
        // Put a swordsman on the tip-ahead cell for a same-tile reface south→… wait:
        // Body at (px, py-3), facing East; tip ahead East = (px+1, py-3).
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 3, Facing::East as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);
        let blocker = Enemy::new(EnemyKind::Swordsman, px + 1, py - 3, Facing::South as u8);
        g.room.enemies.push(blocker);

        // Same tile, keep East facing: ahead tip cell is blocked by swordsman.
        assert!(
            g.pike_block_needs_raise_for_test(0, px, py - 3, Facing::East),
            "blocked tip ahead must raise"
        );
    }

    /// `pikeBlockCheck` corner: 90° turn with entity on sweep cell → raise.
    #[test]
    fn pike_block_raises_on_corner_turn() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        assert!(g.skip_boot());
        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();

        // Facing North, turning East: corner = (tx+1, ty-1).
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 4, Facing::North as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        // Tip ahead North = (px, py-5) — keep clear / walkable outdoor.
        g.room.enemies.push(pike);
        let corner = Enemy::new(EnemyKind::Swordsman, px + 1, py - 5, Facing::South as u8);
        g.room.enemies.push(corner);

        // Ahead North free; corner for N→E occupied → raise.
        assert!(
            g.pike_block_needs_raise_for_test(0, px, py - 4, Facing::East),
            "N→E turn with entity at (tx+1,ty-1) must raise"
        );

        // Same setup but turn East with empty corner → no raise from corner
        // (ahead East tip (px+1, py-4) also empty).
        g.room.enemies[0].facing = Facing::North as u8;
        g.room.enemies[0].pike_up = false;
        g.room.enemies[0].sync_pike_tip();
        g.room.enemies[1].x = px + 5;
        g.room.enemies[1].y = py - 4;
        assert!(
            !g.pike_block_needs_raise_for_test(0, px, py - 4, Facing::East),
            "N→E with clear ahead + clear corner must not raise"
        );
    }

    /// Same-facing walk hop: corner branch skipped (only ahead matters).
    #[test]
    fn pike_block_same_facing_ignores_diagonal() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        assert!(g.skip_boot());
        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();

        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 4, Facing::North as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);
        // Occupy the N→E corner cell — must NOT raise when staying North.
        let decoy = Enemy::new(EnemyKind::Swordsman, px + 1, py - 5, Facing::South as u8);
        g.room.enemies.push(decoy);

        assert!(
            !g.pike_block_needs_raise_for_test(0, px, py - 4, Facing::North),
            "same-facing hop must not use corner cells"
        );
    }

    /// Lua `get_pikeman_target`: approach stops at body manhattan 2 on an axis
    /// (pierce station), not adjacent like a swordsman. Tip then covers the
    /// frontal cell so a head-on selector aim buzzes.
    #[test]
    fn pikeman_approaches_pierce_station_not_adjacent() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Far north, facing south — enough budget to walk past the station if
        // the port wrongly used swordsman melee approach.
        let pike = Enemy::new(EnemyKind::Pikeman, px, py - 6, Facing::South as u8);
        g.room.enemies.push(pike);

        g.run_enemy_phase_for_test(8);

        let e = &g.room.enemies[0];
        assert_eq!(
            (e.x, e.y),
            (px, py - 2),
            "pikeman must park at pierce station (body two north)"
        );
        assert_eq!(e.facing, Facing::South as u8);
        assert!(
            !e.pike_up,
            "braintwo lowers tip once station cell ahead is free"
        );
        assert_eq!(
            (e.child_x, e.child_y),
            (px, py - 1),
            "lowered tip covers the frontal cell between body and player"
        );
        // Tip pierce adjacency + matching facing would kill; tip→player manh is 1.
        assert!(
            g.death_hold_active() || g.enemy_attack_fx_count() > 0,
            "station tip should pierce when adjacent to the player"
        );
    }

    /// Phase-1 re-attach must rotate a lowered pike tip with `setFacing(cameFromDir)`.
    /// Without `sync_pike_tip`, body frame faces south while tip still blocks west.
    #[test]
    fn reenter_rotates_lowered_pike_tip_with_came_from() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        // Clear outdoor locals; inject a west-facing lowered pike into globals.
        g.room.enemies.clear();
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 3, Facing::West as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        assert_eq!(
            (pike.child_x, pike.child_y),
            (px - 1, py - 3),
            "precondition: tip one tile west"
        );
        g.inject_global_enemy_for_test(pike);

        // Room re-enter facing north → cameFromDir = South (`initbackground`).
        g.reload_locals_for_test(Facing::South);

        let e = g
            .room
            .enemies
            .iter()
            .find(|e| matches!(e.kind, EnemyKind::Pikeman) && e.alive)
            .expect("pikeman must reattach");
        assert_eq!(e.facing, Facing::South as u8, "phase-1 setFacing(cameFromDir)");
        assert!(
            !e.pike_up,
            "re-enter must not auto-raise the tip"
        );
        assert_eq!(
            (e.child_x, e.child_y),
            (px, py - 2),
            "lowered tip must follow new facing (south of body), not the stale west cell"
        );
    }

    /// Frontal aim into a lowered tip must not move the selector (Lua buzz).
    #[test]
    fn frontal_aim_blocked_by_lowered_pike_tip_after_approach() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Place already at station with tip down; god mode so pierce does not
        // end the run before we try to aim.
        g.set_god_mode(true);
        let mut pike = Enemy::new(EnemyKind::Pikeman, px, py - 2, Facing::South as u8);
        pike.pike_up = false;
        pike.sync_pike_tip();
        g.room.enemies.push(pike);

        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(
            g.selector_tile(),
            (px, py),
            "frontal step onto tip cell must buzz / stay on player"
        );
    }

    /// Enter first then second interior; print enemy world tiles (debug aid).
    #[test]
    fn dump_second_room_enemy_positions() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let steps = START_PLAY.1 - 170;
        for _ in 0..steps {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        for _ in 0..30 {
            g.update(0.05, input);
            if g.camera == (108, 162) {
                break;
            }
        }
        assert_eq!(g.camera, (108, 162));
        // Exit may open `start` / seen dialog — dismiss before aiming again.
        if g.state == GameState::Dialog {
            g.dismiss_dialog_for_test();
        }
        eprintln!(
            "room1 cam={:?} player={:?} enemies={:?}",
            g.camera,
            (g.player_x, g.player_y),
            g.room
                .enemies
                .iter()
                .map(|e| (e.x, e.y, e.alive))
                .collect::<Vec<_>>()
        );

        // Prefer door cell that shares the player's column when possible.
        let doors = [(g.player_x, 156), (108, 156), (109, 156), (107, 156), (110, 156)];
        let (tx, ty) = doors
            .into_iter()
            .find(|&(x, y)| g.world.terrain_at(x, y).is_door())
            .expect("north door");
        // Retract selector to player first (B), then extend toward the door.
        input.buttons.b = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        for _ in 0..64 {
            if g.selector_tile().0 == tx {
                break;
            }
            input.buttons.right = g.selector_tile().0 < tx;
            input.buttons.left = g.selector_tile().0 > tx;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        for _ in 0..64 {
            if g.selector_tile().1 == ty {
                break;
            }
            input.buttons.up = g.selector_tile().1 > ty;
            input.buttons.down = g.selector_tile().1 < ty;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        eprintln!("selector {:?}", g.selector_tile());
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        for _ in 0..40 {
            g.update(0.05, input);
            if g.camera == (109, 147) {
                break;
            }
        }
        eprintln!(
            "room2 cam={:?} player={:?} enemies={:?}",
            g.camera,
            (g.player_x, g.player_y),
            g.room
                .enemies
                .iter()
                .map(|e| (e.x, e.y, e.alive))
                .collect::<Vec<_>>()
        );
    }

    /// Outdoor streaming must not attach camera-strip enemies into a player-centered
    /// terrain window that does not contain their tile — they would kill while undrawn.
    #[test]
    fn outdoor_reload_skips_enemies_outside_terrain_window() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        // Far enough north that a 16-tile half-window around the player cannot
        // include this tile, but the camera `roomtiles` strip still might.
        let far = Enemy::new(EnemyKind::Swordsman, px, py - 40, Facing::South as u8);
        g.inject_global_only_for_test(far);
        g.room.enemies.clear();

        g.reload_room_terrain_for_test((px, py), 16, 16);

        assert_eq!(
            g.living_local_count(),
            0,
            "enemy outside the loaded terrain window must stay out of room.enemies"
        );
        assert!(
            !g.death_hold_active(),
            "undrawn enemy must not be in the AI list"
        );
    }

    /// Ninja aligned pierce needs a clear line (`checkBlockedShuriken`); a living
    /// body between thrower and player blocks the shot.
    #[test]
    fn ninja_shuriken_blocked_by_body_between() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Ninja three north, facing south; swordsman in between.
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 3, Facing::South as u8));
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 2,
            Facing::South as u8,
        ));

        g.run_enemy_phase_for_test(0);

        assert!(
            !g.death_hold_active() && g.enemy_attack_fx_count() == 0,
            "blocked shuriken must not kill the player"
        );
    }

    /// Intervening cell absent from culled `path_roomtiles` (wall / void / cull)
    /// blocks aligned pierce — Lua `roomtiles[…] == nil`.
    #[test]
    fn ninja_shuriken_blocked_by_wall_between() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 3, Facing::South as u8));
        // Simulate wall / culled-nil on the open cell between thrower and player.
        // Budget 0 → braintwo without `construct_graphs`, so the drop sticks.
        g.remove_path_roomtile_for_test(px, py - 2);

        g.run_enemy_phase_for_test(0);

        assert!(
            !g.death_hold_active() && g.enemy_attack_fx_count() == 0,
            "wall / missing roomtile must block shuriken"
        );
    }

    /// Clear cardinal floor ray → shuriken flight, then deferred pierce kill.
    #[test]
    fn ninja_shuriken_clear_ray_pierces() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 3, Facing::South as u8));

        g.run_enemy_phase_for_test(0);

        assert!(
            g.shuriken_fx_count() > 0,
            "clear floor ray must spawn a flying shuriken"
        );
        assert!(
            !g.death_hold_active(),
            "kill waits for motionFinished (not same tick as throw)"
        );
        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::Shuriken),
            "ninja stab plays shuriken SFX, got {sfx:?}"
        );

        // manhattan 3 → 3 step ticks onto player, then 1 tick with delta 0 → kill.
        g.advance_sim_ticks(4);
        assert!(
            g.death_hold_active(),
            "shuriken motionFinished must kill the player"
        );
        assert_eq!(g.shuriken_fx_count(), 0, "sprite removed on land");
    }

    /// Reverse source door arms `reverseRoom`; a normal door clears it.
    #[test]
    fn reverse_room_flag_follows_exit_reverse() {
        let mut g = test_game();
        assert!(g.skip_boot());
        assert!(!g.reverse_room());

        g.arm_door_exit_for_test(1131);
        assert!(
            g.reverse_room(),
            "exit 1131 has reverse=true → reverseRoom"
        );

        g.arm_door_exit_for_test(568);
        assert!(
            !g.reverse_room(),
            "normal door land clears reverseRoom"
        );
    }

    /// In reverse room, each tip step hops living swordsmen toward the player.
    #[test]
    fn reverse_room_tip_step_hops_enemy() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Swordsman, px, py - 3, Facing::South as u8));
        g.set_reverse_room_for_test(true);
        g.construct_graphs_for_test();

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert_eq!(g.selector_tile(), (px, py - 1));
        assert_eq!(
            (g.room.enemies[0].x, g.room.enemies[0].y),
            (px, py - 2),
            "one tip step → one hop toward the player"
        );
        assert!(!g.death_hold_active(), "still two tiles away — no kill yet");
    }

    /// Reverse men can reach adjacency and kill before A is pressed.
    #[test]
    fn reverse_room_can_kill_before_commit() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        // Two north: first tip hop → adjacent → braintwo kill mid-aim.
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Swordsman, px, py - 2, Facing::South as u8));
        g.set_reverse_room_for_test(true);
        g.construct_graphs_for_test();

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert!(
            g.death_hold_active() || g.enemy_attack_fx_count() > 0,
            "enemy must slash during tip extend before A"
        );
    }

    /// After tip hops, deferred kEnemyMove must not pathfind again (no double move).
    #[test]
    fn reverse_room_post_commit_skips_chase() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Swordsman, px, py - 4, Facing::South as u8));
        g.set_reverse_room_for_test(true);
        g.construct_graphs_for_test();

        // Two tip steps → two hops (enemy ends at py-2); not yet adjacent.
        for _ in 0..2 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!((g.room.enemies[0].x, g.room.enemies[0].y), (px, py - 2));
        assert!(!g.death_hold_active());

        input.buttons.a = true;
        g.update(0.05, input);
        // Walk land + arm deferred enemy phase.
        g.advance_sim_ticks(2);
        assert!(g.pending_enemy_active() || g.death_hold_active());
        let pos_before = (g.room.enemies[0].x, g.room.enemies[0].y);
        // One deferred step tick — must not hop again while reverseRoom.
        if g.pending_enemy_active() {
            g.advance_sim_ticks(1);
            assert_eq!(
                (g.room.enemies[0].x, g.room.enemies[0].y),
                pos_before,
                "post-commit step_enemies must skip pf in reverseRoom"
            );
        }
    }

    /// Adjacent ninja pierce still uses the shuriken oneshot (short flight).
    #[test]
    fn ninja_adjacent_shuriken_delays_kill() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 1, Facing::South as u8));

        g.run_enemy_phase_for_test(0);

        assert!(
            g.shuriken_fx_count() > 0 && !g.death_hold_active(),
            "adjacent ninja still throws before kill"
        );
        g.advance_sim_ticks(2);
        assert!(
            g.death_hold_active(),
            "adjacent shuriken lands after short flight"
        );
    }

    /// `player.onexit` / `pending_exit` blocks throw when skipexitcheck is false.
    #[test]
    fn ninja_shuriken_blocked_when_player_on_exit() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 3, Facing::South as u8));
        g.arm_pending_exit_stub_for_test();

        g.run_enemy_phase_for_test(0);

        assert!(
            !g.death_hold_active() && g.enemy_attack_fx_count() == 0,
            "onexit must block shuriken (skipexitcheck=false)"
        );
    }

    /// Off-axis ninja beside a zip path hops opposite the kill dir when the
    /// landing cell is free (`enemy:kill` dodge) — survives, no score, dodge FX.
    /// Uses a 4-wide outdoor strip (start corridor is only 2 tiles — hop would
    /// land on a wall and correctly die).
    #[test]
    fn ninja_dodges_passing_slash_when_hop_free() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        assert!(g.god_teleport(125, 54));
        let (px, py) = (g.player_x, g.player_y);
        assert_eq!((px, py), (125, 54));

        // Floors at x=124..127 on this strip; zip north along x=125.
        // Ninja at (126, 52); kill_dir West → hop (127, 52) which is floor.
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px + 1, py - 2, Facing::West as u8));
        let start = (px + 1, py - 2);
        let score_before = g.kill_score();

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        assert_eq!(g.selector_tile(), (px, py - 3));

        input.buttons.a = true;
        g.update(0.05, input);
        g.advance_sim_ticks(4); // wind-up → kill resolve

        assert!(
            g.room.enemies[0].alive,
            "ninja must survive a passing slash when hop is free"
        );
        assert_eq!(
            (g.room.enemies[0].x, g.room.enemies[0].y),
            (start.0 + 1, start.1),
            "dodge hops opposite kill dir (further east)"
        );
        assert_eq!(
            Facing::from_u8(g.room.enemies[0].facing),
            Some(Facing::West),
            "facing becomes kill dir after dodge"
        );
        assert!(
            g.enemy_dodge_state(0),
            "dodgestate set on successful dodge"
        );
        assert!(
            g.enemy_just_dodged(0),
            "justdodged set on successful dodge"
        );
        assert!(
            g.enemy_dodge_fx_count() > 0,
            "dodge anim should play"
        );
        assert_eq!(
            g.kill_fx_count(),
            0,
            "successful dodge must not spawn slashed FX"
        );
        assert_eq!(
            g.kill_score(),
            score_before,
            "successful dodge must not add score"
        );
        assert_eq!(g.corpse_count(), 0);

        // First braintwo: clear justdodged, keep dodgestate (skip attack).
        g.drain_enemy_phase_for_test();
        assert!(
            !g.enemy_just_dodged(0),
            "justdodged clears on first braintwo"
        );
        assert!(
            g.enemy_dodge_state(0),
            "ninja keeps dodgestate through the dodge-phase braintwo"
        );

        // Let the short dodge oneshot finish — Lua keeps pose 6 (kneel) until
        // the stand-up braintwo; we must not fall back to idle pose 1 early.
        for _ in 0..16 {
            g.advance_sim_ticks(1);
            if g.enemy_dodge_fx_count() == 0 {
                break;
            }
        }
        assert_eq!(
            g.enemy_dodge_fx_count(),
            0,
            "dodge oneshot should finish within a few ticks"
        );
        assert!(
            g.enemy_dodge_state(0),
            "dodgestate still held after oneshot ends"
        );
        assert_eq!(
            g.enemy_living_draw_pose_for_test(0),
            6,
            "kneel (pose 6) held while dodgestate until next player turn's braintwo"
        );

        // Second enemy phase: clear dodgestate ("stand back up").
        g.run_enemy_phase_for_test(0);
        assert!(
            !g.enemy_dodge_state(0),
            "dodgestate clears on the stand-up braintwo"
        );
        assert_eq!(
            g.enemy_living_draw_pose_for_test(0),
            1,
            "idle pose after stand-up braintwo"
        );
        assert!(g.room.enemies[0].alive);
    }

    /// Off-axis ninja whose hop cell is occupied cannot dodge and dies.
    #[test]
    fn ninja_dies_to_slash_when_hop_blocked() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        assert!(g.god_teleport(125, 54));
        let (px, py) = (g.player_x, g.player_y);

        g.room.enemies.clear();
        // Ninja at (px+1, py-2); hop for kill_dir West is (px+2, py-2).
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px + 1, py - 2, Facing::West as u8));
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px + 2,
            py - 2,
            Facing::West as u8,
        ));
        let score_before = g.kill_score();

        for _ in 0..3 {
            input.buttons.up = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
        }
        input.buttons.a = true;
        g.update(0.05, input);
        g.advance_sim_ticks(4);

        assert!(
            !g.room.enemies[0].alive,
            "ninja must die when hop tile is occupied"
        );
        assert!(
            g.room.enemies[1].alive,
            "blocker swordsman is not on the deathlist"
        );
        assert!(g.kill_fx_count() > 0);
        assert_eq!(g.kill_score(), score_before + 1);
        assert_eq!(g.enemy_dodge_fx_count(), 0);
    }

    /// On-axis (same column) ninja takes the stab / frontal kill — no dodge gate.
    /// Tip one north of player; stab reaches the ninja two north (same as swordsman).
    #[test]
    fn ninja_dies_to_on_axis_stab() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room
            .enemies
            .push(Enemy::new(EnemyKind::Ninja, px, py - 2, Facing::South as u8));

        // Aim tip one north (cannot land on the ninja); stab kills beyond tip.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);
        assert!(
            !g.room.enemies[0].alive,
            "on-axis ninja must die to stab (no dodge)"
        );
        assert_eq!(g.enemy_dodge_fx_count(), 0);
        assert!(g.kill_fx_count() > 0 || g.corpse_count() == 1);
    }

    /// Twinstep facing the kill dir parries (`enemy:kill` dir == facing) — survives,
    /// spark + body oneshot, no score. Stab-only (Lua tip land then kill) leaves the
    /// player one tile behind the tip via `samurai:knockback` (`ex+2*facing`).
    #[test]
    fn twinstep_parries_frontal_stab() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        // Facing south into the player's northbound stab → kill_dir South == facing.
        // Twin at py-2; tip (selector) at py-1. Stab-only: land tip, then parry
        // knockback to twin+(0,+2) = (px, py) — one tile behind the tip.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Twinstep,
            px,
            py - 2,
            Facing::South as u8,
        ));
        let score_before = g.kill_score();
        let life_before = g.life;

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(
            g.room.enemies[0].alive,
            "twinstep must survive a frontal hit"
        );
        assert!(
            g.enemy_dodge_state(0),
            "dodgestate set on successful parry"
        );
        assert!(
            g.enemy_just_dodged(0),
            "justdodged set on successful parry"
        );
        assert!(
            g.enemy_dodge_fx_count() > 0,
            "body parry pose should play"
        );
        assert!(
            g.parry_spark_fx_count() > 0,
            "parry spark overlay should play"
        );
        assert_eq!(g.kill_fx_count(), 0, "parry must not spawn slashed FX");
        assert_eq!(g.kill_score(), score_before, "parry must not add score");
        assert_eq!(
            g.life,
            life_before - 1,
            "on-axis frontal parry knockback spends walk blood"
        );
        assert_eq!(
            g.facing,
            Facing::North,
            "knockback faces opposite the twinstep facing"
        );
        // Stab-only: tip land then kill → knockback sticks (`main.lua` ticks>5).
        // Tip was (px, py-1); knockback to twin + 2*South = (px, py).
        assert_eq!(
            (g.player_x, g.player_y),
            (px, py),
            "frontal stab parry bumps player one tile behind the tip"
        );
        assert_eq!(
            g.selector_tile(),
            (g.player_x, g.player_y),
            "knockback moveToTile must carry the cursor to the player (samurai.lua:482)"
        );

        // Twinstep clears dodge_state on the justdodged braintwo (unlike ninja)
        // and must not counter-stab that same phase (1.10: clang / bump, live).
        g.drain_enemy_phase_for_test();
        assert!(!g.enemy_just_dodged(0));
        assert!(
            !g.enemy_dodge_state(0),
            "twinstep clears dodgestate on the dodge-phase braintwo"
        );
        assert!(g.room.enemies[0].alive);
        assert!(
            !g.death_hold_active(),
            "frontal parry must not kill the player on the recovery braintwo"
        );
        assert_eq!(
            g.state,
            GameState::Aiming,
            "player should still be aiming after parry exchange"
        );
        // Still one tile south of the twin after recovery (manhattan 2).
        assert_eq!((g.player_x, g.player_y), (px, py));
    }

    /// A corpse on the knockback landing blocks the bump: `checkBlockedByEntity`
    /// has no `alive` filter (`pathfinding.lua:436`), so a dead body counts. The
    /// player stays on the stab tip (no walk blood spend) and the cursor stays put.
    #[test]
    fn twinstep_parry_knockback_blocked_by_corpse() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Twinstep,
            px,
            py - 2,
            Facing::South as u8,
        ));
        let life_before = g.life;

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        // Corpse sits where the knockback would land (the player's old tile).
        let mut corpse = Enemy::new(EnemyKind::Swordsman, px, py, Facing::North as u8);
        corpse.alive = false;
        g.room.enemies.push(corpse);

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(g.room.enemies[0].alive, "twinstep must still parry");
        assert!(!g.room.enemies[1].alive, "corpse stays dead");
        assert_eq!(
            (g.player_x, g.player_y),
            (px, py - 1),
            "dead body on the landing tile blocks the knockback"
        );
        assert_eq!(
            g.selector_tile(),
            (g.player_x, g.player_y),
            "blocked knockback leaves the cursor on the player"
        );
        assert_eq!(g.life, life_before, "blocked knockback spends no walk blood");
    }

    /// Castle twin `(114,22)` after "5 steps north" → player `(117,27)` North.
    ///
    /// Isolated (no sibling blockers): port A* prefers **south then east**
    /// (`f,f,f,f,f,ccw`). Playdate 1.10 prefers **cut east at row 24**
    /// (`f,f,ccw,f,f,f`). Same Manhattan family; difference is equal-cost
    /// tie-break in native `findPath` vs Rust `PathGraph::find_path` — **not RNG**.
    /// When A* tie order is fixed to match the SDK, update the expected verbs
    /// to the original sequence.
    #[test]
    fn castle_twin_114_22_isolated_path_south_first() {
        use crate::level::EnemyKind;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        g.facing = Facing::North;
        g.force_door_exit_for_test(641);
        // Twin-room entry may open an exit dialog; enemy ticks pause in Dialog.
        if g.state == GameState::Dialog {
            g.dismiss_dialog_for_test();
        }
        assert_eq!(g.camera, (117, 24));
        assert_eq!((g.player_x, g.player_y), (117, 32));

        // Pose without god_teleport (camera search can leave the twin-room cam).
        g.player_x = 117;
        g.player_y = 27;
        g.facing = Facing::North;
        g.reset_selector_for_test();
        if g.state == GameState::Dialog {
            g.dismiss_dialog_for_test();
        }

        let twin_i = g
            .room
            .enemies
            .iter()
            .position(|e| e.x == 114 && e.y == 22 && matches!(e.kind, EnemyKind::Twinstep))
            .expect("twin at (114,22)");
        assert_eq!(
            Facing::from_u8(g.room.enemies[twin_i].facing),
            Some(Facing::South)
        );
        g.isolate_room_enemies_for_test(&[twin_i]);
        g.construct_graphs_for_test();
        assert_eq!(g.camera, (117, 24));
        assert_eq!(g.room.enemies.len(), 1);
        // `run_enemy_phase_for_test` dismisses dialogs that would pause ticks.
        if g.state == GameState::Dialog {
            g.dismiss_dialog_for_test();
        }
        assert_eq!(g.state, GameState::Aiming, "must be Aiming for enemy ticks");

        let hop_verb = |ox: i32, oy: i32, of: u8, nx: i32, ny: i32, nf: u8| -> &'static str {
            let of = Facing::from_u8(of).unwrap_or(Facing::South);
            let nf = Facing::from_u8(nf).unwrap_or(Facing::South);
            if (nx, ny) == (ox, oy) {
                if nf == of {
                    return "stay";
                }
                let ccw = match of {
                    Facing::North => Facing::West,
                    Facing::West => Facing::South,
                    Facing::South => Facing::East,
                    Facing::East => Facing::North,
                };
                if nf == ccw {
                    return "ccw";
                }
                return "reface";
            }
            if nf == of {
                "f"
            } else {
                "walk?"
            }
        };

        // Bypass Dialog-gated `advance_sim_ticks`: call `step_enemies` directly
        // with segs_left ≥ 2 so short-bar reface cannot fire.
        let mut verbs = Vec::new();
        let mut tiles = vec![(114, 22, Facing::South as u8)];
        for i in 0..6 {
            let e = &g.room.enemies[0];
            let (ox, oy, of) = (e.x, e.y, e.facing);
            g.step_enemies_once_for_test(6 - i);
            assert_eq!(g.room.enemies.len(), 1);
            let e = &g.room.enemies[0];
            verbs.push(hop_verb(ox, oy, of, e.x, e.y, e.facing));
            tiles.push((e.x, e.y, e.facing));
        }

        // Port today: south to player row, then ccw to East.
        // Original 1.10: (114,23)(114,24) ccw→E (115,24)(116,24)(117,24).
        assert_eq!(
            verbs,
            ["f", "f", "f", "f", "f", "ccw"],
            "port A* south-first path; tiles={tiles:?} state={:?}",
            g.state
        );
        assert_eq!(tiles[5], (114, 27, Facing::South as u8));
        assert_eq!(tiles[6], (114, 27, Facing::East as u8));
    }

    /// Twinstep short-readybar reface (`pathfinding.lua:616–650`): when remaining
    /// segments `< 2` and xyf distance is not yet "attack", the hop is a same-tile
    /// turn toward the player (on-axis) instead of a chase walk.
    #[test]
    fn twinstep_short_bar_refaces_toward_player() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        assert!(g.room.walkable(px, py - 3));

        g.room.enemies.clear();
        // On-axis north of player, facing east (off the approach axis).
        // With budget 1 (< 2) and far xyf, Lua overrides A* with facePlayer.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Twinstep,
            px,
            py - 3,
            Facing::East as u8,
        ));

        g.run_enemy_phase_for_test(1);
        let e = &g.room.enemies[0];
        assert_eq!(
            (e.x, e.y),
            (px, py - 3),
            "short-bar reface must stay on tile (no chase walk)"
        );
        assert_eq!(
            e.facing,
            Facing::South as u8,
            "on-axis short bar: turn toward the player"
        );
    }

    /// Twinstep with readybar ≥ 2 still takes a normal A* 90° turn on the first
    /// hop (short-bar override must not fire while `steps_left >= 2`).
    #[test]
    fn twinstep_long_bar_turns_like_swordsman() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (px, py) = (g.player_x, g.player_y);
        assert!(g.room.walkable(px, py - 3));

        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Twinstep,
            px,
            py - 3,
            Facing::East as u8,
        ));

        // Arm two segments but only advance one hop — while steps_left is 2 the
        // short-bar branch is off, so this matches swordsman turn-in-place.
        g.arm_pending_enemy_for_test(2);
        g.advance_sim_ticks(1);
        let e = &g.room.enemies[0];
        assert_eq!(
            (e.x, e.y),
            (px, py - 3),
            "first hop with segs≥2 must be turn-in-place, not a walk"
        );
        assert_eq!(
            e.facing,
            Facing::South as u8,
            "long bar: normal A* 90° turn toward the player"
        );
    }

    /// Twinstep hit from behind (kill dir != facing) dies like a swordsman.
    #[test]
    fn twinstep_dies_to_rear_stab() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        // Facing away from the player → kill_dir South != North facing.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Twinstep,
            px,
            py - 2,
            Facing::North as u8,
        ));
        let score_before = g.kill_score();

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(
            !g.room.enemies[0].alive,
            "rear hit must kill the twinstep"
        );
        assert_eq!(g.enemy_dodge_fx_count(), 0);
        assert_eq!(g.parry_spark_fx_count(), 0);
        assert_eq!(g.kill_score(), score_before + 1);
        assert!(g.kill_fx_count() > 0 || g.corpse_count() == 1);
    }

    /// King dies to on-axis stab like a swordsman and grants +20 blood
    /// (`enemy:kill` kKing). Stationary — no pathfinding / no parry.
    #[test]
    fn king_kill_heals_twenty_blood() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::King,
            px,
            py - 2,
            Facing::South as u8,
        ));
        // Start room LIFE is near maxBlood after intro — drop so +20 is visible.
        g.life = 100;
        let score_before = g.kill_score();

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(!g.room.enemies[0].alive, "king must die to stab");
        assert_eq!(g.kill_score(), score_before + 1);
        assert_eq!(g.life, 120, "killing the king grants +20 blood");
        assert!(g.kill_fx_count() > 0 || g.corpse_count() == 1);
    }

    /// King heal clamps at `maxBlood` (250).
    #[test]
    fn king_kill_heal_clamps_at_max_blood() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::King,
            px,
            py - 2,
            Facing::South as u8,
        ));
        g.life = 245;

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.a = true;
        g.update(0.05, input);

        assert!(!g.room.enemies[0].alive);
        assert_eq!(g.life, 250, "king heal must clamp at maxBlood");
    }

    /// Spirit tip-stab dispel: dies, scores, plays `spiritdispel`, no blood spray.
    /// Tip stab and passing slash both kill Spirits; only the start-adjacent slash
    /// loop skips them (`etype ~= kSpirit` in `selector:detectkills`).
    #[test]
    fn spirit_stab_dispels_without_bleed() {
        use crate::level::{Enemy, EnemyKind};
        use crate::game::SfxId;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Spirit,
            px,
            py - 2,
            Facing::South as u8,
        ));
        let score_before = g.kill_score();
        let _ = g.take_sfx();

        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));

        input.buttons.a = true;
        g.update(0.05, input);

        assert!(!g.room.enemies[0].alive, "spirit must die to tip stab");
        assert_eq!(g.kill_score(), score_before + 1);
        assert_eq!(g.blood_spray_count(), 0, "spirit kill skips enemy:bleed");
        assert!(g.kill_fx_count() > 0 || g.corpse_count() == 1);
        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::SpiritDispel),
            "expected SpiritDispel in {sfx:?}"
        );
        assert!(
            !sfx.contains(&SfxId::Blood),
            "spirit kill must not play blood SFX: {sfx:?}"
        );
    }

    /// Spirit already adjacent to the player start is NOT start-adjacent-slashed
    /// (`etype ~= kSpirit`), matching `selector.lua` 302 / 348.
    #[test]
    fn spirit_not_slashed_when_start_adjacent() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        // Spirit adjacent on the perpendicular axis at the *start* tile — the
        // start-adjacent slash loop skips Spirits (a swordsman would be hit).
        g.room.enemies.push(Enemy::new(
            EnemyKind::Spirit,
            px + 1,
            py,
            Facing::South as u8,
        ));

        // Aim two tiles north so |extent| > 1; start-adjacent check runs each step.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert!(
            g.aim_plan_deathlist_empty_for_test(),
            "spirit must not be marked by start-adjacent slash"
        );
        assert!(g.room.enemies[0].alive);
    }

    /// Passing slash (enemy beside a path cell, not the start tile) *does* kill
    /// Spirits — `selector.lua` 285–295 / 332–340 have no `kSpirit` skip.
    #[test]
    fn spirit_slashed_by_pass_beside() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        // Spirit one tile east of the cell north of the player — first step of a
        // north zip places it beside `second` path cell → passing slash.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Spirit,
            px + 1,
            py - 1,
            Facing::South as u8,
        ));

        // Aim two tiles north (|extent| > 1) so the passing-slash scan runs.
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        assert!(
            !g.aim_plan_deathlist_empty_for_test(),
            "passing slash must mark the spirit on the deathlist"
        );

        input.buttons.a = true;
        g.update(0.05, input);
        // Slash resolve is phased (preslash then kill); drain a few ticks.
        input = Input::default();
        for _ in 0..10 {
            g.update(0.05, input);
        }

        assert!(
            !g.room.enemies[0].alive,
            "spirit must die to a passing slash"
        );
    }

    /// Spirits never pathfind — `step_enemies` skips `kSpirit` (`pathfinding.lua`).
    #[test]
    fn spirit_does_not_move_during_enemy_phase() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        g.room.enemies.push(Enemy::new(
            EnemyKind::Spirit,
            px + 3,
            py,
            Facing::South as u8,
        ));
        let (sx, sy) = (g.room.enemies[0].x, g.room.enemies[0].y);

        g.run_enemy_phase_for_test(8);
        assert_eq!(
            (g.room.enemies[0].x, g.room.enemies[0].y),
            (sx, sy),
            "spirit must stay put through kEnemyMove"
        );
        assert!(g.room.enemies[0].alive);
    }

    /// With spirit-room doors present, a dead Spirit mass-revives when
    /// `updateSpiritsAndDoors` runs without an all-clear kill frame.
    #[test]
    fn spirit_revives_when_doors_present() {
        use crate::level::{Enemy, EnemyKind};
        use crate::game::SfxId;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        let mut dead = Enemy::new(EnemyKind::Spirit, px + 2, py, Facing::South as u8);
        dead.alive = false;
        g.room.enemies.push(dead);
        g.inject_spirit_door_for_test(px + 5, py, 0);
        assert!(g.spirit_doors_closed_for_test());
        assert_eq!(g.roomtile_override_for_test(px + 5, py), Some(3));
        let _ = g.take_sfx();

        g.update_spirits_and_doors_for_test(false);

        assert!(g.room.enemies[0].alive, "dead spirit must revive");
        assert!(g.spirit_revive_fx_count_for_test() > 0);
        assert!(g.spirit_doors_closed_for_test());
        let sfx = g.take_sfx();
        assert!(
            sfx.contains(&SfxId::SpiritRevive),
            "expected SpiritRevive in {sfx:?}"
        );
    }

    /// After a kill frame that clears every Spirit, doors open (`roomtiles = -3-left`).
    #[test]
    fn spirit_all_dead_opens_doors() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        let mut dead = Enemy::new(EnemyKind::Spirit, px + 2, py, Facing::South as u8);
        dead.alive = false;
        g.room.enemies.push(dead);
        g.inject_spirit_door_for_test(px + 5, py, 0);
        g.inject_spirit_door_for_test(px + 6, py, 1);
        assert!(g.spirit_doors_closed_for_test());

        // First call with didkill: sets killed_last_frame, spirits_alive==0 so
        // falls through to open branch.
        g.update_spirits_and_doors_for_test(true);

        assert!(
            g.spirit_doors_open_for_test(),
            "doors must open when all spirits are dead after a kill"
        );
        assert_eq!(g.roomtile_override_for_test(px + 5, py), Some(-3));
        assert_eq!(g.roomtile_override_for_test(px + 6, py), Some(-4));
        assert!(
            !g.room.enemies[0].alive,
            "all-clear open path must not revive"
        );
        assert_eq!(g.spirit_revive_fx_count_for_test(), 0);
    }

    /// Room enter / god teleport must spawn spirit doors via `refresh_viewport`
    /// → `populate_room_enemies`. Without that, door tiles stay walkable exits
    /// and `updateSpiritsAndDoors` never runs (no revive).
    #[test]
    fn spirit_room_enter_spawns_closed_doors() {
        use crate::level::EnemyKind;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.set_god_mode(true);
        // First spirit room pad near doors (192,44)/(193,44).
        assert!(g.god_teleport(191, 49));
        assert!(
            g.spirit_door_count_for_test() >= 2,
            "spirit-room doors must spawn on room enter"
        );
        assert!(g.spirit_doors_closed_for_test());
        assert_eq!(g.roomtile_override_for_test(192, 44), Some(3));
        assert_eq!(g.roomtile_override_for_test(193, 44), Some(4));
        let spirits: Vec<_> = g
            .room
            .enemies
            .iter()
            .filter(|e| matches!(e.kind, EnemyKind::Spirit))
            .collect();
        assert!(!spirits.is_empty(), "spirit room should attach Spirits");
        assert!(spirits.iter().all(|e| e.alive));

        // Partial kill → next spirits/doors pass revives (doors present).
        let i = g
            .room
            .enemies
            .iter()
            .position(|e| matches!(e.kind, EnemyKind::Spirit) && e.alive)
            .unwrap();
        g.room.enemies[i].alive = false;
        g.update_spirits_and_doors_for_test(false);
        assert!(
            g.room.enemies[i].alive,
            "dead spirit must revive while doors are shut"
        );
    }

    /// Spawned doors rest on the closed frame (`doorTable[1]` / `[5]`), not the
    /// open start of `framesclose` — otherwise entry looks like open doors.
    #[test]
    fn spirit_doors_spawn_visually_closed() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.inject_spirit_door_for_test(px + 4, py, 0);
        g.inject_spirit_door_for_test(px + 5, py, 1);
        assert!(g.spirit_doors_closed_for_test());
        // Left half resting closed = cell 0; right half = cell 4.
        assert_eq!(g.spirit_door_blit_cells_for_test(), vec![0, 4]);
        // Stay closed across a few ticks (no open slam on spawn).
        g.advance_sim_ticks(8);
        assert_eq!(g.spirit_door_blit_cells_for_test(), vec![0, 4]);
    }

    /// Closed spirit door blocks the selector (`roomtiles` overlay > 0).
    #[test]
    fn spirit_closed_door_blocks_selector() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Place a closed left door one tile north of the player.
        g.inject_spirit_door_for_test(px, py - 1, 0);
        assert_eq!(g.roomtile_override_for_test(px, py - 1), Some(3));

        input.buttons.up = true;
        g.update(0.05, input);
        // Selector must stay on the player — door cell is not walkable.
        assert_eq!(g.selector_tile(), (px, py));
    }

    /// Non-death A-hide resumes Aiming immediately while close anim still runs
    /// (`dialog:hide` sets Playing before close finishes — Lua-faithful).
    #[test]
    fn dialog_hide_allows_aim_during_close_anim() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // First-kill flavor line (or any non-death key).
        g.show_dialog_for_test("swordsmanKilled", 1, 0.0);
        // Queue → open on next dialog.update.
        g.update(0.05, Input::default());
        assert_eq!(g.state, GameState::Dialog);
        assert!(g.dialog_showing_for_test());

        // Wait past ticks > 3 (and open anim enough to dismiss).
        g.advance_sim_ticks(8);
        assert_eq!(g.state, GameState::Dialog);

        input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(
            g.state,
            GameState::Aiming,
            "hide must restore Playing/Aiming same frame (not wait for ClosedIdle)"
        );
        assert!(
            !g.dialog_showing_for_test(),
            "line is closed; only close chrome remains"
        );
        assert!(
            g.dialog_anim_active_for_test(),
            "close anim should still be in flight after hide"
        );

        // D-pad must move the selector while close chrome animates.
        input = Input::default();
        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(
            g.selector_tile(),
            (px, py - 1),
            "aim must work during dialog close anim"
        );
    }

    /// Empty room: `step_enemies` burns two readybar pips per tick
    /// (`pathfinding.lua` when `numEnemiesAlive == 0`).
    #[test]
    fn empty_room_enemy_phase_drains_two_pips_per_tick() {
        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        g.room.enemies.clear();
        g.arm_pending_enemy_for_test(6);
        assert_eq!(g.pending_enemy_steps_for_test(), Some(6));

        g.advance_sim_ticks(1);
        assert_eq!(
            g.pending_enemy_steps_for_test(),
            Some(4),
            "no living enemies → removeSegment ×2"
        );
        g.advance_sim_ticks(1);
        assert_eq!(g.pending_enemy_steps_for_test(), Some(2));
        g.advance_sim_ticks(1);
        // steps hit 0 this tick; next tick runs braintwo and clears pending.
        assert!(
            g.pending_enemy_steps_for_test() == Some(0) || !g.pending_enemy_active(),
            "budget 6 empties in 3 double-drain ticks"
        );
        g.advance_sim_ticks(1);
        assert!(
            !g.pending_enemy_active(),
            "braintwo tick must clear pending_enemy"
        );
    }

    /// Living enemy still drains one pip per tick (no accidental double-speed).
    #[test]
    fn living_enemy_phase_drains_one_pip_per_tick() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        g.room.enemies.clear();
        // Far enough that one step won't braintwo-kill immediately.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px + 4,
            py,
            Facing::West as u8,
        ));
        g.arm_pending_enemy_for_test(4);
        assert_eq!(g.pending_enemy_steps_for_test(), Some(4));

        g.advance_sim_ticks(1);
        assert_eq!(
            g.pending_enemy_steps_for_test(),
            Some(3),
            "living enemy → one removeSegment per tick"
        );
        g.advance_sim_ticks(1);
        assert_eq!(g.pending_enemy_steps_for_test(), Some(2));
    }

    /// Corpse hop: one D-pad press jumps over `deadenemies==1` and lands past it
    /// (`selector:trymove` → `moveByTile(ox*iter)`). Crossing costs one input, not two.
    #[test]
    fn selector_hops_over_corpse_in_one_press() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Corpse one tile north; free floor beyond (outdoor start corridor).
        let mut corpse = Enemy::new(EnemyKind::Swordsman, px, py - 1, Facing::South as u8);
        corpse.alive = false;
        g.room.enemies.push(corpse);
        g.mark_dead_enemy_tile_for_test(px, py - 1, EnemyKind::Swordsman);
        assert_eq!(g.deadenemies_for_test(px, py - 1), Some(1));
        assert_eq!(g.roomtile_override_for_test(px, py - 1), Some(1));

        input.buttons.up = true;
        g.update(0.05, input);

        assert_eq!(
            g.selector_tile(),
            (px, py - 2),
            "one press must land past the corpse, not on it"
        );
        assert_eq!(
            g.selector_extent_for_test(),
            (0, -2),
            "hop advances extent by iter=2 in one moveByTile"
        );
        assert!(
            g.selector_canmove_for_test(),
            "landing on empty floor keeps canmove"
        );
    }

    /// Line of corpses: hop skips consecutive `deadenemies==1` until free floor.
    #[test]
    fn selector_hops_over_corpse_line() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        for dy in 1..=2 {
            let mut corpse = Enemy::new(EnemyKind::Swordsman, px, py - dy, Facing::South as u8);
            corpse.alive = false;
            g.room.enemies.push(corpse);
            g.mark_dead_enemy_tile_for_test(px, py - dy, EnemyKind::Swordsman);
        }

        input.buttons.up = true;
        g.update(0.05, input);

        assert_eq!(
            g.selector_tile(),
            (px, py - 3),
            "hop must clear both corpses in one press"
        );
        assert_eq!(g.selector_extent_for_test(), (0, -3));
    }

    /// Spirit corpse (`deadenemies==2`) hard-blocks and stays `2` across
    /// `construct_graphs`: Lua runs that sweep only at room init, right after
    /// `findlocalenemies` revives every Spirit (`utils.lua:206-207`), so a dead
    /// Spirit is never downgraded to the hop-over `1`.
    #[test]
    fn spirit_corpse_stays_hard_block_after_graphs() {
        use crate::level::{Enemy, EnemyKind};
        use crate::game::SfxId;

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        let mut corpse = Enemy::new(EnemyKind::Spirit, px, py - 1, Facing::South as u8);
        corpse.alive = false;
        g.room.enemies.push(corpse);
        g.mark_dead_enemy_tile_for_test(px, py - 1, EnemyKind::Spirit);
        assert_eq!(g.deadenemies_for_test(px, py - 1), Some(2));

        let _ = g.take_sfx();
        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py), "spirit body must hard-block");
        let sfx = g.take_sfx();
        assert!(sfx.contains(&SfxId::Buzz), "expected buzz, got {sfx:?}");

        // Release so the next up is a fresh press edge (not hold-repeat).
        input = Input::default();
        g.update(0.05, input);

        // Graphs rebuild must NOT downgrade a dead Spirit to hop-over.
        g.construct_graphs_for_test();
        assert_eq!(
            g.deadenemies_for_test(px, py - 1),
            Some(2),
            "construct_graphs must preserve the Spirit hard-block"
        );

        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(
            g.selector_tile(),
            (px, py),
            "spirit corpse must still hard-block after construct_graphs"
        );
        let sfx = g.take_sfx();
        assert!(sfx.contains(&SfxId::Buzz), "expected buzz, got {sfx:?}");
    }

    /// Landing the tip on a corpse sets `canmove=false` (tip stab suppressed).
    #[test]
    fn corpse_land_disables_tip_stab() {
        use crate::level::{Enemy, EnemyKind};

        let mut g = test_game();
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let px = g.player_x;
        let py = g.player_y;
        // Free floor north, corpse further north — walk onto corpse (roomtiles<=0 path).
        // Simulate pre-graph / free-step by leaving roomtiles at floor (no override).
        let mut corpse = Enemy::new(EnemyKind::Swordsman, px, py - 1, Facing::South as u8);
        corpse.alive = false;
        g.room.enemies.push(corpse);
        // Living enemy beyond the corpse for a tip stab that must NOT register.
        g.room.enemies.push(Enemy::new(
            EnemyKind::Swordsman,
            px,
            py - 2,
            Facing::South as u8,
        ));

        input.buttons.up = true;
        g.update(0.05, input);
        assert_eq!(g.selector_tile(), (px, py - 1));
        assert!(
            !g.selector_canmove_for_test(),
            "tip on corpse must set canmove=false"
        );
        assert!(
            !g.aim_has_stab_for_test(),
            "tip stab must not register while canmove=false"
        );
    }

    /// Chest tile must appear in the outdoor viewport paint list when the camera
    /// is on that room — otherwise `draw_room` never blits `Images/chest`.
    #[test]
    fn chest_tile_in_viewport_when_camera_nearby() {
        let mut g = test_game_seed(840993252);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);

        let (cx, cy) = g.chest_pos().unwrap();
        assert_eq!((cx, cy), (151, 129));
        // Camera / player on the rolled chest's outdoor neighborhood.
        g.camera = (cx, cy);
        g.player_x = cx - 1;
        g.player_y = cy;
        g.reset_selector_for_test();
        g.reload_room_terrain_for_test((cx, cy), 16, 16);
        let order = g.room.paint_order();
        assert!(
            order.iter().any(|&p| p == (cx, cy)),
            "chest tile must be in paint_order for draw_room; room origin=({},{}) size={}x{}",
            g.room.origin_x,
            g.room.origin_y,
            g.room.width,
            g.room.height
        );
        // Redraw; missing `Images/chest` falls back to a diamond so ink still appears.
        g.update(0.05, Input::default());
        let mut ink = 0u32;
        for y in 0..240u32 {
            for x in 0..400u32 {
                if g.fb.get_pixel(x, y) {
                    ink += 1;
                }
            }
        }
        assert!(ink > 100, "room with chest must draw some ink (got {ink})");
    }

    /// Nearer wall / decor must cover the farther player (Lua strip `isoZ-5` vs
    /// actor `isoZ+5` on *their* tile). The v0.9c global "tiles then actors"
    /// pass broke this — interleaved per-cell paint restores it.
    #[test]
    fn nearer_wall_occludes_farther_player() {
        use crate::iso::{actor_blit_pos, grid_to_screen, tile_blit_pos};
        use crate::level::Terrain;
        use crate::worldmap::START_CAMERA;

        let mut g = test_game();
        assert!(g.skip_boot());

        // Synthetic assets: opaque white player + opaque black wall so overlap
        // pixels report wall ink (black) only when the wall paints after the player.
        let mut player_cell = Bitmap::with_alpha(96, 96);
        for y in 0..96u32 {
            for x in 0..96u32 {
                player_cell.set_pixel(x, y, true, true); // white opaque
            }
        }
        let mut wall_cell = Bitmap::with_alpha(32, 64);
        for y in 0..64u32 {
            for x in 0..32u32 {
                wall_cell.set_pixel(x, y, false, true); // black opaque
            }
        }
        // tiles[0] unused (GID 1); tiles[99] = wall GID 100 for the forced cell.
        let mut tile_cells = Vec::with_capacity(100);
        for _ in 0..99 {
            tile_cells.push(Bitmap::transparent(32, 64));
        }
        tile_cells.push(wall_cell);
        let mut assets = DemoAssets::default();
        assets.player = Some(ImageTable {
            cells: vec![player_cell; 64], // 4 facings × 16 poses
            cells_per_row: 16,
        });
        assets.tiles = Some(ImageTable {
            cells: tile_cells,
            cells_per_row: 16,
        });
        g.set_demo_assets(assets);

        let (px, py) = (g.player_x, g.player_y);
        // Wall one step east: depth_key = gx+gy is larger → paints after player.
        let (wx, wy) = (px + 1, py);
        g.camera = START_CAMERA;
        g.reload_room_terrain_for_test(START_CAMERA, 16, 16);
        g.set_room_cell_for_test(wx, wy, Terrain::Wall { tile: 100 });
        g.room.enemies.clear();
        g.update(0.05, Input::default());

        let (ox, oy) = {
            let (cx, cy) = g.camera;
            let ox = 200 + (cy - cx) * 16;
            let oy = 80 - (cx + cy) * 8;
            (ox, oy)
        };
        let (psx, psy) = grid_to_screen(px, py, ox, oy);
        let (wsx, wsy) = grid_to_screen(wx, wy, ox, oy);
        let (pbx, pby) = actor_blit_pos(psx, psy);
        let (wbx, wby) = tile_blit_pos(wsx, wsy);

        // Sample the wall cell's top-left interior — overlaps the player sprite
        // footprint for this geometry; must be black (wall), not white (player).
        let sample_x = (wbx + 16) as u32;
        let sample_y = (wby + 8) as u32;
        assert!(
            sample_x < 400 && sample_y < 240,
            "sample ({sample_x},{sample_y}) must be on-screen"
        );
        // Confirm the sample sits inside both blit rects (otherwise the test is vacuous).
        let sx = sample_x as i32;
        let sy = sample_y as i32;
        assert!(
            sx >= wbx && sx < wbx + 32 && sy >= wby && sy < wby + 64,
            "sample must lie inside wall blit"
        );
        assert!(
            sx >= pbx && sx < pbx + 96 && sy >= pby && sy < pby + 96,
            "sample must also lie inside player blit (overlap required)"
        );
        assert!(
            g.fb.get_pixel(sample_x, sample_y),
            "nearer wall must leave black ink over the farther player at ({sample_x},{sample_y})"
        );
    }

    /// `enemy:bleed` / `samurai:bleed` sprays sit at `isoZ + 1` (player) or
    /// `isoZ ± 4` (enemy), below the actor's `isoZ + 5` — so the burst must paint
    /// *behind* the actor. Regression: it used to blit over the corpse.
    #[test]
    fn blood_spray_paints_behind_actor() {
        use crate::iso::{actor_blit_pos, grid_to_screen};
        use crate::level::Terrain;
        use crate::worldmap::Facing;

        let mut g = test_game();
        assert!(g.skip_boot());

        // Opaque white player + opaque black espray. NXOR flips any pixel the
        // spray lands on *after* the player, so white surviving ⇒ player last.
        let mut player_cell = Bitmap::with_alpha(96, 96);
        for y in 0..96u32 {
            for x in 0..96u32 {
                player_cell.set_pixel(x, y, true, true);
            }
        }
        let mut spray_cell = Bitmap::with_alpha(96, 96);
        for y in 0..96u32 {
            for x in 0..96u32 {
                spray_cell.set_pixel(x, y, false, true);
            }
        }
        let mut assets = DemoAssets::default();
        assets.player = Some(ImageTable {
            cells: vec![player_cell; 64], // 4 facings × 16 poses
            cells_per_row: 16,
        });
        // One cell per facing; `blit_espray` uses `numFrames = len / 4`.
        assets.espray[0] = Some(ImageTable {
            cells: vec![spray_cell; 4],
            cells_per_row: 1,
        });
        // Transparent tile table so `draw_room` blanks floors via no-op blits
        // instead of the fallback diamond, which would ink over the sprite.
        assets.tiles = Some(ImageTable {
            cells: vec![Bitmap::transparent(32, 64); 100],
            cells_per_row: 16,
        });
        g.set_demo_assets(assets);

        let (px, py) = (g.player_x, g.player_y);
        g.camera = (px, py);
        g.reload_room_terrain_for_test((px, py), 16, 16);
        g.room.enemies.clear();
        // Blank every room tile to transparent GID 1 (tiles[0]) so only the
        // player and spray decide the sample pixel — no nearer tile occlusion.
        let (x0, y0) = (g.room.origin_x, g.room.origin_y);
        for wy in y0..y0 + g.room.height {
            for wx in x0..x0 + g.room.width {
                g.set_room_cell_for_test(wx, wy, Terrain::Floor { tile: 1 });
            }
        }
        g.inject_blood_spray_for_test(px, py, Facing::North, 0);
        g.update(0.05, Input::default());

        let (ox, oy) = {
            let (cx, cy) = g.camera;
            (200 + (cy - cx) * 16, 80 - (cx + cy) * 8)
        };
        let (sx, sy) = grid_to_screen(px, py, ox, oy);
        let (bx, by) = actor_blit_pos(sx, sy);
        let sample_x = (bx + 48) as u32;
        let sample_y = (by + 48) as u32;
        assert!(sample_x < 400 && sample_y < 240, "sample must be on-screen");
        // `fb.get_pixel` is true for *ink* (black). White player ink clears it, so
        // a clean pixel here proves the black spray painted behind the player.
        assert!(
            !g.fb.get_pixel(sample_x, sample_y),
            "player (white) must paint over the spray (behind) at ({sample_x},{sample_y})"
        );
    }

    /// Pond / campfire GIDs from `isotile:setIndex` spawn in `mapanimtiles` and
    /// advance at `animspeed` (GID 170: frames 170→171→… every 8 ticks @ 20 Hz).
    #[test]
    fn map_anim_tiles_spawn_and_advance() {
        let mut g = test_game();
        assert!(g.skip_boot());

        // Outdoor start has no pond GIDs; park the camera on a known 170 cluster.
        let (pond_x, pond_y) = (77, 134);
        assert_eq!(g.world.tile(pond_x, pond_y), 170);
        g.camera = (pond_x, pond_y);
        g.player_x = pond_x;
        g.player_y = pond_y;
        // Find a nearby walkable pad so the room reload stays valid.
        let mut landed = false;
        for dy in -2..=2 {
            for dx in -2..=2 {
                let x = pond_x + dx;
                let y = pond_y + dy;
                if g.world.selector_can_step(x, y) {
                    g.player_x = x;
                    g.player_y = y;
                    landed = true;
                    break;
                }
            }
            if landed {
                break;
            }
        }
        assert!(landed, "need a walkable tile near pond ({pond_x},{pond_y})");
        g.reset_selector_for_test();
        g.reload_room_terrain_for_test(g.camera, 16, 16);

        assert!(
            g.map_anim_count_for_test() > 0,
            "viewport around pond must spawn mapanimtiles"
        );
        assert_eq!(
            g.map_anim_gid_at_for_test(pond_x, pond_y),
            Some(170),
            "pond cell starts on base GID 170"
        );

        // animspeed 8 → after 8×0.05s ticks, frame advances to 171.
        for _ in 0..8 {
            g.update(0.05, Input::default());
        }
        assert_eq!(
            g.map_anim_gid_at_for_test(pond_x, pond_y),
            Some(171),
            "GID 170 cycle advances 170→171 after one animspeed period"
        );
    }

    /// Door / teleport `exitroom` runs the loading wipe when `intro_seen`
    /// (`main.lua` `doTransition` + `initbackground`).
    #[test]
    fn room_exit_plays_loading_transition() {
        let mut g = test_game();
        assert!(g.skip_boot());
        let _ = g.take_sfx();

        g.facing = Facing::North;
        g.arm_door_exit_for_test(568);
        g.begin_room_transition_for_test();
        assert_eq!(g.state, GameState::Transition, "out wipe must lock input");

        // Out (1) → Loading: rebuild lands player; stay in Transition for the hold.
        g.tick_room_transition_for_test();
        assert_eq!(g.state, GameState::Transition, "loading hold must keep Transition");
        assert_eq!((g.player_x, g.player_y), (23, 35));

        g.drain_room_transition_for_test();
        assert_ne!(g.state, GameState::Transition);
        assert_eq!(g.camera, (23, 26));
        let sfx = g.take_sfx();
        assert!(
            sfx.iter().any(|s| *s == SfxId::Transition),
            "expected transition SFX, got {sfx:?}"
        );
        assert!(
            sfx.iter().any(|s| *s == SfxId::Transition2),
            "expected transition2 SFX, got {sfx:?}"
        );
    }

    /// Outdoor camera streaming must not flash the loading wipe.
    #[test]
    fn outdoor_reload_skips_loading_transition() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.reload_room_terrain_for_test((g.player_x, g.player_y), 16, 16);
        assert_ne!(g.state, GameState::Transition);
        assert_eq!(g.state, GameState::Aiming);
    }

    /// Worldmap exit 787 is the game-over door (`entrance == exitToGameOver`).
    #[test]
    fn game_over_exit_has_entrance_five() {
        let w = WorldMap::from_bytes(&test_worldmap_bytes()).expect("worldmap");
        let ex = w.exit_by_id(787).expect("exit 787");
        assert_eq!(ex.entrance, 5, "exitToGameOver");
        assert_eq!((ex.x, ex.y), (54, 8));
    }

    /// Drain win/room `initbackground` wipe so tests land on the post-load scene.
    fn drain_win_transition(g: &mut Game) {
        g.drain_room_transition_for_test();
    }

    /// Run `n` win 20 Hz steps, draining any loading wipe that opens mid-sequence.
    fn win_steps(g: &mut Game, n: usize) {
        drain_win_transition(g);
        for _ in 0..n {
            g.update(0.05, Input::default());
            drain_win_transition(g);
        }
    }

    /// `winGame` hides HUD/player, sets ending score from blood, camera (54,5)
    /// after the opening sea loading wipe (`initbackground` / `doTransition`).
    #[test]
    fn win_game_enters_win_cinema() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.life = 180;
        g.force_win_for_test();
        assert_eq!(g.state, GameState::Transition);
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);
        assert_eq!(g.camera, (54, 5));
        assert!(!g.hud_visible_for_test());
        assert!(!g.player_visible_for_test());
        assert_eq!(g.ending_score_for_test(), 180);
    }

    /// Win cinema keeps landscape `mapanimtiles` advancing (sea GID 245 @ 20 Hz).
    /// Lua `kGameWinState` ends each frame with `updatesprites()` / `isotile:update`.
    #[test]
    fn win_cinema_advances_sea_map_anims() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_win_for_test();
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);
        assert_eq!(g.camera, (54, 5));

        // Sea tiles fill the north edge of the win start camera.
        let (sea_x, sea_y) = (54, 1);
        assert_eq!(g.world.tile(sea_x, sea_y), 245);
        assert!(
            g.map_anim_count_for_test() > 0,
            "win start viewport must spawn sea mapanimtiles"
        );
        assert_eq!(
            g.map_anim_gid_at_for_test(sea_x, sea_y),
            Some(245),
            "sea cell starts on base GID 245"
        );

        // animspeed 8 → after 8×0.05s win ticks, frame advances to 246.
        win_steps(&mut g, 8);
        assert_eq!(g.state, GameState::Win);
        assert_eq!(
            g.map_anim_gid_at_for_test(sea_x, sea_y),
            Some(246),
            "GID 245 cycle advances 245→246 during win cinema"
        );
    }

    /// Win pans cameras at Lua tick thresholds via loading wipes; A after 125 → Score.
    #[test]
    fn win_cinema_pans_then_a_skips_to_score() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_win_for_test();
        drain_win_transition(&mut g);

        win_steps(&mut g, 51);
        assert_eq!(g.state, GameState::Win);
        assert_eq!(g.camera, (240, 47));

        win_steps(&mut g, 301 - 51);
        assert_eq!(g.camera, (217, 22));

        win_steps(&mut g, 601 - 301);
        assert_eq!(g.camera, (187, 17));
        assert!(g.credits_active_for_test());

        // A skip once ticks > 125 (already true).
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Score);
    }

    /// Opening win wipe + each pan wipe show `"loading..."` (Lua `initbackground`).
    #[test]
    fn win_cinema_uses_loading_wipe_between_scenes() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_win_for_test();
        assert_eq!(g.state, GameState::Transition);
        // Out holds previous room; first tick → Loading with new sea camera.
        g.tick_room_transition_for_test();
        assert_eq!(g.state, GameState::Transition);
        assert_eq!(g.camera, (54, 5));
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);

        win_steps(&mut g, 51);
        // After the pan threshold, win_steps drains the wipe; camera is the first pan.
        assert_eq!(g.camera, (240, 47));
    }

    /// Score screen creates the panel after 20 ticks; A restarts.
    #[test]
    fn score_screen_shows_last_score_then_restarts() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.life = 99;
        g.force_win_for_test();
        drain_win_transition(&mut g);
        // Jump straight to score via A skip after enough win ticks.
        win_steps(&mut g, 130);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Score);

        // Panel at ticks > 20; A restart needs ticks > 25.
        for _ in 0..26 {
            g.update(0.05, Input::default());
        }
        assert!(g.hs_table_for_test());
        let line = g.hs_player_text_for_test();
        assert!(
            line.starts_with("last score") && line.ends_with("99"),
            "expected last score…99, got {line:?}"
        );
        let table = g.hs_table_text_for_test();
        assert!(
            table.contains("you") && table.contains("99"),
            "expected local board row you…99, got {table:?}"
        );
        assert_eq!(g.highscore_count_for_test(), 1);
        assert!(g.hs_needs_flush());
        let json = g.take_hs_flush().expect("dirty hs");
        assert!(json.contains("99"));
        assert!(!g.hs_needs_flush());

        // Release then press A again (edge).
        g.update(0.05, Input::default());
        input.buttons.a = true;
        g.update(0.05, input);
        assert_eq!(g.state, GameState::Aiming);
        assert!(g.player_visible_for_test());
        assert!(g.hud_visible_for_test());
    }

    /// Second win keeps the higher blood entry first; JSON round-trips.
    #[test]
    fn highscores_persist_across_wins_and_json() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.apply_highscores_json(r#"{"v":1,"scores":[{"player":"you","value":200}]}"#);
        assert_eq!(g.highscore_count_for_test(), 1);

        g.life = 120;
        g.force_win_for_test();
        drain_win_transition(&mut g);
        win_steps(&mut g, 130);
        let mut input = Input::default();
        input.buttons.a = true;
        g.update(0.05, input);
        for _ in 0..26 {
            g.update(0.05, Input::default());
        }
        assert_eq!(g.highscore_count_for_test(), 2);
        let table = g.hs_table_text_for_test();
        let lines: Vec<_> = table.lines().collect();
        assert!(lines[0].ends_with("200"), "higher score first: {table:?}");
        assert!(lines[1].ends_with("120"), "new score second: {table:?}");

        let json = g.export_highscores();
        let mut g2 = test_game();
        g2.apply_highscores_json(&json);
        assert_eq!(g2.highscore_count_for_test(), 2);
        // Delete save must not wipe HS.
        g2.delete_save();
        assert_eq!(g2.highscore_count_for_test(), 2);
    }

    /// Arming exit 787 with N facing → win instead of room transition.
    #[test]
    fn game_over_door_exit_triggers_win() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.facing = Facing::North;
        g.arm_door_exit_for_test(787);
        g.begin_room_transition_for_test();
        assert_eq!(g.state, GameState::Transition);
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);
        assert_eq!(g.camera, (54, 5));
    }

    /// Mid-run save: terminate while alive Playing → `loadsave=1` + blood/key/pos.
    #[test]
    fn mid_run_terminate_snapshots_alive_play() {
        use crate::save::{SavedGame, SAVE_VERSION};
        let mut g = test_game_seed(99);
        assert!(g.skip_boot());
        // Simulate post-door in-progress run.
        g.force_write_save_after_exit_for_test();
        assert!(g.game_in_progress());
        let json = g.game_will_terminate().expect("terminate writes");
        let s = SavedGame::from_json(&json).expect("parse");
        assert_eq!(s.loadsave, 1);
        assert_eq!(s.version, SAVE_VERSION);
        assert_eq!(s.seed, 99);
        assert_eq!(s.blood, g.life);
        assert!(s.player_pos.is_some());
    }

    /// Death → `deletesave` (`loadsave=0`); terminate returns cleared blob.
    #[test]
    fn mid_run_death_clears_loadsave() {
        use crate::save::SavedGame;
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_write_save_after_exit_for_test();
        assert_eq!(
            SavedGame::from_json(&g.export_save()).unwrap().loadsave,
            1
        );
        g.force_player_death_for_test();
        let s = SavedGame::from_json(&g.export_save()).unwrap();
        assert_eq!(s.loadsave, 0);
        assert!(!g.game_in_progress());
    }

    /// Win → `deletesave`.
    #[test]
    fn mid_run_win_clears_loadsave() {
        use crate::save::SavedGame;
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_write_save_after_exit_for_test();
        g.facing = Facing::North;
        g.arm_door_exit_for_test(787);
        g.begin_room_transition_for_test();
        drain_win_transition(&mut g);
        assert_eq!(g.state, GameState::Win);
        let s = SavedGame::from_json(&g.export_save()).unwrap();
        assert_eq!(s.loadsave, 0);
    }

    /// Reverse room terminate → no write (`None`).
    #[test]
    fn mid_run_reverse_room_terminate_skips() {
        let mut g = test_game();
        assert!(g.skip_boot());
        g.force_write_save_after_exit_for_test();
        g.set_reverse_room_for_test(true);
        assert!(g.game_will_terminate().is_none());
    }

    /// Restore: seed + blood + key + playerPos + seen flag round-trip.
    #[test]
    fn mid_run_restore_applies_pos_blood_key_seen() {
        use crate::save::{DeadEnemySave, SavedGame, SAVE_VERSION};
        let seed = 12345u32;
        let mut g = test_game_seed(seed);
        assert!(g.skip_boot());
        let (px, py) = (g.player_x, g.player_y);
        let cam = g.camera;
        // Build a resume blob as if terminate had written it.
        let mut blob = SavedGame::default();
        blob.loadsave = 1;
        blob.version = SAVE_VERSION;
        blob.seed = seed;
        blob.blood = 180;
        blob.key = true;
        blob.player_pos = Some([px, py, cam.0, cam.1, Facing::North as i32]);
        blob.enemies_seen = vec![false, true, true, false, false, false, false];
        blob.enemies_killed = vec![false, true, false, false, false, false, false];
        blob.dead_enemies.push(DeadEnemySave {
            x: px + 2,
            y: py - 1,
            etype: 1,
            facing: 2,
        });
        let json = blob.to_json();

        let mut g2 = test_game_seed(seed);
        // World already loaded via test_game_seed; apply before title.
        assert!(g2.apply_save_json(&json));
        assert_eq!(g2.state, GameState::Aiming);
        assert!(g2.intro_seen());
        assert_eq!(g2.life, 180);
        assert!(g2.has_key());
        assert_eq!(g2.player_x, px);
        assert_eq!(g2.player_y, py);
        assert!(g2.dialog().enemies_seen(2), "pikeman seen restored");
        assert!(g2.dialog().enemies_killed(1), "swordsman killed restored");
        // One-shot: loadsave cleared after restore.
        let cleared = SavedGame::from_json(&g2.export_save()).unwrap();
        assert_eq!(cleared.loadsave, 0);
        assert!(g2.deadenemies_for_test(px + 2, py - 1).is_some());
    }

    /// Bad / missing version → restore false.
    #[test]
    fn mid_run_restore_rejects_bad_version() {
        use crate::save::SavedGame;
        let mut g = test_game();
        let mut blob = SavedGame::default();
        blob.loadsave = 1;
        blob.version = 0;
        blob.seed = 1;
        blob.blood = 100;
        blob.player_pos = Some([109, 182, 110, 180, 1]);
        assert!(!g.apply_save_json(&blob.to_json()));
    }

    /// Host menu **delete save** clears loadsave and restarts Aiming outdoors.
    /// Keeps boot `random_seed` (Lua `startGame` does not re-roll).
    #[test]
    fn mid_run_delete_save_menu_clears_and_restarts() {
        use crate::save::SavedGame;
        let mut g = test_game();
        assert!(g.skip_boot());
        let seed_before = g.random_seed();
        g.force_write_save_after_exit_for_test();
        assert!(g.game_in_progress());
        g.delete_save_menu();
        let s = SavedGame::from_json(&g.export_save()).unwrap();
        assert_eq!(s.loadsave, 0);
        assert!(!g.game_in_progress());
        assert_eq!(g.state, GameState::Aiming);
        assert_eq!(g.player_x, START_PLAY.0);
        assert_eq!(g.player_y, START_PLAY.1);
        assert_eq!(
            g.random_seed(),
            seed_before,
            "delete save must keep boot seed (Lua startGame does not re-roll)"
        );
    }

    /// All monks at ~140 with key: dialog + heal must raise `life`, and LIFE HUD catches up.
    #[test]
    fn all_monks_heal_at_low_life_with_key() {
        // Approach from a walkable neighbor (not all monks have a free south tile).
        let monks = [
            (145, 166, 20, 145, 167),
            (9, 88, 40, 9, 89),
            (30, 57, 40, 30, 58),
            (77, 62, 40, 77, 63),
            (137, 71, 40, 138, 71),
        ];
        for (mx, my, heal, sx, sy) in monks {
            let mut g = test_game();
            let mut input = Input::default();
            input.buttons.a = true;
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);

            g.set_god_mode(true);
            g.set_cheat_key(true);
            g.set_invulnerable(false); // walk still spends 1 blood
            g.set_life_for_test(140);

            let npc = g.world.npc_at(mx, my).expect("monk");
            assert_eq!(npc.heal, heal, "heal at ({mx},{my})");
            assert_eq!(npc.dialog, "monk");

            g.player_x = sx;
            g.player_y = sy;
            g.camera = (mx, my);
            g.reload_room_terrain_for_test((mx, my), 16, 16);
            g.reset_selector_for_test();

            // Aim onto the monk pad.
            let (dx, dy) = (mx - sx, my - sy);
            input = Input::default();
            if dx == 1 {
                input.buttons.right = true;
            } else if dx == -1 {
                input.buttons.left = true;
            } else if dy == 1 {
                input.buttons.down = true;
            } else if dy == -1 {
                input.buttons.up = true;
            }
            g.update(0.05, input);
            input = Input::default();
            g.update(0.05, input);
            assert_eq!(g.selector_tile(), (mx, my), "aim at monk ({mx},{my})");

            let life_before = g.life;
            input.buttons.a = true;
            g.update(0.05, input);

            assert_eq!(
                (g.player_x, g.player_y),
                (mx, my),
                "landed on monk ({mx},{my})"
            );
            let expected = (life_before - 1 + heal).min(250);
            assert_eq!(
                g.life, expected,
                "monk ({mx},{my}) heal={heal}: life {life_before} -> {}",
                g.life
            );

            g.update(0.05, Input::default());
            assert_eq!(g.state, GameState::Dialog, "monk dialog at ({mx},{my})");

            for _ in 0..40 {
                g.update(0.05, Input::default());
            }
            assert_eq!(g.life, expected, "life stays healed during dialog");
            assert_eq!(
                g.display_blood_for_test(),
                expected,
                "LIFE HUD must catch up to healed blood at ({mx},{my})"
            );
        }
    }

}

