//! Bottom dialog bar (`dialog.lua`) — chrome, queue, scripts, A-to-dismiss.
//!
//! Spec: `docs/dialog-system.md` and decompiled `dialog.lua` / `script.lua`.

use std::collections::VecDeque;

use crate::bitmap::{Bitmap, DrawMode, ImageTable};
use crate::dialog_scripts::{dialog_script, dialog_script_count};
use crate::framebuffer::Framebuffer;
use crate::pft::Font;

/// Screen position (`dialogbar:moveTo(46, 163)` with center 0,0).
pub const DIALOG_X: i32 = 46;
pub const DIALOG_Y: i32 = 163;
pub const DIALOG_W: i32 = 307;
pub const DIALOG_H: i32 = 47;

/// Face blit inside the bar (`dialogfaces[…]:draw(8, 7)`).
const FACE_X: i32 = 8;
const FACE_Y: i32 = 7;
/// Text rect (`drawTextInRect(text, 80, 7, 200, 38, -1)`).
const TEXT_X: i32 = 80;
const TEXT_Y: i32 = 7;
const TEXT_W: i32 = 200;
const TEXT_H: i32 = 38;

/// Open/close run at 10 fps (`addAnim(..., 10, false)`).
const FRAME_DT: f32 = 0.1;

/// 1-based open sequence; idle holds the last open frame.
const OPEN_FRAMES: &[usize] = &[1, 2, 3, 4, 5, 6, 7];
const CLOSE_FRAMES: &[usize] = &[7, 6, 5, 4, 3, 2, 1];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimKind {
    Open,
    Close,
}

#[derive(Debug, Clone)]
struct Anim {
    kind: AnimKind,
    /// Index into OPEN/CLOSE_FRAMES.
    index: usize,
    /// Countdown to next frame advance.
    counter: f32,
}

/// Queued line: text + Lua face arg (`f` in `show`) + open delay seconds.
#[derive(Debug, Clone)]
struct QueuedLine {
    text: String,
    /// Face index passed to `show` (0 = player, 1 = swordsman, …, 8 = monk).
    face: i32,
    delay: f32,
}

/// Runtime dialog bar — mirrors `class("dialog")`.
#[derive(Debug)]
pub struct DialogBar {
    open: bool,
    /// 1-based image-table frame currently drawn.
    current_frame: usize,
    anim: Option<Anim>,
    queue: VecDeque<QueuedLine>,
    current_text: String,
    /// 1-based index into `dialogfaces` (Lua `currentFace = f + 1`).
    current_face: usize,
    blood_stun: bool,
    /// `main.lua` `ticks` while `kGameDialogState` (A dismiss when > 3).
    ticks: i32,
    /// `dialog.lua` `startDialogShown` — `start` only once per process.
    start_dialog_shown: bool,
    /// `script.lua` `dialogCounters` — Lua integers (0 = none; N = last shown 1-based).
    counters: Vec<usize>,
    /// First-time enemy seen / killed flags (Lua 1..=6; index 0 unused).
    /// `enemiesSeen[1]` (swordsman) starts true.
    enemies_seen: [bool; 7],
    enemies_killed: [bool; 7],
}

impl Default for DialogBar {
    fn default() -> Self {
        Self::new()
    }
}

impl DialogBar {
    pub fn new() -> Self {
        Self {
            open: false,
            current_frame: 1,
            anim: None,
            queue: VecDeque::new(),
            current_text: String::new(),
            current_face: 1,
            blood_stun: false,
            ticks: 0,
            start_dialog_shown: false,
            // Resize on first show / load_dialogs if corpus arrives later.
            counters: vec![0; dialog_script_count().max(31)],
            enemies_seen: [false, true, false, false, false, false, false],
            enemies_killed: [false; 7],
        }
    }

    /// Ensure `counters` covers the loaded corpus (after late [`crate::dialog_scripts::load_dialogs`]).
    fn ensure_counters(&mut self) {
        let n = dialog_script_count().max(31);
        if self.counters.len() < n {
            self.counters.resize(n, 0);
        }
    }

    /// Death → `startGame` reset (`main.lua`).
    ///
    /// Lua resets `enemiesSeen` / `enemiesKilled` each run, but **keeps**
    /// `dialogCounters` (only wiped by `deleteProgress` / menu "delete save").
    /// So first swordsman kill after a death shows the *next* `*Killed` line,
    /// not "Impossible!" again — until the script hits `stop`.
    /// Keeps `start_dialog_shown` (Lua process-global).
    pub fn reset_run_progress(&mut self) {
        self.enemies_seen = [false, true, false, false, false, false, false];
        self.enemies_killed = [false; 7];
        self.dump();
    }

    /// `deleteProgress()` — wipe `dialogCounters` + seen/killed (`script.lua`).
    ///
    /// Lua also writes empty counters to `"dialog_progress_save"`; host persistence
    /// for that store is still deferred — this clears the in-RAM counters.
    /// Note: Lua's `enemiesSeen` table after delete is 1-based swordsman-first
    /// (`{true,false,…}`); our array keeps index 0 unused and index 1 = swordsman.
    pub fn delete_progress(&mut self) {
        self.ensure_counters();
        for c in &mut self.counters {
            *c = 0;
        }
        // Match `reset_run_progress` layout (swordsman pre-seen), not Lua's
        // 1-based reshuffle after deleteProgress — startGame rebuilds seen/killed
        // again immediately after the menu path anyway.
        self.enemies_seen = [false, true, false, false, false, false, false];
        self.enemies_killed = [false; 7];
        self.dump();
    }

    /// True while a line is up, queued, or chrome is still animating (incl. close).
    /// Prefer [`Self::is_showing`] / `GameState::Dialog` for gameplay gates — close
    /// anim alone must not block aiming (Lua resumes Playing inside `hide`).
    pub fn is_open(&self) -> bool {
        self.open || !self.queue.is_empty() || self.anim.is_some()
    }

    /// True while a dialog line is actively shown (`open`), not merely closing.
    pub fn is_showing(&self) -> bool {
        self.open
    }

    /// True while open/close chrome anim is in flight (tests / draw).
    pub fn anim_active_for_test(&self) -> bool {
        self.anim.is_some()
    }

    /// Drop queue and force-close if open (`dialog:dump`).
    pub fn dump(&mut self) {
        self.queue.clear();
        self.open = false;
        self.anim = None;
        self.current_frame = 1;
        self.blood_stun = false;
    }

    /// Map `dialog:show` string key → 1-based script index (0 = unknown / skip).
    fn script_index_for(
        &mut self,
        key: &str,
        has_key: bool,
        chest_dir_script: Option<usize>,
    ) -> usize {
        match key {
            "start" => {
                if self.start_dialog_shown {
                    return 0;
                }
                self.start_dialog_shown = true;
                1
            }
            "monk" => {
                if has_key {
                    2
                } else {
                    chest_dir_script.unwrap_or(2)
                }
            }
            "reverse" => 3,
            "swordsmanSeen" => 4,
            "pikemanSeen" => 5,
            "ninjaSeen" => 6,
            "guardSeen" => 7,
            "kingSeen" => 8,
            "spiritSeen" => 9,
            "swordsmanKilled" => 10,
            "pikemanKilled" => 11,
            "ninjaKilled" => 12,
            "guardKilled" => 13,
            "kingKilled" => 14,
            "spiritKilled" => 15,
            "playerKilled" => 16,
            "playerStarved" => 17,
            "playerRevived" => 18,
            "blinded" => {
                self.blood_stun = true;
                19
            }
            "hurry" => 20,
            "trap" => 21,
            "castle" => 22,
            "turnback" => 23,
            "lost" => 24,
            "grave" => 25,
            "door" => {
                if has_key {
                    26
                } else {
                    27
                }
            }
            _ => 0,
        }
    }

    /// `dialog:show(t, f, d)` — enqueue next line for script `t`.
    ///
    /// Counters match Lua: `0` = none shown; after first show `1`, text = `dialogs[1]`.
    /// `chest_dir_script` is Lua script index 28..=31 when the monk has no key.
    pub fn show(
        &mut self,
        key: &str,
        face: i32,
        delay: f32,
        has_key: bool,
        chest_dir_script: Option<usize>,
    ) {
        self.blood_stun = false;
        self.ensure_counters();
        let n = dialog_script_count();
        if n == 0 {
            return;
        }
        let script_index = self.script_index_for(key, has_key, chest_dir_script);
        if script_index == 0 || script_index > n {
            return;
        }
        let si = script_index - 1;
        let script = dialog_script(si);
        if script.is_empty() {
            return;
        }

        // Peek `dialogs[counters + 1]` (Lua 1-based) == `script[counters]` (0-based).
        let c = self.counters[si];
        match script.get(c).map(String::as_str) {
            Some("loop") => self.counters[si] = 0,
            Some("hold") => {
                // `counters = #dialogs - 2` then the +1 below yields `#-1` (last real).
                self.counters[si] = script.len().saturating_sub(2);
            }
            Some("stop") => return,
            _ => {}
        }

        let new_c = (self.counters[si] + 1).min(script.len());
        self.counters[si] = new_c;
        let line = &script[new_c - 1];
        if matches!(line.as_str(), "loop" | "hold" | "stop") {
            return;
        }
        self.queue.push_back(QueuedLine {
            text: line.clone(),
            face,
            delay,
        });
    }

    /// `dialog:hide` — play close. Returns `true` if the queue is empty (session done
    /// after close anim; caller restores playing / game over).
    pub fn hide(&mut self) -> bool {
        self.play_close();
        self.open = false;
        self.queue.is_empty()
    }

    fn play_open(&mut self, delay: f32) {
        self.anim = Some(Anim {
            kind: AnimKind::Open,
            index: 0,
            counter: FRAME_DT + delay,
        });
        self.current_frame = OPEN_FRAMES[0];
    }

    fn play_close(&mut self) {
        self.anim = Some(Anim {
            kind: AnimKind::Close,
            index: 0,
            counter: FRAME_DT,
        });
        self.current_frame = CLOSE_FRAMES[0];
    }

    /// Per-frame update (`dialog:update`).
    pub fn update(&mut self, dt: f32) -> DialogEvent {
        let mut ev = DialogEvent::None;

        if !self.queue.is_empty() && !self.open {
            let line = self.queue.pop_front().expect("queue");
            self.play_open(line.delay);
            self.current_face = (line.face + 1).max(1) as usize;
            self.current_text = line.text;
            self.open = true;
            self.ticks = 0;
            ev = DialogEvent::Opened;
        }

        if let Some(anim) = self.anim.as_mut() {
            anim.counter -= dt;
            if anim.counter <= 0.0 {
                anim.index += 1;
                let frames = match anim.kind {
                    AnimKind::Open => OPEN_FRAMES,
                    AnimKind::Close => CLOSE_FRAMES,
                };
                if anim.index >= frames.len() {
                    let kind = anim.kind;
                    self.anim = None;
                    if kind == AnimKind::Close {
                        self.current_frame = 1;
                        if self.queue.is_empty() {
                            ev = DialogEvent::ClosedIdle;
                        }
                    }
                } else {
                    self.current_frame = frames[anim.index];
                    anim.counter = FRAME_DT;
                }
            }
        }

        ev
    }

    /// Advance dialog-state tick counter (20 Hz).
    pub fn tick_dialog(&mut self) {
        self.ticks = self.ticks.saturating_add(1);
    }

    /// `main.lua` `ticks` while the dialog state is active.
    pub fn ticks(&self) -> i32 {
        self.ticks
    }

    /// Lua: A dismisses when `ticks > 3`.
    pub fn ticks_ready(&self) -> bool {
        self.ticks > 3
    }

    pub fn enemies_seen(&self, etype: usize) -> bool {
        self.enemies_seen.get(etype).copied().unwrap_or(true)
    }

    pub fn mark_seen(&mut self, etype: usize) {
        if etype < self.enemies_seen.len() {
            self.enemies_seen[etype] = true;
        }
    }

    pub fn enemies_killed(&self, etype: usize) -> bool {
        self.enemies_killed.get(etype).copied().unwrap_or(true)
    }

    pub fn mark_killed(&mut self, etype: usize) {
        if etype < self.enemies_killed.len() {
            self.enemies_killed[etype] = true;
        }
    }

    /// Snapshot `enemiesSeen` for mid-run save (`gameWillTerminate`).
    pub fn enemies_seen_flags(&self) -> [bool; 7] {
        self.enemies_seen
    }

    /// Snapshot `enemiesKilled` for mid-run save.
    pub fn enemies_killed_flags(&self) -> [bool; 7] {
        self.enemies_killed
    }

    /// Restore seen/killed from save without touching `dialogCounters`.
    pub fn restore_enemy_flags(&mut self, seen: &[bool], killed: &[bool]) {
        for i in 0..7 {
            if let Some(&v) = seen.get(i) {
                self.enemies_seen[i] = v;
            }
            if let Some(&v) = killed.get(i) {
                self.enemies_killed[i] = v;
            }
        }
    }

    /// Draw the bar at its screen position.
    pub fn draw(
        &self,
        fb: &mut Framebuffer,
        dialogbg: Option<&ImageTable>,
        faces: &[Option<Bitmap>; 9],
        faceblood: Option<&Bitmap>,
        font: Option<&Font>,
    ) {
        if !self.open && self.anim.is_none() {
            return;
        }
        let frame_i = self.current_frame.saturating_sub(1);
        if let Some(table) = dialogbg {
            if let Some(cell) = table.get(frame_i) {
                cell.blit(fb, DIALOG_X, DIALOG_Y);
            }
        } else {
            fb.fill_rect(DIALOG_X, DIALOG_Y, DIALOG_W, DIALOG_H, true);
        }

        if self.current_frame < 4 {
            return;
        }

        let fi = self.current_face.saturating_sub(1);
        if let Some(Some(face)) = faces.get(fi) {
            face.blit(fb, DIALOG_X + FACE_X, DIALOG_Y + FACE_Y);
        }
        if self.blood_stun {
            if let Some(blood) = faceblood {
                blood.blit_mode(
                    fb,
                    DIALOG_X + FACE_X,
                    DIALOG_Y + FACE_Y,
                    DrawMode::Inverted,
                );
            }
        }

        if self.current_frame < 6 {
            return;
        }
        if let Some(font) = font {
            font.draw_text_in_rect(
                fb,
                &self.current_text,
                DIALOG_X + TEXT_X,
                DIALOG_Y + TEXT_Y,
                TEXT_W,
                TEXT_H,
                1,
                -1,
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogEvent {
    None,
    /// Entered `kGameDialogState`.
    Opened,
    /// Close anim finished and queue empty — restore playing / game over.
    ClosedIdle,
}

/// Lua `chester:directionFromPlayer` → script index 28..=31.
///
/// Labels are iso / D-pad diagonals (Up=NE, Right=SE, Down=SW, Left=NW), not
/// a north-up geographic compass. Dominant grid axis only; secondary ignored.
pub fn chest_direction_script(chest_x: i32, chest_y: i32, player_x: i32, player_y: i32) -> usize {
    let dx = chest_x - player_x;
    let dy = chest_y - player_y;
    if dx.abs() > dy.abs() {
        if dx > 0 {
            29
        } else {
            31
        }
    } else if dy > 0 {
        30
    } else {
        28
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog_scripts::load_test_dialogs;

    fn bar_with_dialogs() -> DialogBar {
        load_test_dialogs();
        DialogBar::new()
    }

    #[test]
    fn monk_heal_script_advances() {
        let mut d = bar_with_dialogs();
        d.show("monk", 8, 0.0, true, None);
        assert_eq!(d.queue.len(), 1);
        assert!(
            d.queue[0].text.contains("Heading")
                || d.queue[0].text.contains("salve")
                || d.queue[0].text.contains("patch")
        );
        d.show("monk", 8, 0.0, true, None);
        assert_eq!(d.queue.len(), 2);
    }

    #[test]
    fn stop_terminal_blocks() {
        let mut d = bar_with_dialogs();
        for _ in 0..20 {
            d.show("swordsmanKilled", 1, 0.0, false, None);
        }
        let n = d.queue.len();
        d.show("swordsmanKilled", 1, 0.0, false, None);
        assert_eq!(d.queue.len(), n, "stop should refuse further lines");
    }

    /// Death restart clears seen/killed flags but keeps counters (`startGame`).
    #[test]
    fn reset_run_keeps_counters_advances_killed_lines() {
        let mut d = bar_with_dialogs();
        d.show("swordsmanKilled", 1, 0.0, false, None);
        assert_eq!(
            d.queue.back().map(|q| q.text.as_str()),
            Some("Impossible! No human being can move that fast.")
        );
        d.queue.clear();
        d.mark_killed(1);

        d.reset_run_progress();
        assert!(!d.enemies_killed(1), "enemiesKilled resets on startGame");

        d.show("swordsmanKilled", 1, 0.0, false, None);
        assert_eq!(
            d.queue.back().map(|q| q.text.as_str()),
            Some("What!?"),
            "dialogCounters must survive death restart"
        );
    }

    #[test]
    fn hold_repeats_last_real_line() {
        let mut d = bar_with_dialogs();
        // kingSeen ends in hold (script 8).
        for _ in 0..20 {
            d.show("kingSeen", 5, 0.0, false, None);
        }
        let last = d.queue.back().map(|q| q.text.clone());
        d.show("kingSeen", 5, 0.0, false, None);
        assert_eq!(d.queue.back().map(|q| q.text.clone()), last);
    }

    #[test]
    fn chest_direction_quadrants() {
        // Pure N/E/S/W on the grid → iso D-pad labels NE/SE/SW/NW.
        assert_eq!(chest_direction_script(10, 5, 10, 10), 28); // north → NE
        assert_eq!(chest_direction_script(20, 10, 10, 10), 29); // east → SE
        assert_eq!(chest_direction_script(10, 20, 10, 10), 30); // south → SW
        assert_eq!(chest_direction_script(0, 10, 10, 10), 31); // west → NW
    }

    /// Reported play sessions: dominant axis wins; secondary sign ignored
    /// (matches `chestman.luac` `directionFromPlayer`).
    #[test]
    fn chest_direction_reported_coords() {
        // Monk (137,71), chest (174,58): mostly east, a bit north → SE (29).
        assert_eq!(chest_direction_script(174, 58, 137, 71), 29);
        // Monk (145,166), chest (58,173): mostly west → NW (31).
        assert_eq!(chest_direction_script(58, 173, 145, 166), 31);
        // Monk (77,62), chest (58,173): mostly south → SW (30).
        assert_eq!(chest_direction_script(58, 173, 77, 62), 30);
        // Tie on abs → Y branch (Lua `|dy| < |dx|` is strict).
        assert_eq!(chest_direction_script(15, 5, 10, 10), 28);
    }
}
