//! Session state and turn loop.
//!
//! Control scheme follows decompiled `selector.lua` + `main.lua` button handlers:
//! - D-pad: extend/retract the selector one tile at a time (cardinal only)
//! - Hold-repeat: `main.lua` `buttonsdown` at 20 Hz (`setRefreshRate(20)`)
//! - A: commit zip when cursor ≠ player
//! - B: reset cursor to player
//!
//! Exit handling follows `samurai:playerMoved` / `exitroom` (door GIDs + exit objects).

use crate::bitmap::{Bitmap, DrawMode, ImageTable};
use crate::ending::{
    credits_show, draw_credits, draw_highscore_panel, draw_winbackground, format_last_score_line,
    start_hs_open, tick_credits_anim, tick_hs_anim, tick_winbackground, EndingRuntime,
    CREDIT_TICKS, EXIT_TO_GAME_OVER, WIN_CAMERA_PAN, WIN_CAMERA_START,
};
use crate::dialog::{chest_direction_script, DialogBar, DialogEvent};
use crate::framebuffer::{Framebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::iso::{
    actor_blit_pos, drip_blit_pos, floor_blood_blit_pos, grid_to_screen, isosprite_frame_index,
    selector_icon_blit_pos, smoke_blit_pos, tile_blit_pos, ACTOR_CENTER_X, ACTOR_CENTER_Y,
    TILE_HALF_H, TILE_HALF_W,
};
use crate::level::{load_start_room, path_cells, viewport_around, Enemy, EnemyKind, Room, Terrain};
use crate::highscores::HighScoreBoard;
use crate::pft::Font;
use crate::save::{DeadEnemySave, SavedGame, SAVE_VERSION};
use crate::text::draw_text;
use crate::worldmap::{
    face_player, find_local_enemies, locals_from_globals, Facing, SpawnRng, WorldMap,
    MAX_STEPS,
    SCORE_PER_LEVEL, START_CAMERA, START_PLAY, START_TITLE,
};

/// `playdate.display.setRefreshRate(20)` / `Globals.deltaTime = 0.05`.
const INPUT_TICK_DT: f32 = 0.05;
/// `main.lua` playing-state loop: auto-repeat while `buttonsdown[dir] >= 4`
/// (pre-increment `v` from `pairs`), i.e. first repeat on the 5th 20 Hz tick.
const DPAD_REPEAT_AFTER: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameState {
    Boot,
    /// `kGameTitleState` — inverted white hold before the walk intro.
    Title,
    /// `kGameIntroState` — white-field north walk + bennett, then `endintro`.
    Intro,
    /// `kSelectorMove` — aiming with the cursor.
    Aiming,
    /// Transient; enemy phase now uses [`PendingEnemyMove`] across Aiming ticks.
    Resolving,
    /// `kGameDialogState` — bottom dialog bar open; A dismisses after ticks > 3.
    Dialog,
    /// `kGameOverState` — restart prompt slides up; any button after ticks > 5.
    GameOver,
    /// Room-change loading wipe (`main.lua` `initbackground` when `doTransition`).
    Transition,
    /// `kGameWinState` — sea pans + credits after castle exit (`exitToGameOver`).
    Win,
    /// `kGameScoreState` — `winbackground` + local last-score panel.
    Score,
}

/// Phases of [`GameState::Transition`] (tick-phased stand-in for `playdate.wait`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionPhase {
    /// Old room + left diagonal wipe (~50 ms / 1 tick).
    Out,
    /// Clear field + `"loading..."`; room rebuild runs on entry.
    Loading,
    /// New room + right diagonal wipe; then return to Aiming/Dialog.
    In,
}

/// How long `"loading..."` stays up at 20 Hz.
///
/// Lua blocks for the whole strip rebuild (often hundreds of ms). Wasm rebuilds
/// in one frame, so we hold this many ticks (~0.4 s) so the screen is readable.
const TRANSITION_LOADING_TICKS: i32 = 8;

/// Deferred `kPlayerMove` slash commit (`main.lua` ticks==1 preslash, ticks>3 kill+land).
/// Stab-only and walk zips still resolve immediately (slash path is the phased case).
#[derive(Debug, Clone)]
struct PendingSlashMove {
    /// Phase ticks after A commit (`main.lua` `ticks`, reset to 0 on enter).
    ticks: i32,
    tip_x: i32,
    tip_y: i32,
    facing: Facing,
    /// `math.max(absx, absy)` passed to `playerMoved` / floorsmoke.
    numsteps: i32,
    /// Readybar length → enemy step budget after the player phase.
    enemy_budget: i32,
    /// Trailing stab segment → `slashandstab` instead of `slash`.
    has_stab: bool,
    /// Deduped `deathlist` indices into `room.enemies`.
    kill_indices: Vec<usize>,
}

/// Deferred `kEnemyMove` (`main.lua`): one `step_enemies` per readybar segment per
/// 20 Hz tick, then `braintwo` when the bar is empty. Keeps kill→revenge from
/// landing on the same frame as the player's land/kill FX.
#[derive(Debug, Clone)]
struct PendingEnemyMove {
    /// Remaining readybar segments to burn (`#readybar.segmentList`).
    steps_left: i32,
}

/// Result of processing one zip's deathlist (`enemy:kill` / dodge / parry).
struct DeathlistOutcome {
    /// True when a parry knockback walk starved the player mid-resolve.
    starved: bool,
    /// Real deaths (not dodge/parry).
    kills: i32,
    /// Deathlist had at least one living entry (even if all dodged/parried).
    any_attack: bool,
    kill_snapshots: Vec<(usize, i32, i32, Facing, crate::level::EnemyKind, crate::level::Enemy)>,
    deathlist_indices: Vec<usize>,
}

/// Softsynth events for the Playdate-style instrument (`Globals.lua` sho / introchord).
///
/// Timing lives in core; the host (Web Audio / future native synth) renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SynthEvent {
    /// `instrument:playMIDINote(n)` with no length → hold until [`Self::AllNotesOff`].
    NoteOn(u8),
    /// Sequence / timed release for one MIDI note (`Sho.mid` note-off).
    NoteOff(u8),
    /// `instrument:allNotesOff()` (+ synth stop) — `endintro` / skip.
    AllNotesOff,
}

/// One-shot SFX ids matching `soundm` sampleplayers (`soundm.lua`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SfxId {
    /// `soundm.select` / `Sounds/select.pda` — `selector:moveByTile`.
    Select,
    /// `soundm.buzz` / `Sounds/buzz.pda` — blocked selector move (`buzz()`).
    Buzz,
    /// `soundm.slash` — player slash / stab / slashandstab.
    Slash,
    /// `soundm.step` — short walk zip (`samurai:step`, numsteps < 5).
    Step,
    /// `soundm.swoosh` — long dash zip (`samurai:step`, numsteps >= 5).
    Swoosh,
    /// `soundm.falldeadsounds[1]` — enemy death yell / body fall.
    Falldead,
    Falldead2,
    Falldead3,
    Falldead4,
    /// `soundm.playerdeath` — player killed / starved (`samurai:kill`).
    PlayerDeath,
    /// `soundm.deathmusic` / `Sounds/Music/music_death` — dialog tick 30 while dead.
    DeathMusic,
    /// `soundm.blood` — enemy `bleed` splat.
    Blood,
    /// `soundm.lifedown` — lifebar display counting down.
    LifeDown,
    /// `soundm.lifeup` — lifebar display counting up (heal).
    LifeUp,
    /// `soundm.clunk` / `Sounds/trapdoor.pda` — gate slide / spirit-room door.
    Clunk,
    /// `soundm.key` / `Sounds/key.pda` — `chester:checkChest` pickup.
    Key,
    /// `soundm.click` / `Sounds/click.pda` — `endintro` zip logo.
    Click,
    /// `soundm.zzt` / `Sounds/tap-zipper2.pda` — each crank ghost step.
    Zzt,
    /// `soundm.warning` / `Sounds/warning.pda` — ghost threat pose.
    Warning,
    /// `soundm.parry` / `Sounds/parry.pda` — twinstep frontal parry (`enemy:kill`).
    Parry,
    /// `soundm.shuriken` / `Sounds/shuriken.pda` — ninja `enemy:stab` throw.
    Shuriken,
    /// `soundm.spiritdispel` / `Sounds/ghost3.pda` — Spirit kill (`enemy:kill` kSpirit).
    SpiritDispel,
    /// `soundm.spiritrevive` / `Sounds/ghost1.pda` — Spirit revive (`enemy:revive`).
    SpiritRevive,
    /// `soundm.transition` / `Sounds/transition.pda` — room-load out wipe.
    Transition,
    /// `soundm.transition2` / `Sounds/transition2.pda` — room-load in wipe.
    Transition2,
}

impl SfxId {
    /// Pick one of the four `falldead*` samples (`soundm.falldead` /
    /// `math.random(1, #falldeadsounds)`). `roll` is any entropy source
    /// (prefer `Game::next_fx_roll`, not deathlist index — single kills
    /// must not always get the softest sample).
    fn falldead_variant(roll: u32) -> Self {
        match roll % 4 {
            0 => Self::Falldead,
            1 => Self::Falldead2,
            2 => Self::Falldead3,
            _ => Self::Falldead4,
        }
    }
}

/// Pose sequence for `isosprite` anims (1-based frame within facing row).
/// Lua `addAnim(..., 10, false)` → ~10 fps at 20 Hz display (advance every 2 ticks).
/// Lua `addAnim(..., 20, false)` (espray) → 1 tick per pose.
#[derive(Debug, Clone)]
struct SpriteAnim {
    /// 1-based poses within the facing row (`player.pdt` / `enemy.pdt` have 16/facing).
    poses: &'static [u32],
    /// Index into `poses`.
    index: usize,
    /// Sub-tick accumulator; advance pose when `>= ticks_per_pose`.
    sub: i32,
    /// 20 Hz ticks per pose (2 ≈ 10 fps, 1 ≈ 20 fps).
    ticks_per_pose: i32,
}

/// Ticks of 20 Hz display per anim pose at Lua rate 10.
const ANIM_TICKS_PER_POSE: i32 = 2;

/// `endintro` zip oneshot poses (`main.lua` `zip:addAnim(..., 5, false)`).
const INTRO_ZIP_POSES: &[u32] = &[
    1, 2, 3, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 3, 4,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 3, 2, 1,
];

impl SpriteAnim {
    fn new(poses: &'static [u32]) -> Self {
        Self::with_rate(poses, ANIM_TICKS_PER_POSE)
    }

    fn with_rate(poses: &'static [u32], ticks_per_pose: i32) -> Self {
        Self {
            poses,
            index: 0,
            sub: 0,
            ticks_per_pose: ticks_per_pose.max(1),
        }
    }

    fn pose_1based(&self) -> u32 {
        if self.poses.is_empty() {
            return 1;
        }
        // Clamp so a finished oneshot still shows its last pose (death hold).
        self.poses[self.index.min(self.poses.len() - 1)]
    }

    /// Advance one 20 Hz tick. Returns `true` if the anim finished.
    fn tick(&mut self) -> bool {
        if self.poses.is_empty() || self.index >= self.poses.len() {
            return true;
        }
        self.sub += 1;
        if self.sub < self.ticks_per_pose {
            return false;
        }
        self.sub = 0;
        self.index += 1;
        self.index >= self.poses.len()
    }
}

/// Dying enemy still drawn until `slashed*` finishes (`enemy:kill` → playAnim).
#[derive(Debug, Clone)]
struct KillFx {
    x: i32,
    y: i32,
    facing: Facing,
    anim: SpriteAnim,
    kind: crate::level::EnemyKind,
}

/// Living enemy playing `stab` while killing the player (`enemy:stab`).
#[derive(Debug, Clone)]
struct EnemyAttackFx {
    x: i32,
    y: i32,
    facing: Facing,
    anim: SpriteAnim,
    kind: crate::level::EnemyKind,
}

/// Flying `Images/shuriken` oneshot (`enemy:stab` ninja + `oneshotsprite:addMotion`).
/// Kill runs in `motionFinished` when the sprite tile already equals the player.
#[derive(Debug, Clone)]
struct ShurikenFx {
    tile_x: i32,
    tile_y: i32,
    target_x: i32,
    target_y: i32,
    /// Lua `addMotion` 3rd arg — tiles per display update.
    steps_per_frame: i32,
    anim: SpriteAnim,
    kill_dir: Facing,
    kill_type: KillType,
    killer_etype: i32,
}

/// Ninja hop / twinstep body parry oneshot (`enemy:kill` → `playAnim("dodge"|"parry")`).
#[derive(Debug, Clone)]
struct EnemyDodgeFx {
    x: i32,
    y: i32,
    facing: Facing,
    anim: SpriteAnim,
    kind: crate::level::EnemyKind,
}

/// Twinstep spark overlay (`enemy.parry` oneshotsprite / `Images/parry`, NXOR).
#[derive(Debug, Clone)]
struct ParrySparkFx {
    x: i32,
    y: i32,
    /// Lua: N/W → z behind body; S/E → z in front (draw order vs body).
    draw_behind: bool,
    anim: SpriteAnim,
}

/// Spirit `revive` oneshot (`enemy:revive` → playAnim("revive") → idle).
#[derive(Debug, Clone)]
struct SpiritReviveFx {
    x: i32,
    y: i32,
    anim: SpriteAnim,
}

/// Spirit-room `door` class (`door.lua` / `Images/door`) — not castle leftdoor/rightdoor.
#[derive(Debug, Clone)]
struct SpiritDoor {
    x: i32,
    y: i32,
    /// Lua `self.left`: 0 = kLeftDoor frames 1–4; 1 = kRightDoor frames 5–8.
    left: u8,
    /// `door.closing` — true while shut / shutting.
    closing: bool,
    /// `door.tileAnim` — 0..=4 index into open/close frame list.
    tile_anim: i32,
    /// `door.timer` — 20 Hz counter for animspeed.
    timer: i32,
}

impl SpiritDoor {
    fn new(x: i32, y: i32, left: u8) -> Self {
        let mut d = Self {
            x,
            y,
            // Constructor `left` selects the art half (`door.lua` frames from arg).
            // Lua also flips `self.left` for roomtiles; walkability only needs >0 / ≤0.
            left: if left != 0 { 1 } else { 0 },
            closing: false,
            tile_anim: 0,
            timer: 0,
        };
        // `door:init` → `close()` — start shut.
        d.close();
        // Snap to the finished closed pose. Lua leaves `tileAnim = 0`, so the first
        // `door:update` ticks play framesclose[1]=open-looking → closed (a spawn slam).
        // That reads as "doors already open" on entry; resting shut matches the
        // gameplay rule (closed until every Spirit is dead).
        d.tile_anim = 4;
        d.timer = 0;
        d
    }

    /// `door:close` — idempotent; `roomtiles = 3 + left`.
    fn close(&mut self) -> Option<i8> {
        if self.closing {
            return None;
        }
        self.closing = true;
        self.tile_anim = 0;
        self.timer = 0;
        Some(3 + self.left as i8)
    }

    /// `door:open` — idempotent; `roomtiles = -3 - left`.
    fn open(&mut self) -> Option<i8> {
        if !self.closing {
            return None;
        }
        self.closing = false;
        self.tile_anim = 0;
        self.timer = 0;
        Some(-3 - self.left as i8)
    }

    /// 0-based cell in `door.pdt` for the current anim frame (or resting pose).
    /// Left half cells 0..=3 (`doorTable[1..=4]`); right half 4..=7 (`[5..=8]`).
    /// Open sequence: 1→2→3→4 within half; close: 4→3→2→1.
    /// Resting closed = cell 0 / 4 (`doorTable[1]` / `[5]`); open = cell 3 / 7 (`[4]` / `[8]`).
    fn blit_cell(&self) -> usize {
        let base = if self.left == 0 { 0 } else { 4 };
        let within = if self.tile_anim <= 0 {
            // Before the first anim step: show the pose we're leaving.
            // Closing starts from open (4); opening starts from closed (1).
            if self.closing {
                4
            } else {
                1
            }
        } else if self.closing {
            // framesclose[tileAnim] → 5 - tileAnim for tileAnim in 1..=4
            (5 - self.tile_anim.clamp(1, 4)) as usize
        } else {
            self.tile_anim.clamp(1, 4) as usize
        };
        base + within - 1
    }
}

/// Animated blood burst (`enemy:bleed` / `samurai:bleed` → espray / isospray).
#[derive(Debug, Clone)]
struct BloodSprayFx {
    x: i32,
    y: i32,
    /// Facing used for the 4-row spray table.
    facing: Facing,
    /// Which `espray1`…`espray5` table (0..=4). Ignored when `use_isospray`.
    table_i: u8,
    /// Pierce deaths use `Images/isospray` instead of espray.
    use_isospray: bool,
    anim: SpriteAnim,
}

/// Persistent floor blood puddle / directional splat (`hereblood`, `floorspray`).
#[derive(Debug, Clone)]
struct FloorBlood {
    x: i32,
    y: i32,
    /// `None` → `hereblood.pdi` on the death tile; `Some(i)` → `floorspray` cell.
    floorspray_idx: Option<usize>,
    /// Remaining 20 Hz ticks before visible (`isotile:delay`); 0 = show now.
    delay: i32,
}

/// Player blood footprint left on the tile they leave (`samurai:trailBlood`).
/// Lua stamps into the iso strip bitmap; we keep stamps until room change.
#[derive(Debug, Clone, Copy)]
struct TrailStamp {
    x: i32,
    y: i32,
    /// 0-based index into `Images/trail` (Lua `bloodTable[1..=8]`).
    cell: u8,
}

/// Landscape anim overlay (`main.lua` `mapanimtiles` / `isotile:setIndex`).
/// Pond / campfire / waterfall GIDs cycle frames on top of the strip cell.
#[derive(Debug, Clone)]
struct MapAnimTile {
    x: i32,
    y: i32,
    /// 1-based map GIDs in cycle order (`isotile.lua` `self.frames`).
    frames: &'static [u16],
    /// Index into `frames`.
    frame_i: usize,
    /// `isotile.timer` — advances when `timer % animspeed == 0`.
    timer: i32,
    /// `isotile.animspeed` (20 Hz ticks per frame step).
    animspeed: i32,
}

/// Player-death drip spray cell (`samurai:sploosh` → dripsC/N/S/E/W).
#[derive(Debug, Clone)]
struct DripFx {
    x: i32,
    y: i32,
    /// 0=C, 1=N, 2=S, 3=E, 4=W.
    which: u8,
    anim: SpriteAnim,
}

/// Lua `Globals` kill types passed to `samurai:kill` / `enemy:stab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillType {
    Starve = 0,
    Slash = 1,
    Pierce = 2,
}

/// Player attack / walk anims (`samurai.lua` addAnim).
/// Walk `step` / `zstep` use rate 5 → 4 ticks/pose at 20 Hz; others use rate 10 → 2.
const PLAYER_STEP: &[u32] = &[2, 1];
const PLAYER_ZSTEP: &[u32] = &[1, 1];
const PLAYER_FASTSTEP: &[u32] = &[16, 2, 2, 1];
const PLAYER_SLASH: &[u32] = &[4, 5, 5, 5, 5, 5, 5, 5, 8, 2, 1];
/// Wind-up while still on the start tile (`samurai` `preslash`, rate 10).
const PLAYER_PRESLASH: &[u32] = &[2, 3, 8, 9, 9, 9];
const PLAYER_STAB: &[u32] = &[16, 6, 7, 7, 7, 8, 13, 2, 1];
const PLAYER_SLASH_AND_STAB: &[u32] = &[4, 5, 5, 5, 6, 7, 7, 7, 8, 13, 2, 1];
/// Player death pose (`samurai:kill` → `slashed`).
const PLAYER_SLASHED: &[u32] = &[8, 8, 8, 8, 8, 8, 9, 10, 11];
/// `main.lua`: after `player.alive == false`, wait `ticks > 30` at 20 Hz before game over.

/// `Globals.maxBlood` — starting / max `player.blood`.
const MAX_BLOOD: i32 = 250;
/// Readybar sprite top-left (`movebar:moveTo(1,1)`, center 0,0).
const MOVEBAR_X: i32 = 1;
const MOVEBAR_Y: i32 = 1;
/// Lifebar top-right (`lifebar:moveTo(screenWidth-1, 1)`, center 1,0) → left = 400-70.
const LIFEBAR_X: i32 = SCREEN_WIDTH as i32 - 70;
const LIFEBAR_Y: i32 = 1;
/// Lua `addAnim(..., 5, false)` → 5 fps at 20 Hz display.
const ANIM_TICKS_PER_POSE_5FPS: i32 = 4;

/// Door exit armed by `playerMoved` on a door tile; fired after move/kill anims
/// finish (`main.lua` calls `exitroom` only once the enemy phase ends).
#[derive(Debug, Clone)]
struct PendingExit {
    id: i32,
    camera: (i32, i32),
    /// `exitTeleport`: land on `exits[teleport_id].x/y` instead of one step past the door.
    teleport: bool,
    /// `entrance == exitToGameOver` — `exitroom` calls `winGame` instead of loading.
    to_game_over: bool,
}

/// Swordsman `slashed` / `slashed2` / `slashed3` (`enemy.lua`).
const ENEMY_SLASHED: &[u32] = &[8, 8, 8, 8, 8, 8, 9, 10, 11];
const ENEMY_SLASHED2: &[u32] = &[8, 8, 8, 8, 8, 8, 8, 8, 9, 10, 11];
const ENEMY_SLASHED3: &[u32] = &[8, 8, 8, 8, 8, 8, 9, 10, 10, 10, 11];
/// King `slashed` / `slashed2` / `slashed3` — identical; `king.pdt` only has poses 1–4.
const KING_SLASHED: &[u32] = &[2, 2, 2, 2, 3, 3, 3, 3, 3, 4];
/// Spirit `slashed` / `slashed2` / `slashed3` (`enemy.lua` kSpirit; `demon.pdt` 16/facing).
const SPIRIT_SLASHED: &[u32] = &[1, 1, 9, 10, 11, 12];
const SPIRIT_SLASHED2: &[u32] = &[1, 1, 1, 1, 9, 9, 10, 10, 11, 11, 12];
const SPIRIT_SLASHED3: &[u32] = &[1, 1, 1, 9, 9, 9, 10, 11, 12];
/// Spirit `revive` (`enemy.lua` kSpirit `addAnim("revive", …, 10, false)`).
const SPIRIT_REVIVE: &[u32] = &[12, 12, 12, 11, 11, 10, 10, 9, 9, 1];
/// Spirit idle `{2, 1}` at 1 fps looping (`enemy.lua` kSpirit `addAnim("idle", …, 1, true)`).
/// Each pose lasts 1.0s of wall time (`isosprite` animCounter = 1/fps).
/// Spirit-room `door:update` — advance frame when `timer % animspeed == 0`.
const SPIRIT_DOOR_ANIM_SPEED: i32 = 2;
/// Swordsman `stab` when killing the player (`enemy.lua` kSwordsman).
const ENEMY_STAB: &[u32] = &[16, 6, 7, 7, 7, 8, 12, 2, 1];
/// Ninja `stab` (`enemy.lua` kNinja) — shorter wind-up, held pose 5.
const NINJA_STAB: &[u32] = &[2, 3, 3, 4, 5, 5, 5, 5, 5, 5, 4];
/// `shuriken:addAnim("main", {1,2}×10, 10, false)` — spin while in flight.
const SHURIKEN_SPIN: &[u32] = &[
    1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2,
];
/// King has no `stab` anim — reuse step poses within the 4-cell row.
const KING_STAB: &[u32] = &[2, 1];
/// Ninja `dodge` (`enemy.lua` `{2, 6}` @ 10 fps). Last cell is the kneel hold —
/// Lua `isosprite` keeps that frame after the oneshot ends until `braintwo`
/// clears `dodgestate` and `playAnim("idle")`.
const NINJA_DODGE: &[u32] = &[2, 6];
const NINJA_DODGE_HOLD: u32 = 6;
/// Twinstep `parry` (`enemy.lua` `{4}`). Held until the dodge-phase `braintwo`
/// clears `dodgestate` + `playAnim("idle")` (same frame-hold as ninja dodge).
const TWINSTEP_PARRY: &[u32] = &[4];
const TWINSTEP_PARRY_HOLD: u32 = 4;
/// `enemy.lua` twinstep `parry:addAnim("main", {1,2,3,4}, 10, false)`.
const PARRY_SPARK: &[u32] = &[1, 2, 3, 4];

/// `enemy.lua` espray `main` pose sequences (1-based cells within facing row).
const ESPRAY_MAIN_A: &[u32] = &[
    1, 2, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 7,
];
const ESPRAY_MAIN_B: &[u32] = &[
    1, 2, 1, 2, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 7,
];
const ESPRAY_MAIN_C: &[u32] = &[
    1, 2, 1, 2, 1, 2, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 7,
];
const ESPRAY_MAIN_D: &[u32] = &[
    1, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 7,
];
/// `samurai:bleed` slash branch — long espray at 20 fps.
const PLAYER_BLEED_ESPRAY: &[u32] = &[
    1, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 7,
];
/// `samurai:bleed` pierce branch — `isospray` at 10 fps.
const PLAYER_BLEED_ISOSPRAY: &[u32] = &[1, 2, 3, 4, 5, 5, 6, 6, 6];
/// `samurai:sploosh` drip oneshot (cells 1..=5).
const PLAYER_DRIPS: &[u32] = &[1, 1, 1, 1, 2, 3, 4, 5];

fn enemy_slashed_variant(i: usize) -> &'static [u32] {
    match i % 3 {
        0 => ENEMY_SLASHED,
        1 => ENEMY_SLASHED2,
        _ => ENEMY_SLASHED3,
    }
}

/// King death poses (`enemy.lua` kKing — all three slashed* sequences match).
fn king_slashed_variant(_i: usize) -> &'static [u32] {
    KING_SLASHED
}

/// Spirit dispel poses (`enemy.lua` kSpirit slashed / slashed2 / slashed3).
fn spirit_slashed_variant(i: usize) -> &'static [u32] {
    match i % 3 {
        0 => SPIRIT_SLASHED,
        1 => SPIRIT_SLASHED2,
        _ => SPIRIT_SLASHED3,
    }
}

/// Spirit idle pose from wall time (`idle` `{2,1}` @ 1 fps looping).
fn spirit_idle_pose(time: f32) -> u32 {
    let phase = (time.max(0.0).floor() as u64) % 2;
    if phase == 0 {
        2
    } else {
        1
    }
}

/// Last frame of a successful dodge/parry oneshot, held while `dodgestate`
/// (`isosprite` leaves `currentFrame` on the finished anim until idle).
fn living_dodge_hold_pose(kind: crate::level::EnemyKind) -> u32 {
    match kind {
        crate::level::EnemyKind::Ninja => NINJA_DODGE_HOLD,
        crate::level::EnemyKind::Twinstep => TWINSTEP_PARRY_HOLD,
        _ => 1,
    }
}

/// Persistent corpse pose after `slashed*` ends (`enemy.lua` `addAnim("dead", …)`).
/// `isosprite` keeps the last frame when a non-looping anim finishes; we mirror that
/// with the `dead` table's resting cell (or the final `slashed` cell when identical).
fn enemy_corpse_pose(kind: crate::level::EnemyKind) -> u32 {
    use crate::level::EnemyKind;
    match kind {
        EnemyKind::Ninja => 9,
        EnemyKind::King => 4,
        // Swordsman `dead` = {10, 11} → rests on 11; others use single-cell `dead`.
        EnemyKind::Swordsman | EnemyKind::Reverse => 11,
        EnemyKind::Twinstep | EnemyKind::Pikeman => 10,
        // Spirit `slashed*` ends on 12; no separate `dead` table.
        EnemyKind::Spirit => 12,
        // Entrances are never killed; idle pose if drawn as "corpse".
        EnemyKind::LeftSouthEntrance
        | EnemyKind::RightSouthEntrance
        | EnemyKind::LeftEastEntrance
        | EnemyKind::RightEastEntrance => 1,
    }
}

fn espray_anim_variant(i: usize) -> &'static [u32] {
    match i % 4 {
        0 => ESPRAY_MAIN_A,
        1 => ESPRAY_MAIN_B,
        2 => ESPRAY_MAIN_C,
        _ => ESPRAY_MAIN_D,
    }
}

fn facing_opposite(f: Facing) -> Facing {
    match f {
        Facing::North => Facing::South,
        Facing::South => Facing::North,
        Facing::East => Facing::West,
        Facing::West => Facing::East,
    }
}

/// Kill-direction → spray facing remap from `enemy:bleed`.
fn bleed_spray_facing(dir: Facing) -> Facing {
    match dir {
        Facing::North => Facing::South,
        Facing::East => Facing::East,
        Facing::West => Facing::North,
        Facing::South => Facing::West,
    }
}

/// `samurai:bleed` slash-branch facing remap (espray1/5).
fn player_bleed_slash_facing(dir: Facing) -> Facing {
    match dir {
        Facing::North => Facing::North,
        Facing::East => Facing::South,
        Facing::West => Facing::East,
        Facing::South => Facing::West,
    }
}

/// `samurai:bleed` pierce-branch facing remap (isospray).
fn player_bleed_pierce_facing(dir: Facing) -> Facing {
    match dir {
        Facing::North => Facing::North,
        Facing::East => Facing::West,
        Facing::West => Facing::South,
        Facing::South => Facing::East,
    }
}

/// Kill direction from killer → player (`enemy:braintwo` diffx/diffy).
fn kill_dir_from_to(from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Facing {
    let diffx = from_x - to_x;
    let diffy = from_y - to_y;
    if diffx == 0 {
        if diffy < 0 {
            Facing::South
        } else {
            Facing::North
        }
    } else if diffy == 0 {
        if diffx < 0 {
            Facing::East
        } else {
            Facing::West
        }
    } else if diffx.abs() >= diffy.abs() {
        if diffx < 0 {
            Facing::East
        } else {
            Facing::West
        }
    } else if diffy < 0 {
        Facing::South
    } else {
        Facing::North
    }
}

/// Adjacent floor-blood offsets for `enemy:bleed` (ox, oy) per splat index 0..3.
fn bleed_floor_offsets(dir: Facing) -> [(i32, i32); 4] {
    match dir {
        Facing::North => [(0, 2), (1, 1), (0, 1), (-1, 1)],
        Facing::South => [(0, -2), (-1, -1), (0, -1), (1, -1)],
        Facing::East => [(-2, 0), (-1, 1), (-1, 0), (-1, -1)],
        Facing::West => [(2, 0), (1, -1), (1, 0), (1, 1)],
    }
}

/// `isotile:delay` frames for floor bloods[i] (1-based Lua i=1..4 → delays 3,2,0,1).
fn bleed_floor_delay(i: usize) -> i32 {
    match i {
        0 => 3,
        1 => 2,
        3 => 1,
        _ => 0,
    }
}

/// `floorspray` 0-based cell: Lua `floorBloodTable[(dir-1)*4 + i]` with i=1..4.
fn floorspray_cell(dir: Facing, i: usize) -> usize {
    ((dir as u8 as usize) - 1) * 4 + i
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub a: bool,
    pub b: bool,
    pub menu: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Input {
    pub buttons: Buttons,
    pub crank_degrees: f32,
    pub crank_delta: f32,
    pub crank_docked: bool,
}

#[derive(Default)]
pub struct DemoAssets {
    pub player_face: Option<Bitmap>,
    pub ninja_face: Option<Bitmap>,
    pub key: Option<Bitmap>,
    /// `Images/chest` — outdoor key chest (`chester.chestSprite`, 32×64 isotile).
    pub chest: Option<Bitmap>,
    pub card: Option<Bitmap>,
    /// `Images/passicon` — walk trail (`selector.walkicons`, NXOR).
    pub passicon: Option<Bitmap>,
    /// `Images/moveicon` — selector tip (`selector` self image, NXOR).
    pub moveicon: Option<Bitmap>,
    /// `Images/centerdot` — blinking tip accent (NXOR).
    pub centerdot: Option<Bitmap>,
    /// `Images/killicon` — slash markers on the path (NXOR).
    pub killicon: Option<Bitmap>,
    /// `Images/stabicon` — tip when last readybar segment is stab (NXOR).
    pub stabicon: Option<Bitmap>,
    /// `Images/exiticon` — tip when selector stands on an exit object (`onExit`).
    /// Cells are 1-based in Lua (`exiticon[kNorth…kWest]`); Rust table is 0-based.
    pub exiticon: Option<ImageTable>,
    pub tiles: Option<ImageTable>,
    pub ninja: Option<ImageTable>,
    pub player: Option<ImageTable>,
    pub enemy: Option<ImageTable>,
    /// `Images/pikeman` — pikeman body (`enemy.lua` kPikeman).
    pub pikeman: Option<ImageTable>,
    /// `Images/piketip` — pike tip child sprite (drawn at body screen pos).
    pub piketip: Option<ImageTable>,
    /// `Images/twinstep` — double swordsman (`enemy.lua` kTwinstep).
    pub twinstep: Option<ImageTable>,
    /// `Images/parry` — twinstep frontal parry spark oneshot (NXOR).
    pub parry: Option<ImageTable>,
    /// `Images/shuriken` — ninja throw oneshot (2 cells, NXOR).
    pub shuriken: Option<ImageTable>,
    /// `Images/king` — stationary king (`enemy.lua` kKing; 4 poses / facing).
    pub king: Option<ImageTable>,
    /// `Images/demon` — Spirit body (`Globals.spiritTable`; 16 poses / facing).
    pub spirit: Option<ImageTable>,
    /// `Images/door` — spirit-room door class (`door.lua` / `doorTable`; 8 cells).
    pub spirit_door: Option<ImageTable>,
    /// `Images/leftdoor` — left castle / outdoor gate half (`kLeft*Entrance`).
    pub leftdoor: Option<ImageTable>,
    /// `Images/rightdoor` — right castle / outdoor gate half (`kRight*Entrance`).
    pub rightdoor: Option<ImageTable>,
    /// `Images/smoke` — floor zip trail for N/W (`smoker.smoketable`).
    pub smoke: Option<ImageTable>,
    /// `Images/smoke2` — floor zip trail for S/E (`smoker.smoketable2`).
    pub smoke2: Option<ImageTable>,
    /// `Images/espray1`…`espray5` — enemy blood burst (`enemy:bleed`).
    pub espray: [Option<ImageTable>; 5],
    /// `Images/floorspray` — directional floor blood splatters (16 cells).
    pub floorspray: Option<ImageTable>,
    /// `Images/hereblood` — puddle on the death tile.
    pub hereblood: Option<Bitmap>,
    /// `Images/trail` — player blood footprints (`samurai:trailBlood` / `bloodTable`).
    pub trail: Option<ImageTable>,
    /// `Images/dripsC/N/S/E/W` — player death sploosh (`samurai:sploosh`).
    pub drips: [Option<ImageTable>; 5],
    /// `Images/isospray` — pierce kill spray (`samurai:bleed` kPierce).
    pub isospray: Option<ImageTable>,
    /// Top-left readybar chrome (`Images/movebar`).
    pub movebar: Option<Bitmap>,
    /// Empty readybar label (`Images/readyword` — "READY").
    pub readyword: Option<Bitmap>,
    /// Aimed readybar label (`Images/pressx` — "PRESS A").
    pub pressx: Option<Bitmap>,
    /// Cap after the last segment pip (`Images/barmatte`).
    pub barmatte: Option<Bitmap>,
    /// Walk segment pip (`Images/readysegwalk`).
    pub readysegwalk: Option<Bitmap>,
    /// First walk pip variant (`Images/readysegwalk_0`).
    pub readysegwalk_0: Option<Bitmap>,
    /// Second walk pip variant (`Images/readysegwalk_1`).
    pub readysegwalk_1: Option<Bitmap>,
    /// Slash / doubleslash / stab pip (`Images/readysegkill`).
    pub readysegkill: Option<Bitmap>,
    /// Top-right life chrome (`Images/lifebar`).
    pub lifebar: Option<Bitmap>,
    /// Hourglass frames beside LIFE (`Images/hourglass`).
    pub hourglass: Option<ImageTable>,
    /// `Fonts/headerwhite` — lifebar `LIFE:###` / dialog text (`whitefont` in Lua).
    pub headerwhite: Option<Font>,
    /// `Fonts/monoblack` — highscore `hsfont` (`last score` line).
    pub monoblack: Option<Font>,
    /// `Images/endbg` — meditative end art (`winbackground` overlay).
    pub endbg: Option<Bitmap>,
    /// `Images/highscore` — score panel open frames (`endHSTable`, 4 cells).
    pub highscore: Option<ImageTable>,
    /// `Images/wipe` — credits reveal mask (`wipeTable`, 14 cells).
    pub wipe: Option<ImageTable>,
    /// `Images/dialogbg` — 7-frame dialog chrome.
    pub dialogbg: Option<ImageTable>,
    /// `dialogfaces[1..=9]` — player, sword, pike, ninja, twinstep, king, spirit, sword, monk.
    pub dialog_faces: [Option<Bitmap>; 9],
    /// `Images/faceblood` — inverted overlay when `bloodStun`.
    pub faceblood: Option<Bitmap>,
    /// `Images/continue` — revive/quit bar (assets staged; UI later).
    pub continue_bar: Option<ImageTable>,
    /// `Images/restart` — "PRESS ANY BUTTON TO RESTART" (`restartSprite`).
    pub restart: Option<Bitmap>,
    /// `Images/bennett` — intro logo sprite (`main.lua` Intro ticks==15).
    pub bennett: Option<Bitmap>,
    /// `Images/zip` — endintro oneshot logo (`zipTable`).
    pub zip: Option<ImageTable>,
    /// `Images/enemy_ghost` — swordsman ghost body (`swordGhostTable`).
    pub enemy_ghost: Option<ImageTable>,
    /// `Images/ninja_ghost`.
    pub ninja_ghost: Option<ImageTable>,
    /// `Images/pikeman_ghost`.
    pub pikeman_ghost: Option<ImageTable>,
    /// `Images/twinstep_ghost` — twinstep crank preview (`twinstepGhostTable`).
    pub twinstep_ghost: Option<ImageTable>,
    /// Readybar ghost pip frames (`Images/readysegghost1/2`).
    pub readysegghost1: Option<Bitmap>,
    pub readysegghost2: Option<Bitmap>,
    /// `Images/crankhint` — readybar crank tutorial sprite.
    pub crankhint: Option<ImageTable>,
}

/// Max concurrent smoke puffs (`smoker.numSmokes`).
const NUM_SMOKES: usize = 20;

/// Inactive frame sentinel (`smoker.frames[i] = -999`).
const SMOKE_INACTIVE: i32 = -999;

/// Long zip anim indices (`smoker.anim`, 1-based image table cells).
const SMOKE_ANIM: &[usize] = &[1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 9];
/// Short zip anim (`smoker.smallanim`) when `numsteps <= 3`.
const SMOKE_SMALL_ANIM: &[usize] = &[2, 4, 5, 6, 7, 8, 9];

/// One floor-smoke particle (`smoker` parallel arrays).
#[derive(Debug, Clone, Copy)]
struct SmokePuff {
    /// Animation clock; `SMOKE_INACTIVE` when free.
    frame: i32,
    /// Zip facing that spawned this puff (controls table + flip).
    dir: Facing,
    /// `0` = long `anim`, `1` = `smallanim` (`smoker.speeds`).
    speed: u8,
    tile_x: i32,
    tile_y: i32,
}

impl SmokePuff {
    fn inactive() -> Self {
        Self {
            frame: SMOKE_INACTIVE,
            dir: Facing::North,
            speed: 0,
            tile_x: 0,
            tile_y: 0,
        }
    }

    fn active(&self) -> bool {
        self.frame > SMOKE_INACTIVE
    }

    fn anim(&self) -> &'static [usize] {
        if self.speed == 0 {
            SMOKE_ANIM
        } else {
            SMOKE_SMALL_ANIM
        }
    }
}

/// Floor zip smoke pool (`smoker.lua`).
#[derive(Debug, Clone)]
struct FloorSmoke {
    puffs: [SmokePuff; NUM_SMOKES],
    /// Accumulates real time into 20 Hz anim ticks (`setRefreshRate(20)`).
    tick_accum: f32,
}

impl Default for FloorSmoke {
    fn default() -> Self {
        Self {
            puffs: [SmokePuff::inactive(); NUM_SMOKES],
            tick_accum: 0.0,
        }
    }
}

impl FloorSmoke {
    fn clear(&mut self) {
        for p in &mut self.puffs {
            *p = SmokePuff::inactive();
        }
        self.tick_accum = 0.0;
    }

    /// `smoker:do_smoke(direction, numsteps)` — place one puff per path step behind
    /// the player (opposite the zip facing). Only called when `numsteps > 1`.
    fn do_smoke(&mut self, direction: Facing, numsteps: i32, player_x: i32, player_y: i32) {
        if numsteps <= 1 {
            return;
        }
        // Behind the landing tile: opposite of movement facing.
        let (dx, dy) = match direction {
            Facing::North => (0, 1),
            Facing::South => (0, -1),
            Facing::East => (-1, 0),
            Facing::West => (1, 0),
        };
        let speed = if numsteps > 3 { 0u8 } else { 1u8 };
        for step in 1..=numsteps {
            let posx = player_x + dx * step;
            let posy = player_y + dy * step;
            // Lua: math.floor((step - numsteps - 2 + math.random(2)) / 2)
            // random(2) ∈ {1,2}; use 1 for deterministic trails.
            let stagger = floor_div(step - numsteps - 2 + 1, 2);
            if let Some(slot) = self.puffs.iter_mut().find(|p| !p.active()) {
                *slot = SmokePuff {
                    frame: stagger,
                    dir: direction,
                    speed,
                    tile_x: posx,
                    tile_y: posy,
                };
            }
        }
    }

    /// Advance one 20 Hz tick (`smoker:update`).
    fn tick(&mut self) {
        for p in &mut self.puffs {
            if !p.active() {
                continue;
            }
            p.frame += 1;
            if p.frame > p.anim().len() as i32 {
                *p = SmokePuff::inactive();
            }
        }
    }
}

/// Readybar segment kinds from `selector:detectkills` / `movebar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathSegment {
    Walk,
    Slash,
    DoubleSlash,
    Stab,
}

impl PathSegment {
    fn is_slash(self) -> bool {
        matches!(self, Self::Slash | Self::DoubleSlash)
    }
}

/// Aim plan rebuilt every selector move (`detectkills` → deathlist + readybar).
#[derive(Debug, Clone, Default)]
struct AimPlan {
    /// One entry per zip step (walk/slash/…); optional trailing `Stab`.
    segments: Vec<PathSegment>,
    /// Indices into `room.enemies` to kill on commit (may contain duplicates).
    deathlist: Vec<usize>,
}

/// Per-local crank ghost (`enemy.ghost` + `enemy.moveList` in Lua).
/// Kept off [`Enemy`] so combatants stay `Copy`.
#[derive(Debug, Clone)]
struct EnemyGhost {
    x: i32,
    y: i32,
    facing: Facing,
    visible: bool,
    /// 1 = idle, 2 = threat (`enemy.ghost:setFrame`).
    frame: u8,
    /// Graph node ids stepped onto (`enemy.moveList`).
    move_list: Vec<u32>,
}

impl Default for EnemyGhost {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            facing: Facing::South,
            visible: false,
            frame: 1,
            move_list: Vec::new(),
        }
    }
}

/// Selector state (`selector.lua`: tile_x/y, extent_x/y, blockedDir, canmove).
#[derive(Debug, Clone, Copy)]
struct Selector {
    tile_x: i32,
    tile_y: i32,
    extent_x: i32,
    extent_y: i32,
    /// When set, further steps in this facing are blocked (standing on an exit).
    blocked: Option<Facing>,
    /// `selector.canmove` — false when the tip sits on a corpse (tip stab disabled).
    canmove: bool,
}

impl Selector {
    fn at_player(px: i32, py: i32) -> Self {
        Self {
            tile_x: px,
            tile_y: py,
            extent_x: 0,
            extent_y: 0,
            blocked: None,
            canmove: true,
        }
    }

    fn can_commit(&self, px: i32, py: i32) -> bool {
        self.tile_x != px || self.tile_y != py
    }

    fn step_dir(&self) -> (i32, i32) {
        let sx = self.extent_x.signum();
        let sy = self.extent_y.signum();
        (sx, sy)
    }
}

/// `main.lua` local `buttonsdown` — per-direction hold counters.
/// `-1` = released; `0+` = frames held at the 20 Hz display tick.
#[derive(Debug, Clone, Copy)]
struct DpadHeld {
    up: i32,
    down: i32,
    left: i32,
    right: i32,
}

impl Default for DpadHeld {
    fn default() -> Self {
        Self {
            up: -1,
            down: -1,
            left: -1,
            right: -1,
        }
    }
}

pub struct Game {
    pub state: GameState,
    pub fb: Framebuffer,
    pub time: f32,
    pub ticks: u64,
    pub last_input: Input,
    pub assets: DemoAssets,
    pub world: WorldMap,
    pub room: Room,
    pub camera: (i32, i32),
    pub player_x: i32,
    pub player_y: i32,
    pub facing: Facing,
    /// `player.blood` (`Globals.maxBlood` start).
    pub life: i32,
    /// `lifebar.displayBlood` — eases toward `life` one point per update tick.
    display_blood: i32,
    /// `lifebar.hourglassFrame` (1..=7 in Lua table).
    hourglass_frame: i32,
    /// `globalenemies` — persist across rooms (`utils.lua` findlocalenemies).
    global_enemies: Vec<Enemy>,
    /// `maingraph` — Playdate-shaped pathfinder (`pathfinding.lua`).
    main_graph: crate::pathfinding::PathGraph,
    /// Camera-centered room↔world tables for graph indices.
    room_tables: crate::pathfinding::RoomTables,
    /// `random_seed` from save / boot (`main.lua`).
    random_seed: u32,
    /// Live `math.random` stand-in for the process. Boot: `SpawnRng(random_seed)`.
    /// Room enter reseeds to `random_seed + cam_x + cam_y` inside `find_local_enemies`.
    /// `chester:reset` draws from this stream without reseeding (Lua-faithful).
    run_rng: SpawnRng,
    /// `currentDifficulty` 1..=6 (`Globals.enemySets` index).
    difficulty: usize,
    /// Cumulative kills this run (`score` driving difficulty).
    score_kills: i32,
    /// Dev cheat panel unlocked (`#god`): tile HUD + God tools menu.
    /// Survives `restart_at_start` so a death mid-run cannot clear it while the hash stays set.
    god_mode: bool,
    /// God-tools Invulnerability switch: no blood spend, no player death.
    /// Independent of `cheat_grant_key`; defaults on when `#god` enables.
    invulnerable: bool,
    /// God-tools Key switch: grant / restore `chester.hasKey` on enable and on restart.
    cheat_grant_key: bool,
    boot_phase: f32,
    prev_buttons: Buttons,
    /// Held-frame counters (`main.lua` `buttonsdown`), advanced at 20 Hz.
    dpad_held: DpadHeld,
    /// Accumulates real `dt` into 20 Hz input ticks.
    input_tick_accum: f32,
    /// `dt` from the current `update` call (for 20 Hz accumulation).
    frame_dt: f32,
    selector: Selector,
    /// `selector:detectkills` result for the current aim (segments + deathlist).
    aim_plan: AimPlan,
    /// Floor zip dust trail (`floorsmoke` / `smoker.lua`).
    floorsmoke: FloorSmoke,
    /// Player attack / death pose sequence (`samurai:slash` / `stab` / `slashed`).
    player_anim: Option<SpriteAnim>,
    /// Dying enemies still drawn until `slashed*` finishes (`enemy:kill`).
    kill_fx: Vec<KillFx>,
    /// Killer playing `stab` while the player dies (`enemy:stab`).
    enemy_attack_fx: Vec<EnemyAttackFx>,
    /// In-flight ninja shuriken (`Images/shuriken` + `addMotion`).
    shuriken_fx: Vec<ShurikenFx>,
    /// Ninja dodge / twinstep body-parry oneshots (`enemy:kill`).
    enemy_dodge_fx: Vec<EnemyDodgeFx>,
    /// Twinstep spark overlay (`Images/parry`, NXOR).
    parry_spark_fx: Vec<ParrySparkFx>,
    /// Spirit revive oneshots (`enemy:revive`).
    spirit_revive_fx: Vec<SpiritReviveFx>,
    /// Spirit-room `doors` list (`main.lua` / `door.lua`).
    spirit_doors: Vec<SpiritDoor>,
    /// `killedPeople` — any real kill this zip (`main.lua`); fed to `updateSpiritsAndDoors`.
    killed_people: bool,
    /// `killedlastframe` — one-frame grace after a kill zip (`updateSpiritsAndDoors`).
    killed_last_frame: bool,
    /// Per-cell `roomtiles` overlays from spirit doors (3/4 closed, −3/−4 open)
    /// and corpse occupancy (`1` after kill / `construct_graphs`).
    roomtile_overrides: std::collections::HashMap<(i32, i32), i8>,
    /// Culled `roomtiles` membership for enemy graphs (`cullroomtiles`).
    /// Floors past room doors are absent — that is the AI room fence.
    path_roomtiles: std::collections::HashSet<(i32, i32)>,
    /// Base culled `roomtiles` for the current room, captured once when the
    /// room is populated (`initbackground`). Lua computes `cullroomtiles` only
    /// at room load, so the fence must not follow the player's later moves —
    /// otherwise standing on an exit tile re-opens the far side of a door and
    /// enemies path into the next room.
    room_fence: std::collections::HashSet<(i32, i32)>,
    /// `deadenemies` — `1` hop-over corpse, `2` Spirit body hard-block (`enemy:kill`).
    deadenemies: std::collections::HashMap<(i32, i32), u8>,
    /// Animated espray / isospray bursts (`enemy:bleed` / `samurai:bleed`).
    blood_sprays: Vec<BloodSprayFx>,
    /// Persistent floor blood puddles / splatters (`hereblood`, `floorspray`).
    floor_bloods: Vec<FloorBlood>,
    /// Player blood footprints on tiles left behind (`samurai:trailBlood`).
    trail_stamps: Vec<TrailStamp>,
    /// Last `bloodTable` cell used (1..=8 Lua); avoid immediate repeats.
    current_blood: u8,
    /// Player-death drip sprays (`samurai:sploosh`).
    drip_fx: Vec<DripFx>,
    /// Player is dead: hold `slashed` on the death tile until this 20 Hz countdown hits 0, then restart.
    /// Mirrors `main.lua` `player.alive == false and ticks > 30`.
    death_hold_ticks: Option<i32>,
    /// Door landed on this zip; `exitroom` waits until move/kill anims finish.
    pending_exit: Option<PendingExit>,
    /// Slash `kPlayerMove` in flight (`preslash` then kill+land on ticks > 3).
    pending_slash: Option<PendingSlashMove>,
    /// `kEnemyMove` in flight: step budget then `braintwo` across later ticks.
    pending_enemy: Option<PendingEnemyMove>,
    /// Deterministic-ish variety counter for espray table / anim pick (`math.random` stand-in).
    fx_rng: u32,
    /// `player.hasBlindedEnemies` — first blood-blind shows a tip once per run.
    has_blinded_enemies: bool,
    /// `reverseRoom` — castle reverse room: enemies hop while the tip moves
    /// (`selector:moveByTile` → `step_reverseMen`); post-commit chase is skipped.
    reverse_room: bool,
    /// Transient debug flashes (god mode, etc.). Flavor lines use [`Self::dialog`].
    message: Option<(String, f32)>,
    /// Bottom dialog bar (`dialogbar`).
    dialog: DialogBar,
    /// Exit/NPC object ids already visited (`ex.visited` / `npc.visited`).
    visited_exits: Vec<i32>,
    /// Lua `game_in_progress` — set on room exit / restore; cleared on death.
    /// Gates `gameWillTerminate` snapshot writes.
    game_in_progress: bool,
    /// In-memory `"save"` blob (`utils.lua` `saved_game`).
    saved_game: SavedGame,
    /// Host should drain [`Self::take_save_flush`] into `localStorage`.
    save_dirty: bool,
    /// Local Catalog stand-in (`zipper.hs.v1`) — persists across wins / Delete save.
    highscores: HighScoreBoard,
    /// Host should drain [`Self::take_hs_flush`] into `localStorage`.
    hs_dirty: bool,
    /// `chester.hasKey`.
    has_key: bool,
    /// Active chest tile (`chester.chestSprite` after `reset`).
    chest_pos: Option<(i32, i32)>,
    /// Set by [`Self::roll_chest`]; host drains via [`Self::take_chest_spawn`]
    /// (god-mode console log on each `chester:reset`, including death restart).
    chest_spawn_pending: bool,
    /// `lifebar.warningshown` — half-blood `hurry` dialog once per run.
    hurry_shown: bool,
    /// State to restore when a dialog session ends (`kGamePlayingState` / over).
    dialog_return: GameState,
    /// Pending exit dialog key+face to show in next `initbackground`.
    pending_room_dialog: Option<(String, i32)>,
    /// Player is dead; after dialog closes, Lua either returns to Playing (blood > 0)
    /// then promotes to GameOver when `ticks > 30`, or goes straight to GameOver (blood ≤ 0).
    pending_death_restart: bool,
    /// `restartSprite.y` while in `kGameOverState` (eases from 240 toward 209).
    restart_sprite_y: f32,
    /// Lua global `ticks` while dead — reset when the death dialog opens; **not**
    /// reset on GameOver entry (`main.lua`). Drives deathmusic (==30), Playing→Over
    /// (`> 30`), invert flash (`< 5`), and eitherbutton (`> 5`).
    dead_ticks: i32,
    /// `playdate.display.setInverted` — Title/Intro white field; GameOver flash.
    screen_inverted: bool,
    /// SFX requested this update (`selector:moveByTile` → `select`, etc.).
    /// Host drains via [`Game::take_sfx`].
    sfx_queue: Vec<SfxId>,
    /// Softsynth events this update (`introchord` / `Sho.mid` NoteOn/Off).
    /// Host drains via [`Game::take_synth`].
    synth_queue: Vec<SynthEvent>,
    /// Lua `introchord` MIDI notes — loaded at runtime (BYOA / demo JSON), not baked in.
    introchord: Option<Vec<u8>>,
    /// Ending-credit card prose — extracted at runtime from the user's
    /// `main.luac` (BYOA), never baked into wasm. Paired with [`CREDIT_TICKS`].
    credit_cards: Vec<String>,
    /// Lua `intronote` — 1-based index into `introchord` during white-walk.
    intro_note: usize,
    /// Parsed `Sounds/Sho.mid` (BYOA / optional demo). Not baked into wasm.
    sho_midi: Option<crate::MidiSequence>,
    /// `shomusic:play()` active — advance on `update` dt.
    sho_playing: bool,
    sho_elapsed: f64,
    sho_cursor: usize,
    /// Emit `AllNotesOff` once on the first tick after `play_victory_music`.
    sho_cut_pending: bool,
    /// Port-only easter egg: victory sho pitch bend in semitones (crank while
    /// the win cinema is on screen). Sticky across frames; host reads via
    /// [`Self::sho_pitch_bend`]. Not in 1.10 Lua — see
    /// `docs/victory-crank-pitch-easter-egg.md`.
    sho_pitch_bend: f32,
    /// Lua `roomsvisited` — camera cells already entered (`main.lua` initbackground).
    rooms_visited: Vec<(i32, i32)>,
    /// Lua `introSeen` — after first Title/Intro (or splash click skip), death
    /// restart goes straight to Aiming.
    intro_seen: bool,
    /// Lua `ticks` while in Title / Intro (reset on each intro step / state entry).
    intro_ticks: i32,
    /// Accumulators so Title/Intro advance at 20 Hz independent of Playing ticks.
    intro_tick_accum: f32,
    /// `bennett` sprite visible (`main.lua` Intro ticks==15 until endintro).
    bennett_visible: bool,
    /// `zip` oneshot after `endintro` (`Images/zip` at 320,190).
    zip_anim: Option<SpriteAnim>,
    /// Hide readybar / lifebar during Title/Intro (`setVisible(false)`).
    hud_visible: bool,
    /// `ghostStep` — how many crank preview hops are active.
    ghost_step: i32,
    /// Accumulated `getCrankChange` degrees while aiming (`main.lua` `crank`).
    crank_accum: f32,
    /// `crankusecounter` — crankhint hides after 40 steps.
    crank_use_counter: i32,
    /// `player.outline` — NXOR pose-12 blink while ghosts are previewing.
    player_outline: bool,
    /// Outline blink sub-tick (`samurai:update` `_t` 0..=7).
    outline_blink: i32,
    /// `Globals.canBuzz` — `buzz()` plays once until a D-pad ButtonUp re-arms.
    can_buzz: bool,
    /// Readybar pip blink frame (`movebar.frame` 0/1).
    readybar_blink: i32,
    /// Readybar frame countdown (`movebar.frameCounter`, resets every 4 ticks).
    readybar_frame_counter: i32,
    /// Crankhint slide-out animation index (`movebar.crankposindex` 1..=7).
    crankhint_pos_index: i32,
    /// Crankhint imagetable cell cycle index into `CRANKHINT_FRAMES`.
    crankhint_frame: i32,
    /// Whether crankhint is sliding out (`movebar.crankout`).
    crankhint_out: bool,
    /// Parallel ghost state for each local (`enemy.ghost` / `moveList`).
    enemy_ghosts: Vec<EnemyGhost>,
    /// Active landscape anims in the current viewport (`mapanimtiles`).
    map_anims: Vec<MapAnimTile>,
    /// Active room-change wipe (`doTransition` path); `None` when not transitioning.
    transition_phase: Option<TransitionPhase>,
    /// Dialog key+face to open after the in-wipe finishes.
    transition_pending_dialog: Option<(String, i32)>,
    /// 20 Hz accumulator while in [`GameState::Transition`] (separate from smoke).
    transition_tick_accum: f32,
    /// Remaining 20 Hz ticks to hold [`TransitionPhase::Loading`].
    transition_hold_ticks: i32,
    /// State to restore when the wipe finishes (`Aiming` for doors, `Win` for cinema pans).
    transition_resume: GameState,
    /// Win-cinema camera to install on Out→Loading (`initbackground` after camera set).
    pending_win_camera: Option<(i32, i32)>,
    /// `gameoverScreen` to apply when a win wipe resumes.
    pending_win_gameover_screen: Option<u8>,
    /// Arm credits scroller after the third pan wipe (Lua creates it post-`initbackground`).
    pending_win_credits: bool,
    /// Win / score ending runtime (`kGameWinState` / `kGameScoreState`).
    ending: EndingRuntime,
    /// 20 Hz accumulator for Win / Score ticks.
    ending_tick_accum: f32,
}

/// Fixed boot seed for unit tests / examples (`Game::new`).
/// Play builds should pass `playdate.getSecondsSinceEpoch()` (or a saved seed).
pub const DEFAULT_RANDOM_SEED: u32 = 0x5EED_2026;

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

impl Game {
    /// Deterministic boot for tests — same as `new_with_seed(DEFAULT_RANDOM_SEED)`.
    ///
    /// Starts with an **empty** map; call [`Self::load_worldmap`] (or the test
    /// helper that reads `data/worldmap.bin`) before play.
    pub fn new() -> Self {
        Self::new_with_seed(DEFAULT_RANDOM_SEED)
    }

    /// Boot with an explicit run seed (`main.lua` `random_seed`).
    ///
    /// Lua picks `playdate.getSecondsSinceEpoch()` (seconds since 2000-01-01 UTC)
    /// on a fresh boot, or `saved_game.seed` when `loadsave == 1`. The seed is
    /// kept for the whole process: death → `startGame` does **not** re-roll it.
    /// Per-room variety comes from `findlocalenemies` reseeding with
    /// `random_seed + cameratile_x + cameratile_y`.
    ///
    /// World map is empty until [`Self::load_worldmap`].
    pub fn new_with_seed(random_seed: u32) -> Self {
        let world = WorldMap::empty();
        let room = load_start_room(&world);
        let (px, py) = START_PLAY;
        let mut g = Self {
            state: GameState::Boot,
            fb: Framebuffer::default(),
            time: 0.0,
            ticks: 0,
            last_input: Input::default(),
            assets: DemoAssets::default(),
            world,
            room,
            camera: START_CAMERA,
            player_x: px,
            player_y: py,
            facing: Facing::North,
            // Spawn at START_PLAY is a teleport (`step(0)` → maxBlood-1).
            life: MAX_BLOOD - 1,
            display_blood: MAX_BLOOD - 1,
            hourglass_frame: 1,
            global_enemies: Vec::new(),
            main_graph: crate::pathfinding::PathGraph::new(),
            room_tables: crate::pathfinding::RoomTables::for_camera(START_CAMERA),
            random_seed,
            run_rng: SpawnRng::new(random_seed),
            difficulty: 1,
            score_kills: 0,
            god_mode: false,
            invulnerable: false,
            cheat_grant_key: false,
            boot_phase: 0.0,
            prev_buttons: Buttons::default(),
            dpad_held: DpadHeld::default(),
            input_tick_accum: 0.0,
            frame_dt: 0.0,
            selector: Selector::at_player(px, py),
            aim_plan: AimPlan::default(),
            floorsmoke: FloorSmoke::default(),
            player_anim: None,
            kill_fx: Vec::new(),
            enemy_attack_fx: Vec::new(),
            shuriken_fx: Vec::new(),
            enemy_dodge_fx: Vec::new(),
            parry_spark_fx: Vec::new(),
            spirit_revive_fx: Vec::new(),
            spirit_doors: Vec::new(),
            killed_people: false,
            killed_last_frame: false,
            roomtile_overrides: std::collections::HashMap::new(),
            path_roomtiles: std::collections::HashSet::new(),
            room_fence: std::collections::HashSet::new(),
            deadenemies: std::collections::HashMap::new(),
            blood_sprays: Vec::new(),
            floor_bloods: Vec::new(),
            trail_stamps: Vec::new(),
            current_blood: 1,
            drip_fx: Vec::new(),
            death_hold_ticks: None,
            pending_exit: None,
            pending_slash: None,
            pending_enemy: None,
            fx_rng: 1,
            has_blinded_enemies: false,
            reverse_room: false,
            message: None,
            dialog: DialogBar::new(),
            visited_exits: Vec::new(),
            game_in_progress: false,
            saved_game: SavedGame::default(),
            save_dirty: false,
            highscores: HighScoreBoard::new(),
            hs_dirty: false,
            has_key: false,
            chest_pos: None,
            chest_spawn_pending: false,
            hurry_shown: false,
            dialog_return: GameState::Aiming,
            pending_room_dialog: None,
            pending_death_restart: false,
            restart_sprite_y: 240.0,
            dead_ticks: 0,
            screen_inverted: false,
            sfx_queue: Vec::new(),
            synth_queue: Vec::new(),
            introchord: None,
            credit_cards: Vec::new(),
            intro_note: 0,
            sho_midi: None,
            sho_playing: false,
            sho_elapsed: 0.0,
            sho_cursor: 0,
            sho_cut_pending: false,
            sho_pitch_bend: 0.0,
            rooms_visited: Vec::new(),
            intro_seen: false,
            intro_ticks: 0,
            intro_tick_accum: 0.0,
            bennett_visible: false,
            zip_anim: None,
            hud_visible: true,
            ghost_step: 0,
            crank_accum: 0.0,
            crank_use_counter: 0,
            player_outline: false,
            outline_blink: 0,
            can_buzz: true,
            readybar_blink: 0,
            readybar_frame_counter: 4,
            crankhint_pos_index: 1,
            crankhint_frame: 0,
            crankhint_out: false,
            enemy_ghosts: Vec::new(),
            map_anims: Vec::new(),
            transition_phase: None,
            transition_pending_dialog: None,
            transition_tick_accum: 0.0,
            transition_hold_ticks: 0,
            transition_resume: GameState::Aiming,
            pending_win_camera: None,
            pending_win_gameover_screen: None,
            pending_win_credits: false,
            ending: EndingRuntime::default(),
            ending_tick_accum: 0.0,
        };
        g.rebuild_map_anims();
        g.draw();
        g
    }

    /// Lua `movebar.crankframes` — 1-based imagetable cells cycled while hint shows.
    const CRANKHINT_FRAMES: &'static [usize] =
        &[1, 2, 3, 4, 5, 6, 7, 8, 1, 1, 8, 7, 6, 5, 4, 3, 2, 1];
    /// Lua `movebar.crankpositions` — x while sliding out (index 1-based).
    const CRANKHINT_POSITIONS: &'static [i32] = &[144, 152, 160, 166, 163, 164, 165];

    /// `isotile:setIndex` frame lists (1-based GIDs) + `animspeed`.
    fn map_anim_spec(gid: u16) -> Option<(&'static [u16], i32)> {
        match gid {
            153 => Some((&[153, 154, 155, 156], 4)),
            245 => Some((&[245, 246, 247, 248], 8)),
            310 => Some((&[310, 311, 312, 313], 8)),
            314 => Some((&[314, 315, 316, 317], 8)),
            249 => Some((&[249, 250, 251, 252], 2)),
            166 => Some((&[166, 167], 8)),
            168 => Some((&[168, 169], 8)),
            170 => Some((&[170, 171, 172, 173, 172, 171], 8)),
            357 => Some((&[357, 358], 12)),
            _ => None,
        }
    }

    /// Parse `worldmap.bin` (`ZMAP`) and install start room / chest / locals.
    /// Keeps `random_seed`, god-tool flags, and already-loaded `assets`.
    pub fn load_worldmap(&mut self, data: &[u8]) -> Result<(), crate::worldmap::WorldMapError> {
        let world = WorldMap::from_bytes(data)?;
        self.apply_world(world);
        Ok(())
    }

    /// True after a successful [`Self::load_worldmap`].
    pub fn worldmap_loaded(&self) -> bool {
        self.world.is_loaded()
    }

    /// Rebuild map-derived run state from `world` (start pad, room, chest).
    fn apply_world(&mut self, world: WorldMap) {
        let (px, py) = START_PLAY;
        let room = load_start_room(&world);
        // Cold / first `loadWorldmap`: reseed like Lua boot `math.randomseed(random_seed)`
        // before `chester:reset`, then room enter reseeds again in `find_local_enemies`.
        self.run_rng = SpawnRng::new(self.random_seed);
        self.world = world;
        self.room = room;
        self.global_enemies.clear();
        self.camera = START_CAMERA;
        self.player_x = px;
        self.player_y = py;
        self.facing = Facing::North;
        self.selector = Selector::at_player(px, py);
        self.aim_plan = AimPlan::default();
        self.floorsmoke.clear();
        self.player_anim = None;
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        self.clear_spirit_doors();
        self.killed_people = false;
        self.killed_last_frame = false;
        self.blood_sprays.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.drip_fx.clear();
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.visited_exits.clear();
        self.rooms_visited.clear();
        // Fresh world load (app init): the Lua `shomusic` global does not exist
        // yet, so reset it — but release any residual voices first.
        if self.sho_playing {
            self.sho_cut_pending = true;
        }
        self.sho_playing = false;
        self.sho_elapsed = 0.0;
        self.sho_cursor = 0;
        self.sho_pitch_bend = 0.0;
        self.pending_room_dialog = None;
        self.chest_pos = None;
        self.zip_anim = None;
        self.message = None;
        self.reset_ghosts();
        self.crank_accum = 0.0;
        self.sync_enemy_ghosts();
        // `chester:reset` before `initbackground` / findlocalenemies (`main.lua` startGame).
        self.roll_chest();
        if self.cheat_grant_key {
            self.has_key = true;
        }
        if self.world.is_loaded() {
            self.populate_room_enemies(Facing::South); // came from south of outdoor start
        }
        self.rebuild_map_anims();
        self.draw();
    }

    /// `initbackground` mapanimtiles pass — spawn anims for known GIDs in the room.
    fn rebuild_map_anims(&mut self) {
        self.map_anims.clear();
        if !self.world.is_loaded() {
            return;
        }
        for ly in 0..self.room.height {
            for lx in 0..self.room.width {
                let gx = self.room.origin_x + lx;
                let gy = self.room.origin_y + ly;
                let gid = self.world.tile(gx, gy);
                let Some((frames, animspeed)) = Self::map_anim_spec(gid) else {
                    continue;
                };
                self.map_anims.push(MapAnimTile {
                    x: gx,
                    y: gy,
                    frames,
                    frame_i: 0,
                    timer: 0,
                    animspeed,
                });
            }
        }
    }

    /// `isotile:update` — advance landscape frame cycles at 20 Hz.
    fn tick_map_anims(&mut self) {
        for anim in &mut self.map_anims {
            if anim.frames.is_empty() {
                continue;
            }
            anim.timer += 1;
            if anim.timer % anim.animspeed == 0 {
                anim.timer = 0;
                anim.frame_i += 1;
                if anim.frame_i >= anim.frames.len() {
                    anim.frame_i = 0;
                }
            }
        }
    }

    /// Current 1-based GID to blit for a map-anim cell (base tile if inactive).
    fn map_anim_draw_gid(&self, gx: i32, gy: i32, base_gid: u16) -> u16 {
        self.map_anims
            .iter()
            .find(|a| a.x == gx && a.y == gy)
            .and_then(|a| a.frames.get(a.frame_i).copied())
            .unwrap_or(base_gid)
    }

    /// Keep `enemy_ghosts` parallel to `room.enemies` (bodies as default pose).
    fn sync_enemy_ghosts(&mut self) {
        let n = self.room.enemies.len();
        self.enemy_ghosts.resize_with(n, EnemyGhost::default);
        for i in 0..n {
            let en = self.room.enemies[i];
            let g = &mut self.enemy_ghosts[i];
            if g.move_list.is_empty() && !g.visible {
                g.x = en.x;
                g.y = en.y;
                g.facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
                g.frame = 1;
            }
        }
    }

    fn ghost_xy_snapshot(&self) -> Vec<(i32, i32)> {
        self.enemy_ghosts.iter().map(|g| (g.x, g.y)).collect()
    }



    /// Run seed (`main.lua` `random_seed`) — stable across death restart.
    pub fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// Unlock God tools (`#god` URL fragment). Defaults: invuln on, key on.
    pub fn set_god_mode(&mut self, on: bool) {
        self.god_mode = on;
        if on {
            self.invulnerable = true;
            self.cheat_grant_key = true;
            self.has_key = true;
        } else {
            self.invulnerable = false;
            self.cheat_grant_key = false;
        }
    }

    pub fn god_mode(&self) -> bool {
        self.god_mode
    }

    /// God-tools Invulnerability switch.
    pub fn set_invulnerable(&mut self, on: bool) {
        if !self.god_mode {
            return;
        }
        self.invulnerable = on;
    }

    pub fn invulnerable(&self) -> bool {
        self.invulnerable
    }

    /// God-tools Key switch — grants or clears `chester.hasKey`.
    pub fn set_cheat_key(&mut self, on: bool) {
        if !self.god_mode {
            return;
        }
        self.cheat_grant_key = on;
        self.has_key = on;
    }

    pub fn cheat_key(&self) -> bool {
        self.cheat_grant_key
    }

    /// `chester.hasKey` — castle key (chest pickup or god Key switch).
    pub fn has_key(&self) -> bool {
        self.has_key
    }

    /// Current player tile (`tile_x`,`tile_y`) — God tools teleport prefills.
    pub fn player_tile(&self) -> (i32, i32) {
        (self.player_x, self.player_y)
    }

    /// Dev teleport to outdoor grid `(x, y)`. Only while `god_mode`.
    ///
    /// Picks the nearest exit whose room camera still includes the tile; falls
    /// back to the start-pad camera offset `(+1, −2)`. Clears in-room FX and
    /// rebuilds locals like a room enter — does **not** run `playerMoved` NPC/chest.
    pub fn god_teleport(&mut self, x: i32, y: i32) -> bool {
        if !self.god_mode {
            return false;
        }
        let w = self.world.width as i32;
        let h = self.world.height as i32;
        if w <= 0 || h <= 0 || x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }

        let camera = self.camera_for_god_teleport(x, y);
        self.player_x = x;
        self.player_y = y;
        self.camera = camera;
        self.selector = Selector::at_player(x, y);
        self.aim_plan = AimPlan::default();
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.floorsmoke.clear();
        self.player_anim = None;
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        self.clear_spirit_doors();
        self.killed_people = false;
        self.killed_last_frame = false;
        self.blood_sprays.clear();
        self.drip_fx.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.zip_anim = None;
        self.dpad_held = DpadHeld::default();
        self.message = None;
        self.refresh_viewport();
        self.mark_room_entrance();
        self.draw();
        true
    }

    /// Nearest exit camera whose `roomtiles_for` contains `(x,y)`, else start offset.
    fn camera_for_god_teleport(&self, x: i32, y: i32) -> (i32, i32) {
        let mut best: Option<(i32, (i32, i32))> = None;
        for ex in &self.world.exits {
            // Door / exit objects only (kind 0); NPCs don't define room cameras.
            if ex.kind != 0 {
                continue;
            }
            let dist = (ex.x - x).abs() + (ex.y - y).abs();
            for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
                let cam = self.world.exit_camera_for(ex, facing);
                if !self.world.roomtiles_for(cam, (x, y)).contains(&(x, y)) {
                    continue;
                }
                match best {
                    Some((best_dist, _)) if best_dist <= dist => {}
                    _ => best = Some((dist, cam)),
                }
            }
        }
        best.map(|(_, cam)| cam).unwrap_or((x + 1, y - 2))
    }

    /// Active chest tile for this run (`chester.chestSprite` after `reset`), if any.
    pub fn chest_pos(&self) -> Option<(i32, i32)> {
        self.chest_pos
    }

    /// Drain a pending `chester:reset` notification (one-shot per roll).
    /// Returns the new chest tile when a roll happened since the last take.
    pub fn take_chest_spawn(&mut self) -> Option<(i32, i32)> {
        if !self.chest_spawn_pending {
            return None;
        }
        self.chest_spawn_pending = false;
        self.chest_pos
    }

    /// Test helper: force the rolled chest onto a known tile.
    pub fn set_chest_pos_for_test(&mut self, pos: (i32, i32)) {
        self.chest_pos = Some(pos);
    }

    /// Test helper: advance `run_rng` as if a room enter reseeding had been
    /// followed by further `math.random` draws (Lua live stream after play).
    pub fn advance_run_rng_for_test(&mut self) {
        // Mimic `findlocalenemies` reseed at a non-start camera, then burn draws.
        self.run_rng = SpawnRng::new(
            self.random_seed
                .wrapping_add(108)
                .wrapping_add(162),
        );
        for _ in 0..8 {
            let _ = self.run_rng.gen_1_to(10);
            let _ = self.run_rng.gen_unit();
        }
    }

    /// Test helper: death → `startGame` path (`restart_at_start`).
    pub fn force_restart_at_start_for_test(&mut self) {
        self.restart_at_start();
    }

    /// Dismiss the host splash card with a click/key: skip white intro, land in
    /// the first room immediately. Returns `true` if boot was skipped.
    pub fn skip_boot(&mut self) -> bool {
        if self.state != GameState::Boot {
            return false;
        }
        // Cold boot path: same chest as `math.randomseed(random_seed)` + reset.
        self.restart_at_start_with_rng(true);
        self.intro_seen = true;
        self.hud_visible = true;
        self.screen_inverted = false;
        self.state = GameState::Aiming;
        self.prev_buttons = Buttons {
            left: true,
            right: true,
            up: true,
            down: true,
            a: true,
            b: true,
            menu: true,
        };
        self.draw();
        true
    }

    /// True after the white intro (or splash click) has finished once this run.
    pub fn intro_seen(&self) -> bool {
        self.intro_seen
    }

    /// True while the `endintro` zip oneshot is still on screen (tests).
    pub fn zip_anim_active(&self) -> bool {
        self.zip_anim.is_some()
    }

    /// Host splash idle timeout → Lua Title (`main.lua` `startGame` first-run path).
    ///
    /// Spawns at `START_TITLE` (109,188), invert on, HUD hidden, strips effectively
    /// invisible (we skip `draw_room` while Title/Intro).
    fn begin_title_intro(&mut self) {
        // First-run Title: cold RNG like boot `math.randomseed(random_seed)`.
        self.restart_at_start_with_rng(true);
        self.player_x = START_TITLE.0;
        self.player_y = START_TITLE.1;
        self.facing = Facing::North;
        // Park cursor off-play like Lua `cursor:moveToTile(200,200)` during Title —
        // Title/Intro draw path omits the selector entirely.
        self.selector = Selector::at_player(200, 200);
        self.aim_plan = AimPlan::default();
        self.screen_inverted = true;
        self.hud_visible = false;
        self.bennett_visible = false;
        self.zip_anim = None;
        self.player_anim = None;
        self.intro_ticks = 0;
        self.intro_tick_accum = 0.0;
        // Lua sets `introSeen = true` when entering Title so death restart skips it.
        self.intro_seen = true;
        self.boot_phase = 0.0;
        self.state = GameState::Title;
    }

    /// One 20 Hz Title/Intro tick (`main.lua` update Title / Intro arms).
    ///
    /// `pressed_ab` is an A/B edge this display frame — Lua `buttondown` skip when
    /// Intro and `ticks > 1`. Title ignores A/B for skip (only advances on ticks).
    fn tick_intro_frame(&mut self, pressed_ab: bool) {
        match self.state {
            GameState::Title => {
                // Title never skips on A; only ticks > 10 → Intro.
                if self.intro_ticks > 10 {
                    self.state = GameState::Intro;
                    self.intro_ticks = 0;
                }
            }
            GameState::Intro => {
                // Mid-intro A/B skip (`main.lua` buttondown, ticks > 1).
                if pressed_ab && self.intro_ticks > 1 {
                    self.end_intro();
                    return;
                }
                if self.player_y > self.camera.1 + 2 {
                    // Walk north: 188→182, one step every 10 ticks.
                    if self.intro_ticks == 10 {
                        self.intro_ticks = 0;
                        self.player_y -= 1;
                        self.facing = Facing::North;
                        self.player_anim =
                            Some(SpriteAnim::with_rate(PLAYER_STEP, ANIM_TICKS_PER_POSE_5FPS));
                        // Lua: `intronote` + `instrument:playMIDINote` — not `soundm.step`.
                        self.intro_note = self.intro_note.saturating_add(1);
                        if let Some(chord) = self.introchord.as_ref() {
                            if self.intro_note >= 1 && self.intro_note <= chord.len() {
                                self.queue_synth(SynthEvent::NoteOn(chord[self.intro_note - 1]));
                            }
                        }
                        // `samurai:step(1)` — title teleport already spent 1 → 249;
                        // six intro walks → 243 at (109,182).
                        if !self.invulnerable {
                            self.life = (self.life - 1).max(0);
                            self.display_blood = self.life;
                        }
                    }
                } else {
                    // Landed at y <= 182: bennett @15, endintro @60.
                    if self.intro_ticks == 15 {
                        self.bennett_visible = true;
                    }
                    if self.intro_ticks == 60 {
                        self.end_intro();
                    }
                }
            }
            _ => {}
        }
    }

    /// `endintro` (`main.lua:286`) — invert off, HUD on, zip oneshot + click, Aiming.
    fn end_intro(&mut self) {
        // `instrument:allNotesOff` + synth stop before click (`main.lua:287–290`).
        self.queue_synth(SynthEvent::AllNotesOff);
        self.screen_inverted = false;
        self.hud_visible = true;
        self.bennett_visible = false;
        self.camera = START_CAMERA;
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.aim_plan = AimPlan::default();
        self.player_anim = None;
        self.zip_anim = Some(SpriteAnim::with_rate(
            INTRO_ZIP_POSES,
            ANIM_TICKS_PER_POSE_5FPS,
        ));
        self.play_sfx(SfxId::Click);
        self.intro_seen = true;
        self.intro_ticks = 0;
        self.intro_tick_accum = 0.0;
        self.state = GameState::Aiming;
    }

    /// Playdate system-menu **seppuku** (`main.lua` `addMenuItems`):
    /// `player:kill(player.facing, kPierce)` — death pose + pierce bleed, **no**
    /// `playerKilled` dialog. Stays in Playing while dead until `ticks > 30`,
    /// then GameOver / restart. No-op while already dead, not playing, or `#god`.
    pub fn seppuku(&mut self) {
        if self.invulnerable {
            self.message = Some((self.cheat_invuln_label().into(), 0.8));
            return;
        }
        if self.pending_death_restart || self.state == GameState::GameOver {
            return;
        }
        if !matches!(
            self.state,
            GameState::Aiming | GameState::Resolving | GameState::Dialog
        ) {
            return;
        }
        // Dump any open non-death dialog (system menu over a talking NPC, etc.).
        if self.state == GameState::Dialog {
            let _ = self.dialog.hide();
        }
        // `samurai:kill` → `game_in_progress = false` + `deletesave()`.
        self.game_in_progress = false;
        self.delete_save();
        let dir = self.facing;
        self.player_anim = Some(SpriteAnim::new(PLAYER_SLASHED));
        self.play_sfx(SfxId::PlayerDeath);
        self.spawn_player_bleed_spray(dir, KillType::Pierce);
        self.spawn_player_sploosh();
        self.play_sfx(SfxId::Blood);
        // `samurai:kill` with kPierce does not open a dialog (enemy:stab does).
        self.death_hold_ticks = None;
        self.pending_death_restart = true;
        self.dead_ticks = 0;
        self.screen_inverted = false;
        self.aim_plan = AimPlan::default();
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.dpad_held = DpadHeld::default();
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.message = None;
        // Stay in Playing-dead; existing tick path promotes to GameOver at >30.
        self.state = GameState::Aiming;
        self.draw();
    }

    /// Playdate system-menu **delete save** (`main.lua` `addMenuItems`).
    ///
    /// Wipes mid-run resume (`deletesave`), clears in-RAM dialog counters
    /// (`deleteProgress`), and restarts like `requestStartGame` after
    /// `introSeen` (outdoor start, Aiming). Does **not** re-roll `random_seed`
    /// — Lua only sets that at module load; a new seed needs app relaunch /
    /// page reload. Host must flush LS after this.
    pub fn delete_save_menu(&mut self) {
        self.game_in_progress = false;
        self.delete_save();
        self.dialog.delete_progress();
        // `startGame` with introSeen: spawn post-intro play tile, HUD on.
        self.restart_at_start();
        // `deleteProgress` already wiped seen/killed; `restart_at_start` calls
        // `reset_run_progress` again — fine (idempotent for flags).
        self.intro_seen = true;
        self.hud_visible = true;
        self.screen_inverted = false;
        self.state = GameState::Aiming;
        self.dialog_return = GameState::Aiming;
        self.prev_buttons = Buttons {
            left: true,
            right: true,
            up: true,
            down: true,
            a: true,
            b: true,
            menu: true,
        };
        self.draw();
    }

    pub fn set_demo_assets(&mut self, assets: DemoAssets) {
        self.assets = assets;
        self.draw();
    }

    /// Selector / cursor tile (world space) — for tests and HUD.
    pub fn selector_tile(&self) -> (i32, i32) {
        (self.selector.tile_x, self.selector.tile_y)
    }

    /// `selector.onExit` / `blockedDir` when the tip sits on an exit object.
    pub fn selector_exit_facing(&self) -> Option<Facing> {
        self.selector.blocked
    }

    /// Number of active floor-smoke puffs (tests / HUD).
    pub fn floorsmoke_active_count(&self) -> usize {
        self.floorsmoke.puffs.iter().filter(|p| p.active()).count()
    }

    /// Number of floor blood stamps (tests).
    pub fn floor_blood_count(&self) -> usize {
        self.floor_bloods.len()
    }

    /// Corpse count in the current room (`alive == false` locals) — for tests.
    pub fn corpse_count(&self) -> usize {
        self.room.enemies.iter().filter(|e| !e.alive).count()
    }

    /// In-flight `slashed*` death anims (tests).
    pub fn kill_fx_count(&self) -> usize {
        self.kill_fx.len()
    }

    /// Current player sprite pose (1-based within facing row); tests.
    pub fn player_pose_1based(&self) -> u32 {
        self.player_anim
            .as_ref()
            .map(|a| a.pose_1based())
            .unwrap_or(1)
    }

    /// True while a player oneshot anim is still held (including finished death pose).
    pub fn player_anim_active(&self) -> bool {
        self.player_anim.is_some()
    }

    /// Test helper: wipe floor blood as `initbackground` does to `mapsprites`.
    pub fn clear_room_blood_for_test(&mut self) {
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.blood_sprays.clear();
        self.drip_fx.clear();
        self.kill_fx.clear();
    }

    /// Number of `trailBlood` stamps in the current room (tests).
    pub fn trail_stamp_count(&self) -> usize {
        self.trail_stamps.len()
    }

    /// Test helper: re-run `findlocalenemies` for the current camera (room re-entry).
    pub fn reload_locals_for_test(&mut self, came_from: Facing) {
        self.populate_room_enemies(came_from);
    }

    /// Test helper: inject a global enemy and mirror it into the current room.
    pub fn inject_global_enemy_for_test(&mut self, mut en: Enemy) {
        en.global_index = Some(self.global_enemies.len());
        self.global_enemies.push(en);
        self.room.enemies.push(en);
    }

    /// Test helper: add a closed spirit-room door at `(x, y)` (`left` 0/1).
    pub fn inject_spirit_door_for_test(&mut self, x: i32, y: i32, left: u8) {
        let door = SpiritDoor::new(x, y, left);
        self.roomtile_overrides
            .insert((door.x, door.y), 3 + door.left as i8);
        self.spirit_doors.push(door);
    }

    /// Test helper: number of spirit-room doors in the current room.
    pub fn spirit_door_count_for_test(&self) -> usize {
        self.spirit_doors.len()
    }

    /// Test helper: true when every spirit door is closed / closing.
    pub fn spirit_doors_closed_for_test(&self) -> bool {
        !self.spirit_doors.is_empty() && self.spirit_doors.iter().all(|d| d.closing)
    }

    /// Test helper: true when every spirit door is open / opening.
    pub fn spirit_doors_open_for_test(&self) -> bool {
        !self.spirit_doors.is_empty() && self.spirit_doors.iter().all(|d| !d.closing)
    }

    /// Test helper: 0-based `door.pdt` cells currently blitted (spawn should be closed resting).
    pub fn spirit_door_blit_cells_for_test(&self) -> Vec<usize> {
        self.spirit_doors.iter().map(|d| d.blit_cell()).collect()
    }

    /// Test helper: `roomtile_overrides` flag for a cell.
    pub fn roomtile_override_for_test(&self, x: i32, y: i32) -> Option<i8> {
        self.roomtile_overrides.get(&(x, y)).copied()
    }

    /// Test helper: `deadenemies` value (`1` hop / `2` Spirit block).
    pub fn deadenemies_for_test(&self, x: i32, y: i32) -> Option<u8> {
        self.deadenemies.get(&(x, y)).copied()
    }

    /// Test helper: selector extent (`extent_x`, `extent_y`).
    pub fn selector_extent_for_test(&self) -> (i32, i32) {
        (self.selector.extent_x, self.selector.extent_y)
    }

    /// Test helper: `selector.canmove` (false on corpse land).
    pub fn selector_canmove_for_test(&self) -> bool {
        self.selector.canmove
    }

    /// Test helper: current aim plan includes a tip stab segment.
    pub fn aim_has_stab_for_test(&self) -> bool {
        self.aim_plan
            .segments
            .iter()
            .any(|s| matches!(s, PathSegment::Stab))
    }

    /// Test helper: mark a kill tile like `enemy:kill` (`deadenemies` + `roomtiles=1`).
    pub fn mark_dead_enemy_tile_for_test(&mut self, x: i32, y: i32, kind: crate::level::EnemyKind) {
        self.mark_dead_enemy_tile(x, y, kind);
    }

    /// Test helper: rebuild graphs / rewrite non-Spirit corpse hop flags
    /// (`construct_graphs`).
    pub fn construct_graphs_for_test(&mut self) {
        self.construct_graphs();
    }

    /// Test helper: keep only the given local indices as living combatants
    /// (room + matching `global_enemies` slots). Used to isolate A* without
    /// sibling entity blocks. Indices into the current `room.enemies` list.
    pub fn isolate_room_enemies_for_test(&mut self, keep: &[usize]) {
        let kept: Vec<Enemy> = keep
            .iter()
            .filter_map(|&i| self.room.enemies.get(i).copied())
            .collect();
        self.global_enemies.clear();
        self.room.enemies.clear();
        for mut e in kept {
            e.global_index = Some(self.global_enemies.len());
            self.global_enemies.push(e);
            self.room.enemies.push(e);
        }
    }

    /// Test helper: run `updateSpiritsAndDoors(didkill)` directly.
    pub fn update_spirits_and_doors_for_test(&mut self, didkill: bool) {
        self.update_spirits_and_doors(didkill);
    }

    /// Test helper: in-flight spirit revive oneshots.
    pub fn spirit_revive_fx_count_for_test(&self) -> usize {
        self.spirit_revive_fx.len()
    }

    /// Test helper: inject a global only (not mirrored into `room.enemies`).
    pub fn inject_global_only_for_test(&mut self, mut en: Enemy) {
        en.global_index = Some(self.global_enemies.len());
        self.global_enemies.push(en);
    }

    /// Test helper: outdoor-style terrain reload around `camera`.
    pub fn reload_room_terrain_for_test(&mut self, camera: (i32, i32), half_w: i32, half_h: i32) {
        self.reload_room_terrain(camera, half_w, half_h);
    }

    /// Test helper: snap selector back to the player (B / cancel aim).
    pub fn reset_selector_for_test(&mut self) {
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.aim_plan = AimPlan::default();
        self.reset_ghosts();
    }

    /// Test helper: feed crank degrees (`Input.crank_delta`) while aiming.
    pub fn crank_for_test(&mut self, delta: f32) {
        let mut input = Input::default();
        input.crank_delta = delta;
        self.update(INPUT_TICK_DT, input);
    }

    /// `ghostStep` (tests / HUD).
    pub fn ghost_step_count(&self) -> i32 {
        self.ghost_step
    }

    /// `player.outline` while crank ghosts are active (tests).
    pub fn player_outline_for_test(&self) -> bool {
        self.player_outline
    }

    /// Ghost tile for local enemy `i`, if visible.
    pub fn ghost_tile_for_test(&self, i: usize) -> Option<(i32, i32)> {
        self.enemy_ghosts.get(i).and_then(|g| {
            if g.visible {
                Some((g.x, g.y))
            } else {
                None
            }
        })
    }

    /// Ensure `enemy_ghosts` parallels locals (tests that inject enemies).
    pub fn sync_enemy_ghosts_for_test(&mut self) {
        self.sync_enemy_ghosts();
    }

    /// `#readybar.segmentList` for crank-cap tests.
    pub fn aim_plan_len_for_test(&self) -> usize {
        self.aim_plan.segments.len()
    }

    /// True when aim deathlist is empty (ghost preview eligible locals).
    pub fn aim_plan_deathlist_empty_for_test(&self) -> bool {
        self.aim_plan.deathlist.is_empty()
    }

    /// Test helper: warp onto a door exit and fire `exitroom` immediately.
    /// Drains the loading wipe so callers see the post-exit room synchronously.
    pub fn force_door_exit_for_test(&mut self, exit_id: i32) {
        self.arm_door_exit_for_test(exit_id);
        self.begin_room_transition();
        self.drain_room_transition_for_test();
    }

    /// Test helper: arm `pending_exit` for `exit_id` without running the wipe.
    pub fn arm_door_exit_for_test(&mut self, exit_id: i32) {
        let Some(ex) = self.world.exit_by_id(exit_id).cloned() else {
            return;
        };
        self.player_x = ex.x;
        self.player_y = ex.y;
        self.selector = Selector::at_player(self.player_x, self.player_y);
        // Same as `playerMoved` door land (`samurai.lua` reverseRoom assign).
        self.reverse_room = ex.reverse;
        if !ex.teleports.is_empty() {
            let tid = ex.teleports[0];
            if let Some(dest) = self.world.exit_by_id(tid) {
                let camera = self.world.exit_camera_for(dest, self.facing);
                self.pending_exit = Some(PendingExit {
                    id: tid,
                    camera,
                    teleport: true,
                    to_game_over: false,
                });
            }
        } else {
            let camera = self.world.exit_camera_for(&ex, self.facing);
            self.pending_exit = Some(PendingExit {
                id: ex.id,
                camera,
                teleport: false,
                to_game_over: ex.entrance == EXIT_TO_GAME_OVER
                    && matches!(self.facing, Facing::North | Facing::East),
            });
        }
    }

    /// Test helper: enter the loading wipe without draining it.
    pub fn begin_room_transition_for_test(&mut self) {
        self.begin_room_transition();
    }

    /// One 20 Hz transition tick (tests).
    pub fn tick_room_transition_for_test(&mut self) {
        if self.state == GameState::Transition {
            self.tick_room_transition();
            self.draw();
        }
    }

    /// Advance Out → Loading → In until Aiming/Dialog/Win (tests).
    pub fn drain_room_transition_for_test(&mut self) {
        // Out (1) + Loading hold (8) + In (1) + slack.
        for _ in 0..16 {
            if self.state != GameState::Transition {
                break;
            }
            self.tick_room_transition();
            self.draw();
        }
    }

    /// Living locals currently attached to the room (AI + draw list).
    pub fn living_local_count(&self) -> usize {
        self.room.enemies.iter().filter(|e| e.alive).count()
    }

    /// Test helper: run deferred `kEnemyMove` to completion (steps + `braintwo`).
    pub fn run_enemy_phase_for_test(&mut self, budget: i32) {
        self.pending_enemy = Some(PendingEnemyMove {
            steps_left: budget.max(0),
        });
        self.drain_enemy_phase_for_test();
    }

    /// True while deferred `kEnemyMove` is draining segments / awaiting braintwo.
    pub fn pending_enemy_active(&self) -> bool {
        self.pending_enemy.is_some()
    }

    /// Test helper: remaining readybar segments in deferred `kEnemyMove`.
    pub fn pending_enemy_steps_for_test(&self) -> Option<i32> {
        self.pending_enemy.as_ref().map(|p| p.steps_left)
    }

    /// Test helper: arm deferred `kEnemyMove` without draining it.
    pub fn arm_pending_enemy_for_test(&mut self, budget: i32) {
        self.pending_enemy = Some(PendingEnemyMove {
            steps_left: budget.max(0),
        });
    }

    /// Test helper: one `step_enemies` pass (no readybar pop, no Dialog gate).
    /// Sets `pending_enemy.steps_left` so twinstep short-bar uses this budget.
    pub fn step_enemies_once_for_test(&mut self, segs_left: i32) {
        self.pending_enemy = Some(PendingEnemyMove {
            steps_left: segs_left.max(0),
        });
        self.step_enemies_once();
    }

    /// Test helper: full A* node path for enemy `i` toward the player (or type goal).
    /// Returns world `(x, y, facing)` per node including start.
    pub fn enemy_astar_path_for_test(&self, i: usize) -> Option<Vec<(i32, i32, Facing)>> {
        use crate::level::EnemyKind;
        use crate::pathfinding::facing_for_id;

        let en = self.room.enemies.get(i)?;
        let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
        let start_id = self.room_tables.index_world_facing(en.x, en.y, facing)?;
        let goal_id = match en.kind {
            EnemyKind::Pikeman | EnemyKind::Ninja => {
                // Fall back to player for this probe helper.
                self.room_tables
                    .index_world_facing(self.player_x, self.player_y, self.facing)?
            }
            _ => self
                .room_tables
                .index_world_facing(self.player_x, self.player_y, self.facing)?,
        };
        let path = self.main_graph.find_path(start_id, goal_id)?;
        Some(
            path.into_iter()
                .filter_map(|id| {
                    let (x, y) = self.main_graph.xy(id)?;
                    Some((x, y, facing_for_id(id)))
                })
                .collect(),
        )
    }

    /// Advance until `kEnemyMove` finishes. Dismisses non-death dialogs that pause
    /// the phase (first-kill `*Killed`, NPC, etc.) the way a player would hit A.
    pub fn drain_enemy_phase_for_test(&mut self) {
        for _ in 0..80 {
            if self.pending_death_restart || self.death_hold_ticks.is_some() {
                return;
            }
            if self.pending_enemy.is_none() {
                return;
            }
            if self.state == GameState::Dialog {
                // Open anim / tick gate, then A to hide (non-death → back to Aiming).
                self.advance_sim_ticks(8);
                if self.state == GameState::Dialog {
                    let mut input = Input::default();
                    input.buttons.a = true;
                    self.update(INPUT_TICK_DT, input);
                    self.advance_sim_ticks(16);
                }
                continue;
            }
            self.advance_sim_ticks(1);
        }
    }

    /// Test helper: true if any global slot is a corpse at `(x, y)`.
    pub fn global_has_corpse_at(&self, x: i32, y: i32) -> bool {
        self.global_enemies
            .iter()
            .any(|e| !e.alive && e.x == x && e.y == y)
    }

    /// Active player-death drip sprays (`samurai:sploosh`) — for tests.
    pub fn drip_fx_count(&self) -> usize {
        self.drip_fx.len()
    }

    /// Killer stab anims in flight (`enemy:stab`) — for tests.
    pub fn enemy_attack_fx_count(&self) -> usize {
        self.enemy_attack_fx.len()
    }

    /// In-flight ninja shuriken sprites — for tests.
    pub fn shuriken_fx_count(&self) -> usize {
        self.shuriken_fx.len()
    }

    /// Ninja dodge / twinstep body-parry oneshots in flight — for tests.
    pub fn enemy_dodge_fx_count(&self) -> usize {
        self.enemy_dodge_fx.len()
    }

    /// Pose the living body would draw for local index `i` (1-based within facing
    /// row) once oneshot FX are gone — idle `1`, or dodge/parry hold while
    /// `dodgestate` (`isosprite` keeps last anim frame).
    pub fn enemy_living_draw_pose_for_test(&self, i: usize) -> u32 {
        let Some(en) = self.room.enemies.get(i) else {
            return 1;
        };
        if !en.alive {
            return enemy_corpse_pose(en.kind);
        }
        if matches!(en.kind, crate::level::EnemyKind::Pikeman) && en.pike_up {
            return 12;
        }
        if matches!(en.kind, crate::level::EnemyKind::Spirit) {
            return spirit_idle_pose(self.time);
        }
        if en.dodge_state {
            return living_dodge_hold_pose(en.kind);
        }
        1
    }

    /// Twinstep spark overlays in flight (`Images/parry`) — for tests.
    pub fn parry_spark_fx_count(&self) -> usize {
        self.parry_spark_fx.len()
    }

    /// `enemy.dodge_state` for local index `i` (tests).
    pub fn enemy_dodge_state(&self, i: usize) -> bool {
        self.room
            .enemies
            .get(i)
            .map(|e| e.dodge_state)
            .unwrap_or(false)
    }

    /// `enemy.just_dodged` for local index `i` (tests).
    pub fn enemy_just_dodged(&self, i: usize) -> bool {
        self.room
            .enemies
            .get(i)
            .map(|e| e.just_dodged)
            .unwrap_or(false)
    }

    /// Kill score counter (real deaths only; dodges excluded) — for tests.
    pub fn kill_score(&self) -> i32 {
        self.score_kills
    }

    /// Animated blood sprays (espray / isospray) — for tests.
    pub fn blood_spray_count(&self) -> usize {
        self.blood_sprays.len()
    }

    /// Test helper: push a single-pose espray at `(x, y)` so draw-order tests can
    /// overlap a burst with an actor without rolling a real kill.
    #[cfg(test)]
    pub fn inject_blood_spray_for_test(&mut self, x: i32, y: i32, facing: Facing, table_i: u8) {
        // Two poses so the burst survives the frame's `tick_blood_fx` (a single
        // pose would finish immediately and be removed before `draw_room`).
        const SPRAY_POSES: &[u32] = &[1, 1];
        self.blood_sprays.push(BloodSprayFx {
            x,
            y,
            facing,
            table_i,
            use_isospray: false,
            anim: SpriteAnim::with_rate(SPRAY_POSES, 1),
        });
    }

    /// `enemy.stunned` for local index `i` (tests).
    pub fn enemy_stunned(&self, i: usize) -> i32 {
        self.room.enemies.get(i).map(|e| e.stunned).unwrap_or(0)
    }

    /// `player.hasBlindedEnemies` (tests).
    pub fn has_blinded_enemies(&self) -> bool {
        self.has_blinded_enemies
    }

    /// `reverseRoom` (tests).
    pub fn reverse_room(&self) -> bool {
        self.reverse_room
    }

    /// Test helper: arm/clear `reverseRoom` without a door land.
    pub fn set_reverse_room_for_test(&mut self, on: bool) {
        self.reverse_room = on;
    }

    /// True while the player is dead (death pose / dialog / game-over prompt).
    pub fn death_hold_active(&self) -> bool {
        self.death_hold_ticks.is_some()
            || self.pending_death_restart
            || self.state == GameState::GameOver
    }

    /// `playdate.display.setInverted` — host should invert the RGBA blit when true.
    pub fn screen_inverted(&self) -> bool {
        self.screen_inverted
    }

    /// True while slash `kPlayerMove` wind-up is in flight (tests).
    pub fn pending_slash_active(&self) -> bool {
        self.pending_slash.is_some()
    }

    /// Advance deferred slash / smoke / anim clocks by `n` 20 Hz ticks (tests).
    pub fn advance_sim_ticks(&mut self, n: i32) {
        let mut input = Input::default();
        for _ in 0..n {
            self.update(INPUT_TICK_DT, input);
            input = Input::default();
        }
    }

    /// Test helper: enqueue a flavor dialog line (`dialogbar:show`).
    pub fn show_dialog_for_test(&mut self, key: &str, face: i32, delay: f32) {
        self.show_dialog(key, face, delay);
    }

    /// Test helper: dialog chrome still animating (open or close).
    pub fn dialog_anim_active_for_test(&self) -> bool {
        self.dialog.anim_active_for_test()
    }

    /// Test helper: a dialog line is actively shown (not merely closing).
    pub fn dialog_showing_for_test(&self) -> bool {
        self.dialog.is_showing()
    }

    /// Dismiss an open dialog (A after ticks > 3) and wait for close + death→GameOver→restart.
    /// `lifebar.displayBlood` (tests / HUD assertions).
    pub fn display_blood_for_test(&self) -> i32 {
        self.display_blood
    }

    /// Set `player.blood` and sync the lifebar display (tests).
    pub fn set_life_for_test(&mut self, life: i32) {
        self.life = life;
        self.display_blood = life;
    }

    pub fn dismiss_dialog_for_test(&mut self) {
        // Let open anim / tick gate pass (and push dead_ticks past 30 so Playing→Over is immediate).
        self.advance_sim_ticks(40);
        let mut input = Input::default();
        input.buttons.a = true;
        self.update(INPUT_TICK_DT, input);
        // Close anim is 7 frames @ 10fps ≈ 0.7s → ~14 ticks of 0.05.
        // Combat deaths: ClosedIdle → Aiming-dead → ticks>30 → GameOver.
        self.advance_sim_ticks(20);
        // GameOver: any button after dead_ticks > 5 (already true after the wait above).
        if self.state == GameState::GameOver {
            let mut input = Input::default();
            input.buttons.a = true;
            self.update(INPUT_TICK_DT, input);
        }
    }

    /// Drain one-shot SFX queued during the last `update` (play order preserved).
    pub fn take_sfx(&mut self) -> Vec<SfxId> {
        core::mem::take(&mut self.sfx_queue)
    }

    /// Drain softsynth events from the last `update` (play order preserved).
    pub fn take_synth(&mut self) -> Vec<SynthEvent> {
        core::mem::take(&mut self.synth_queue)
    }

    /// Drain dirty mid-run save JSON for host `localStorage` (exit / delete / terminate).
    pub fn take_save_flush(&mut self) -> Option<String> {
        if !self.save_dirty {
            return None;
        }
        self.save_dirty = false;
        Some(self.saved_game.to_json())
    }

    /// True when the host should write the current save blob.
    pub fn save_needs_flush(&self) -> bool {
        self.save_dirty
    }

    /// Current in-memory save JSON (no dirty clear).
    pub fn export_save(&self) -> String {
        self.saved_game.to_json()
    }

    /// Drain dirty high-score JSON for host `localStorage` (`zipper.hs.v1`).
    pub fn take_hs_flush(&mut self) -> Option<String> {
        if !self.hs_dirty {
            return None;
        }
        self.hs_dirty = false;
        Some(self.highscores.to_json())
    }

    /// True when the host should write the high-score blob.
    pub fn hs_needs_flush(&self) -> bool {
        self.hs_dirty
    }

    /// Current high-score JSON (no dirty clear).
    pub fn export_highscores(&self) -> String {
        self.highscores.to_json()
    }

    /// Replace the in-memory board from host `localStorage` (boot).
    pub fn apply_highscores_json(&mut self, json: &str) {
        self.highscores = HighScoreBoard::from_json(json);
        self.hs_dirty = false;
    }

    /// `deletesave()` — `loadsave = 0` + mark dirty for host write.
    /// Does **not** clear high scores (Catalog scores survive across runs).
    pub fn delete_save(&mut self) {
        self.saved_game.loadsave = 0;
        self.saved_game.version = SAVE_VERSION;
        self.save_dirty = true;
    }

    /// Lua `game_in_progress` (tests / host).
    pub fn game_in_progress(&self) -> bool {
        self.game_in_progress
    }

    /// Test helper: run exit `writesave` without a door wipe.
    pub fn force_write_save_after_exit_for_test(&mut self) {
        self.write_save_after_exit();
    }

    /// Test helper: `samurai:kill` path (clears save).
    pub fn force_player_death_for_test(&mut self) {
        self.begin_player_death(0, self.facing, KillType::Slash);
    }

    /// Dialog bar (tests — seen/killed flags after restore).
    pub fn dialog(&self) -> &DialogBar {
        &self.dialog
    }

    /// `samurai:exitroom` tail — update `playerPos` and write.
    ///
    /// Browser hardening vs 1.10: also set `loadsave = 1` and fill the full
    /// terminate-style snapshot so a refresh after a door (without an
    /// intervening hide) still resumes with corpses / seen flags. Lua only
    /// flipped `loadsave` in `gameWillTerminate`; door exit alone called
    /// `writesave()` with whatever flag already was.
    fn write_save_after_exit(&mut self) {
        self.saved_game.player_pos = Some([
            self.player_x,
            self.player_y,
            self.camera.0,
            self.camera.1,
            self.facing as i32,
        ]);
        self.fill_terminate_snapshot();
        // fill_terminate_snapshot may overwrite player_pos if it was None; keep post-exit.
        self.saved_game.player_pos = Some([
            self.player_x,
            self.player_y,
            self.camera.0,
            self.camera.1,
            self.facing as i32,
        ]);
        self.game_in_progress = true;
        self.save_dirty = true;
    }

    /// Fill terminate snapshot fields (seen/killed, NPCs, dead globals, …).
    fn fill_terminate_snapshot(&mut self) {
        self.sync_globals_from_room();
        let seen = self.dialog.enemies_seen_flags();
        let killed = self.dialog.enemies_killed_flags();
        self.saved_game.enemies_seen = seen.to_vec();
        self.saved_game.enemies_killed = killed.to_vec();
        self.saved_game.seed = self.random_seed;
        self.saved_game.blood = self.life;
        self.saved_game.key = self.has_key;
        self.saved_game.npcs_visited = self
            .world
            .exits
            .iter()
            .filter(|e| e.kind == 1)
            .map(|npc| self.exit_visited(npc.id))
            .collect();
        self.saved_game.dead_enemies.clear();
        for g in &self.global_enemies {
            // Lua: `alive == false and isVisible() == false` — off-strip corpses
            // are hidden; port has no sprite visibility, so all dead non-Spirit
            // globals match (on-strip corpses are still "visible" in Lua and
            // skipped — approximate by requiring not in current room locals).
            if g.alive || matches!(g.kind, EnemyKind::Spirit) || g.kind.is_inanimate() {
                continue;
            }
            let in_room = self
                .room
                .enemies
                .iter()
                .any(|l| l.global_index == g.global_index && !l.alive);
            // Prefer parked (off-strip) corpses like Lua `isVisible() == false`.
            if in_room {
                continue;
            }
            self.saved_game.dead_enemies.push(DeadEnemySave {
                x: g.x,
                y: g.y,
                etype: g.kind.etype() as u8,
                facing: g.facing,
            });
        }
        // Keep existing playerPos if set (from last door); else snapshot now.
        if self.saved_game.player_pos.is_none() {
            self.saved_game.player_pos = Some([
                self.player_x,
                self.player_y,
                self.camera.0,
                self.camera.1,
                self.facing as i32,
            ]);
        }
        self.saved_game.loadsave = 1;
        self.saved_game.version = SAVE_VERSION;
    }

    /// `playdate.gameWillTerminate` — return JSON to persist, or `None` to leave LS alone.
    ///
    /// - Not in progress → `None`
    /// - Wrong state / dead → delete blob (`loadsave=0`) as `Some`
    /// - `reverse_room` → `None` (Lua returns without write)
    /// - Else full snapshot with `loadsave=1`
    pub fn game_will_terminate(&mut self) -> Option<String> {
        if !self.game_in_progress {
            return None;
        }
        // Lua: Playing / Paused / GameOver / Dialog only. Port: Aiming ≈ Playing;
        // Transition / Resolving treated as Playing so a mid-wipe hide keeps resume.
        let resumable = matches!(
            self.state,
            GameState::Aiming
                | GameState::Dialog
                | GameState::GameOver
                | GameState::Resolving
                | GameState::Transition
        );
        if !resumable {
            self.delete_save();
            return Some(self.saved_game.to_json());
        }
        if self.reverse_room {
            return None;
        }
        if self.death_hold_active() || self.pending_death_restart {
            self.delete_save();
            return Some(self.saved_game.to_json());
        }
        self.fill_terminate_snapshot();
        self.save_dirty = true;
        Some(self.saved_game.to_json())
    }

    /// `check_for_save()` — restore mid-run blob. Returns `true` if resumed.
    ///
    /// Caller must have constructed `Game` with `saved_game.seed` already.
    /// On success: Aiming, HUD on, `intro_seen`, `loadsave` cleared + dirty.
    pub fn try_restore_save(&mut self, raw: &SavedGame) -> bool {
        if raw.loadsave != 1 || !raw.version_ok() {
            return false;
        }
        self.saved_game = raw.clone();
        // Seen / killed (do not touch dialogCounters).
        self.dialog
            .restore_enemy_flags(&raw.enemies_seen, &raw.enemies_killed);
        // NPC visited flags (stable world NPC order).
        let npcs: Vec<_> = self
            .world
            .exits
            .iter()
            .filter(|e| e.kind == 1)
            .cloned()
            .collect();
        for (i, npc) in npcs.iter().enumerate() {
            if raw.npcs_visited.get(i).copied().unwrap_or(false) {
                self.mark_exit_visited(npc.id);
            }
        }
        let Some(pos) = raw.player_pos else {
            return false;
        };
        self.player_x = pos[0];
        self.player_y = pos[1];
        self.camera = (pos[2], pos[3]);
        if let Some(f) = Facing::from_u8(pos[4] as u8) {
            self.facing = f;
        }
        self.has_key = raw.key;
        self.life = raw.blood;
        self.display_blood = self.life;
        // Rehydrate dead globals (skip Spirit).
        self.global_enemies.clear();
        for d in &raw.dead_enemies {
            let Some(kind) = EnemyKind::from_etype(d.etype as usize) else {
                continue;
            };
            if matches!(kind, EnemyKind::Spirit) {
                continue;
            }
            let mut en = Enemy::new(kind, d.x, d.y, d.facing);
            en.alive = false;
            en.global_index = Some(self.global_enemies.len());
            self.global_enemies.push(en);
        }
        // Rebuild room at restored camera / player (teleport land).
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.floorsmoke.clear();
        self.player_anim = None;
        self.kill_fx.clear();
        self.blood_sprays.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.clear_spirit_doors();
        self.refresh_viewport();
        // `refresh_viewport` → `clear_spirit_doors` wiped hop flags; restore them.
        for d in &raw.dead_enemies {
            if let Some(kind) = EnemyKind::from_etype(d.etype as usize) {
                if !matches!(kind, EnemyKind::Spirit) {
                    self.mark_dead_enemy_tile(d.x, d.y, kind);
                }
            }
        }
        self.screen_inverted = false;
        self.intro_seen = true;
        self.hud_visible = true;
        self.game_in_progress = true;
        self.state = GameState::Aiming;
        self.dialog_return = GameState::Aiming;
        // One-shot continue: clear loadsave and write back.
        self.saved_game.loadsave = 0;
        self.saved_game.version = SAVE_VERSION;
        self.save_dirty = true;
        self.draw();
        true
    }

    /// Parse JSON and [`Self::try_restore_save`].
    pub fn apply_save_json(&mut self, json: &str) -> bool {
        let Some(raw) = SavedGame::from_json(json) else {
            return false;
        };
        self.try_restore_save(&raw)
    }

    fn play_sfx(&mut self, id: SfxId) {
        self.sfx_queue.push(id);
    }

    fn queue_synth(&mut self, ev: SynthEvent) {
        self.synth_queue.push(ev);
    }

    /// Install `introchord` MIDI notes (BYOA / demo). Empty clears (silent intro).
    pub fn load_intro_music(&mut self, notes: &[u8]) {
        if notes.is_empty() {
            self.introchord = None;
        } else {
            self.introchord = Some(notes.to_vec());
        }
    }

    /// True after a non-empty [`Self::load_intro_music`].
    pub fn intro_music_loaded(&self) -> bool {
        self.introchord.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Extract `introchord` from Playdate `Globals.luac` and install it.
    pub fn load_intro_music_from_globals_luac(&mut self, data: &[u8]) -> Result<(), crate::LuacError> {
        let notes = crate::introchord_from_globals_luac(data)?;
        self.load_intro_music(&notes);
        Ok(())
    }

    /// Install ending-credit card prose (BYOA). Paired with [`CREDIT_TICKS`].
    pub fn load_credit_cards(&mut self, cards: Vec<String>) {
        self.credit_cards = cards;
    }

    /// True after a non-empty [`Self::load_credit_cards`].
    pub fn credits_loaded(&self) -> bool {
        !self.credit_cards.is_empty()
    }

    /// Extract the ending-credit prose from the user's `main.luac` and install it.
    pub fn load_credits_from_main_luac(&mut self, data: &[u8]) -> Result<(), crate::LuacError> {
        let cards = crate::credits_from_main_luac(data)?;
        self.load_credit_cards(cards);
        Ok(())
    }

    /// Install parsed `Sounds/Sho.mid` (BYOA / optional demo). Empty / bad → silent.
    pub fn load_sho_midi(&mut self, data: &[u8]) -> Result<(), crate::MidiError> {
        if data.is_empty() {
            if self.sho_playing {
                self.sho_cut_pending = true;
            }
            self.sho_midi = None;
            self.sho_playing = false;
            self.sho_pitch_bend = 0.0;
            return Ok(());
        }
        let seq = crate::parse_smf(data)?;
        self.sho_midi = Some(seq);
        Ok(())
    }

    /// True after a successful [`Self::load_sho_midi`].
    pub fn sho_midi_loaded(&self) -> bool {
        self.sho_midi
            .as_ref()
            .is_some_and(|s| !s.events.is_empty())
    }

    /// `playvictorymusic` — start `shomusic:play()` from the top.
    pub fn play_victory_music(&mut self) {
        if self.sho_midi.is_none() {
            return;
        }
        self.sho_playing = true;
        self.sho_elapsed = 0.0;
        self.sho_cursor = 0;
        // Fresh playback starts centered (no leftover bend from a prior play).
        self.sho_pitch_bend = 0.0;
        // Cut any lingering holds on the next `tick_sho_sequence` (survives
        // `synth_queue.clear()` at the top of `update` / god-teleport outside update).
        self.sho_cut_pending = true;
    }

    /// Port easter egg: current victory sho bend in semitones (±12). Host applies.
    pub fn sho_pitch_bend(&self) -> f32 {
        self.sho_pitch_bend
    }

    /// True while `Sho.mid` sequence is advancing (tests / host).
    pub fn sho_playing_for_test(&self) -> bool {
        self.sho_playing
    }

    /// Degrees of crank per semitone of victory bend (port easter egg).
    const SHO_BEND_DEG_PER_SEMITONE: f32 = 30.0;
    /// Clamp: ±1 octave.
    const SHO_BEND_MAX_SEMITONES: f32 = 12.0;
    /// Semitones/sec toward center when the crank is docked.
    const SHO_BEND_CENTER_RATE: f32 = 8.0;

    /// True while the win ending cinema is on screen: the sea pans, the
    /// loading wipes between them, and the credits scroller. The crank bend is
    /// scoped here so it stays off during the Sho fight and once the player is
    /// returned to play while `Sho.mid` keeps sounding.
    fn win_cinema_active(&self) -> bool {
        self.state == GameState::Win
            || (self.state == GameState::Transition
                && self.transition_resume == GameState::Win)
    }

    /// While the win cinema is on screen: integrate crank into pitch bend;
    /// dock eases to 0. Aiming ghost crank (`handle_crank_ghosts`) is separate
    /// and unchanged.
    fn tick_sho_pitch_bend(&mut self, dt: f32) {
        if !self.sho_playing || !self.win_cinema_active() {
            self.sho_pitch_bend = 0.0;
            return;
        }
        if self.last_input.crank_docked {
            let step = Self::SHO_BEND_CENTER_RATE * dt;
            if self.sho_pitch_bend.abs() <= step {
                self.sho_pitch_bend = 0.0;
            } else {
                self.sho_pitch_bend -= self.sho_pitch_bend.signum() * step;
            }
            return;
        }
        let delta = self.last_input.crank_delta;
        if delta == 0.0 {
            return;
        }
        // Clockwise (positive `getCrankChange`) → pitch up.
        self.sho_pitch_bend = (self.sho_pitch_bend
            + delta / Self::SHO_BEND_DEG_PER_SEMITONE)
            .clamp(-Self::SHO_BEND_MAX_SEMITONES, Self::SHO_BEND_MAX_SEMITONES);
    }

    /// Advance an active `Sho.mid` playback; queue due NoteOn/NoteOff.
    fn tick_sho_sequence(&mut self, dt: f32) {
        // A cut may have been armed by `play_victory_music` or a stop path while
        // `synth_queue` was cleared earlier in `update`; settle it first so it
        // survives regardless of the current play state.
        if self.sho_cut_pending {
            self.sho_cut_pending = false;
            self.queue_synth(SynthEvent::AllNotesOff);
        }
        if !self.sho_playing {
            return;
        }
        if self.sho_midi.is_none() {
            self.sho_playing = false;
            self.sho_pitch_bend = 0.0;
            return;
        }
        self.sho_elapsed += f64::from(dt);
        loop {
            let ev = {
                let seq = self.sho_midi.as_ref().unwrap();
                if self.sho_cursor >= seq.events.len() {
                    self.sho_playing = false;
                    self.sho_pitch_bend = 0.0;
                    // Playdate releases the instrument voices when the sequence
                    // reaches the end; without this any note still on at EOT
                    // (or a lost NoteOff) sustains forever. Matches `shomusic`
                    // stopping even though `stopvictorymusic()` is never called.
                    self.queue_synth(SynthEvent::AllNotesOff);
                    return;
                }
                let ev = seq.events[self.sho_cursor];
                if ev.t > self.sho_elapsed {
                    return;
                }
                ev
            };
            if ev.on {
                self.queue_synth(SynthEvent::NoteOn(ev.note));
            } else {
                self.queue_synth(SynthEvent::NoteOff(ev.note));
            }
            self.sho_cursor += 1;
        }
    }

    /// `soundm.entrance` + `roomsvisited` insert (`main.lua` initbackground ~462–472).
    fn mark_room_entrance(&mut self) {
        let cam = self.camera;
        let backtracking = self.rooms_visited.iter().any(|&c| c == cam);
        self.sound_entrance(cam, backtracking);
        self.rooms_visited.push(cam);
    }

    /// `soundm.entrance(roomx, roomy, backtracking)` — victory MIDI only this landing.
    fn sound_entrance(&mut self, room: (i32, i32), backtracking: bool) {
        // Lua: `if backtracking ~= false or room == (110,180) then` → no-op
        if backtracking || room == crate::worldmap::START_CAMERA {
            return;
        }
        // (45,44) music_start.pda — out of scope for this landing.
        if room == (54, 26) {
            self.play_victory_music();
        }
    }

    /// Test helper: set camera and run `mark_room_entrance` (Lua initbackground tail).
    #[cfg(test)]
    pub fn test_mark_room_entrance_at(&mut self, cam: (i32, i32)) {
        self.camera = cam;
        self.mark_room_entrance();
    }

    /// Redraw the framebuffer from the current state without advancing the
    /// simulation. Exposed for the native host's headless `--bench`.
    pub fn redraw(&mut self) {
        self.draw();
    }

    pub fn update(&mut self, dt: f32, input: Input) {
        self.frame_dt = dt;
        self.time += dt;
        self.ticks = self.ticks.wrapping_add(1);
        self.last_input = input;
        self.sfx_queue.clear();
        self.synth_queue.clear();
        self.tick_sho_sequence(dt);
        self.tick_sho_pitch_bend(dt);

        if let Some((_, ref mut t)) = self.message {
            *t -= dt;
            if *t <= 0.0 {
                self.message = None;
            }
        }

        // Edge presses this frame (A is used by both dialog hide and eitherbuttondown).
        // Computed before Title/Intro ticks so A can skip mid-intro (`buttondown`).
        let pressed_a = input.buttons.a && !self.prev_buttons.a;
        let pressed_b = input.buttons.b && !self.prev_buttons.b;
        let pressed_any = pressed_a || pressed_b;
        let pressed_boot_skip = pressed_any
            || (input.buttons.left && !self.prev_buttons.left)
            || (input.buttons.right && !self.prev_buttons.right)
            || (input.buttons.up && !self.prev_buttons.up)
            || (input.buttons.down && !self.prev_buttons.down);

        // Capture before exitroom may enter Transition mid-update.
        let was_transitioning = self.state == GameState::Transition;

        // Dialog bar always ticks (queue → open, open/close anims).
        let dialog_ev = self.dialog.update(dt);
        match dialog_ev {
            DialogEvent::Opened => {
                if self.state != GameState::Dialog {
                    self.dialog_return = match self.state {
                        GameState::Dialog => self.dialog_return,
                        other => other,
                    };
                    self.state = GameState::Dialog;
                }
                // `dialog:update` sets `ticks = 0` when a line opens.
                if self.pending_death_restart {
                    self.dead_ticks = 0;
                }
            }
            DialogEvent::ClosedIdle => {
                // Death transitions happen in `dialog:hide` (below), not after close anim.
                if !self.pending_death_restart && self.state == GameState::Dialog {
                    self.state = self.dialog_return;
                }
            }
            DialogEvent::None => {}
        }

        // Title / Intro use their own 20 Hz tick counter (`main.lua` update).
        if matches!(self.state, GameState::Title | GameState::Intro) {
            self.intro_tick_accum += dt;
            while self.intro_tick_accum >= INPUT_TICK_DT {
                self.intro_tick_accum -= INPUT_TICK_DT;
                self.intro_ticks = self.intro_ticks.saturating_add(1);
                self.tick_intro_frame(pressed_a || pressed_b);
                self.tick_kill_anims();
                // `endintro` leaves Title/Intro — stop consuming intro ticks this frame.
                if !matches!(self.state, GameState::Title | GameState::Intro) {
                    break;
                }
            }
        }

        // Smoke + kill/attack/death anims advance at display rate (20 Hz).
        if matches!(
            self.state,
            GameState::Aiming | GameState::Resolving | GameState::Dialog | GameState::GameOver
        ) {
            self.floorsmoke.tick_accum += dt;
            while self.floorsmoke.tick_accum >= INPUT_TICK_DT {
                self.floorsmoke.tick_accum -= INPUT_TICK_DT;

                // Advance Lua `ticks` while dead (Dialog / Playing-dead / GameOver).
                if self.pending_death_restart || self.state == GameState::GameOver {
                    self.dead_ticks = self.dead_ticks.saturating_add(1);
                    if self.state == GameState::Dialog && self.dead_ticks == 30 {
                        // `main.lua`: Dialog/Revive/Continue + dead + ticks==30.
                        self.play_sfx(SfxId::DeathMusic);
                    }
                    // Playing + dead + ticks > 30 → GameOver + show restartSprite.
                    if self.state == GameState::Aiming
                        && self.pending_death_restart
                        && self.dead_ticks > 30
                    {
                        self.enter_game_over();
                    }
                }

                if self.state == GameState::Dialog {
                    self.dialog.tick_dialog();
                }
                if self.state == GameState::GameOver {
                    // Ease restart sprite toward y=209: `y = (209 + y) * 0.5`.
                    self.restart_sprite_y = (209.0 + self.restart_sprite_y) * 0.5;
                    // Invert only while `ticks < 5` — combat deaths enter with ticks>30,
                    // so this is a no-op (matches Lua; avoids a spurious flash).
                    self.screen_inverted =
                        self.dead_ticks < 5 && (self.dead_ticks % 2) != 0;
                }
                // Room wipe freezes combat / smoke / anims (Lua blocks in initbackground).
                if self.state == GameState::Transition {
                    break;
                }
                // `kEnemyMove` before `kPlayerMove` resolve so arming pending_enemy
                // (from slash land or from A-commit walk/stab) never steps same frame.
                // Matches Lua if/elseif: transition to kEnemyMove skips enemy work until next tick.
                if self.state != GameState::Dialog && self.state != GameState::GameOver {
                    self.tick_pending_enemy();
                }
                // Slash `kPlayerMove`: ticks++ then ticks==1 preslash / ticks>3 resolve
                // (`main.lua`), before smoke/anim advance so land+kill+trail share a frame.
                if self.state != GameState::Dialog && self.state != GameState::GameOver {
                    self.tick_pending_slash();
                }
                self.floorsmoke.tick();
                self.tick_kill_anims();
                self.tick_map_anims();
                self.tick_spirit_doors();
                // Readybar pip blink + crankhint slide (`movebar:update`).
                if self.state == GameState::Aiming && self.hud_visible {
                    self.tick_readybar_hint();
                }
                // Player outline blink while ghost preview active (`samurai:update`).
                if self.player_outline {
                    self.outline_blink = (self.outline_blink + 1) % 8;
                } else {
                    self.outline_blink = 0;
                }
                // Death hold → dialog or GameOver (no auto-restart).
                if self.state != GameState::Dialog && self.state != GameState::GameOver {
                    if let Some(left) = self.death_hold_ticks.as_mut() {
                        *left -= 1;
                        if *left <= 0 {
                            self.death_hold_ticks = None;
                            if !self.dialog.is_open() {
                                self.enter_game_over();
                            } else {
                                self.pending_death_restart = true;
                            }
                        }
                    }
                }
                // `lifebar:update` — ease displayBlood toward player.blood.
                if self.state != GameState::GameOver {
                    self.tick_lifebar();
                }
                // `exitroom` only after move/kill anims finish (Lua: after enemy phase).
                if self.state != GameState::Dialog
                    && self.state != GameState::GameOver
                    && !self.pending_death_restart
                    && self.pending_enemy.is_none()
                    && self.pending_exit.is_some()
                    && self.exit_anims_done()
                {
                    self.begin_room_transition();
                    break;
                }
            }
        }

        // Room-change wipe advances on its own 20 Hz clock (mirrors playdate.wait + flush).
        // Skip the update that *enters* Transition so Out holds ~50 ms / one tick.
        if was_transitioning && self.state == GameState::Transition {
            self.transition_tick_accum += dt;
            while self.state == GameState::Transition
                && self.transition_tick_accum >= INPUT_TICK_DT
            {
                self.transition_tick_accum -= INPUT_TICK_DT;
                self.tick_room_transition();
            }
        }

        // Win / Score cinema at 20 Hz (`main.lua` kGameWinState / kGameScoreState).
        if matches!(self.state, GameState::Win | GameState::Score) {
            self.ending_tick_accum += dt;
            while matches!(self.state, GameState::Win | GameState::Score)
                && self.ending_tick_accum >= INPUT_TICK_DT
            {
                self.ending_tick_accum -= INPUT_TICK_DT;
                match self.state {
                    GameState::Win => self.tick_win_frame(),
                    GameState::Score => self.tick_score_frame(),
                    _ => break,
                }
            }
        }

        match self.state {
            GameState::Boot => {
                self.boot_phase += dt;
                // Host splash: click/key → first room; idle 2s → Lua white Title+Intro.
                if pressed_boot_skip {
                    let _ = self.skip_boot();
                    return;
                }
                if self.boot_phase > 2.0 {
                    self.begin_title_intro();
                    self.prev_buttons = input.buttons;
                    self.draw();
                    return;
                }
            }
            GameState::Title | GameState::Intro => {
                // Advanced in the 20 Hz tick loop below (intro_ticks).
            }
            GameState::Aiming => {
                // Dead-but-playing: input locked until GameOver (Lua still runs selector
                // only while alive; we skip aiming while `pending_death_restart`).
                if !self.pending_death_restart
                    && self.pending_exit.is_none()
                    && self.pending_slash.is_none()
                    && self.pending_enemy.is_none()
                {
                    self.handle_selector();
                    // Crank ghost preview (`main.lua` kSelectorMove).
                    self.handle_crank_ghosts();
                }
            }
            GameState::Resolving => {
                self.state = GameState::Aiming;
            }
            GameState::Dialog => {
                // `buttondown`: A after ticks > 3 → `dialog:hide`.
                // Lua sets Playing/GameOver **inside** hide while close anim runs.
                if pressed_a && self.dialog.ticks_ready() {
                    let _done = self.dialog.hide();
                    if self.pending_death_restart {
                        if self.life > 0 {
                            // Combat / non-starve: return to Playing while dead.
                            self.state = GameState::Aiming;
                        } else {
                            // Starve (blood ≤ 0): GameOver immediately.
                            self.enter_game_over();
                        }
                    } else {
                        // Flavor / first-kill / NPC: resume aiming immediately;
                        // close chrome keeps drawing via dialog.update.
                        self.state = self.dialog_return;
                    }
                }
            }
            GameState::GameOver => {}
            GameState::Transition => {
                // Input locked; phase advanced in the 20 Hz block above.
            }
            GameState::Win => {
                // A after ticks > 125 skips to the score screen (`buttondown`).
                if pressed_a && self.ending.ticks > 125 {
                    self.high_score_screen();
                }
            }
            GameState::Score => {
                // A after ticks > 25 and table exists → `requestStartGame`.
                if pressed_a && self.ending.ticks > 25 && self.ending.hs_table {
                    self.restart_at_start();
                    self.state = GameState::Aiming;
                    self.prev_buttons = input.buttons;
                    self.draw();
                    return;
                }
            }
        }

        // `eitherbuttondown` runs on A and B in the same press that hid a dialog
        // (Lua: AButtonDown → buttondown then eitherbuttondown). Starve with
        // dead_ticks > 5: one A both enters GameOver and restarts.
        if self.state == GameState::GameOver && pressed_any && self.dead_ticks > 5 {
            self.restart_sprite_y = 240.0;
            self.screen_inverted = false;
            self.restart_at_start();
            self.state = GameState::Aiming;
            self.prev_buttons = input.buttons;
            self.draw();
            return;
        }

        self.prev_buttons = input.buttons;
        self.draw();
    }

    /// Enter `kGameOverState` — show restart prompt at (109, 240).
    /// Does **not** reset `dead_ticks` (Lua keeps the global `ticks` counter).
    fn enter_game_over(&mut self) {
        self.pending_death_restart = false;
        self.death_hold_ticks = None;
        self.state = GameState::GameOver;
        self.restart_sprite_y = 240.0;
        self.screen_inverted = self.dead_ticks < 5 && (self.dead_ticks % 2) != 0;
        self.floorsmoke.tick_accum = 0.0;
    }

    /// `winGame()` — sea cinema after `exitToGameOver` (`main.lua`).
    fn win_game(&mut self) {
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.transition_phase = None;
        self.transition_pending_dialog = None;
        self.pending_win_camera = None;
        self.pending_win_gameover_screen = None;
        self.pending_win_credits = false;
        self.pending_death_restart = false;
        self.death_hold_ticks = None;
        self.dialog.hide();
        self.state = GameState::Win;
        self.ending.reset_for_win(self.life);
        self.ending_tick_accum = 0.0;
        self.hud_visible = false;
        // Keep castle camera for the Out wipe; Loading installs `WIN_CAMERA_START`.
        // Matches Lua `doTransition` + `initbackground` after intro (`main.lua:362`).
        self.floorsmoke.clear();
        self.player_anim = None;
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        self.clear_spirit_doors();
        self.blood_sprays.clear();
        self.drip_fx.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.zip_anim = None;
        self.aim_plan = AimPlan::default();
        self.selector = Selector::at_player(self.player_x, self.player_y);
        // `winGame` → `deletesave()` — clear mid-run resume (`utils.lua`).
        self.game_in_progress = false;
        self.delete_save();
        self.begin_win_camera_transition(WIN_CAMERA_START, 0, false);
    }

    /// Win-cinema `initbackground` with loading wipe (`doTransition` path).
    ///
    /// Camera is applied on Out→Loading so the Out frame still shows the previous scene.
    fn begin_win_camera_transition(
        &mut self,
        camera: (i32, i32),
        next_gameover_screen: u8,
        arm_credits: bool,
    ) {
        self.floorsmoke.clear();
        self.pending_win_camera = Some(camera);
        self.pending_win_gameover_screen = Some(next_gameover_screen);
        self.pending_win_credits = arm_credits;
        self.transition_resume = GameState::Win;
        self.transition_pending_dialog = None;
        self.transition_phase = Some(TransitionPhase::Out);
        self.transition_tick_accum = 0.0;
        self.transition_hold_ticks = 0;
        self.state = GameState::Transition;
        self.draw();
    }

    /// `highScoreScreen()` — meditative end art + local last-score panel.
    fn high_score_screen(&mut self) {
        self.cleanup_for_score_screen();
        self.state = GameState::Score;
        self.ending.reset_for_score();
        self.ending_tick_accum = 0.0;
        self.hud_visible = false;
    }

    /// `cleanup()` before the score screen — drop room actors / tables.
    fn cleanup_for_score_screen(&mut self) {
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.transition_phase = None;
        self.transition_pending_dialog = None;
        self.global_enemies.clear();
        self.room.enemies.clear();
        self.clear_spirit_doors();
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        self.blood_sprays.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.drip_fx.clear();
        self.floorsmoke.clear();
        self.player_anim = None;
        self.zip_anim = None;
        self.map_anims.clear();
        self.enemy_ghosts.clear();
        self.aim_plan = AimPlan::default();
        self.screen_inverted = false;
        self.dialog.hide();
    }

    /// One 20 Hz step of `kGameWinState` (`main.lua`).
    fn tick_win_frame(&mut self) {
        // Lua ends `kGameWinState` with `updatesprites()` — sea / waterfall
        // `isotile:update` cycles must keep advancing during the pans.
        self.tick_map_anims();
        self.ending.ticks = self.ending.ticks.saturating_add(1);
        let t = self.ending.ticks;
        let phase = self.ending.gameover_screen;
        if t > 50 && phase == 0 {
            // `initbackground` with doTransition — wipe into first pan.
            self.begin_win_camera_transition(WIN_CAMERA_PAN[0], 1, false);
            return;
        } else if t > 300 && phase == 1 {
            self.begin_win_camera_transition(WIN_CAMERA_PAN[1], 2, false);
            return;
        } else if t > 600 && phase == 2 {
            // Credits scroller is created after this initbackground returns.
            self.begin_win_camera_transition(WIN_CAMERA_PAN[2], 3, true);
            return;
        } else if t == 2220 && phase == 3 {
            self.high_score_screen();
            return;
        }
        if self.ending.gameover_screen == 3 {
            for (i, &at) in CREDIT_TICKS.iter().enumerate() {
                if t == at {
                    if let Some(text) = self.credit_cards.get(i) {
                        credits_show(&mut self.ending, text);
                    }
                }
            }
        }
        let wipe_len = self.assets.wipe.as_ref().map(|w| w.len()).unwrap_or(14);
        tick_credits_anim(&mut self.ending, INPUT_TICK_DT, wipe_len);
    }

    /// One 20 Hz step of `kGameScoreState` (`main.lua`).
    fn tick_score_frame(&mut self) {
        self.ending.ticks = self.ending.ticks.saturating_add(1);
        tick_winbackground(&mut self.ending.win_bg_t);
        if self.ending.ticks > 20 && !self.ending.hs_table {
            // Catalog `addScore` stand-in: record blood, then paint top rows + last score.
            self.highscores.submit(self.ending.ending_score);
            self.hs_dirty = true;
            let font = self.assets.monoblack.as_ref();
            self.ending.hs_player_text =
                format_last_score_line(font, self.ending.ending_score);
            self.ending.hs_table_text = self.highscores.format_table_text(font);
            start_hs_open(&mut self.ending);
        }
        let n = self.assets.highscore.as_ref().map(|t| t.len()).unwrap_or(4);
        tick_hs_anim(&mut self.ending, INPUT_TICK_DT, n);
    }

    /// Force the win cinema (Lua Simulator `"i"` / tests / god tools).
    pub fn force_win_for_test(&mut self) {
        self.win_game();
    }

    pub fn hud_visible_for_test(&self) -> bool {
        self.hud_visible
    }

    pub fn player_visible_for_test(&self) -> bool {
        self.ending.player_visible
    }

    pub fn ending_score_for_test(&self) -> i32 {
        self.ending.ending_score
    }

    pub fn credits_active_for_test(&self) -> bool {
        self.ending.credits_active
    }

    pub fn hs_table_for_test(&self) -> bool {
        self.ending.hs_table
    }

    pub fn hs_player_text_for_test(&self) -> String {
        self.ending.hs_player_text.clone()
    }

    pub fn hs_table_text_for_test(&self) -> String {
        self.ending.hs_table_text.clone()
    }

    pub fn highscore_count_for_test(&self) -> usize {
        self.highscores.scores.len()
    }

    /// Enqueue a dialog line (`dialogbar:show`).
    fn show_dialog(&mut self, key: &str, face: i32, delay: f32) {
        let chest_dir = if !self.has_key {
            self.chest_pos
                .map(|(cx, cy)| chest_direction_script(cx, cy, self.player_x, self.player_y))
        } else {
            None
        };
        self.dialog
            .show(key, face, delay, self.has_key, chest_dir);
    }

    /// Death / starve restart — mirrors Lua `startGame` after game over.
    /// Keeps `random_seed` (set once at boot; not re-rolled).
    ///
    /// `reseed_rng`: when `true` (splash skip / first Title path), reset the live
    /// stream to `random_seed` before `chester:reset` like a cold boot. When
    /// `false` (death), draw from the advancing stream — chest pad can move.
    fn restart_at_start(&mut self) {
        self.restart_at_start_with_rng(false);
    }

    fn restart_at_start_with_rng(&mut self, reseed_rng: bool) {
        if reseed_rng {
            self.run_rng = SpawnRng::new(self.random_seed);
        }
        self.player_x = START_PLAY.0;
        self.player_y = START_PLAY.1;
        self.camera = START_CAMERA;
        self.facing = Facing::North;
        // `maxBlood` then `moveToTile(..., "teleport")` → `step(0)` still spends 1
        // (`samurai.lua` playerMoved/step) → first Aiming LIFE is 249, not 250.
        self.life = MAX_BLOOD - 1;
        self.display_blood = self.life;
        self.hourglass_frame = 1;
        self.global_enemies.clear();
        self.difficulty = 1;
        self.score_kills = 0;
        self.room = load_start_room(&self.world);
        self.rebuild_map_anims();
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.aim_plan = AimPlan::default();
        self.floorsmoke.clear();
        self.player_anim = None;
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        self.clear_spirit_doors();
        self.killed_people = false;
        self.killed_last_frame = false;
        self.blood_sprays.clear();
        self.floor_bloods.clear();
        self.trail_stamps.clear();
        self.current_blood = 1;
        self.drip_fx.clear();
        self.death_hold_ticks = None;
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.transition_phase = None;
        self.transition_pending_dialog = None;
        self.transition_tick_accum = 0.0;
        self.transition_hold_ticks = 0;
        self.has_blinded_enemies = false;
        self.reverse_room = false;
        self.intro_note = 0;
        self.hurry_shown = false;
        // Without cheat key, clear; with God tools Key on, restore so restart cannot strand you.
        self.has_key = self.cheat_grant_key;
        self.visited_exits.clear();
        self.rooms_visited.clear();
        // `shomusic` is a module-level global in Lua (`Globals.lua:361`); `startGame`
        // never stops it, so the victory sequence keeps playing across death / a new
        // game (its pitch bend too). Do not reset `sho_*` here.
        self.pending_room_dialog = None;
        self.pending_death_restart = false;
        self.restart_sprite_y = 240.0;
        self.dead_ticks = 0;
        self.screen_inverted = false;
        self.bennett_visible = false;
        self.zip_anim = None;
        self.hud_visible = true;
        self.ending.clear();
        self.ending_tick_accum = 0.0;
        self.dialog.reset_run_progress();
        // `chester:reset` before `findlocalenemies` (`main.lua` startGame): draws
        // from the live stream left by the previous life — do not reseed here.
        self.roll_chest();
        self.populate_room_enemies(Facing::South);
        self.dpad_held = DpadHeld::default();
        self.input_tick_accum = 0.0;
        self.message = None;
    }

    /// `chester:reset` — random chest from `worldmap` chests layer.
    ///
    /// Lua (`chestman:reset`): `chestLocs[math.random(1, #chestLocs)]` on the
    /// **live** RNG — no `math.randomseed(random_seed)` here. Boot reseeds once
    /// then rolls; death `startGame` rolls again after room enters have advanced
    /// the stream (`randomseed(seed+cam)`), so the pad often moves. Clears key
    /// ownership is handled by the caller (`has_key = cheat_grant_key`).
    fn roll_chest(&mut self) {
        let n = self.world.chests.len() as u32;
        if n == 0 {
            self.chest_pos = None;
            self.chest_spawn_pending = true;
            return;
        }
        let i = (self.run_rng.gen_1_to(n) - 1) as usize;
        let c = &self.world.chests[i];
        self.chest_pos = Some((c.x, c.y));
        self.chest_spawn_pending = true;
    }

    fn exit_visited(&self, id: i32) -> bool {
        self.visited_exits.contains(&id)
    }

    fn mark_exit_visited(&mut self, id: i32) {
        if !self.visited_exits.contains(&id) {
            self.visited_exits.push(id);
        }
    }

    /// `lifebar:update` — step `displayBlood` toward `player.blood` and cycle hourglass.
    fn tick_lifebar(&mut self) {
        // `lifebar:showWarning` at half blood once per run.
        if !self.hurry_shown && self.life == MAX_BLOOD / 2 {
            self.hurry_shown = true;
            self.show_dialog("hurry", 0, 0.0);
        }
        if self.life < self.display_blood {
            self.display_blood -= 1;
            self.hourglass_frame += 1;
        } else if self.life > self.display_blood {
            // Lua heal ease: displayBlood = ceil(blood*0.25 + display*0.75)
            self.display_blood =
                ((self.life as f32 * 0.25) + (self.display_blood as f32 * 0.75)).ceil() as i32;
            self.hourglass_frame -= 1;
        }
        // SFX only while still catching up after the one-step ease (Lua order).
        if self.life - self.display_blood < 0 {
            self.play_sfx(SfxId::LifeDown);
        } else if self.life - self.display_blood > 0 {
            self.play_sfx(SfxId::LifeUp);
        }
        if self.hourglass_frame < 1 {
            self.hourglass_frame = 1;
        }
        // Lua: when frame > 4, keep advancing and wrap past 7 → 1 (7-cell table).
        if self.hourglass_frame > 4 {
            self.hourglass_frame += 1;
            if self.hourglass_frame > 7 {
                self.hourglass_frame = 1;
            }
        }
    }

    /// Sync `alive` / position / facing from locals back into `globalenemies`
    /// via `global_index` (Lua shares the same enemy object).
    fn sync_globals_from_room(&mut self) {
        for local in &self.room.enemies {
            let Some(gi) = local.global_index else {
                continue;
            };
            if let Some(g) = self.global_enemies.get_mut(gi) {
                g.x = local.x;
                g.y = local.y;
                g.facing = local.facing;
                g.alive = local.alive;
                g.stunned = local.stunned;
                g.kind = local.kind;
                g.child_x = local.child_x;
                g.child_y = local.child_y;
                g.pike_up = local.pike_up;
                g.just_dodged = local.just_dodged;
                g.dodge_state = local.dodge_state;
                g.global_index = Some(gi);
            }
        }
    }

    /// Mark a local enemy's global slot dead (`enemy:kill`).
    fn mark_global_dead(&mut self, local: &Enemy) {
        let Some(gi) = local.global_index else {
            return;
        };
        if let Some(g) = self.global_enemies.get_mut(gi) {
            g.alive = false;
            g.x = local.x;
            g.y = local.y;
            g.facing = local.facing;
            g.child_x = local.child_x;
            g.child_y = local.child_y;
            g.pike_up = local.pike_up;
        }
    }

    /// `findlocalenemies` for the current camera / player pose (room enter / restart).
    /// Call after terrain/`camera`/player are set. Syncs prior room locals first so
    /// moves/kills persist when leaving a room that already had globals.
    fn populate_room_enemies(&mut self, came_from: Facing) {
        self.sync_globals_from_room();
        let roomtiles = self
            .world
            .roomtiles_for(self.camera, (self.player_x, self.player_y));
        // Freeze the cull fence for this room (Lua `cullroomtiles` runs once per
        // `initbackground`). Later player moves must not widen it.
        self.room_fence = roomtiles.clone();
        // Track which globals were already present so phase-1 Spirit revive can run
        // (`utils.lua` `en.revive()` on re-attach).
        let had_globals: std::collections::HashSet<usize> = self
            .global_enemies
            .iter()
            .enumerate()
            .filter(|(_, g)| roomtiles.contains(&(g.x, g.y)))
            .map(|(i, _)| i)
            .collect();
        self.room.enemies = find_local_enemies(
            &self.world,
            &roomtiles,
            &mut self.global_enemies,
            self.random_seed,
            self.camera,
            self.difficulty,
            came_from,
            (self.player_x, self.player_y),
            &mut self.run_rng,
        );
        // Spirit-room doors: rebuild for this roomtiles set (markers 17/18).
        // Cleared on room change; re-scanned so doors work after re-entry.
        self.spawn_spirit_doors_for(&roomtiles);
        // Phase-1 Spirit re-attach → `enemy:revive` (`utils.lua:206–208`).
        if !had_globals.is_empty() {
            let idxs: Vec<usize> = self
                .room
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    matches!(e.kind, crate::level::EnemyKind::Spirit)
                        && e.global_index
                            .map(|gi| had_globals.contains(&gi))
                            .unwrap_or(false)
                })
                .map(|(i, _)| i)
                .collect();
            for i in idxs {
                self.revive_spirit_at(i);
            }
        }
        // `construct_graphs` after locals exist (`pathfinding.lua` / initbackground).
        self.construct_graphs();
    }

    /// Drop spirit-room doors + roomtile overlays (`initbackground` door teardown).
    fn clear_spirit_doors(&mut self) {
        self.spirit_doors.clear();
        self.roomtile_overrides.clear();
        // Corpse / hop flags are rebuilt with the room; clear so stale outdoor
        // cells don't keep blocking after `initbackground`.
        self.deadenemies.clear();
    }

    /// `roomtiles` value used by selector movement (door overlays + corpse `1`).
    fn roomtile_value(&self, x: i32, y: i32) -> Option<i8> {
        if let Some(&flag) = self.roomtile_overrides.get(&(x, y)) {
            return Some(flag);
        }
        self.world.roomtile_flag(x, y)
    }

    /// True when the cell is in roomtiles and walkable for a free selector step
    /// (`roomtiles ~= nil and roomtiles <= 0`).
    fn selector_free_step(&self, x: i32, y: i32) -> bool {
        matches!(self.roomtile_value(x, y), Some(f) if f <= 0)
    }

    /// Mark a kill tile in `deadenemies` / selector `roomtiles` (`enemy:kill`).
    /// Non-Spirit → hop-over (`1`); Spirit → hard-block (`2`), preserved across
    /// `construct_graphs` (Lua revives Spirits before that sweep runs).
    fn mark_dead_enemy_tile(&mut self, x: i32, y: i32, kind: crate::level::EnemyKind) {
        use crate::level::EnemyKind;
        if kind.is_inanimate() {
            return;
        }
        // Living enemies write `roomtiles = 1` on their tile; that flag remains
        // after death so `trymove` does not take the free-step (`<= 0`) branch.
        self.roomtile_overrides.insert((x, y), 1);
        if matches!(kind, EnemyKind::Spirit) {
            // Hard-block; `construct_graphs` never rewrites a dead Spirit because
            // Lua revives Spirits (`utils.lua:206-207`) before that sweep runs.
            self.deadenemies.insert((x, y), 2);
        } else {
            self.deadenemies.insert((x, y), 1);
        }
    }

    /// Spawn `door` class from kLeftDoor/kRightDoor markers in `roomtiles`.
    fn spawn_spirit_doors_for(&mut self, roomtiles: &std::collections::HashSet<(i32, i32)>) {
        self.clear_spirit_doors();
        for (x, y, left) in self.world.spirit_door_markers_in_tiles(roomtiles) {
            // `door:init` → `close()` — shut, `roomtiles = 3 + left`.
            let door = SpiritDoor::new(x, y, left);
            self.roomtile_overrides
                .insert((door.x, door.y), 3 + door.left as i8);
            self.spirit_doors.push(door);
        }
    }

    /// `enemy:revive` — Spirit only; sets alive immediately, plays revive anim + SFX.
    fn revive_spirit_at(&mut self, i: usize) {
        use crate::level::EnemyKind;
        let Some(en) = self.room.enemies.get(i).copied() else {
            return;
        };
        if !matches!(en.kind, EnemyKind::Spirit) {
            return;
        }
        let (x, y) = (en.x, en.y);
        let gi = en.global_index;
        if let Some(en) = self.room.enemies.get_mut(i) {
            en.alive = true;
            en.facing = Facing::South as u8;
        }
        // Clear corpse hop / ghost-bod marks for this tile.
        self.deadenemies.remove(&(x, y));
        if self.roomtile_overrides.get(&(x, y)) == Some(&1) {
            self.roomtile_overrides.remove(&(x, y));
        }
        // Replace any in-flight revive on this tile.
        self.spirit_revive_fx.retain(|fx| fx.x != x || fx.y != y);
        self.spirit_revive_fx.push(SpiritReviveFx {
            x,
            y,
            anim: SpriteAnim::new(SPIRIT_REVIVE),
        });
        self.play_sfx(SfxId::SpiritRevive);
        // Push alive back to global slot.
        if let Some(gi) = gi {
            if let Some(g) = self.global_enemies.get_mut(gi) {
                g.alive = true;
                g.facing = Facing::South as u8;
            }
        }
    }

    /// `updateSpiritsAndDoors(didkill)` (`main.lua`).
    fn update_spirits_and_doors(&mut self, didkill: bool) {
        if self.spirit_doors.is_empty() {
            return;
        }
        let spirits_alive = self
            .room
            .enemies
            .iter()
            .filter(|e| matches!(e.kind, crate::level::EnemyKind::Spirit) && e.alive)
            .count();
        if didkill {
            self.killed_last_frame = true;
            if spirits_alive > 0 {
                return;
            }
        } else if self.killed_last_frame {
            self.killed_last_frame = false;
        }
        if spirits_alive > 0 || !self.killed_last_frame {
            // Close doors + mass revive every dead local (Spirit-only inside revive).
            for i in 0..self.spirit_doors.len() {
                if let Some(flag) = self.spirit_doors[i].close() {
                    let (x, y) = (self.spirit_doors[i].x, self.spirit_doors[i].y);
                    self.roomtile_overrides.insert((x, y), flag);
                }
            }
            let dead: Vec<usize> = self
                .room
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.alive)
                .map(|(i, _)| i)
                .collect();
            for i in dead {
                self.revive_spirit_at(i);
            }
        } else {
            for i in 0..self.spirit_doors.len() {
                if let Some(flag) = self.spirit_doors[i].open() {
                    let (x, y) = (self.spirit_doors[i].x, self.spirit_doors[i].y);
                    self.roomtile_overrides.insert((x, y), flag);
                }
            }
        }
    }

    /// Advance spirit-room door open/close frames (`door:update`); clunk on last.
    fn tick_spirit_doors(&mut self) {
        let mut clunks = 0usize;
        for door in &mut self.spirit_doors {
            let frames_len = 4i32;
            let advancing = door.tile_anim < frames_len;
            if !advancing {
                continue;
            }
            door.timer += 1;
            if door.timer % SPIRIT_DOOR_ANIM_SPEED == 0 {
                door.timer = 0;
                door.tile_anim += 1;
                if door.tile_anim == frames_len {
                    clunks += 1;
                }
            }
        }
        for _ in 0..clunks {
            self.play_sfx(SfxId::Clunk);
        }
    }

    /// Selector step with spirit-door / corpse `roomtiles` overlays (`<= 0` walkable).
    fn selector_can_step(&self, x: i32, y: i32) -> bool {
        self.selector_free_step(x, y)
    }

    /// Terrain-only viewport reload, then attach locals whose tiles sit in the
    /// current Lua `roomtiles` set (no new rolls). Off-screen globals stay out
    /// of AI — matching `setUpdatesEnabled(false)` outside the strip.
    ///
    /// Locals must also lie on a non-void cell of the loaded terrain window.
    /// Otherwise a far outdoor zip can attach camera-strip enemies into a
    /// player-centered rect that does not contain their tile: they keep
    /// attacking (and killing) while `draw_room` never blits them.
    fn reload_room_terrain(&mut self, camera: (i32, i32), half_w: i32, half_h: i32) {
        self.sync_globals_from_room();
        let mut room = viewport_around(&self.world, camera, half_w, half_h);
        let roomtiles = self
            .world
            .roomtiles_for(self.camera, (self.player_x, self.player_y));
        // Outdoor stream recomputes the roomtiles set for the new data window;
        // freeze it as this window's fence (Lua `initbackground`).
        self.room_fence = roomtiles.clone();
        // Outdoor free-roam never re-runs full `findlocalenemies`, so castle gate
        // markers would stay missing. Spawn any entrance props whose tiles are now
        // in `roomtiles` (combat weight rolls stay door-exit-only, as in Lua).
        self.ensure_entrance_globals(&roomtiles);
        room.enemies = locals_from_globals(&roomtiles, &self.global_enemies)
            .into_iter()
            .filter(|e| !matches!(room.cell(e.x, e.y), Terrain::Void))
            .collect();
        self.room = room;
        self.rebuild_map_anims();
        // Spirit-room doors for markers now in the streamed roomtiles set.
        self.spawn_spirit_doors_for(&roomtiles);
        // Outdoor camera stream also rebuilds maingraph (`construct_graphs`).
        self.construct_graphs();
    }

    /// Create dormant `k*Entrance` globals for markers in `roomtiles` that are not
    /// already tracked (`utils.lua` fixed-marker path, outdoor streaming only).
    fn ensure_entrance_globals(&mut self, roomtiles: &std::collections::HashSet<(i32, i32)>) {
        use crate::level::{Enemy, EnemyKind};
        for &(x, y) in roomtiles {
            let m = self.world.enemy_marker(x, y);
            if m <= 9 {
                continue;
            }
            let etype = m as i32 - 9;
            let kind = match etype {
                10 => EnemyKind::LeftSouthEntrance,
                11 => EnemyKind::RightSouthEntrance,
                12 => EnemyKind::LeftEastEntrance,
                13 => EnemyKind::RightEastEntrance,
                _ => continue,
            };
            if self
                .global_enemies
                .iter()
                .any(|e| e.kind.is_inanimate() && e.x == x && e.y == y)
            {
                continue;
            }
            let facing = match kind {
                EnemyKind::LeftSouthEntrance | EnemyKind::RightSouthEntrance => Facing::South,
                EnemyKind::LeftEastEntrance | EnemyKind::RightEastEntrance => Facing::East,
                _ => Facing::South,
            };
            let mut e = Enemy::new(kind, x, y, facing as u8);
            e.global_index = Some(self.global_enemies.len());
            self.global_enemies.push(e);
        }
    }

    /// `main.lua` score → `currentDifficulty` via `Globals.scorePerLevel`.
    fn bump_score_for_kills(&mut self, kills: i32) {
        if kills <= 0 {
            return;
        }
        self.score_kills += kills;
        while self.difficulty < SCORE_PER_LEVEL.len()
            && self.score_kills > SCORE_PER_LEVEL[self.difficulty - 1]
        {
            self.difficulty += 1;
            if self.difficulty > SCORE_PER_LEVEL.len() {
                self.difficulty = SCORE_PER_LEVEL.len();
                break;
            }
        }
    }

    fn next_fx_roll(&mut self) -> u32 {
        // xorshift-ish; enough variety for spray table / anim picks.
        self.fx_rng ^= self.fx_rng << 13;
        self.fx_rng ^= self.fx_rng >> 17;
        self.fx_rng ^= self.fx_rng << 5;
        if self.fx_rng == 0 {
            self.fx_rng = 1;
        }
        self.fx_rng
    }

    /// Kill direction for `enemy:kill` / `bleed` (`main.lua` deathlist loop).
    /// `tip` is the selector commit cell (`cursor.tile_x/y`); extent is tip − player.
    fn kill_dir_for_enemy(&self, ex: i32, ey: i32, tip_x: i32, _tip_y: i32) -> Facing {
        let extent_x = tip_x - self.player_x;
        let dir = if extent_x.abs() > 0 {
            if ey < self.player_y {
                Facing::South
            } else {
                Facing::North
            }
        } else if ex < self.player_x {
            Facing::East
        } else {
            Facing::West
        };
        // On-axis kills use opposite of player facing (stab / same-row slash).
        if ex == self.player_x || ey == self.player_y {
            facing_opposite(self.facing)
        } else {
            dir
        }
    }

    /// `enemy:kill` ninja branch — hop opposite `dir` when off both player axes
    /// and the landing cell is in roomtiles / not entity-blocked. Returns true
    /// when the dodge succeeded (caller must not kill / score / bleed).
    fn try_ninja_dodge(&mut self, i: usize, dir: Facing) -> bool {
        use crate::level::EnemyKind;
        let Some(en) = self.room.enemies.get(i).copied() else {
            return false;
        };
        if !matches!(en.kind, EnemyKind::Ninja) {
            return false;
        }
        // On-axis with the player → stab / frontal slash; no dodge gate.
        if en.x == self.player_x || en.y == self.player_y {
            return false;
        }
        // Hop opposite kill dir (`enemy.lua:kill` tx/ty adjustments).
        let (hx, hy) = match dir {
            Facing::North => (en.x, en.y + 1),
            Facing::South => (en.x, en.y - 1),
            Facing::East => (en.x - 1, en.y),
            Facing::West => (en.x + 1, en.y),
        };
        // `roomtiles[…] ~= nil` — any classified cell (floor / door / npc).
        if self.world.roomtile_flag(hx, hy).is_none() {
            return false;
        }
        // `checkBlockedByEntity(tx, ty, self, false)` — player / other bodies / tips.
        if self.enemy_step_blocked(i, hx, hy) {
            return false;
        }
        let Some(en) = self.room.enemies.get_mut(i) else {
            return false;
        };
        en.x = hx;
        en.y = hy;
        en.facing = dir as u8;
        en.just_dodged = true;
        en.dodge_state = true;
        en.sync_pike_tip();
        self.enemy_dodge_fx.push(EnemyDodgeFx {
            x: hx,
            y: hy,
            facing: dir,
            anim: SpriteAnim::new(NINJA_DODGE),
            kind: EnemyKind::Ninja,
        });
        true
    }

    /// `enemy:kill` twinstep branch — frontal hit (`dir == facing`) plays parry,
    /// survives, and optionally knockbacks the player two tiles along facing when
    /// the player shares a row/col with the twinstep (Lua checks pre-land tip).
    /// Returns true when the parry succeeded (caller must not kill / score / bleed).
    fn try_twinstep_parry(&mut self, i: usize, dir: Facing) -> bool {
        use crate::level::EnemyKind;
        let Some(en) = self.room.enemies.get(i).copied() else {
            return false;
        };
        if !matches!(en.kind, EnemyKind::Twinstep) {
            return false;
        }
        let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
        if dir != facing {
            return false;
        }
        // Body pose + spark + SFX (`enemy.lua:kill` twinstep branch).
        self.enemy_dodge_fx.push(EnemyDodgeFx {
            x: en.x,
            y: en.y,
            facing,
            anim: SpriteAnim::new(TWINSTEP_PARRY),
            kind: EnemyKind::Twinstep,
        });
        let draw_behind = matches!(facing, Facing::North | Facing::West);
        self.parry_spark_fx.push(ParrySparkFx {
            x: en.x,
            y: en.y,
            draw_behind,
            anim: SpriteAnim::new(PARRY_SPARK),
        });
        self.play_sfx(SfxId::Parry);
        if let Some(en) = self.room.enemies.get_mut(i) {
            en.just_dodged = true;
            en.dodge_state = true;
        }
        // Axis check uses the player's tile *at kill time* (`enemy.lua`):
        // slash = still on pre-tip tile; stab-only = already on tip.
        if self.player_x == en.x || self.player_y == en.y {
            let _starved = self.player_knockback(en.x, en.y, facing);
        }
        true
    }

    /// `samurai:knockback` — if the cell two tiles along `dir` from `(ex, ey)` is
    /// free, `moveToTile(..., "walk")` there (trail + blood spend + smoke) and
    /// face opposite `dir`.
    ///
    /// Call timing matches Lua `kPlayerMove`:
    /// - **Slash**: kill before tip land → knockback from pre-tip, then tip land
    ///   overwrites position (blood spend + facing flip still apply).
    /// - **Stab-only**: tip land first, then kill → knockback sticks at
    ///   `ex+2*facing` (typically one tile behind the contact tip).
    /// Returns `true` if the player starved from the walk blood spend.
    fn player_knockback(&mut self, ex: i32, ey: i32, dir: Facing) -> bool {
        let (ox, oy) = dir.offsets();
        let nx = ex + 2 * ox;
        let ny = ey + 2 * oy;
        if self.player_land_blocked(nx, ny) {
            return false;
        }
        // `moveToTile(..., "walk")` → trail on old tile, land, `playerMoved` step.
        let absx = (nx - self.player_x).abs();
        let absy = (ny - self.player_y).abs();
        let numsteps = absx.max(absy);
        let walk_dir = Facing::from_delta(nx - self.player_x, ny - self.player_y)
            .unwrap_or(dir);
        self.stamp_trail_blood(self.player_x, self.player_y);
        self.player_x = nx;
        self.player_y = ny;
        self.facing = facing_opposite(dir);
        // `samurai.lua:482`: `playerMoved` ends with `cursor:moveToTile(player.tile)`,
        // so this knockback walk carries the selector to the player. Without it the
        // cursor stays on the abandoned tip and the next aim starts a tile ahead.
        self.selector = Selector::at_player(self.player_x, self.player_y);
        // Walk anim + blood spend (`samurai:step`). Tip-land slash anim may
        // overwrite the step pose a few lines later (Lua does the same).
        if !self.invulnerable {
            self.life = (self.life - 1).max(0);
            if self.life <= 0 {
                self.begin_player_death(0, self.facing, KillType::Starve);
                return true;
            }
        }
        if numsteps <= 0 {
            self.player_anim = Some(SpriteAnim::with_rate(PLAYER_ZSTEP, ANIM_TICKS_PER_POSE_5FPS));
        } else if numsteps < 5 {
            self.player_anim = Some(SpriteAnim::with_rate(PLAYER_STEP, ANIM_TICKS_PER_POSE_5FPS));
            self.play_sfx(SfxId::Step);
        } else {
            self.player_anim = Some(SpriteAnim::new(PLAYER_FASTSTEP));
            self.play_sfx(SfxId::Swoosh);
        }
        if numsteps > 1 {
            self.floorsmoke
                .do_smoke(walk_dir, numsteps, self.player_x, self.player_y);
        }
        false
    }

    /// `checkBlockedByEntity(..., player, false)` for player knockback landing:
    /// any enemy body (corpse included), or non-walkable terrain. Player self-tile
    /// is fine. Lua has no `alive` filter here, so dead bodies block the bump too.
    fn player_land_blocked(&self, x: i32, y: i32) -> bool {
        if !self.room.walkable(x, y) {
            return true;
        }
        self.room.enemies.iter().any(|e| {
            if e.x == x && e.y == y {
                return true;
            }
            e.tip_blocks(x, y)
        })
    }

    /// `enemy:bleed` — espray burst + hereblood + directional floorspray + blood SFX.
    /// Floorspray tiles that land on living locals (not on this zip's deathlist)
    /// call `enemy:stun` (`stunned = 2`, inverted draw, skip AI).
    fn spawn_enemy_bleed(
        &mut self,
        x: i32,
        y: i32,
        kill_dir: Facing,
        variety: usize,
        deathlist: &[usize],
    ) {
        let spray_facing = bleed_spray_facing(kill_dir);
        let table_i = (self.next_fx_roll() % 5) as u8;
        let poses = espray_anim_variant(variety.wrapping_add(self.next_fx_roll() as usize));
        self.blood_sprays.push(BloodSprayFx {
            x,
            y,
            facing: spray_facing,
            table_i,
            use_isospray: false,
            anim: SpriteAnim::with_rate(poses, 1),
        });

        self.floor_bloods.push(FloorBlood {
            x,
            y,
            floorspray_idx: None,
            delay: 0,
        });

        let offsets = bleed_floor_offsets(kill_dir);
        for (i, (ox, oy)) in offsets.iter().enumerate() {
            let tx = x + ox;
            let ty = y + oy;
            // Lua: roomtiles[index] ~= nil and >= 0 (any placed tile).
            if self.room.cell(tx, ty).tile().is_none() {
                continue;
            }
            self.floor_bloods.push(FloorBlood {
                x: tx,
                y: ty,
                floorspray_idx: Some(floorspray_cell(kill_dir, i)),
                delay: bleed_floor_delay(i),
            });
            // Stun any living neighbor on this splat tile (`enemy:bleed` loop).
            self.stun_enemies_at(tx, ty, deathlist);
        }

        self.play_sfx(SfxId::Blood);
    }

    /// `enemy:stun` — blood-blind a living non-spirit on `(tx, ty)`.
    fn stun_enemies_at(&mut self, tx: i32, ty: i32, deathlist: &[usize]) {
        for i in 0..self.room.enemies.len() {
            if deathlist.contains(&i) {
                continue;
            }
            let en = &self.room.enemies[i];
            if !en.alive || en.x != tx || en.y != ty {
                continue;
            }
            if matches!(en.kind, crate::level::EnemyKind::Spirit) {
                continue;
            }
            if !self.has_blinded_enemies {
                self.has_blinded_enemies = true;
                // `dialogbar:show("blinded", self.etype, 0)` — face of the stunned enemy.
                let face = en.kind.etype() as i32;
                self.show_dialog("blinded", face, 0.0);
            }
            self.room.enemies[i].stunned = 2;
        }
    }

    /// `samurai:bleed` — spray on the player tile (pierce → isospray, else espray).
    fn spawn_player_bleed_spray(&mut self, kill_dir: Facing, kill_type: KillType) {
        let (facing, use_isospray, poses, ticks) = match kill_type {
            KillType::Pierce => (
                player_bleed_pierce_facing(kill_dir),
                true,
                PLAYER_BLEED_ISOSPRAY,
                ANIM_TICKS_PER_POSE,
            ),
            _ => (
                player_bleed_slash_facing(kill_dir),
                false,
                PLAYER_BLEED_ESPRAY,
                1,
            ),
        };
        let table_i = if use_isospray {
            0
        } else if self.next_fx_roll() % 2 == 0 {
            0 // espray1
        } else {
            4 // espray5
        };
        self.blood_sprays.push(BloodSprayFx {
            x: self.player_x,
            y: self.player_y,
            facing,
            table_i,
            use_isospray,
            anim: SpriteAnim::with_rate(poses, ticks),
        });
    }

    /// `samurai:sploosh` — dripsC on player + N/S/E/W on adjacent tiles.
    fn spawn_player_sploosh(&mut self) {
        let px = self.player_x;
        let py = self.player_y;
        // (which, ox, oy): C on player, then N/S/E/W neighbors.
        let spots: [(u8, i32, i32); 5] = [
            (0, 0, 0),
            (1, 0, -1),
            (2, 0, 1),
            (3, 1, 0),
            (4, -1, 0),
        ];
        for (which, ox, oy) in spots {
            self.drip_fx.push(DripFx {
                x: px + ox,
                y: py + oy,
                which,
                anim: SpriteAnim::new(PLAYER_DRIPS),
            });
        }
    }

    /// `samurai:kill` — `slashed` + playerdeath; bleed/sploosh unless starve.
    /// No-op when Invulnerability is on (god tools).
    ///
    /// `killer_etype` is the Lua enemy type for `playerKilled` face (0 = player /
    /// starve). Dialog shows with 1.3s open delay; A dismisses then restart.
    fn begin_player_death(
        &mut self,
        killer_etype: i32,
        kill_dir: Facing,
        kill_type: KillType,
    ) {
        self.begin_player_death_ex(killer_etype, kill_dir, kill_type, true);
    }

    /// `samurai:kill` body. When `show_killed_dialog` is false, the caller already
    /// opened `playerKilled` at ninja throw time (Lua `enemy:stab` order).
    fn begin_player_death_ex(
        &mut self,
        killer_etype: i32,
        kill_dir: Facing,
        kill_type: KillType,
        show_killed_dialog: bool,
    ) {
        if self.invulnerable {
            self.message = Some((self.cheat_invuln_label().into(), 0.8));
            return;
        }
        // `samurai:kill` → `game_in_progress = false` + `deletesave()`.
        self.game_in_progress = false;
        self.delete_save();
        self.player_anim = Some(SpriteAnim::new(PLAYER_SLASHED));
        self.play_sfx(SfxId::PlayerDeath);
        if kill_type != KillType::Starve {
            self.spawn_player_bleed_spray(kill_dir, kill_type);
            self.spawn_player_sploosh();
            self.play_sfx(SfxId::Blood);
        } else {
            // Starve still splooshes drips (`samurai:kill` → sploosh), no spray.
            self.spawn_player_sploosh();
        }
        // Hold death pose / drips while the dialog plays; GameOver after dismiss.
        self.death_hold_ticks = None;
        self.pending_death_restart = true;
        self.dead_ticks = 0;
        self.screen_inverted = false;
        self.aim_plan = AimPlan::default();
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.dpad_held = DpadHeld::default();
        self.pending_exit = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.message = None;
        if show_killed_dialog {
            if kill_type == KillType::Starve {
                self.show_dialog("playerStarved", 0, 1.3);
            } else {
                self.show_dialog("playerKilled", killer_etype, 1.3);
            }
        }
    }

    /// Advance player attack + enemy death / spray / drip pose sequences one 20 Hz tick.
    fn tick_kill_anims(&mut self) {
        // Capture before borrowing `player_anim` (dialog / game-over keep the fallen pose).
        let hold_death_pose = self.death_hold_active();
        if let Some(anim) = self.player_anim.as_mut() {
            let finished = anim.tick();
            // Clearing here falls back to idle pose 1 in `blit_player` — standing again.
            if finished && !hold_death_pose {
                self.player_anim = None;
            }
        }
        if let Some(anim) = self.zip_anim.as_mut() {
            if anim.tick() {
                self.zip_anim = None;
            }
        }
        let mut i = 0;
        while i < self.kill_fx.len() {
            if self.kill_fx[i].anim.tick() {
                self.kill_fx.swap_remove(i);
            } else {
                i += 1;
            }
        }
        i = 0;
        while i < self.enemy_attack_fx.len() {
            if self.enemy_attack_fx[i].anim.tick() {
                self.enemy_attack_fx.swap_remove(i);
            } else {
                i += 1;
            }
        }
        self.tick_shuriken_fx();
        i = 0;
        while i < self.enemy_dodge_fx.len() {
            if self.enemy_dodge_fx[i].anim.tick() {
                self.enemy_dodge_fx.swap_remove(i);
            } else {
                i += 1;
            }
        }
        i = 0;
        while i < self.parry_spark_fx.len() {
            if self.parry_spark_fx[i].anim.tick() {
                self.parry_spark_fx.swap_remove(i);
            } else {
                i += 1;
            }
        }
        i = 0;
        while i < self.spirit_revive_fx.len() {
            if self.spirit_revive_fx[i].anim.tick() {
                self.spirit_revive_fx.swap_remove(i);
            } else {
                i += 1;
            }
        }
        i = 0;
        while i < self.blood_sprays.len() {
            if self.blood_sprays[i].anim.tick() {
                self.blood_sprays.swap_remove(i);
            } else {
                i += 1;
            }
        }
        // `samurai:sploosh`: `hideOnFinish = false` / `deleteOnFinish = false` —
        // drips stay on the last frame until room change / restart.
        for drip in &mut self.drip_fx {
            let _ = drip.anim.tick();
        }
        for fb in &mut self.floor_bloods {
            if fb.delay > 0 {
                fb.delay -= 1;
            }
        }
    }

    /// `oneshotsprite:update` motion + anim for in-flight shuriken.
    /// Kill fires from `motionFinished` when pre-move delta is already (0,0).
    fn tick_shuriken_fx(&mut self) {
        let mut finished: Vec<(Facing, KillType, i32)> = Vec::new();
        let mut i = 0;
        while i < self.shuriken_fx.len() {
            let fx = &mut self.shuriken_fx[i];
            let diffx = fx.target_x - fx.tile_x;
            let diffy = fx.target_y - fx.tile_y;
            if diffx == 0 && diffy == 0 {
                finished.push((fx.kill_dir, fx.kill_type, fx.killer_etype));
                self.shuriken_fx.swap_remove(i);
                continue;
            }
            let steps = fx.steps_per_frame.max(1);
            let tx = if diffx > 0 {
                diffx.min(steps)
            } else if diffx < 0 {
                diffx.max(-steps)
            } else {
                0
            };
            let ty = if diffy > 0 {
                diffy.min(steps)
            } else if diffy < 0 {
                diffy.max(-steps)
            } else {
                0
            };
            fx.tile_x += tx;
            fx.tile_y += ty;
            // Anim may finish while motion continues — Lua keeps the sprite until
            // motionFinished; keep last pose via SpriteAnim clamp.
            let _ = fx.anim.tick();
            i += 1;
        }
        for (dir, ktype, etype) in finished {
            // Dialog already opened at throw time (`enemy:stab`).
            self.begin_player_death_ex(etype, dir, ktype, false);
        }
    }

    /// Door-exit room load: new camera terrain + `findlocalenemies(oppositeFacing)`.
    ///
    /// Also spawns spirit-room `door` overlays (`utils.lua` kLeftDoor/kRightDoor)
    /// via [`Self::populate_room_enemies`] — without that, exit tiles stay walkable
    /// (`roomtile_flag == -1`) and `updateSpiritsAndDoors` never runs.
    fn refresh_viewport(&mut self) {
        // Terrain window first; locals + spirit doors come from populate.
        self.sync_globals_from_room();
        self.room = viewport_around(&self.world, self.camera, 16, 16);
        self.rebuild_map_anims();
        // `main.lua` initbackground → findlocalenemies(oppositeFacing(player.facing))
        // Player has already stepped one tile past the door into the new room.
        let came_from = self.facing.opposite();
        self.populate_room_enemies(came_from);
    }

    /// Test helper: count active landscape anims (`mapanimtiles`).
    pub fn map_anim_count_for_test(&self) -> usize {
        self.map_anims.len()
    }

    /// Test helper: current animated GID at `(gx, gy)`, or `None` if not animating.
    pub fn map_anim_gid_at_for_test(&self, gx: i32, gy: i32) -> Option<u16> {
        self.map_anims
            .iter()
            .find(|a| a.x == gx && a.y == gy)
            .and_then(|a| a.frames.get(a.frame_i).copied())
    }

    /// Test helper: force a wall GID into the current room cell (occlusion tests).
    pub fn set_room_cell_for_test(&mut self, wx: i32, wy: i32, terrain: Terrain) {
        let lx = wx - self.room.origin_x;
        let ly = wy - self.room.origin_y;
        if lx < 0 || ly < 0 || lx >= self.room.width || ly >= self.room.height {
            return;
        }
        self.room.cells[(ly * self.room.width + lx) as usize] = terrain;
    }

    /// Test helper: drop a cell from culled `path_roomtiles` (wall / void / cull).
    /// `roomtiles_for` reads the worldmap, so strip-injected walls need this to
    /// exercise `checkBlockedShuriken` membership without mutating bytecode.
    pub fn remove_path_roomtile_for_test(&mut self, wx: i32, wy: i32) {
        self.path_roomtiles.remove(&(wx, wy));
    }

    /// Test helper: culled `path_roomtiles` membership (enemy path fence).
    pub fn path_roomtile_contains_for_test(&self, wx: i32, wy: i32) -> bool {
        self.path_roomtiles.contains(&(wx, wy))
    }

    /// Test helper: arm `pending_exit` in place (`player.onexit`) without moving
    /// the player or starting a wipe — for shuriken `skipexitcheck=false` LOS.
    pub fn arm_pending_exit_stub_for_test(&mut self) {
        self.pending_exit = Some(PendingExit {
            id: -1,
            camera: self.camera,
            teleport: false,
            to_game_over: false,
        });
    }

    /// True when move/kill pose sequences that should finish before `exitroom` are done.
    fn exit_anims_done(&self) -> bool {
        let player_done = match &self.player_anim {
            None => true,
            Some(a) => a.index >= a.poses.len(),
        };
        player_done && self.kill_fx.is_empty()
    }

    /// Start `doTransition` wipe, or complete the exit immediately when skipped.
    ///
    /// Lua skips the wipe only before `introSeen`; after intro / splash skip every
    /// `initbackground` from `exitroom` shows the loading screen.
    /// `exitToGameOver` → `winGame()` instead of a room load.
    fn begin_room_transition(&mut self) {
        if self.pending_exit.as_ref().is_some_and(|p| p.to_game_over) {
            self.pending_exit = None;
            self.win_game();
            return;
        }
        if self.pending_exit.is_none() {
            return;
        }
        if !self.intro_seen {
            self.complete_pending_exit_load();
            return;
        }
        self.floorsmoke.clear();
        self.transition_phase = Some(TransitionPhase::Out);
        self.transition_pending_dialog = None;
        self.transition_resume = GameState::Aiming;
        self.pending_win_camera = None;
        self.pending_win_gameover_screen = None;
        self.pending_win_credits = false;
        self.transition_tick_accum = 0.0;
        self.transition_hold_ticks = 0;
        self.state = GameState::Transition;
        self.draw();
    }

    /// One 20 Hz step of the room-change wipe (`Out` → `Loading` → `In` → resume).
    fn tick_room_transition(&mut self) {
        let Some(phase) = self.transition_phase else {
            self.state = GameState::Aiming;
            return;
        };
        match phase {
            TransitionPhase::Out => {
                // After ~50 ms hold: clear + "loading..." + rebuild (Lua wait then clear).
                // `soundm.transition` plays with the loading text (`main.lua:379`).
                self.play_sfx(SfxId::Transition);
                if let Some(cam) = self.pending_win_camera.take() {
                    self.camera = cam;
                    self.refresh_viewport();
                } else {
                    self.complete_pending_exit_load();
                }
                self.transition_hold_ticks = TRANSITION_LOADING_TICKS;
                self.transition_phase = Some(TransitionPhase::Loading);
            }
            TransitionPhase::Loading => {
                // Hold so the text is readable — wasm rebuild is instant vs Lua strips.
                if self.transition_hold_ticks > 1 {
                    self.transition_hold_ticks -= 1;
                    return;
                }
                // `soundm.transition2` as the in-wipe flashes (`main.lua:510`).
                self.play_sfx(SfxId::Transition2);
                self.transition_hold_ticks = 0;
                self.transition_phase = Some(TransitionPhase::In);
            }
            TransitionPhase::In => {
                self.transition_phase = None;
                let resume = self.transition_resume;
                self.transition_resume = GameState::Aiming;
                if resume == GameState::Win {
                    self.state = GameState::Win;
                    if let Some(screen) = self.pending_win_gameover_screen.take() {
                        self.ending.gameover_screen = screen;
                    }
                    if self.pending_win_credits {
                        self.pending_win_credits = false;
                        self.ending.credits_active = true;
                        self.ending.credits_frame = 1;
                    }
                } else {
                    self.state = GameState::Aiming;
                    if let Some((key, face)) = self.transition_pending_dialog.take() {
                        self.show_dialog(&key, face, 0.0);
                    }
                }
            }
        }
    }

    /// `samurai:exitroom` body + `initbackground` rebuild (no wipe / dialog open).
    /// Dialog is deferred into `transition_pending_dialog` when mid-wipe.
    fn complete_pending_exit_load(&mut self) {
        let Some(pending) = self.pending_exit.take() else {
            return;
        };
        self.camera = pending.camera;
        if pending.teleport {
            // `exitroom` → `moveToTile(exits[onexit].x/y, "teleport", onexit)`.
            // Teleport skips `trailBlood` (`samurai:moveToTile` type "teleport").
            // `fromexitID` skips re-arming the destination door in `playerMoved`.
            if let Some(dest) = self.world.exit_by_id(pending.id) {
                let (dx, dy) = (dest.x, dest.y);
                self.player_x = dx;
                self.player_y = dy;
                // `step(0)` still decrements blood (`samurai:step`).
                if !self.invulnerable {
                    self.life = (self.life - 1).max(0);
                    if self.life <= 0 {
                        self.begin_player_death(0, self.facing, KillType::Starve);
                        // Still finish the room load so the corpse isn't left outdoors.
                    }
                }
            }
        } else {
            // One step past the door in facing (`exitroom` → moveByTile(offset)).
            // Lua `moveByTile` stamps trailBlood on the door tile first; then
            // `initbackground` wipes strip-baked stamps with mapsprites.
            let (ox, oy) = self.facing.offsets();
            let dest_x = self.player_x + ox;
            let dest_y = self.player_y + oy;
            if self.selector_can_step(dest_x, dest_y) {
                self.stamp_trail_blood(self.player_x, self.player_y);
                self.player_x = dest_x;
                self.player_y = dest_y;
            }
        }
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.floorsmoke.clear(); // `floorsmoke:clear_smoke` on room change
        self.player_anim = None;
        self.kill_fx.clear();
        self.enemy_attack_fx.clear();
        self.shuriken_fx.clear();
        self.enemy_dodge_fx.clear();
        self.parry_spark_fx.clear();
        self.spirit_revive_fx.clear();
        // Lua `initbackground` removes `doors` sprites before rebuilding roomtiles.
        self.clear_spirit_doors();
        self.killed_people = false;
        self.killed_last_frame = false;
        self.blood_sprays.clear();
        self.drip_fx.clear();
        self.floor_bloods.clear();
        // Strip-baked trailBlood + mapsprites wipe on `initbackground`.
        // Zip oneshot lives in `mapsprites` in Lua → removed here too.
        self.trail_stamps.clear();
        self.zip_anim = None;
        self.pending_slash = None;
        self.pending_enemy = None;
        self.refresh_viewport();
        // `soundm.entrance` + `roomsvisited` insert (`main.lua` initbackground).
        self.mark_room_entrance();
        // Arm exit / first-seen dialog; open after in-wipe (or immediately if no wipe).
        let dialog = if let Some((key, face)) = self.pending_room_dialog.take() {
            Some((key, face))
        } else {
            self.take_enemy_seen_dialog()
        };
        if self.state == GameState::Transition {
            self.transition_pending_dialog = dialog;
        } else if let Some((key, face)) = dialog {
            self.show_dialog(&key, face, 0.0);
        }
        // `samurai:exitroom` → write `playerPos` + `writesave()` (+ browser loadsave=1).
        self.write_save_after_exit();
    }

    /// First unseen enemy type in the new room (`initbackground` seen branch).
    ///
    /// Lua scans locals with if/elseif priority: King, then Pike/Ninja/Twinstep/Spirit
    /// (first match in foreach order). Swordsman is pre-marked seen.
    fn take_enemy_seen_dialog(&mut self) -> Option<(String, i32)> {
        use crate::level::EnemyKind;
        let mut newenemy = 0usize;
        let mut newface = 0i32;
        for en in self.room.enemies.iter().filter(|e| e.alive) {
            let et = en.kind.etype();
            if matches!(en.kind, EnemyKind::King) && !self.dialog.enemies_seen(et) {
                newenemy = et;
                newface = et as i32;
                break;
            }
        }
        if newenemy == 0 {
            for en in self.room.enemies.iter().filter(|e| e.alive) {
                let et = en.kind.etype();
                if !self.dialog.enemies_seen(et)
                    && matches!(
                        en.kind,
                        EnemyKind::Pikeman
                            | EnemyKind::Ninja
                            | EnemyKind::Twinstep
                            | EnemyKind::Spirit
                    )
                {
                    newenemy = et;
                    newface = 0;
                    break;
                }
            }
        }
        if newenemy == 0 {
            return None;
        }
        self.dialog.mark_seen(newenemy);
        let key = match newenemy {
            1 => "swordsmanSeen",
            2 => "pikemanSeen",
            3 => "ninjaSeen",
            4 => "guardSeen",
            5 => "kingSeen",
            6 => "spiritSeen",
            _ => return None,
        };
        Some((key.to_string(), newface))
    }

    /// `samurai:trailBlood` — stamp a random `Images/trail` cell on `(x, y)`.
    /// Lua draws into the iso strip under the player; we keep an explicit stamp
    /// list (same lifetime as mapsprites / cleared on room change).
    fn stamp_trail_blood(&mut self, x: i32, y: i32) {
        // Pick a cell ≠ last (`while lastBlood == currentBlood`).
        let mut next = self.current_blood;
        for _ in 0..8 {
            next = (1 + (self.next_fx_roll() % 8)) as u8;
            if next != self.current_blood {
                break;
            }
        }
        self.current_blood = next;
        self.trail_stamps.push(TrailStamp {
            x,
            y,
            cell: next.saturating_sub(1), // Lua 1-based → 0-based table index
        });
    }

    fn edge(now: bool, was: bool) -> bool {
        now && !was
    }

    /// `selector:trymove` / `moveByTile` plus `main.lua` hold-repeat.
    fn handle_selector(&mut self) {
        // Input locked while the death pose is playing (`player.alive == false`),
        // during slash `kPlayerMove` wind-up, or while `kEnemyMove` drains.
        if self.death_hold_ticks.is_some()
            || self.pending_slash.is_some()
            || self.pending_enemy.is_some()
        {
            return;
        }

        let b = self.last_input.buttons;
        let prev = self.prev_buttons;

        // B → resetToPlayer
        if Self::edge(b.b, prev.b) {
            self.selector = Selector::at_player(self.player_x, self.player_y);
            self.aim_plan = AimPlan::default();
            self.reset_ghosts();
            self.dpad_held = DpadHeld::default();
            return;
        }

        // A → commit when cursor moved (`buttondown` in main.lua)
        if Self::edge(b.a, prev.a) {
            if self.selector.can_commit(self.player_x, self.player_y) {
                self.commit_zip();
            }
            return;
        }

        // ButtonDown: immediate trymove + buttonsdown = 0
        // ButtonUp: buttonsdown = -1
        self.sync_dpad_press_edges(b, prev);

        // 20 Hz tick: increment held counters; repeat while v >= 4
        self.input_tick_accum += self.frame_dt;
        while self.input_tick_accum >= INPUT_TICK_DT {
            self.input_tick_accum -= INPUT_TICK_DT;
            self.tick_dpad_hold_repeat();
        }
    }

    /// `*ButtonDown` / `*ButtonUp` handlers in `main.lua`.
    /// D-pad ButtonUp re-arms `canBuzz` (`main.lua` left/right/up/down ButtonUp).
    fn sync_dpad_press_edges(&mut self, b: Buttons, prev: Buttons) {
        if Self::edge(b.up, prev.up) {
            self.dpad_held.up = 0;
            self.try_move_selector(0, -1);
        } else if !b.up {
            if prev.up {
                self.can_buzz = true;
            }
            self.dpad_held.up = -1;
        }

        if Self::edge(b.down, prev.down) {
            self.dpad_held.down = 0;
            self.try_move_selector(0, 1);
        } else if !b.down {
            if prev.down {
                self.can_buzz = true;
            }
            self.dpad_held.down = -1;
        }

        if Self::edge(b.left, prev.left) {
            self.dpad_held.left = 0;
            self.try_move_selector(-1, 0);
        } else if !b.left {
            if prev.left {
                self.can_buzz = true;
            }
            self.dpad_held.left = -1;
        }

        if Self::edge(b.right, prev.right) {
            self.dpad_held.right = 0;
            self.try_move_selector(1, 0);
        } else if !b.right {
            if prev.right {
                self.can_buzz = true;
            }
            self.dpad_held.right = -1;
        }
    }

    /// `selector.lua` `buzz()` — play `soundm.buzz` at most once until D-pad Up.
    fn buzz(&mut self) {
        if self.can_buzz {
            self.play_sfx(SfxId::Buzz);
        }
        self.can_buzz = false;
    }

    /// One 20 Hz playing-state pass over `buttonsdown` (`main.lua` ~570–588).
    fn tick_dpad_hold_repeat(&mut self) {
        // Mirror Lua: read old v, increment store, repeat if old v >= 4.
        macro_rules! tick_dir {
            ($field:ident, $ox:expr, $oy:expr) => {{
                let v = self.dpad_held.$field;
                if v >= 0 {
                    self.dpad_held.$field = v + 1;
                }
                if v >= DPAD_REPEAT_AFTER {
                    self.try_move_selector($ox, $oy);
                }
            }};
        }
        tick_dir!(up, 0, -1);
        tick_dir!(down, 0, 1);
        tick_dir!(right, 1, 0);
        tick_dir!(left, -1, 0);
    }

    /// Port of `selector:trymove` + `moveByTile` (axis lock, roomtiles, corpse hop,
    /// maxSteps, exit block). On success plays `soundm.select` and rebuilds aim via
    /// `detectkills`.
    fn try_move_selector(&mut self, ox: i32, oy: i32) {
        let mut sel = self.selector;

        // Changing axis while already extended on the other → reset to player first
        // (`shouldreset` in trymove). Lua still checks the *player-adjacent* cell is
        // in roomtiles (any flag); we only require a classified cell.
        if (ox != 0 && sel.extent_y != 0) || (oy != 0 && sel.extent_x != 0) {
            if self.roomtile_value(self.player_x + ox, self.player_y + oy).is_none() {
                self.buzz();
                return;
            }
            sel = Selector::at_player(self.player_x, self.player_y);
            self.aim_plan = AimPlan::default();
            self.selector = sel;
        }

        let adj_x = sel.tile_x + ox;
        let adj_y = sel.tile_y + oy;
        let Some(adj_flag) = self.roomtile_value(adj_x, adj_y) else {
            self.buzz();
            return;
        };

        // Branch order matches `selector:trymove` (`selector.lua:122–154`):
        // free (`<=0`) → hop (`deadenemies==1`) → ghost-bod (`==2`) → else buzz.
        if adj_flag <= 0 {
            self.move_selector_by_tile(ox, oy);
            return;
        }
        if self.deadenemies.get(&(adj_x, adj_y)) == Some(&1) {
            // Hop over consecutive hop-corpses; land on first free cell beyond.
            let mut iter = 2i32;
            loop {
                let hx = sel.tile_x + ox * iter;
                let hy = sel.tile_y + oy * iter;
                if self.deadenemies.get(&(hx, hy)) == Some(&1) {
                    iter += 1;
                    continue;
                }
                match self.roomtile_value(hx, hy) {
                    Some(f) if f <= 0 => {
                        self.move_selector_by_tile(ox * iter, oy * iter);
                        return;
                    }
                    Some(_) | None => {
                        self.buzz();
                        return;
                    }
                }
            }
        }
        if self.deadenemies.get(&(adj_x, adj_y)) == Some(&2) {
            // Spirit corpse hard-block; stays `2` across `construct_graphs`
            // because Lua revives Spirits before sweeping corpses.
            self.buzz();
            return;
        }
        self.buzz();
    }

    /// `selector:moveByTile(tilex, tiley)` — land tip, set `canmove`, detectkills.
    fn move_selector_by_tile(&mut self, tilex: i32, tiley: i32) {
        let mut sel = self.selector;

        // blockedDir: cannot keep walking into the exit's facing.
        if let Some(blocked) = sel.blocked {
            let (bx, by) = blocked.offsets();
            // `tilex`/`tiley` may be a hop (>1); block if the step sign matches.
            if tilex.signum() == bx && tiley.signum() == by && (bx != 0 || by != 0) {
                self.buzz();
                return;
            }
        }

        let nx = sel.tile_x + tilex;
        let ny = sel.tile_y + tiley;

        // Spirit on landing cell → silent return (`selector.lua:184-188`). The Lua
        // loop walks all `localenemies` and never tests `alive`, so a dead Spirit
        // body blocks a landing too.
        if let Some(i) = self.room.any_enemy_at(nx, ny) {
            if matches!(self.room.enemies[i].kind, crate::level::EnemyKind::Spirit) {
                return;
            }
        }

        sel.canmove = true;
        // Living enemy blocks; dead enemy lands but disables tip stab (`canmove=false`).
        if let Some(i) = self.room.any_enemy_at(nx, ny) {
            let en = &self.room.enemies[i];
            if en.alive {
                self.buzz();
                return;
            }
            sel.canmove = false;
        }
        // Living pike tip also blocks (`selector.lua` "can't move selector on pike").
        if self.room.pike_tip_at(nx, ny).is_some() {
            self.buzz();
            return;
        }

        let new_ex = sel.extent_x + tilex;
        let new_ey = sel.extent_y + tiley;
        if new_ex.abs() > MAX_STEPS || new_ey.abs() > MAX_STEPS {
            self.buzz();
            return;
        }

        sel.extent_x = new_ex;
        sel.extent_y = new_ey;
        sel.tile_x = nx;
        sel.tile_y = ny;
        sel.blocked = None;

        // Standing on an exit object → block further steps in arrival facing.
        if self.world.exit_at(nx, ny).is_some() {
            if let Some(dir) = Facing::from_delta(tilex.signum(), tiley.signum()) {
                sel.blocked = Some(dir);
            }
        }

        self.selector = sel;
        self.play_sfx(SfxId::Select);
        // Any tip move that lands → `reset_ghosts` (`selector.lua:125/135`).
        self.reset_ghosts();
        let (sx, sy) = self.selector.step_dir();
        self.detect_kills(sx, sy);
        // Castle reverse room: enemies hop toward the player while aiming
        // (`selector:moveByTile` → `step_reverseMen` → `detectkills` again).
        if self.reverse_room {
            self.step_reverse_men();
            self.detect_kills(sx, sy);
        }
    }

    /// Port of `selector:detectkills(dx, dy)`.
    ///
    /// Kill cases (Lua):
    /// 1. **Passing slash / doubleslash** — while zipping along an axis (`|extent| > 1`),
    ///    an enemy on a cell *adjacent* to a path cell (not on the path) is slashed.
    ///    Living Spirits **are** included here (`alive and not inanimate` only).
    /// 2. **Start-adjacent slash** — enemy already orthogonally adjacent to the *player
    ///    start* on the perpendicular axis is marked slash for every step. Spirits are
    ///    **excluded** (`etype ~= kSpirit`) — that is the only Spirit slash skip.
    /// 3. **Stab** — living enemy on the cell one step *beyond* the tip
    ///    (`player + extent + dx/dy`) while `canmove` (selector not on a corpse).
    ///    Spirits included (tip dispel).
    /// 4. Path is **trimmed** if a living enemy sits *on* a future path cell
    ///    (cannot land on / pass through living enemies); Spirits trim too.
    fn detect_kills(&mut self, dx: i32, dy: i32) {
        let mut plan = AimPlan::default();
        let mut sel = self.selector;

        if sel.extent_x == 0 && sel.extent_y == 0 {
            self.aim_plan = plan;
            return;
        }

        let px = self.player_x;
        let py = self.player_y;

        // Lua `for step = dx, self.extent_*, dx` evaluates the end once; we snapshot it.
        if dx != 0 {
            let end = sel.extent_x;
            let mut step = dx;
            loop {
                let past_end = if dx > 0 { step > end } else { step < end };
                if past_end {
                    break;
                }
                plan.segments.push(PathSegment::Walk);
                let mut kills_this_step = 0i32;

                if sel.extent_x.abs() > 1 {
                    kills_this_step = 0;
                    let this_x = px + step;
                    let second_x = this_x - dx;
                    let mut trim_to: Option<i32> = None;

                    for (i, en) in self.room.enemies.iter().enumerate() {
                        // Passing slash + trim: Spirits included (`selector.lua` 276–295).
                        if !Self::enemy_combatant(en) {
                            continue;
                        }
                        // Enemy on the zip lane → trim extent to the cell before them.
                        if en.x == this_x && en.y == py {
                            trim_to = Some(step - dx);
                            break;
                        }
                        // Adjacent to the previous path cell (side slash / pass-in-front).
                        if (sel.tile_y - en.y).abs() == 1
                            && en.x == second_x
                            && step.abs() > 1
                        {
                            kills_this_step += 1;
                            Self::mark_slash(&mut plan, i, kills_this_step);
                        }
                    }

                    if let Some(new_ex) = trim_to {
                        sel.extent_x = new_ex;
                        sel.tile_x = px + sel.extent_x;
                        if step.abs() >= sel.extent_x.abs() {
                            break;
                        }
                    }
                }

                // Enemies beside the player's start on the perpendicular axis.
                // Spirits excluded (`etype ~= kSpirit`, `selector.lua` 302).
                for (i, en) in self.room.enemies.iter().enumerate() {
                    if !Self::enemy_start_adjacent_slashable(en) {
                        continue;
                    }
                    if (en.x - px).abs() == 0 && (en.y - py).abs() == 1 {
                        kills_this_step += 1;
                        Self::mark_slash(&mut plan, i, kills_this_step);
                    }
                }

                step += dx;
                if sel.extent_x == 0 {
                    break;
                }
            }
        } else if dy != 0 {
            let end = sel.extent_y;
            let mut step = dy;
            loop {
                let past_end = if dy > 0 { step > end } else { step < end };
                if past_end {
                    break;
                }
                plan.segments.push(PathSegment::Walk);
                let mut kills_this_step = 0i32;

                if sel.extent_y.abs() > 1 {
                    kills_this_step = 0;
                    let this_y = py + step;
                    let second_y = this_y - dy;
                    let mut trim_to: Option<i32> = None;

                    for (i, en) in self.room.enemies.iter().enumerate() {
                        // Passing slash + trim: Spirits included (`selector.lua` 322–340).
                        if !Self::enemy_combatant(en) {
                            continue;
                        }
                        if en.y == this_y && en.x == px {
                            trim_to = Some(step - dy);
                            break;
                        }
                        if (sel.tile_x - en.x).abs() == 1
                            && en.y == second_y
                            && step.abs() > 1
                        {
                            kills_this_step += 1;
                            Self::mark_slash(&mut plan, i, kills_this_step);
                        }
                    }

                    if let Some(new_ey) = trim_to {
                        sel.extent_y = new_ey;
                        sel.tile_y = py + sel.extent_y;
                        if step.abs() >= sel.extent_y.abs() {
                            break;
                        }
                    }
                }

                // Spirits excluded (`etype ~= kSpirit`, `selector.lua` 348).
                for (i, en) in self.room.enemies.iter().enumerate() {
                    if !Self::enemy_start_adjacent_slashable(en) {
                        continue;
                    }
                    if (en.x - px).abs() == 1 && (en.y - py).abs() == 0 {
                        kills_this_step += 1;
                        Self::mark_slash(&mut plan, i, kills_this_step);
                    }
                }

                step += dy;
                if sel.extent_y == 0 {
                    break;
                }
            }
        }

        // Drop walk/slash segments past the (possibly trimmed) extent.
        let keep = sel.extent_x.abs().max(sel.extent_y.abs()) as usize;
        if plan.segments.len() > keep {
            plan.segments.truncate(keep);
        }

        // Stab: enemy one cell beyond the tip (Spirits included — tip dispel),
        // only while `canmove` (selector tip not sitting on a corpse).
        let next_x = px + sel.extent_x + dx;
        let next_y = py + sel.extent_y + dy;
        if sel.canmove {
            for (i, en) in self.room.enemies.iter().enumerate() {
                if !Self::enemy_combatant(en) {
                    continue;
                }
                if en.x == next_x && en.y == next_y {
                    plan.segments.push(PathSegment::Stab);
                    plan.deathlist.push(i);
                }
            }
        }

        // `movebar:addSegment` inserts a bonus leading "walk" on the first add
        // (`if #segmentList == 1 then insert walk`). Mirror that so the readybar
        // pip count / enemy budget match Lua (`#segmentList == extent + 1`).
        if !plan.segments.is_empty() {
            plan.segments.insert(0, PathSegment::Walk);
        }

        if sel.extent_x == 0 && sel.extent_y == 0 {
            plan = AimPlan::default();
        }

        self.selector = sel;
        self.aim_plan = plan;
    }

    /// Living combatant (`alive and not inanimate`) — includes Spirits.
    fn enemy_combatant(en: &crate::level::Enemy) -> bool {
        en.alive && !en.kind.is_inanimate()
    }

    /// Start-adjacent slash loop only (`etype ~= kSpirit`).
    fn enemy_start_adjacent_slashable(en: &crate::level::Enemy) -> bool {
        Self::enemy_combatant(en) && !matches!(en.kind, crate::level::EnemyKind::Spirit)
    }

    /// `enemy:braintwo` inanimate + `chester.hasKey` branch — slide castle gates
    /// aside when the player stands on the cell the gate faces.
    fn tick_entrance_gates(&mut self) {
        if !self.has_key {
            return;
        }
        let px = self.player_x;
        let py = self.player_y;
        let n = self.room.enemies.len();
        for i in 0..n {
            let en = self.room.enemies[i];
            if !en.alive || !en.kind.is_inanimate() {
                continue;
            }
            let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
            let (mut tx, mut ty) = (en.x, en.y);
            let (mut ox, mut oy) = (en.x, en.y);
            match facing {
                Facing::East => {
                    tx += 1;
                    if matches!(en.kind, crate::level::EnemyKind::LeftEastEntrance) {
                        oy += 1;
                    } else {
                        oy -= 1;
                    }
                }
                Facing::South => {
                    ty += 1;
                    if matches!(en.kind, crate::level::EnemyKind::LeftSouthEntrance) {
                        ox -= 1;
                    } else {
                        ox += 1;
                    }
                }
                Facing::North | Facing::West => {}
            }
            if px == tx && py == ty && (ox, oy) != (en.x, en.y) {
                self.room.enemies[i].x = ox;
                self.room.enemies[i].y = oy;
                self.play_sfx(SfxId::Clunk);
            }
        }
    }

    fn mark_slash(plan: &mut AimPlan, enemy_i: usize, kills_this_step: i32) {
        let movetype = if kills_this_step >= 2 {
            PathSegment::DoubleSlash
        } else {
            PathSegment::Slash
        };
        plan.deathlist.push(enemy_i);
        if let Some(last) = plan.segments.last_mut() {
            *last = movetype;
        }
    }

    fn commit_zip(&mut self) {
        let cx = self.selector.tile_x;
        let cy = self.selector.tile_y;
        let path = path_cells(self.player_x, self.player_y, cx, cy);
        if path.is_empty() {
            return;
        }

        // Commit clears ghost preview (`clearmovelist` / leave kSelectorMove).
        self.reset_ghosts();
        self.crank_accum = 0.0;

        let facing = Facing::from_delta(cx - self.player_x, cy - self.player_y)
            .unwrap_or(self.facing);
        self.facing = facing;

        // Readybar length drives enemy steps (`step_enemies` removes one segment
        // per tick). Includes trailing `stab` — so a tip+stab zip gives one more
        // enemy step than the walk distance alone.
        let enemy_budget = self.aim_plan.segments.len().max(1) as i32;

        // Kill only what `detectkills` put on the deathlist (slash / stab).
        // Deduplicate indices — Lua can insert the same enemy multiple times.
        let mut kill_set = self.aim_plan.deathlist.clone();
        kill_set.sort_unstable();
        kill_set.dedup();

        let has_slash = self.aim_plan.segments.iter().any(|s| s.is_slash());
        let has_stab = self
            .aim_plan
            .segments
            .last()
            .is_some_and(|s| *s == PathSegment::Stab);

        // Lua `moveToTile` → `playerMoved(..., math.max(absx, absy), type)` — for a
        // cardinal zip that equals path length (start exclusive, tip inclusive).
        let numsteps = path.len() as i32;

        // Slash path: defer kill+land until after `preslash` wind-up
        // (`main.lua` kPlayerMove: ticks==1 face+preslash, ticks>3 kill+moveToTile).
        // Stab-only / walk still resolve immediately this frame.
        if has_slash && !kill_set.is_empty() {
            self.pending_slash = Some(PendingSlashMove {
                ticks: 0,
                tip_x: cx,
                tip_y: cy,
                facing,
                numsteps,
                enemy_budget,
                has_stab,
                kill_indices: kill_set,
            });
            // Aim icons clear when readybar empties on resolve; keep aim_plan until
            // then so the readybar still shows the committed slash during wind-up.
            return;
        }

        self.finish_zip_resolve(cx, cy, facing, numsteps, enemy_budget, has_slash, has_stab, kill_set);
    }

    /// Advance deferred slash `kPlayerMove` one 20 Hz tick (`main.lua` ticks++).
    fn tick_pending_slash(&mut self) {
        let Some(pending) = self.pending_slash.as_mut() else {
            return;
        };
        pending.ticks += 1;
        let t = pending.ticks;
        if t == 1 {
            // `player:preslash(cursor)` — face tip, play wind-up, stay on start tile.
            self.facing = pending.facing;
            self.player_anim = Some(SpriteAnim::new(PLAYER_PRESLASH));
            return;
        }
        if t <= 3 {
            return;
        }
        // ticks > 3: kill deathlist, then moveToTile+smoke, then kEnemyMove.
        let pending = self.pending_slash.take().expect("pending slash");
        self.finish_zip_resolve(
            pending.tip_x,
            pending.tip_y,
            pending.facing,
            pending.numsteps,
            pending.enemy_budget,
            true,
            pending.has_stab,
            pending.kill_indices,
        );
    }

    /// Run `enemy:kill` for each deathlist index (ninja dodge / twinstep parry / die).
    /// Caller chooses whether this runs before or after tip land (Lua slash vs stab-only).
    fn apply_deathlist_kills(
        &mut self,
        tip_x: i32,
        tip_y: i32,
        kill_set: Vec<usize>,
    ) -> DeathlistOutcome {
        let mut kill_snapshots = Vec::new();
        let mut deathlist_indices = Vec::new();
        let mut kills = 0i32;
        let mut any_attack = false;
        for (kill_i, i) in kill_set.into_iter().enumerate() {
            let Some(en) = self.room.enemies.get(i).copied() else {
                continue;
            };
            if !en.alive {
                continue;
            }
            any_attack = true;
            deathlist_indices.push(i);
            let kill_dir = self.kill_dir_for_enemy(en.x, en.y, tip_x, tip_y);
            if self.try_ninja_dodge(i, kill_dir) {
                continue;
            }
            if self.try_twinstep_parry(i, kill_dir) {
                if self.death_hold_ticks.is_some() {
                    self.sync_globals_from_room();
                    return DeathlistOutcome {
                        starved: true,
                        kills,
                        any_attack,
                        kill_snapshots,
                        deathlist_indices,
                    };
                }
                continue;
            }
            if let Some(en) = self.room.enemies.get_mut(i) {
                kill_snapshots.push((
                    kill_i,
                    en.x,
                    en.y,
                    Facing::from_u8(en.facing).unwrap_or(Facing::South),
                    en.kind,
                    *en,
                ));
                en.alive = false;
                kills += 1;
            }
        }
        for (_, x, y, _, kind, _) in &kill_snapshots {
            self.mark_dead_enemy_tile(*x, *y, *kind);
        }
        for (_, _, _, _, _, snapshot) in &kill_snapshots {
            self.mark_global_dead(snapshot);
        }
        self.sync_globals_from_room();
        self.bump_score_for_kills(kills);
        if kills > 0 {
            self.killed_people = true;
        }
        let king_kills = kill_snapshots
            .iter()
            .filter(|(_, _, _, _, kind, _)| matches!(kind, crate::level::EnemyKind::King))
            .count() as i32;
        if king_kills > 0 {
            self.life = (self.life + 20 * king_kills).min(MAX_BLOOD);
        }
        DeathlistOutcome {
            starved: false,
            kills,
            any_attack,
            kill_snapshots,
            deathlist_indices,
        }
    }

    /// Tip land (`samurai:moveToTile`) + optional floorsmoke. Does not spend blood.
    fn land_player_on_tip(&mut self, tip_x: i32, tip_y: i32, facing: Facing, numsteps: i32) {
        self.stamp_trail_blood(self.player_x, self.player_y);
        self.player_x = tip_x;
        self.player_y = tip_y;
        self.selector = Selector::at_player(self.player_x, self.player_y);
        self.aim_plan = AimPlan::default();
        if numsteps > 1 {
            self.floorsmoke
                .do_smoke(facing, numsteps, self.player_x, self.player_y);
        }
    }

    /// Kill / dodge FX after a deathlist pass that produced real kills (or still
    /// play the attack anim for a deathlist of only dodges/parries).
    fn play_zip_attack_fx(
        &mut self,
        tip_x: i32,
        tip_y: i32,
        has_slash: bool,
        has_stab: bool,
        kill_snapshots: &[(usize, i32, i32, Facing, crate::level::EnemyKind, crate::level::Enemy)],
        deathlist_indices: &[usize],
        set_player_anim: bool,
    ) {
        if set_player_anim {
            let poses = if has_slash && has_stab {
                PLAYER_SLASH_AND_STAB
            } else if has_stab {
                PLAYER_STAB
            } else {
                PLAYER_SLASH
            };
            self.player_anim = Some(SpriteAnim::new(poses));
            self.play_sfx(SfxId::Slash);
        }
        for (kill_i, x, y, en_facing, kind, _) in kill_snapshots {
            let kill_dir = self.kill_dir_for_enemy(*x, *y, tip_x, tip_y);
            let is_spirit = matches!(kind, crate::level::EnemyKind::Spirit);
            let slashed = if matches!(kind, crate::level::EnemyKind::King) {
                king_slashed_variant(*kill_i)
            } else if is_spirit {
                spirit_slashed_variant(*kill_i)
            } else {
                enemy_slashed_variant(*kill_i)
            };
            let fx_facing = if is_spirit {
                Facing::South
            } else {
                *en_facing
            };
            self.kill_fx.push(KillFx {
                x: *x,
                y: *y,
                facing: fx_facing,
                anim: SpriteAnim::new(slashed),
                kind: *kind,
            });
            if is_spirit {
                self.play_sfx(SfxId::SpiritDispel);
            }
            let yell = SfxId::falldead_variant(self.next_fx_roll());
            self.play_sfx(yell);
            if !is_spirit {
                self.spawn_enemy_bleed(*x, *y, kill_dir, *kill_i, deathlist_indices);
            }
        }
    }

    /// Apply land + kill FX + floorsmoke + enemy phase (shared by walk/stab and
    /// delayed slash resolve). Callers pass tip = commit cursor cell.
    ///
    /// Order matches `main.lua` kPlayerMove:
    /// - **Slash** (any slash segment): `enemy:kill` then `moveToTile(tip)`.
    /// - **Stab-only**: `moveToTile(tip)` first, then `enemy:kill`. Frontal
    ///   twinstep parry knockback therefore sticks one tile behind the tip
    ///   (Lua `samurai:knockback` → `ex+2*facing` from the twin).
    fn finish_zip_resolve(
        &mut self,
        tip_x: i32,
        tip_y: i32,
        facing: Facing,
        numsteps: i32,
        enemy_budget: i32,
        has_slash: bool,
        has_stab: bool,
        kill_set: Vec<usize>,
    ) {
        self.facing = facing;

        // Stab-only frontal approach (2–3 tiles into a twin) is walk segs + trailing
        // stab — no slash. Lua: tip land at ticks==1, kill at ticks>5.
        let stab_only = has_stab && !has_slash && !kill_set.is_empty();

        let kill_snapshots;

        if stab_only {
            // Lua ticks==1: moveToTile(tip, "stab") before any enemy:kill.
            self.player_anim = Some(SpriteAnim::new(PLAYER_STAB));
            self.play_sfx(SfxId::Slash);
            self.land_player_on_tip(tip_x, tip_y, facing, numsteps);
            let outcome = self.apply_deathlist_kills(tip_x, tip_y, kill_set);
            if outcome.starved {
                return;
            }
            // Real kills after tip land still need bleed / slashed FX. Parry-only
            // keeps the stab anim (knockback may have overwritten with walk).
            if outcome.kills > 0 {
                self.play_zip_attack_fx(
                    tip_x,
                    tip_y,
                    has_slash,
                    has_stab,
                    &outcome.kill_snapshots,
                    &outcome.deathlist_indices,
                    false, // stab anim + slash SFX already played above
                );
            }
            kill_snapshots = outcome.kill_snapshots;
        } else {
            let outcome = self.apply_deathlist_kills(tip_x, tip_y, kill_set);
            if outcome.starved {
                return;
            }
            if outcome.any_attack {
                self.play_zip_attack_fx(
                    tip_x,
                    tip_y,
                    has_slash,
                    has_stab,
                    &outcome.kill_snapshots,
                    &outcome.deathlist_indices,
                    true,
                );
            } else if numsteps <= 0 {
                self.player_anim =
                    Some(SpriteAnim::with_rate(PLAYER_ZSTEP, ANIM_TICKS_PER_POSE_5FPS));
            } else if numsteps < 5 {
                self.player_anim =
                    Some(SpriteAnim::with_rate(PLAYER_STEP, ANIM_TICKS_PER_POSE_5FPS));
                self.play_sfx(SfxId::Step);
            } else {
                self.player_anim = Some(SpriteAnim::new(PLAYER_FASTSTEP));
                self.play_sfx(SfxId::Swoosh);
            }
            self.land_player_on_tip(tip_x, tip_y, facing, numsteps);
            // `samurai:step` — only walk (and teleport) spend 1 blood.
            if !outcome.any_attack && !self.invulnerable {
                self.life = (self.life - 1).max(0);
                if self.life <= 0 {
                    self.begin_player_death(0, self.facing, KillType::Starve);
                    return;
                }
            }
            kill_snapshots = outcome.kill_snapshots;
        }

        // `chester:checkChest` — step onto the chest tile grants the key.
        // Uses the tile the player currently occupies (after tip land / knockback).
        if !self.has_key {
            if let Some((cx, cy)) = self.chest_pos {
                if self.player_x == cx && self.player_y == cy {
                    self.has_key = true;
                    self.play_sfx(SfxId::Key);
                }
            }
        }

        // NPC pad (GID 253): first visit shows dialog + heal (`samurai:playerMoved`).
        if matches!(
            self.world.terrain_at(self.player_x, self.player_y),
            Terrain::Npc { .. }
        ) {
            if let Some(npc) = self.world.npc_at(self.player_x, self.player_y).cloned() {
                if !self.exit_visited(npc.id) {
                    if !npc.dialog.is_empty() {
                        self.show_dialog(&npc.dialog, npc.face, 0.0);
                        if npc.heal != 0 {
                            self.life = (self.life + npc.heal).min(MAX_BLOOD);
                        }
                    }
                }
                self.mark_exit_visited(npc.id);
            }
        }

        // Door: arm `onexit` only (`samurai:playerMoved`). Actual `exitroom` waits
        // until the deferred enemy phase ends + move/kill anims finish.
        if self.world.terrain_at(self.player_x, self.player_y).is_door() {
            if let Some(ex) = self.world.exit_at(self.player_x, self.player_y).cloned() {
                self.reverse_room = ex.reverse;
                if !self.exit_visited(ex.id) && !ex.dialog.is_empty() {
                    self.pending_room_dialog = Some((ex.dialog.clone(), ex.face));
                }
                self.mark_exit_visited(ex.id);
                if !ex.teleports.is_empty() {
                    let pick = (self.next_fx_roll() as usize) % ex.teleports.len();
                    let tid = ex.teleports[pick];
                    if let Some(dest) = self.world.exit_by_id(tid) {
                        let camera = self.world.exit_camera_for(dest, self.facing);
                        self.pending_exit = Some(PendingExit {
                            id: tid,
                            camera,
                            teleport: true,
                            to_game_over: false,
                        });
                    }
                } else {
                    let camera = self.world.exit_camera_for(&ex, self.facing);
                    let to_game_over = ex.entrance == EXIT_TO_GAME_OVER
                        && matches!(self.facing, Facing::North | Facing::East);
                    self.pending_exit = Some(PendingExit {
                        id: ex.id,
                        camera,
                        teleport: false,
                        to_game_over,
                    });
                }
            }
        }

        self.ensure_player_in_room();

        // First-kill-per-type dialog (`enemy:kill` → `*Killed`).
        for (_, _, _, _, kind, _) in &kill_snapshots {
            let et = kind.etype();
            if self.dialog.enemies_killed(et) {
                continue;
            }
            self.dialog.mark_killed(et);
            let key = match et {
                1 => "swordsmanKilled",
                2 => "pikemanKilled",
                3 => "ninjaKilled",
                4 => "guardKilled",
                5 => "kingKilled",
                6 => "spiritKilled",
                _ => continue,
            };
            self.show_dialog(key, et as i32, 0.0);
        }

        self.pending_enemy = Some(PendingEnemyMove {
            steps_left: enemy_budget.max(0),
        });
    }

    /// Advance deferred `kEnemyMove` one 20 Hz tick (`main.lua` movePhase).
    fn tick_pending_enemy(&mut self) {
        let Some(pending) = self.pending_enemy.as_mut() else {
            return;
        };
        // Pause only while in dialog *state* (Lua `kGameDialogState`). Close-anim
        // chrome after hide must not stall enemy ticks (`dialog.is_open` stays true).
        if self.state == GameState::Dialog {
            return;
        }
        if pending.steps_left > 0 {
            // `step_enemies` (`pathfinding.lua:131–165`):
            // - no living locals → `removeSegment` twice, no pathfinding
            // - otherwise → one `pf` pass + one `removeSegment`
            let living = self.room.enemies.iter().filter(|e| e.alive).count();
            if living == 0 {
                if let Some(p) = self.pending_enemy.as_mut() {
                    p.steps_left = (p.steps_left - 2).max(0);
                }
                return;
            }
            self.resolve_enemies(1);
            self.sync_globals_from_room();
            if let Some(p) = self.pending_enemy.as_mut() {
                p.steps_left -= 1;
            }
            return;
        }
        // `#readybar.segmentList == 0` → `braintwo` for each local, then selector.
        self.pending_enemy = None;
        self.sync_globals_from_room();
        if let Some((idx, kill_dir, kill_type)) = self.find_player_killer() {
            if self.invulnerable {
                self.message = Some((self.cheat_invuln_label().into(), 0.8));
            } else {
                let (etype, is_ninja) = self
                    .room
                    .enemies
                    .get(idx)
                    .map(|e| {
                        (
                            e.kind.etype() as i32,
                            matches!(e.kind, crate::level::EnemyKind::Ninja),
                        )
                    })
                    .unwrap_or((1, false));
                self.play_enemy_stab(idx, kill_dir, kill_type);
                if is_ninja {
                    // Lua: shuriken oneshot + addMotion; kill in animcallback.
                    // Dialog + shuriken SFX fire immediately; death waits for land.
                    self.spawn_shuriken_throw(idx, kill_dir, kill_type, etype);
                    self.show_dialog("playerKilled", etype, 1.3);
                } else {
                    self.begin_player_death(etype, kill_dir, kill_type);
                }
                return;
            }
        }
        // `updateSpiritsAndDoors(killedPeople)` then clear the flag (`main.lua:692–693`).
        let didkill = self.killed_people;
        self.update_spirits_and_doors(didkill);
        self.killed_people = false;
        self.sync_globals_from_room();
        // Armed door exit waits until anims finish (checked in the main tick loop).
        if self.pending_exit.is_some() && self.exit_anims_done() {
            self.begin_room_transition();
        }
    }

    /// Reload the cell window if the player left it. Keeps `self.camera` fixed.
    /// Outdoor streaming does **not** re-roll weight spawns — only attaches
    /// already-created globals that sit in the new window (Lua never calls
    /// `findlocalenemies` mid-room; that only happens in `initbackground`).
    fn ensure_player_in_room(&mut self) {
        let lx = self.player_x - self.room.origin_x;
        let ly = self.player_y - self.room.origin_y;
        if lx >= 0 && ly >= 0 && lx < self.room.width && ly < self.room.height {
            return;
        }
        // Prefer keeping the draw camera; enlarge the data window around it.
        self.reload_room_terrain(self.camera, 24, 24);
        let lx = self.player_x - self.room.origin_x;
        let ly = self.player_y - self.room.origin_y;
        if lx >= 0 && ly >= 0 && lx < self.room.width && ly < self.room.height {
            return;
        }
        // Still outside (far zip): stream around the player; camera unchanged.
        self.reload_room_terrain((self.player_x, self.player_y), 16, 16);
    }

    /// Port of the attack half of `enemy:braintwo` after the move phase.
    /// Returns `(enemy_index, kill_dir, kill_type)` for the first killer, if any.
    /// Melee types kill when manhattan == 1; ninja also when aligned and facing you.
    fn find_player_killer(&mut self) -> Option<(usize, Facing, KillType)> {
        use crate::level::EnemyKind;
        // Gate open check runs in the same `braintwo` pass as combat (before attacks).
        self.tick_entrance_gates();
        let px = self.player_x;
        let py = self.player_y;
        let n = self.room.enemies.len();
        for i in 0..n {
            if !self.room.enemies[i].alive {
                continue;
            }
            if self.room.enemies[i].kind.is_inanimate() {
                continue;
            }
            // `enemy:braintwo` dodge recovery — before stun / attack.
            // Phase of the dodge: clear `justdodged` only (ninja keeps `dodgestate`).
            // Next phase: clear `dodgestate` and skip move/attack ("stand back up").
            // Twinstep clears `dodgestate` on the justdodged tick; unluac omits a
            // `return` after idle, but 1.10 does not counter-stab that same phase
            // (frontal parry → clang / knockback, player lives). Skip attack here.
            // TODO(parity): matches enemy.lua:braintwo justdodged twinstep — return
            // after idle so manhattan stab does not run this pass.
            if self.room.enemies[i].just_dodged {
                self.room.enemies[i].just_dodged = false;
                if matches!(self.room.enemies[i].kind, EnemyKind::Twinstep) {
                    self.room.enemies[i].dodge_state = false;
                    continue;
                }
            } else if self.room.enemies[i].dodge_state {
                self.room.enemies[i].dodge_state = false;
                continue;
            }
            // `enemy:braintwo`: if stunned > 0, decrement and skip this turn.
            if self.room.enemies[i].stunned > 0 {
                self.room.enemies[i].stunned -= 1;
                continue;
            }
            let kind = self.room.enemies[i].kind;
            let (ex, ey) = (self.room.enemies[i].x, self.room.enemies[i].y);
            let facing_u8 = self.room.enemies[i].facing;
            let pike_up = self.room.enemies[i].pike_up;
            let (cx, cy) = (
                self.room.enemies[i].child_x,
                self.room.enemies[i].child_y,
            );
            let dx = (ex - px).abs();
            let dy = (ey - py).abs();
            let manhattan = dx + dy;
            match kind {
                EnemyKind::Swordsman
                | EnemyKind::King
                | EnemyKind::Twinstep
                | EnemyKind::Reverse => {
                    if manhattan == 1 {
                        let dir = kill_dir_from_to(ex, ey, px, py);
                        return Some((i, dir, KillType::Slash));
                    }
                }
                EnemyKind::Pikeman => {
                    // `enemy:braintwo` kPikeman: if raised and cell ahead free → lower.
                    // Kill from tip adjacency when lowered and dir == facing.
                    let facing = Facing::from_u8(facing_u8).unwrap_or(Facing::South);
                    let mut tip_x = cx;
                    let mut tip_y = cy;
                    let mut lowered = !pike_up;
                    if pike_up {
                        let (ox, oy) = facing.offsets();
                        let ahead_x = ex + ox;
                        let ahead_y = ey + oy;
                        if !self.enemy_step_blocked(i, ahead_x, ahead_y) {
                            self.room.enemies[i].pike_up = false;
                            self.room.enemies[i].sync_pike_tip();
                            tip_x = self.room.enemies[i].child_x;
                            tip_y = self.room.enemies[i].child_y;
                            lowered = true;
                        }
                    }
                    if lowered {
                        let tip_manh = (tip_x - px).abs() + (tip_y - py).abs();
                        if tip_manh == 1 {
                            let dir = kill_dir_from_to(tip_x, tip_y, px, py);
                            if Facing::from_u8(facing_u8) == Some(dir) {
                                return Some((i, dir, KillType::Pierce));
                            }
                        }
                    }
                }
                EnemyKind::Ninja => {
                    let dir = kill_dir_from_to(ex, ey, px, py);
                    if manhattan == 1 {
                        return Some((i, dir, KillType::Pierce));
                    }
                    // Aligned on a row/col and facing the player → shuriken / pierce
                    // (`enemy:braintwo` + `checkBlockedShuriken`).
                    if (dx == 0 || dy == 0)
                        && manhattan > 1
                        && Facing::from_u8(facing_u8) == Some(dir)
                        && !self.shuriken_blocked(ex, ey, px, py)
                    {
                        return Some((i, dir, KillType::Pierce));
                    }
                }
                EnemyKind::Spirit
                | EnemyKind::LeftSouthEntrance
                | EnemyKind::RightSouthEntrance
                | EnemyKind::LeftEastEntrance
                | EnemyKind::RightEastEntrance => {}
            }
        }
        None
    }

    /// `enemy:stab` — face the player, play stab anim, queue slash/shuriken SFX.
    fn play_enemy_stab(&mut self, idx: usize, _kill_dir: Facing, _kill_type: KillType) {
        use crate::level::EnemyKind;
        let Some(e) = self.room.enemies.get_mut(idx) else {
            return;
        };
        let kind = e.kind;
        // Swordsman/king/twinstep face the player; pikeman keeps facing (tip aim).
        let face = if matches!(kind, EnemyKind::Pikeman) {
            Facing::from_u8(e.facing).unwrap_or(Facing::South)
        } else {
            let face = face_player(e.x, e.y, self.player_x, self.player_y);
            e.facing = face as u8;
            e.sync_pike_tip();
            face
        };
        let poses = if matches!(kind, EnemyKind::Ninja) {
            NINJA_STAB
        } else if matches!(kind, EnemyKind::King) {
            // No `stab` anim on kingTable — stay within poses 1–4.
            KING_STAB
        } else {
            ENEMY_STAB
        };
        let (x, y) = (e.x, e.y);
        self.enemy_attack_fx.push(EnemyAttackFx {
            x,
            y,
            facing: face,
            anim: SpriteAnim::new(poses),
            kind,
        });
        // Lua: ninja `slashSound = soundm.shuriken`; others `soundm.slash`.
        if matches!(kind, EnemyKind::Ninja) {
            self.play_sfx(SfxId::Shuriken);
        } else {
            self.play_sfx(SfxId::Slash);
        }
    }

    /// Spawn flying `Images/shuriken` toward the player (`enemy:stab` ninja branch).
    fn spawn_shuriken_throw(
        &mut self,
        idx: usize,
        kill_dir: Facing,
        kill_type: KillType,
        killer_etype: i32,
    ) {
        let Some(e) = self.room.enemies.get(idx) else {
            return;
        };
        self.shuriken_fx.push(ShurikenFx {
            tile_x: e.x,
            tile_y: e.y,
            target_x: self.player_x,
            target_y: self.player_y,
            steps_per_frame: 1,
            anim: SpriteAnim::with_rate(SHURIKEN_SPIN, ANIM_TICKS_PER_POSE),
            kill_dir,
            kill_type,
            killer_etype,
        });
    }

    /// Refresh `path_roomtiles` from the room's frozen `room_fence` + live
    /// overlays / bodies. Mirrors Lua `roomtiles ~= nil` after `cullroomtiles`
    /// (doors stay; far floors past doors are dropped — enemy path fence).
    ///
    /// The base fence is captured at room populate; it is intentionally **not**
    /// recomputed from the player's live tile. Recomputing let a player standing
    /// on an exit re-open the far side of a door (`cullroomtiles` seeds its BFS
    /// from the player), letting enemies path into the next room.
    fn refresh_path_roomtiles(&mut self) {
        let mut tiles = self.room_fence.clone();
        for e in &self.room.enemies {
            if e.kind.is_inanimate() {
                continue;
            }
            tiles.insert((e.x, e.y));
            if matches!(e.kind, crate::level::EnemyKind::Pikeman) {
                tiles.insert((e.child_x, e.child_y));
            }
        }
        for &(x, y) in self.roomtile_overrides.keys() {
            tiles.insert((x, y));
        }
        self.path_roomtiles = tiles;
    }

    /// `construct_graphs` — rebuild `maingraph` weights for the current camera /
    /// locals (`pathfinding.lua`). Call after room populate / camera change.
    fn construct_graphs(&mut self) {
        use crate::pathfinding::{reset_graph_weights, RoomTables};
        self.room_tables = RoomTables::for_camera(self.camera);
        self.sync_enemy_ghosts();
        // Collect corpse tiles first (need owned xy before borrowing enemies for ctx).
        let corpses: Vec<(i32, i32, crate::level::EnemyKind)> = self
            .room
            .enemies
            .iter()
            .filter(|e| !e.alive && !e.kind.is_inanimate())
            .map(|e| (e.x, e.y, e.kind))
            .collect();
        // `construct_graphs`: every dead local → `deadenemies=1`, `roomtiles=1`.
        // Dead Spirits are excluded: in Lua `construct_graphs` runs only at room
        // init, right after `findlocalenemies` revives every Spirit
        // (`utils.lua:206-207`), so a dead Spirit is never swept here. The port
        // reuses this as a per-tick graph rebuild, so a killed Spirit must keep
        // its hard-block (`deadenemies=2`, `enemy.lua:765`).
        for &(x, y, kind) in &corpses {
            if matches!(kind, crate::level::EnemyKind::Spirit) {
                continue;
            }
            self.deadenemies.insert((x, y), 1);
            self.roomtile_overrides.insert((x, y), 1);
        }
        self.refresh_path_roomtiles();
        let ghost_xy = self.ghost_xy_snapshot();
        let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: false,
            ghost_xy: &ghost_xy,
            deathlist: &[],
            roomtiles: &self.path_roomtiles,
        };
        let has_living = self.room.enemies.iter().any(|e| e.alive);
        if has_living {
            reset_graph_weights(&mut self.main_graph, &self.room_tables, &ctx);
        }
        // Mark corpse tiles disconnected even when no living enemies remain.
        for &(x, y, _) in &corpses {
            crate::pathfinding::reset_graph_xy(
                &mut self.main_graph,
                &self.room_tables,
                &ctx,
                x,
                y,
            );
        }
    }

    /// `step_enemies` × budget: one Lua `pf(..., false)` step per readybar
    /// segment per eligible local. Replaces the old greedy approach so paths
    /// match the original (and v0.15 crank ghosts will share this graph).
    fn resolve_enemies(&mut self, budget: i32) {
        if budget <= 0 {
            return;
        }
        // Ensure graph matches current camera / blockers before pathing.
        self.construct_graphs();
        for _ in 0..budget {
            self.step_enemies_once();
        }
    }

    /// One `step_enemies` pass (`pathfinding.lua:131–165`) without readybar pop
    /// (port burns segments via `PendingEnemyMove`).
    fn step_enemies_once(&mut self) {
        use crate::level::EnemyKind;
        use crate::pathfinding::xyf_manhattan;

        // Lua `step_enemies`: when `reverseRoom`, skip all pathfinding (mid-aim
        // hops already happened); segment burn still occurs in `tick_pending_enemy`.
        if self.reverse_room {
            return;
        }

        let n = self.room.enemies.len();
        if n == 0 {
            return;
        }
        // `sort_local_enemies_by_distance` — farthest first (Lua `>` sort).
        let mut order: Vec<usize> = (0..n).collect();
        let px = self.player_x;
        let py = self.player_y;
        let pfacing = self.facing;
        order.sort_by(|&a, &b| {
            let ea = &self.room.enemies[a];
            let eb = &self.room.enemies[b];
            let fa = Facing::from_u8(ea.facing).unwrap_or(Facing::South);
            let fb = Facing::from_u8(eb.facing).unwrap_or(Facing::South);
            let da = xyf_manhattan(ea.x, ea.y, fa, px, py, pfacing);
            let db = xyf_manhattan(eb.x, eb.y, fb, px, py, pfacing);
            db.cmp(&da)
        });

        for i in order {
            if !self.room.enemies[i].alive {
                continue;
            }
            if self.room.enemies[i].stunned > 0 || self.room.enemies[i].dodge_state {
                continue;
            }
            let kind = self.room.enemies[i].kind;
            if kind.is_inanimate() || matches!(kind, EnemyKind::Spirit) {
                continue;
            }
            if matches!(kind, EnemyKind::King) {
                self.room.enemies[i].facing = Facing::South as u8;
                continue;
            }
            self.pf_enemy_step(i);
        }
    }

    /// `step_enemies_single` (`pathfinding.lua:167–184`) — one hop each living
    /// unstunned local, no readybar pop. Used by reverse-room mid-aim chase.
    fn step_enemies_single(&mut self) {
        use crate::level::EnemyKind;
        use crate::pathfinding::xyf_manhattan;

        let n = self.room.enemies.len();
        if n == 0 {
            return;
        }
        self.construct_graphs();
        let mut order: Vec<usize> = (0..n).collect();
        let px = self.player_x;
        let py = self.player_y;
        let pfacing = self.facing;
        order.sort_by(|&a, &b| {
            let ea = &self.room.enemies[a];
            let eb = &self.room.enemies[b];
            let fa = Facing::from_u8(ea.facing).unwrap_or(Facing::South);
            let fb = Facing::from_u8(eb.facing).unwrap_or(Facing::South);
            let da = xyf_manhattan(ea.x, ea.y, fa, px, py, pfacing);
            let db = xyf_manhattan(eb.x, eb.y, fb, px, py, pfacing);
            db.cmp(&da)
        });

        for i in order {
            if !self.room.enemies[i].alive || self.room.enemies[i].stunned > 0 {
                continue;
            }
            let kind = self.room.enemies[i].kind;
            if matches!(kind, EnemyKind::King) {
                // Lua single: still skips King path (only `etype ~= kKing` → pf).
                continue;
            }
            if kind.is_inanimate() {
                continue;
            }
            self.pf_enemy_step(i);
        }
        self.sync_globals_from_room();
    }

    /// `step_reverseMen` (`main.lua:868–875`) — mid-aim hop + immediate braintwo.
    fn step_reverse_men(&mut self) {
        self.step_enemies_single();
        if let Some((idx, kill_dir, kill_type)) = self.find_player_killer() {
            if self.invulnerable {
                self.message = Some((self.cheat_invuln_label().into(), 0.8));
                return;
            }
            let (etype, is_ninja) = self
                .room
                .enemies
                .get(idx)
                .map(|e| {
                    (
                        e.kind.etype() as i32,
                        matches!(e.kind, crate::level::EnemyKind::Ninja),
                    )
                })
                .unwrap_or((1, false));
            self.play_enemy_stab(idx, kill_dir, kill_type);
            if is_ninja {
                self.spawn_shuriken_throw(idx, kill_dir, kill_type, etype);
                self.show_dialog("playerKilled", etype, 1.3);
            } else {
                self.begin_player_death(etype, kill_dir, kill_type);
            }
        }
    }

    /// Twinstep short-readybar reface (`pathfinding.lua:616–650`).
    ///
    /// When remaining readybar segments `< 2` and the facing-aware manhattan to
    /// the goal is **not** yet in attack range (`xyf < segs+1`), replace the A*
    /// hop with a same-tile facing change:
    /// - on-axis with the goal → face the player
    /// - near-axis (`absx==1` or `absy==1`) → face toward the zip line
    ///
    /// `segs_left` is `#readybar.segmentList` (ghost) or deferred `steps_left`
    /// (real). Returns `Some((x, y, facing, node_id))` when the override applies.
    fn twinstep_short_bar_reface(
        &self,
        start_x: i32,
        start_y: i32,
        start_facing: Facing,
        goal_id: u32,
        ghost: bool,
    ) -> Option<(i32, i32, Facing, u32)> {
        use crate::pathfinding::{facing_for_id, xyf_manhattan};

        let segs_left = if ghost {
            self.aim_plan.segments.len() as i32
        } else {
            self.pending_enemy
                .as_ref()
                .map(|p| p.steps_left)
                .unwrap_or(0)
        };
        if segs_left >= 2 {
            return None;
        }
        let (goal_x, goal_y) = self.main_graph.xy(goal_id)?;
        let goal_facing = facing_for_id(goal_id);
        let attack = xyf_manhattan(
            start_x,
            start_y,
            start_facing,
            goal_x,
            goal_y,
            goal_facing,
        ) < segs_left + 1;
        if attack {
            return None;
        }
        let dx = goal_x - start_x;
        let dy = goal_y - start_y;
        let absx = dx.abs();
        let absy = dy.abs();
        let new_facing = if dx == 0 || dy == 0 {
            // On-axis: turn toward the player/goal.
            face_player(start_x, start_y, goal_x, goal_y)
        } else if absx == 1 || absy == 1 {
            // Near-axis: face the zip line (dominant axis of the offset).
            if absx > absy {
                if dy == 1 {
                    Facing::South
                } else if dy == -1 {
                    Facing::North
                } else {
                    return None;
                }
            } else if dx == 1 {
                Facing::East
            } else if dx == -1 {
                Facing::West
            } else {
                return None;
            }
        } else {
            return None;
        };
        let node_id = self
            .room_tables
            .index_world_facing(start_x, start_y, new_facing)?;
        Some((start_x, start_y, new_facing, node_id))
    }

    /// `pf(start, goal, enemy, false)` — one graph hop toward the player / type target.
    fn pf_enemy_step(&mut self, i: usize) {
        use crate::level::EnemyKind;
        use crate::pathfinding::{facing_for_id, reset_graph_xy};

        let en = self.room.enemies[i];
        let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
        let Some(start_id) = self
            .room_tables
            .index_world_facing(en.x, en.y, facing)
        else {
            return;
        };

        let goal_id = match en.kind {
            EnemyKind::Pikeman => self.pikeman_goal_node(i),
            EnemyKind::Ninja => self.ninja_goal_node(i),
            _ => self
                .room_tables
                .index_world_facing(self.player_x, self.player_y, self.facing),
        };
        let Some(goal_id) = goal_id else {
            return;
        };
        if start_id == goal_id {
            return;
        }

        let Some(path) = self.main_graph.find_path(start_id, goal_id) else {
            return;
        };
        if path.len() < 2 {
            return;
        }
        let target_id = path[1];
        let Some((tx, ty)) = self.main_graph.xy(target_id) else {
            return;
        };
        let t_facing = facing_for_id(target_id);

        // Entity block at next tile.
        {
            let ghost_xy = self.ghost_xy_snapshot();
            let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: false,
            ghost_xy: &ghost_xy,
            deathlist: &[],
            roomtiles: &self.path_roomtiles,
        };
            if ctx.blocked_by_entity(tx, ty, i) {
                return;
            }
        }

        // Ninja safety bail (`pf` when remaining segments < 2 and not facing player).
        if matches!(en.kind, EnemyKind::Ninja) && (tx == self.player_x || ty == self.player_y) {
            let face_at = face_player(tx, ty, self.player_x, self.player_y);
            // Port: one segment left on the deferred bar ≈ Lua `#segmentList < 2`
            // when this is the last step of the phase; use pending steps as proxy.
            let segs_left = self
                .pending_enemy
                .as_ref()
                .map(|p| p.steps_left)
                .unwrap_or(0);
            if t_facing != face_at && segs_left < 2 {
                return;
            }
        }

        // Twinstep short-readybar reface (`pathfinding.lua:616–650`).
        // When `#readybar.segmentList < 2` and xyf distance is not yet an
        // "attack" range, override the A* hop with a same-tile facing change
        // toward the player (on-axis) or toward the zip line (near-axis).
        let (mut tx, mut ty, mut t_facing, mut target_id) = (tx, ty, t_facing, target_id);
        if matches!(en.kind, EnemyKind::Twinstep) {
            if let Some((ntx, nty, nf, nid)) =
                self.twinstep_short_bar_reface(en.x, en.y, facing, goal_id, false)
            {
                tx = ntx;
                ty = nty;
                t_facing = nf;
                target_id = nid;
            }
        }
        let _ = target_id; // used for reface override only; walk uses (tx,ty,t_facing)

        let (ox, oy) = (en.x, en.y);
        let prev_child = (en.child_x, en.child_y);

        // Lua `pf` (`pathfinding.lua:601–603`): before walk/reface, lowered
        // pikeman may `setPikeUp` via `pikeBlockCheck(enemy, targetnode)`.
        if matches!(en.kind, EnemyKind::Pikeman) && !en.pike_up {
            if self.pike_block_needs_raise(i, tx, ty, facing, t_facing) {
                self.room.enemies[i].pike_up = true;
                self.room.enemies[i].sync_pike_tip();
            }
        }

        // Lua `pf` real mode (`pathfinding.lua:651–658`):
        // - different tile → `moveToTile` only (`changeFacing=false`)
        // - same tile, new facing → `setFacing` only
        // A 90° turn is its own hop / readybar segment; never turn+walk together.
        if tx == ox && ty == oy {
            if t_facing != facing {
                self.room.enemies[i].facing = t_facing as u8;
                self.room.enemies[i].sync_pike_tip();
            }
            return;
        }

        // Walk hop: facing must stay on the walk edge (same as start facing).
        // If A* returned a facing change with a tile change, the graph is wrong.
        debug_assert_eq!(
            t_facing, facing,
            "pf walk hop must keep facing (Lua moveToTile changeFacing=false)"
        );

        // `checkForPikes` — raise other pikes whose tip we step onto.
        for j in 0..self.room.enemies.len() {
            if j == i || !self.room.enemies[j].alive {
                continue;
            }
            if matches!(self.room.enemies[j].kind, EnemyKind::Pikeman)
                && !self.room.enemies[j].pike_up
                && self.room.enemies[j].child_x == tx
                && self.room.enemies[j].child_y == ty
            {
                self.room.enemies[j].pike_up = true;
                self.room.enemies[j].sync_pike_tip();
            }
        }

        self.room.enemies[i].x = tx;
        self.room.enemies[i].y = ty;
        // Keep `facing` — `enemy:moveToTile` → `isosprite:moveToTile(..., false)`.
        self.room.enemies[i].sync_pike_tip();

        // Refresh graph at old/new body (and pike tips).
        let ghost_xy = self.ghost_xy_snapshot();
        let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: false,
            ghost_xy: &ghost_xy,
            deathlist: &[],
            roomtiles: &self.path_roomtiles,
        };
        reset_graph_xy(&mut self.main_graph, &self.room_tables, &ctx, ox, oy);
        reset_graph_xy(&mut self.main_graph, &self.room_tables, &ctx, tx, ty);
        if matches!(self.room.enemies[i].kind, EnemyKind::Pikeman) {
            let (cx, cy) = (
                self.room.enemies[i].child_x,
                self.room.enemies[i].child_y,
            );
            reset_graph_xy(&mut self.main_graph, &self.room_tables, &ctx, cx, cy);
            reset_graph_xy(
                &mut self.main_graph,
                &self.room_tables,
                &ctx,
                prev_child.0,
                prev_child.1,
            );
        }
    }

    /// Goal node for pikeman (`get_pikeman_target` without useghost).
    fn pikeman_goal_node(&self, i: usize) -> Option<u32> {
        use crate::pathfinding::xyf_manhattan;
        let en = &self.room.enemies[i];
        let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
        let start_id = self.room_tables.index_world_facing(en.x, en.y, facing)?;
        let (px, py) = (self.player_x, self.player_y);
        let tx = self.room_tables.world_to_room_x(px);
        let ty = self.room_tables.world_to_room_y(py);
        let stations = [
            (tx, ty - 2, Facing::South),
            (tx + 2, ty, Facing::West),
            (tx, ty + 2, Facing::North),
            (tx - 2, ty, Facing::East),
        ];
        let mut best = start_id;
        let mut best_dist = i32::MAX;
        let ctx_player = (self.player_x, self.player_y);
        for (scol, srow, sface) in stations {
            if scol < 1
                || scol > crate::pathfinding::GRAPH_COLS
                || srow < 1
                || srow > crate::pathfinding::GRAPH_ROWS
            {
                continue;
            }
            let id = crate::pathfinding::graph_index_xyf(scol, srow, sface as u8);
            if !self.main_graph.has_connections(id) {
                continue;
            }
            let Some((sx, sy)) = self.main_graph.xy(id) else {
                continue;
            };
            if self.enemy_step_blocked(i, sx, sy) && (en.x, en.y) != (sx, sy) {
                continue;
            }
            let dist = xyf_manhattan(en.x, en.y, facing, sx, sy, sface);
            if dist < best_dist {
                best_dist = dist;
                best = id;
            }
            let _ = ctx_player;
        }
        Some(best)
    }

    /// Goal node for ninja (`get_ninja_target` without useghost) — nearest on-axis
    /// station facing the player along row/col from the player pad.
    fn ninja_goal_node(&self, i: usize) -> Option<u32> {
        let en = &self.room.enemies[i];
        let (px, py) = (self.player_x, self.player_y);
        let (ex, ey) = (en.x, en.y);
        let dx = px - ex;
        let dy = py - ey;
        let test_x_dir = -dx.signum();
        let test_y_dir = -dy.signum();
        let xf = if dx > 0 {
            Facing::East
        } else if dx < 0 {
            Facing::West
        } else {
            Facing::South
        };
        let yf = if dy > 0 {
            Facing::South
        } else if dy < 0 {
            Facing::North
        } else {
            Facing::South
        };

        let mut x_target: Option<(i32, i32, Facing)> = None;
        if test_x_dir.abs() > 0 {
            for step in 1..=dx.abs() {
                let ty = py;
                let tx = px + test_x_dir * step;
                if let Some(id) = self.room_tables.index_world_facing(tx, ty, xf) {
                    if self.main_graph.has_connections(id) {
                        x_target = Some((tx, ty, xf));
                    }
                }
            }
        }
        let mut y_target: Option<(i32, i32, Facing)> = None;
        if test_y_dir.abs() > 0 {
            for step in 1..=dy.abs() {
                let tx = px;
                let ty = py + test_y_dir * step;
                if let Some(id) = self.room_tables.index_world_facing(tx, ty, yf) {
                    if self.main_graph.has_connections(id) {
                        y_target = Some((tx, ty, yf));
                    }
                }
            }
        }

        match (x_target, y_target) {
            (Some((x, y, f)), Some((x2, y2, f2))) => {
                let mx = (ex - x).abs() + (ey - y).abs();
                let my = (ex - x2).abs() + (ey - y2).abs();
                if mx < my {
                    self.room_tables.index_world_facing(x, y, f)
                } else {
                    self.room_tables.index_world_facing(x2, y2, f2)
                }
            }
            (Some((x, y, f)), None) => self.room_tables.index_world_facing(x, y, f),
            (None, Some((x, y, f))) => self.room_tables.index_world_facing(x, y, f),
            (None, None) => self
                .room_tables
                .index_world_facing(px, py, self.facing),
        }
    }

    /// Goal from ghost pose (`get_*_target(..., true)` — cursor tip as stand-in player).
    fn pikeman_goal_node_ghost(&self, i: usize) -> Option<u32> {
        use crate::pathfinding::xyf_manhattan;
        let g = self.enemy_ghosts.get(i)?;
        let facing = g.facing;
        let start_id = self.room_tables.index_world_facing(g.x, g.y, facing)?;
        // Cursor tip is the preview goal pad (`selectednode` / cursor).
        let (px, py) = (self.selector.tile_x, self.selector.tile_y);
        let tx = self.room_tables.world_to_room_x(px);
        let ty = self.room_tables.world_to_room_y(py);
        let stations = [
            (tx, ty - 2, Facing::South),
            (tx + 2, ty, Facing::West),
            (tx, ty + 2, Facing::North),
            (tx - 2, ty, Facing::East),
        ];
        let mut best = start_id;
        let mut best_dist = i32::MAX;
        let ghost_xy = self.ghost_xy_snapshot();
        let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: true,
            ghost_xy: &ghost_xy,
            deathlist: &self.aim_plan.deathlist,
            roomtiles: &self.path_roomtiles,
        };
        for (scol, srow, sface) in stations {
            if scol < 1
                || scol > crate::pathfinding::GRAPH_COLS
                || srow < 1
                || srow > crate::pathfinding::GRAPH_ROWS
            {
                continue;
            }
            let id = crate::pathfinding::graph_index_xyf(scol, srow, sface as u8);
            if !self.main_graph.has_connections(id) {
                continue;
            }
            let Some((sx, sy)) = self.main_graph.xy(id) else {
                continue;
            };
            if ctx.blocked_by_entity(sx, sy, i) && (g.x, g.y) != (sx, sy) {
                continue;
            }
            let dist = xyf_manhattan(g.x, g.y, facing, sx, sy, sface);
            if dist < best_dist {
                best_dist = dist;
                best = id;
            }
        }
        Some(best)
    }

    fn ninja_goal_node_ghost(&self, i: usize) -> Option<u32> {
        let g = self.enemy_ghosts.get(i)?;
        let (px, py) = (self.selector.tile_x, self.selector.tile_y);
        let (ex, ey) = (g.x, g.y);
        let dx = px - ex;
        let dy = py - ey;
        let test_x_dir = -dx.signum();
        let test_y_dir = -dy.signum();
        let xf = if dx > 0 {
            Facing::East
        } else if dx < 0 {
            Facing::West
        } else {
            Facing::South
        };
        let yf = if dy > 0 {
            Facing::South
        } else if dy < 0 {
            Facing::North
        } else {
            Facing::South
        };

        let mut x_target: Option<(i32, i32, Facing)> = None;
        if test_x_dir.abs() > 0 {
            for step in 1..=dx.abs() {
                let ty = py;
                let tx = px + test_x_dir * step;
                if let Some(id) = self.room_tables.index_world_facing(tx, ty, xf) {
                    if self.main_graph.has_connections(id) {
                        x_target = Some((tx, ty, xf));
                    }
                }
            }
        }
        let mut y_target: Option<(i32, i32, Facing)> = None;
        if test_y_dir.abs() > 0 {
            for step in 1..=dy.abs() {
                let tx = px;
                let ty = py + test_y_dir * step;
                if let Some(id) = self.room_tables.index_world_facing(tx, ty, yf) {
                    if self.main_graph.has_connections(id) {
                        y_target = Some((tx, ty, yf));
                    }
                }
            }
        }

        match (x_target, y_target) {
            (Some((x, y, f)), Some((x2, y2, f2))) => {
                let mx = (ex - x).abs() + (ey - y).abs();
                let my = (ex - x2).abs() + (ey - y2).abs();
                if mx < my {
                    self.room_tables.index_world_facing(x, y, f)
                } else {
                    self.room_tables.index_world_facing(x2, y2, f2)
                }
            }
            (Some((x, y, f)), None) => self.room_tables.index_world_facing(x, y, f),
            (None, Some((x, y, f))) => self.room_tables.index_world_facing(x, y, f),
            (None, None) => self
                .room_tables
                .index_world_facing(px, py, self.facing),
        }
    }

    /// `reset_ghosts` (`pathfinding.lua:11–26`).
    fn reset_ghosts(&mut self) {
        self.player_outline = false;
        self.outline_blink = 0;
        if self.ghost_step <= 0 {
            // Still clear move lists / hide when already at 0 (idempotent sync).
            for g in &mut self.enemy_ghosts {
                g.move_list.clear();
                g.visible = false;
                g.frame = 1;
            }
            return;
        }
        self.ghost_step = 0;
        self.sync_enemy_ghosts();
        let n = self.room.enemies.len();
        for i in 0..n {
            let en = self.room.enemies[i];
            let past = (self.enemy_ghosts[i].x, self.enemy_ghosts[i].y);
            self.enemy_ghosts[i].x = en.x;
            self.enemy_ghosts[i].y = en.y;
            self.enemy_ghosts[i].facing =
                Facing::from_u8(en.facing).unwrap_or(Facing::South);
            self.enemy_ghosts[i].visible = false;
            self.enemy_ghosts[i].frame = 1;
            self.enemy_ghosts[i].move_list.clear();
            let ghost_xy = self.ghost_xy_snapshot();
            let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: true,
            ghost_xy: &ghost_xy,
            deathlist: &self.aim_plan.deathlist,
            roomtiles: &self.path_roomtiles,
        };
            crate::pathfinding::reset_graph_xy(
                &mut self.main_graph,
                &self.room_tables,
                &ctx,
                past.0,
                past.1,
            );
            crate::pathfinding::reset_graph_xy(
                &mut self.main_graph,
                &self.room_tables,
                &ctx,
                en.x,
                en.y,
            );
        }
    }

    /// `step_ghosts(amt)` (`pathfinding.lua:28–128`).
    fn step_ghosts(&mut self, amt: i32) {
        self.player_outline = true;
        if amt == 1 {
            if self.ghost_step >= self.aim_plan.segments.len() as i32 {
                return;
            }
            let any_alive = self.room.enemies.iter().any(|e| e.alive);
            if !any_alive {
                return;
            }
            self.construct_graphs();
            self.sync_enemy_ghosts();

            use crate::level::EnemyKind;
            use crate::pathfinding::xyf_manhattan;

            let n = self.room.enemies.len();
            let mut order: Vec<usize> = (0..n).collect();
            let px = self.player_x;
            let py = self.player_y;
            let pfacing = self.facing;
            order.sort_by(|&a, &b| {
                let ea = &self.room.enemies[a];
                let eb = &self.room.enemies[b];
                let fa = Facing::from_u8(ea.facing).unwrap_or(Facing::South);
                let fb = Facing::from_u8(eb.facing).unwrap_or(Facing::South);
                let da = xyf_manhattan(ea.x, ea.y, fa, px, py, pfacing);
                let db = xyf_manhattan(eb.x, eb.y, fb, px, py, pfacing);
                db.cmp(&da)
            });

            let mut increase_by = 0i32;
            for i in order {
                let en = self.room.enemies[i];
                if self.aim_plan.deathlist.iter().any(|&d| d == i) {
                    self.enemy_ghosts[i].move_list.clear();
                    // Disconnect body facing nodes (Lua removeAllConnections).
                    if let Some(base) = self.room_tables.index_world_facing(
                        en.x,
                        en.y,
                        Facing::from_u8(en.facing).unwrap_or(Facing::South),
                    ) {
                        let tile_base = base - ((base - 1) % 4);
                        for f in 0u32..4 {
                            self.main_graph
                                .remove_all_connections_from(tile_base + f);
                        }
                    }
                    continue;
                }
                if !en.alive || en.dodge_state || en.stunned > 0 {
                    continue;
                }
                if en.kind.is_inanimate() || matches!(en.kind, EnemyKind::Spirit | EnemyKind::King)
                {
                    continue;
                }

                if self.ghost_step == 0 {
                    let ghost_xy = self.ghost_xy_snapshot();
                    let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: true,
            ghost_xy: &ghost_xy,
            deathlist: &self.aim_plan.deathlist,
            roomtiles: &self.path_roomtiles,
        };
                    crate::pathfinding::reset_graph_xy(
                        &mut self.main_graph,
                        &self.room_tables,
                        &ctx,
                        en.x,
                        en.y,
                    );
                }

                // Refresh body weights (non-ghost) before pathing this local.
                {
                    let ghost_xy = self.ghost_xy_snapshot();
                    let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: false,
            ghost_xy: &ghost_xy,
            deathlist: &[],
            roomtiles: &self.path_roomtiles,
        };
                    crate::pathfinding::reset_graph_xy(
                        &mut self.main_graph,
                        &self.room_tables,
                        &ctx,
                        en.x,
                        en.y,
                    );
                }

                let at_goal = self.pf_ghost_step(i);
                increase_by = 1;

                let (gx, gy) = (self.enemy_ghosts[i].x, self.enemy_ghosts[i].y);
                let cursor = (self.selector.tile_x, self.selector.tile_y);
                match en.kind {
                    EnemyKind::Pikeman => {
                        if at_goal {
                            self.enemy_ghosts[i].frame = 2;
                            self.play_sfx(SfxId::Warning);
                        } else {
                            self.enemy_ghosts[i].frame = 1;
                        }
                    }
                    EnemyKind::Ninja => {
                        if self.shuriken_blocked(gx, gy, cursor.0, cursor.1) {
                            self.enemy_ghosts[i].frame = 1;
                        } else {
                            self.enemy_ghosts[i].facing =
                                face_player(gx, gy, cursor.0, cursor.1);
                            self.enemy_ghosts[i].frame = 2;
                            self.play_sfx(SfxId::Warning);
                        }
                    }
                    EnemyKind::Swordsman | EnemyKind::Twinstep => {
                        let goal = cursor;
                        let dist = (goal.0 - gx).abs() + (goal.1 - gy).abs();
                        if dist <= 1 {
                            self.enemy_ghosts[i].facing =
                                face_player(gx, gy, cursor.0, cursor.1);
                            self.enemy_ghosts[i].frame = 2;
                            self.play_sfx(SfxId::Warning);
                        } else {
                            self.enemy_ghosts[i].frame = 1;
                        }
                    }
                    _ => {}
                }
            }
            self.ghost_step += increase_by;
        } else if amt == -1 {
            if self.ghost_step < 2 {
                self.player_outline = false;
            }
            if self.ghost_step <= 0 {
                return;
            }
            let mut reduce_by = 0i32;
            let n = self.enemy_ghosts.len();
            for i in 0..n {
                if !self.enemy_ghosts[i].move_list.is_empty() {
                    self.move_ghost(i, true);
                    reduce_by = 1;
                }
            }
            self.ghost_step -= reduce_by;
        }
    }

    /// `enemy:moveGhost` (`enemy.lua:1235–1257`).
    fn move_ghost(&mut self, i: usize, rewind: bool) {
        use crate::pathfinding::facing_for_id;
        if rewind {
            if self.enemy_ghosts[i].move_list.is_empty() {
                return;
            }
            if self.ghost_step > self.enemy_ghosts[i].move_list.len() as i32 {
                return;
            }
            self.enemy_ghosts[i].move_list.pop();
            self.enemy_ghosts[i].frame = 1;
            if self.enemy_ghosts[i].move_list.is_empty() {
                self.enemy_ghosts[i].visible = false;
                let en = self.room.enemies[i];
                self.enemy_ghosts[i].x = en.x;
                self.enemy_ghosts[i].y = en.y;
                self.enemy_ghosts[i].facing =
                    Facing::from_u8(en.facing).unwrap_or(Facing::South);
            } else {
                let last = *self.enemy_ghosts[i].move_list.last().unwrap();
                if let Some((x, y)) = self.main_graph.xy(last) {
                    self.enemy_ghosts[i].x = x;
                    self.enemy_ghosts[i].y = y;
                    self.enemy_ghosts[i].facing = facing_for_id(last);
                    self.enemy_ghosts[i].visible = true;
                }
            }
        } else {
            // Forward path is applied in `pf_ghost_step` via node push.
        }
    }

    /// `pf(..., ghost=true)` — one hop onto `move_list`. Returns true if landed on goal.
    fn pf_ghost_step(&mut self, i: usize) -> bool {
        use crate::level::EnemyKind;
        use crate::pathfinding::{facing_for_id, reset_graph_xy};

        let en = self.room.enemies[i];
        let (start_x, start_y, start_facing) = {
            let g = &self.enemy_ghosts[i];
            if let Some(&last) = g.move_list.last() {
                let (x, y) = self.main_graph.xy(last).unwrap_or((g.x, g.y));
                (x, y, facing_for_id(last))
            } else {
                let f = Facing::from_u8(en.facing).unwrap_or(Facing::South);
                (en.x, en.y, f)
            }
        };
        let Some(start_id) = self
            .room_tables
            .index_world_facing(start_x, start_y, start_facing)
        else {
            return false;
        };

        let cursor = (self.selector.tile_x, self.selector.tile_y);
        let goal_id = match en.kind {
            EnemyKind::Pikeman => self.pikeman_goal_node_ghost(i),
            EnemyKind::Ninja => self.ninja_goal_node_ghost(i),
            _ => self
                .room_tables
                .index_world_facing(cursor.0, cursor.1, self.facing),
        };
        let Some(goal_id) = goal_id else {
            return false;
        };
        if start_id == goal_id {
            return true;
        }

        let Some(path) = self.main_graph.find_path(start_id, goal_id) else {
            return false;
        };
        if path.len() < 2 {
            return false;
        }
        let target_id = path[1];
        let Some((tx, ty)) = self.main_graph.xy(target_id) else {
            return false;
        };
        let t_facing = facing_for_id(target_id);

        {
            let ghost_xy = self.ghost_xy_snapshot();
            let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: true,
            ghost_xy: &ghost_xy,
            deathlist: &self.aim_plan.deathlist,
            roomtiles: &self.path_roomtiles,
        };
            if ctx.blocked_by_entity(tx, ty, i) {
                return false;
            }
        }

        // Ninja safety bail (`#readybar.segmentList - ghostStep == 1`).
        if matches!(en.kind, EnemyKind::Ninja) && (tx == cursor.0 || ty == cursor.1) {
            let face_at = face_player(tx, ty, cursor.0, cursor.1);
            let segs_left = self.aim_plan.segments.len() as i32 - self.ghost_step;
            if t_facing != face_at && segs_left == 1 {
                return false;
            }
        }

        // Twinstep short-readybar reface (same as real `pf`, ghost path).
        let (mut tx, mut ty, mut t_facing, mut target_id) = (tx, ty, t_facing, target_id);
        if matches!(en.kind, EnemyKind::Twinstep) {
            if let Some((ntx, nty, nf, nid)) =
                self.twinstep_short_bar_reface(start_x, start_y, start_facing, goal_id, true)
            {
                tx = ntx;
                ty = nty;
                t_facing = nf;
                target_id = nid;
            }
        }

        // Swordsman/twinstep: skip hop onto the cursor tip itself.
        if matches!(en.kind, EnemyKind::Swordsman | EnemyKind::Twinstep)
            && tx == cursor.0
            && ty == cursor.1
        {
            return target_id == goal_id;
        }

        // Same-tile reface: record facing on the ghost without moving.
        if tx == start_x && ty == start_y {
            self.enemy_ghosts[i].move_list.push(target_id);
            self.enemy_ghosts[i].facing = t_facing;
            self.enemy_ghosts[i].visible = true;
            return target_id == goal_id;
        }

        let (ox, oy) = (start_x, start_y);
        self.enemy_ghosts[i].move_list.push(target_id);
        self.enemy_ghosts[i].x = tx;
        self.enemy_ghosts[i].y = ty;
        self.enemy_ghosts[i].facing = t_facing;
        self.enemy_ghosts[i].visible = true;

        let ghost_xy = self.ghost_xy_snapshot();
        let ctx = crate::pathfinding::GraphBuildCtx {
            world: &self.world,
            enemies: &self.room.enemies,
            player: (self.player_x, self.player_y),
            cursor: (self.selector.tile_x, self.selector.tile_y),
            chest: self.chest_pos.filter(|_| !self.has_key),
            has_key: self.has_key,
            ghost: true,
            ghost_xy: &ghost_xy,
            deathlist: &self.aim_plan.deathlist,
            roomtiles: &self.path_roomtiles,
        };
        reset_graph_xy(&mut self.main_graph, &self.room_tables, &ctx, ox, oy);
        reset_graph_xy(&mut self.main_graph, &self.room_tables, &ctx, tx, ty);

        target_id == goal_id
    }

    /// Aiming-only crank → `step_ghosts` (`main.lua:704–717`).
    fn handle_crank_ghosts(&mut self) {
        self.crank_accum += self.last_input.crank_delta;
        if self.crank_accum > 15.0 {
            self.step_ghosts(1);
            self.crank_accum = 0.0;
            self.crank_use_counter += 1;
            self.play_sfx(SfxId::Zzt);
        } else if self.crank_accum < -15.0 {
            self.step_ghosts(-1);
            self.crank_accum = 0.0;
            self.crank_use_counter += 1;
            self.play_sfx(SfxId::Zzt);
        }
    }

    /// Tick readybar / crankhint blink + slide (`movebar:update`).
    fn tick_readybar_hint(&mut self) {
        self.readybar_frame_counter -= 1;
        if self.readybar_frame_counter <= 0 {
            self.readybar_frame_counter = 4;
            self.readybar_blink = 1 - self.readybar_blink;
            self.crankhint_frame =
                (self.crankhint_frame + 1) % Self::CRANKHINT_FRAMES.len() as i32;
        }

        let show = self.should_show_crankhint();
        if show {
            self.crankhint_out = true;
        } else {
            self.crankhint_out = false;
        }
        if self.crankhint_out {
            if self.crankhint_pos_index < 7 {
                self.crankhint_pos_index += 1;
            }
            if self.crank_use_counter > 40 {
                self.crankhint_out = false;
            }
        } else if self.crankhint_pos_index > 1 {
            self.crankhint_pos_index -= 1;
        }
    }

    /// `movebar:updatehint` — crankhint + host mobile swipe tip share this gate.
    pub fn should_show_crankhint(&self) -> bool {
        if self.crank_use_counter > 40 {
            return false;
        }
        if self.aim_plan.segments.len() <= 1 {
            return false;
        }
        self.room.enemies.iter().any(|e| e.alive)
    }

    /// Host mobile UI: Down pad covers outdoor chest `(145,176)` (monk “southwest”).
    ///
    /// True only while that chest is still present and the current culled room
    /// includes its pad — so the host can drop `backdrop-filter` on Down alone.
    pub fn should_clear_touch_down_blur(&self) -> bool {
        const SW_CHEST: (i32, i32) = (145, 176);
        if self.has_key {
            return false;
        }
        if self.chest_pos != Some(SW_CHEST) {
            return false;
        }
        self.world
            .roomtiles_for(self.camera, (self.player_x, self.player_y))
            .contains(&SW_CHEST)
    }

    /// `pikeBlockCheck` (`pathfinding.lua:465–517`) — true → `setPikeUp` before the hop.
    ///
    /// 1. Tip cell ahead of `target` facing blocked / unwalkable.
    /// 2. If facing changes on this hop, also the **corner** cell for that turn
    ///    (entity block only — Lua does not walkability-check the corner).
    fn pike_block_needs_raise(
        &self,
        i: usize,
        tx: i32,
        ty: i32,
        current_facing: Facing,
        t_facing: Facing,
    ) -> bool {
        let ahead = match t_facing {
            Facing::North => (tx, ty - 1),
            Facing::South => (tx, ty + 1),
            Facing::East => (tx + 1, ty),
            Facing::West => (tx - 1, ty),
        };
        // Lua: checkBlockedByEntity OR NOT checkWalkableTile on tip ahead.
        if self.pike_entity_blocks(i, ahead.0, ahead.1) || !self.room.walkable(ahead.0, ahead.1) {
            return true;
        }

        if t_facing == current_facing {
            return false;
        }

        // Corner sweep while rotating (`facingForID(target) ~= enemy.facing`).
        let corner = match (current_facing, t_facing) {
            (Facing::North, Facing::East) | (Facing::East, Facing::North) => (tx + 1, ty - 1),
            (Facing::North, Facing::West) | (Facing::West, Facing::North) => (tx - 1, ty - 1),
            (Facing::South, Facing::East) | (Facing::East, Facing::South) => (tx + 1, ty + 1),
            (Facing::South, Facing::West) | (Facing::West, Facing::South) => (tx - 1, ty + 1),
            // 180° turns are not in Lua's branch table — no extra corner cell.
            _ => return false,
        };
        self.pike_entity_blocks(i, corner.0, corner.1)
    }

    /// `checkBlockedByEntity(..., ghost=false)` for pike tip / corner cells.
    fn pike_entity_blocks(&self, myself: usize, x: i32, y: i32) -> bool {
        if (x, y) == (self.player_x, self.player_y) {
            return true;
        }
        self.room.enemies.iter().enumerate().any(|(j, e)| {
            if j == myself || !e.alive {
                return false;
            }
            (e.x, e.y) == (x, y) || e.tip_blocks(x, y)
        })
    }

    /// Test helper: full `pikeBlockCheck` for enemy `i` toward `(tx,ty,t_facing)`.
    pub fn pike_block_needs_raise_for_test(
        &self,
        i: usize,
        tx: i32,
        ty: i32,
        t_facing: Facing,
    ) -> bool {
        let facing = Facing::from_u8(self.room.enemies[i].facing).unwrap_or(Facing::South);
        self.pike_block_needs_raise(i, tx, ty, facing, t_facing)
    }

    /// `checkBlockedByEntity` for enemy steps: player tile, other living enemies,
    /// living pike tips, or non-walkable terrain.
    fn enemy_step_blocked(&self, self_i: usize, x: i32, y: i32) -> bool {
        if x == self.player_x && y == self.player_y {
            return true;
        }
        if !self.room.walkable(x, y) {
            return true;
        }
        self.room.enemies.iter().enumerate().any(|(j, e)| {
            if !e.alive {
                return false;
            }
            if j != self_i && e.x == x && e.y == y {
                return true;
            }
            // Other (or same) lowered tip occupies the cell — Lua raises own tip
            // when stepping onto it; we treat tip tiles as blocked for core slice.
            j != self_i && e.tip_blocks(x, y)
        })
    }

    /// `pathfinding.lua` `checkBlockedShuriken` — true if the shot cannot reach.
    /// Axis-only; `pending_exit` ≈ `player.onexit` with `skipexitcheck=false`;
    /// intervening cells must be in culled `path_roomtiles` and free of living
    /// body / pike tip (`checkBlockedByEntity`).
    fn shuriken_blocked(&self, ex: i32, ey: i32, tx: i32, ty: i32) -> bool {
        if tx != ex && ty != ey {
            return true;
        }
        // Lua: `skipexitcheck == false and player.onexit ~= nil` → blocked.
        // Both kill and crank-ghost callers pass skipexitcheck=false.
        if self.pending_exit.is_some() {
            return true;
        }
        let (ox, oy) = if tx == ex {
            (0, if ey < ty { 1 } else { -1 })
        } else {
            (if ex < tx { 1 } else { -1 }, 0)
        };
        let numsteps = (tx - ex).abs().max((ty - ey).abs()) - 1;
        let mut testx = ex;
        let mut testy = ey;
        for _ in 0..numsteps {
            testx += ox;
            testy += oy;
            // Wall / void / culled island ≈ `roomtiles[…] == nil`.
            if !self.path_roomtiles.contains(&(testx, testy)) {
                return true;
            }
            if self.room.enemy_at(testx, testy).is_some() {
                return true;
            }
            if self.room.pike_tip_at(testx, testy).is_some() {
                return true;
            }
        }
        false
    }

    /// Screen origin for `grid_to_screen`, matching Lua `tileToScreen` with fixed
    /// `cameratile_x/y` (`Globals.lua`):
    /// `px = (x-cam_x-(y-cam_y))*16 + 200`, `py = (x-cam_x+(y-cam_y))*8 + 80`.
    fn origin(&self) -> (i32, i32) {
        let (cx, cy) = self.camera;
        let ox = 200 + (cy - cx) * TILE_HALF_W;
        let oy = 80 - (cx + cy) * TILE_HALF_H;
        (ox, oy)
    }

    fn draw(&mut self) {
        match self.state {
            GameState::Boot => self.draw_boot(),
            GameState::Title | GameState::Intro => self.draw_title_intro(),
            GameState::Transition => self.draw_room_transition(),
            GameState::Aiming | GameState::Resolving | GameState::Dialog | GameState::GameOver
            | GameState::Win => self.draw_room(),
            GameState::Score => self.draw_score_screen(),
        }
        if self.state == GameState::GameOver {
            self.draw_restart_prompt();
        }
        if self.state == GameState::Win && self.ending.credits_active {
            draw_credits(
                &mut self.fb,
                &self.ending,
                self.assets.headerwhite.as_ref(),
                self.assets.wipe.as_ref(),
            );
        }
    }

    /// `kGameScoreState` — black + `winbackground` + delayed highscore panel.
    fn draw_score_screen(&mut self) {
        draw_winbackground(
            &mut self.fb,
            self.assets.endbg.as_ref(),
            self.ending.win_bg_t,
        );
        draw_highscore_panel(
            &mut self.fb,
            &self.ending,
            self.assets.highscore.as_ref(),
            self.assets.monoblack.as_ref(),
        );
    }

    /// `initbackground` wipe frames: Out (old room + left poly), Loading text, In (new + right).
    fn draw_room_transition(&mut self) {
        match self.transition_phase.unwrap_or(TransitionPhase::Loading) {
            TransitionPhase::Out => {
                self.draw_room();
                self.fb.fill_transition_wipe_out();
            }
            TransitionPhase::Loading => {
                // Lua: gfx.clear(kColorClear) then whitefont "loading...".
                // Clear framebuffer is white (0); host draws black ink on white.
                // Playdate clear-to-clear over previous flush looks black in practice
                // after the out wipe — match with a black field + light glyphs.
                self.fb.clear(true);
                const LOADING_X: i32 = SCREEN_WIDTH as i32 - 54; // 346
                const LOADING_Y: i32 = SCREEN_HEIGHT as i32 - 18; // 222
                if let Some(font) = self.assets.headerwhite.as_ref() {
                    font.draw_text(&mut self.fb, LOADING_X, LOADING_Y, "loading...", 1);
                } else {
                    draw_text(&mut self.fb, LOADING_X, LOADING_Y, "loading...", false);
                }
            }
            TransitionPhase::In => {
                self.draw_room();
                self.fb.fill_transition_wipe_in();
            }
        }
    }

    /// Title / Intro white field: clear + invert (host flips palette) + player (+ bennett).
    /// Map strips stay hidden (`main.lua` `strips[row]:setVisible(false)`).
    fn draw_title_intro(&mut self) {
        // Black clear + `screen_inverted` → white field on the host blit.
        self.fb.clear(true);
        let (ox, oy) = self.origin();
        let (sx, sy) = grid_to_screen(self.player_x, self.player_y, ox, oy);
        self.blit_player(sx, sy);
        if self.bennett_visible {
            if let Some(img) = self.assets.bennett.as_ref() {
                // Lua `bennett:moveTo(232, 132)` with default sprite center (0.5, 0.5).
                let bx = 232 - (img.width as i32) / 2;
                let by = 132 - (img.height as i32) / 2;
                img.blit(&mut self.fb, bx, by);
            }
        }
    }

    /// `restartSprite` — 177×21 "PRESS ANY BUTTON TO RESTART" at (109, y).
    fn draw_restart_prompt(&mut self) {
        let y = self.restart_sprite_y.round() as i32;
        if let Some(img) = self.assets.restart.as_ref() {
            img.blit(&mut self.fb, 109, y);
        } else {
            // Fallback text if the asset is not loaded (unit tests).
            draw_text(&mut self.fb, 109, y.max(0), "PRESS ANY BUTTON TO RESTART", true);
        }
    }

    fn draw_boot(&mut self) {
        self.fb.clear(false);
        if let Some(card) = &self.assets.card {
            let x = (SCREEN_WIDTH as i32 - card.width as i32) / 2;
            card.blit(&mut self.fb, x, 28);
        } else {
            draw_text(&mut self.fb, 148, 100, "ZIPPER", true);
        }
    }

    fn draw_room(&mut self) {
        // `main.lua` / `startGame`: gfx.setBackgroundColor(gfx.kColorBlack).
        // Void cells and transparent tile padding must show black, not white.
        self.fb.clear(true);
        let (ox, oy) = self.origin();

        let order = self.room.paint_order();
        let aim_end = if self
            .selector
            .can_commit(self.player_x, self.player_y)
        {
            Some((self.selector.tile_x, self.selector.tile_y))
        } else {
            None
        };
        let tip_is_stab = self
            .aim_plan
            .segments
            .last()
            .is_some_and(|s| *s == PathSegment::Stab);

        // Interleaved depth paint (Lua strips rise with `isoZ`, actors are
        // `isoZ + 5` on *their* tile). Nearer walls/decor must cover farther
        // actors — the v0.9c global "all tiles then all actors" pass broke that.
        // Tile alpha keeps transparent upper halves from wiping farther sprites.
        for &(gx, gy) in &order {
            let (sx, sy) = grid_to_screen(gx, gy, ox, oy);
            let terrain = self.room.cell(gx, gy);

            if let Some(tile_id) = terrain.tile() {
                let draw_gid = self.map_anim_draw_gid(gx, gy, tile_id);
                let frame = draw_gid.saturating_sub(1) as usize;
                if let Some(tiles) = &self.assets.tiles {
                    if let Some(tile) = tiles.get(frame) {
                        let (bx, by) = tile_blit_pos(sx, sy);
                        tile.blit(&mut self.fb, bx, by);
                    }
                } else {
                    draw_diamond(
                        &mut self.fb,
                        sx,
                        sy,
                        matches!(terrain, Terrain::Wall { .. }),
                    );
                }
            }

            // Player blood footprints (`samurai:trailBlood` → strip NXOR).
            self.blit_trail_at(gx, gy, sx, sy);

            // Floor blood (`mapsprites` hereblood / floorspray, NXOR, isoZ+1).
            self.blit_floor_blood_at(gx, gy, sx, sy);

            // Blood spray (`enemy:bleed` / `samurai:bleed`). Sprite z is `isoZ + 1`
            // (player) / `isoZ ± 4` (enemy) — below the actor's `isoZ + 5`, so the
            // burst paints *behind* the corpse / player (`moveToTile` runs after
            // the `setZIndex(+1)` in both Lua functions, leaving `zoffset` in charge).
            let sprays: Vec<(Facing, u32, u8, bool)> = self
                .blood_sprays
                .iter()
                .filter(|s| s.x == gx && s.y == gy)
                .map(|s| (s.facing, s.anim.pose_1based(), s.table_i, s.use_isospray))
                .collect();
            for (facing, pose, table_i, use_isospray) in sprays {
                if use_isospray {
                    self.blit_isospray(sx, sy, facing, pose);
                } else {
                    self.blit_espray(sx, sy, facing, pose, table_i);
                }
            }

            // Player-death drips (`samurai:sploosh`, oneshot `zoffset = 1` → behind).
            let drips: Vec<(u8, u32)> = self
                .drip_fx
                .iter()
                .filter(|d| d.x == gx && d.y == gy)
                .map(|d| (d.which, d.anim.pose_1based()))
                .collect();
            for (which, pose) in drips {
                self.blit_drip(sx, sy, which, pose);
            }

            // Enemy / corpse draw order (Lua keeps dead enemies in localenemies):
            // 1. `enemy:stab` while killing the player
            // 2. `slashed*` death anim (`kill_fx`)
            // 3. Ninja `dodge` hop oneshot
            // 4. Living idle, or persistent corpse pose after anim ends / on re-entry
            let stabbing = self
                .enemy_attack_fx
                .iter()
                .find(|fx| fx.x == gx && fx.y == gy)
                .cloned();
            let dying = self
                .kill_fx
                .iter()
                .find(|fx| fx.x == gx && fx.y == gy)
                .cloned();
            let dodging = self
                .enemy_dodge_fx
                .iter()
                .find(|fx| fx.x == gx && fx.y == gy)
                .cloned();
            let reviving = self
                .spirit_revive_fx
                .iter()
                .find(|fx| fx.x == gx && fx.y == gy)
                .cloned();
            // Spirit-room door overlay (`door.lua` isotile on the cell).
            self.blit_spirit_door_at(gx, gy, sx, sy);
            if let Some(fx) = stabbing {
                self.blit_enemy_kind_pose(sx, sy, fx.facing, fx.anim.pose_1based(), fx.kind, false);
                // Tip mirrors body stab anim (`enemy:stab` → child:playAnim("stab")).
                if matches!(fx.kind, crate::level::EnemyKind::Pikeman) {
                    self.blit_piketip_pose(sx, sy, fx.facing, fx.anim.pose_1based(), false);
                }
            } else if let Some(fx) = dying {
                self.blit_enemy_kind_pose(sx, sy, fx.facing, fx.anim.pose_1based(), fx.kind, false);
                if matches!(fx.kind, crate::level::EnemyKind::Pikeman) {
                    self.blit_piketip_pose(sx, sy, fx.facing, fx.anim.pose_1based(), false);
                }
            } else if let Some(fx) = dodging {
                self.blit_parry_sparks_at(gx, gy, sx, sy, true);
                self.blit_enemy_kind_pose(
                    sx,
                    sy,
                    fx.facing,
                    fx.anim.pose_1based(),
                    fx.kind,
                    false,
                );
                self.blit_parry_sparks_at(gx, gy, sx, sy, false);
            } else if let Some(fx) = reviving {
                // `enemy:revive` oneshot (south); alive already true.
                self.blit_enemy_kind_pose(
                    sx,
                    sy,
                    Facing::South,
                    fx.anim.pose_1based(),
                    crate::level::EnemyKind::Spirit,
                    false,
                );
            } else if let Some(i) = self.room.any_enemy_at(gx, gy) {
                let en = self.room.enemies[i];
                let facing = Facing::from_u8(en.facing).unwrap_or(Facing::South);
                if en.alive {
                    // `enemy:stun` → `kDrawModeInverted` while stunned > 0.
                    let inverted = en.stunned > 0;
                    let is_spirit = matches!(en.kind, crate::level::EnemyKind::Spirit);
                    // Spirits lock facing south (`enemy:setFacing` kSpirit).
                    let draw_facing = if is_spirit { Facing::South } else { facing };
                    let pose = if matches!(en.kind, crate::level::EnemyKind::Pikeman) && en.pike_up
                    {
                        12 // raised idle pose from `pikeup` resting cell
                    } else if is_spirit {
                        spirit_idle_pose(self.time)
                    } else if en.dodge_state {
                        // After `dodge`/`parry` oneshot ends, Lua keeps the last
                        // anim frame until `braintwo` clears `dodgestate` + idle.
                        living_dodge_hold_pose(en.kind)
                    } else {
                        1
                    };
                    // Twinstep spark may outlast the body `parry` oneshot.
                    self.blit_parry_sparks_at(gx, gy, sx, sy, true);
                    self.blit_enemy_kind_pose(sx, sy, draw_facing, pose, en.kind, inverted);
                    self.blit_parry_sparks_at(gx, gy, sx, sy, false);
                    if matches!(en.kind, crate::level::EnemyKind::Pikeman) {
                        // Tip sprite is moved to the *body* screen point in Lua
                        // (`setPikeUp` / `setVisible`); logical tip tile is separate.
                        self.blit_piketip_pose(sx, sy, facing, pose, inverted);
                    }
                } else {
                    // Corpse: last `dead` / `slashed` resting pose (`isosprite` holds frame).
                    let pose = enemy_corpse_pose(en.kind);
                    let draw_facing = if matches!(en.kind, crate::level::EnemyKind::Spirit) {
                        Facing::South
                    } else {
                        facing
                    };
                    self.blit_enemy_kind_pose(sx, sy, draw_facing, pose, en.kind, false);
                    if matches!(en.kind, crate::level::EnemyKind::Pikeman) {
                        self.blit_piketip_pose(sx, sy, facing, pose, false);
                    }
                }
            }

            // Key chest (`chester.chestSprite`) — isotile, hidden after pickup.
            if !self.has_key {
                if let Some((cx, cy)) = self.chest_pos {
                    if gx == cx && gy == cy {
                        self.blit_chest(sx, sy);
                    }
                }
            }

            if self.ending.player_visible && gx == self.player_x && gy == self.player_y {
                self.blit_player(sx, sy);
            }

            // Crank ghost preview sprites (`enemy.ghost`, NXOR / Invert blink).
            let ghost_draws: Vec<(Facing, u8, crate::level::EnemyKind)> = self
                .enemy_ghosts
                .iter()
                .enumerate()
                .filter(|(_, g)| g.visible && g.x == gx && g.y == gy)
                .filter_map(|(i, g)| {
                    let kind = self.room.enemies.get(i)?.kind;
                    Some((g.facing, g.frame, kind))
                })
                .collect();
            for (gface, gframe, kind) in ghost_draws {
                self.blit_ghost_pose(sx, sy, gface, gframe, kind);
            }
        }

        // Floor zip smoke (`smoker:update`) — behind path, NXOR, zoffset +4.
        self.draw_floor_smoke(ox, oy);

        // Flying shuriken (`oneshotsprite` zoffset 10 — above actors).
        self.draw_shuriken_fx(ox, oy);

        // Path icons from readybar segments (`selector:update`).
        // Intermediate cells: walk → passicon, slash/doubleslash → killicon.
        // Tip priority: stab → exiticon[onExit] (Copy) → moveicon + blinking centerdot.
        // Hidden during win cinema (player/HUD already off).
        if aim_end.is_some() && self.state != GameState::Win {
            self.draw_aim_path_icons(ox, oy);
            let (ex, ey) = aim_end.unwrap();
            let (sx, sy) = grid_to_screen(ex, ey, ox, oy);
            if tip_is_stab {
                self.blit_stabicon(sx, sy);
            } else if let Some(dir) = self.selector.blocked {
                // `onExit > 0`: arrival facing doubles as blockedDir (`moveByTile`).
                self.blit_exiticon(sx, sy, dir);
            } else {
                self.blit_moveicon(sx, sy);
                if ((self.time * 12.0) as u32 % 12) > 5 {
                    self.blit_centerdot(sx, sy);
                }
            }
        }

        // HUD sprites (`movebar` top-left, `lifebar` top-right) — hidden in Title/Intro.
        if self.hud_visible {
            self.draw_readybar();
            self.draw_lifebar();
            self.draw_god_coords();

            // Key icon (`chester.keyIcon` at 314,2) when the player has the castle key.
            if self.has_key {
                if let Some(key) = self.assets.key.as_ref() {
                    key.blit(&mut self.fb, 314, 2);
                }
            }
        }

        // Bottom dialog bar (`dialogbar`, z 1500 — above world, below continue).
        self.dialog.draw(
            &mut self.fb,
            self.assets.dialogbg.as_ref(),
            &self.assets.dialog_faces,
            self.assets.faceblood.as_ref(),
            self.assets.headerwhite.as_ref(),
        );

        // `endintro` zip oneshot at (320, 190) until finished (`oneshotsprite`).
        self.draw_zip_overlay();

        // Transient debug flashes (god mode) — keep clear of the readybar.
        if let Some((ref msg, _)) = self.message {
            draw_text(&mut self.fb, 8, 28, msg, true);
        }
    }

    /// `endintro` zip logo (`Images/zip` oneshot at screen 320,190).
    fn draw_zip_overlay(&mut self) {
        let Some(anim) = self.zip_anim.as_ref() else {
            return;
        };
        let pose = anim.pose_1based();
        let Some(table) = self.assets.zip.as_ref() else {
            return;
        };
        let Some(frame) = table.get(pose.saturating_sub(1) as usize) else {
            return;
        };
        // `oneshotsprite:setCenter(0.5, 0.27083334)` — same as isosprite.
        let bx = 320 - ((frame.width as f32) * ACTOR_CENTER_X).round() as i32;
        let by = 190 - ((frame.height as f32) * ACTOR_CENTER_Y).round() as i32;
        frame.blit(&mut self.fb, bx, by);
    }

    /// `movebar:draw` — chrome at (1,1); READY when idle, PRESS A + segment pips when aimed.
    fn draw_readybar(&mut self) {
        let bx = MOVEBAR_X;
        let by = MOVEBAR_Y;
        if let Some(bg) = self.assets.movebar.as_ref() {
            bg.blit(&mut self.fb, bx, by);
        } else {
            self.fb.fill_rect(bx, by, 153, 19, true);
        }

        let segs = self.aim_plan.segments.clone();
        if segs.is_empty() {
            if let Some(word) = self.assets.readyword.as_ref() {
                word.blit(&mut self.fb, bx + 54, by + 4);
            }
            return;
        }

        if let Some(word) = self.assets.pressx.as_ref() {
            word.blit(&mut self.fb, bx + 38, by + 2);
        }
        // Cap matte after the last pip (`#segmentList * 7 + x - 5`).
        if let Some(matte) = self.assets.barmatte.as_ref() {
            let mx = segs.len() as i32 * 7 + bx - 5;
            matte.blit(&mut self.fb, mx, by + 1);
        }
        for (i, seg) in segs.iter().enumerate() {
            // Lua 1-based `i * 7 + x - 5` → 0-based `(i+1)*7 + x - 5`.
            let px = (i as i32 + 1) * 7 + bx - 5;
            let py = by + 1;
            match seg {
                PathSegment::Walk => {
                    let piece = if i == 0 {
                        self.assets
                            .readysegwalk_0
                            .as_ref()
                            .or(self.assets.readysegwalk.as_ref())
                    } else if i == 1 {
                        self.assets
                            .readysegwalk_1
                            .as_ref()
                            .or(self.assets.readysegwalk.as_ref())
                    } else {
                        self.assets.readysegwalk.as_ref()
                    };
                    if let Some(img) = piece {
                        img.blit(&mut self.fb, px, py);
                    } else {
                        self.fb.fill_rect(px, py, 7, 14, false);
                    }
                }
                PathSegment::Slash | PathSegment::DoubleSlash | PathSegment::Stab => {
                    if let Some(img) = self.assets.readysegkill.as_ref() {
                        img.blit(&mut self.fb, px, py);
                    } else {
                        self.fb.fill_rect(px, py, 7, 14, true);
                    }
                }
            }
        }
        // Ghost pip at `ghostStep * 7 + x - 5` (Inverted).
        if self.ghost_step > 0 {
            let gpx = self.ghost_step * 7 + bx - 5;
            let gpy = by + 1;
            let ghost_img = if self.readybar_blink == 1 {
                self.assets
                    .readysegghost1
                    .as_ref()
                    .or(self.assets.readysegghost2.as_ref())
            } else {
                self.assets
                    .readysegghost2
                    .as_ref()
                    .or(self.assets.readysegghost1.as_ref())
            };
            if let Some(img) = ghost_img {
                img.blit_mode(&mut self.fb, gpx, gpy, DrawMode::Inverted);
            } else {
                self.fb.fill_rect(gpx, gpy, 7, 14, false);
            }
        }
        self.draw_crankhint();
    }

    /// `movebar.crankhint` — slide-out tutorial when aiming with living enemies.
    fn draw_crankhint(&mut self) {
        if self.crankhint_pos_index <= 1 && !self.crankhint_out {
            return;
        }
        let Some(table) = self.assets.crankhint.as_ref() else {
            return;
        };
        let pos_i = (self.crankhint_pos_index as usize)
            .saturating_sub(1)
            .min(Self::CRANKHINT_POSITIONS.len() - 1);
        let hx = Self::CRANKHINT_POSITIONS[pos_i];
        let hy = 11;
        let cell = Self::CRANKHINT_FRAMES
            .get(self.crankhint_frame as usize)
            .copied()
            .unwrap_or(1);
        if let Some(frame) = table.get(cell.saturating_sub(1)) {
            // Hint is a small sprite centered on (hx, hy) in Lua; blit top-left approx.
            let (bw, bh) = (frame.width as i32, frame.height as i32);
            frame.blit(&mut self.fb, hx - bw / 2, hy - bh / 2);
        }
    }

    /// `lifebar:draw` — chrome at top-right; hourglass + `LIFE:###` text.
    fn draw_lifebar(&mut self) {
        let bx = LIFEBAR_X;
        let by = LIFEBAR_Y;
        if let Some(bg) = self.assets.lifebar.as_ref() {
            bg.blit(&mut self.fb, bx, by);
        } else {
            self.fb.fill_rect(bx, by, 70, 19, true);
        }
        // Hourglass at (2,2) within the sprite; 1-based frame.
        if let Some(table) = self.assets.hourglass.as_ref() {
            let idx = (self.hourglass_frame as usize).saturating_sub(1);
            if let Some(frame) = table.get(idx) {
                frame.blit(&mut self.fb, bx + 2, by + 2);
            }
        }
        // Lua: `gfx.setFont(whitefont); gfx.setFontTracking(1); drawText(..., 16, 1)`.
        let label = format!("LIFE:{}", self.display_blood);
        if let Some(font) = self.assets.headerwhite.as_ref() {
            font.draw_text(&mut self.fb, bx + 16, by + 1, &label, 1);
        } else {
            draw_text(&mut self.fb, bx + 16, by + 2, &label, false);
        }
    }

    fn cheat_invuln_label(&self) -> &'static str {
        "GOD MODE"
    }

    /// `#god` HUD: player tile (`tile_x`,`tile_y`) just left of the lifebar.
    ///
    /// Same 256×256 outdoor grid as `worldmap.bin` chests/exits — not screen pixels.
    /// When the key icon is up (`chester.keyIcon` at 314,2), sit left of that so the
    /// glyphs are not covered.
    fn draw_god_coords(&mut self) {
        if !self.god_mode {
            return;
        }
        let label = format!("{},{}", self.player_x, self.player_y);
        let w = label.len() as i32 * 4;
        let right = if self.has_key { 314 } else { LIFEBAR_X };
        let x = right - 4 - w;
        let y = LIFEBAR_Y + 2;
        // Black matte + light glyphs so the readout stays legible on both void and tiles.
        self.fb.fill_rect(x - 1, y - 1, w + 2, 7, true);
        draw_text(&mut self.fb, x, y, &label, false);
    }

    fn blit_player(&mut self, sx: i32, sy: i32) {
        // Outline mode: force pose 12 + blink NXOR/XOR (`samurai:setOutline` / update).
        let pose = if self.player_outline {
            12
        } else {
            self.player_anim
                .as_ref()
                .map(|a| a.pose_1based())
                .unwrap_or(1)
        };
        let mode = if self.player_outline {
            if self.outline_blink >= 4 {
                DrawMode::Xor
            } else {
                DrawMode::Nxor
            }
        } else {
            DrawMode::Copy
        };
        if let Some(table) = &self.assets.player {
            let per = (table.len() / 4).max(1);
            let idx = isosprite_frame_index(self.facing, pose, per);
            if let Some(frame) = table.get(idx) {
                let (bx, by) = actor_blit_pos(sx, sy);
                frame.blit_mode(&mut self.fb, bx, by, mode);
                return;
            }
        }
        self.fb.fill_rect(sx - 2, sy - 14, 5, 14, true);
    }

    /// Ghost body (`enemy_ghost` / `ninja_ghost` / `pikeman_ghost`, NXOR; Invert blink).
    fn blit_ghost_pose(
        &mut self,
        sx: i32,
        sy: i32,
        facing: Facing,
        frame_1based: u8,
        kind: crate::level::EnemyKind,
    ) {
        use crate::level::EnemyKind;
        let table = match kind {
            EnemyKind::Ninja => self
                .assets
                .ninja_ghost
                .as_ref()
                .or(self.assets.enemy_ghost.as_ref()),
            EnemyKind::Pikeman => self
                .assets
                .pikeman_ghost
                .as_ref()
                .or(self.assets.enemy_ghost.as_ref()),
            EnemyKind::Twinstep => self
                .assets
                .twinstep_ghost
                .as_ref()
                .or(self.assets.enemy_ghost.as_ref()),
            _ => self.assets.enemy_ghost.as_ref(),
        };
        let Some(table) = table else {
            // Fallback: faint diamond so missing assets are still visible.
            self.fb.fill_rect(sx - 2, sy - 10, 5, 10, false);
            return;
        };
        let pose = frame_1based.max(1) as u32;
        let per = (table.len() / 4).max(1);
        let idx = isosprite_frame_index(facing, pose, per);
        if let Some(frame) = table.get(idx) {
            let (bx, by) = actor_blit_pos(sx, sy);
            // Lua `enemy:update`: NXOR normally, Invert on even ticks while visible.
            let mode = if self.readybar_blink == 0 {
                DrawMode::Nxor
            } else {
                DrawMode::Inverted
            };
            frame.blit_mode(&mut self.fb, bx, by, mode);
        }
    }

    /// Flying `Images/shuriken` oneshots (NXOR, center 0.5 / -2.6).
    fn draw_shuriken_fx(&mut self, ox: i32, oy: i32) {
        let snaps: Vec<(i32, i32, u32)> = self
            .shuriken_fx
            .iter()
            .map(|fx| (fx.tile_x, fx.tile_y, fx.anim.pose_1based()))
            .collect();
        for (tx, ty, pose) in snaps {
            let (sx, sy) = grid_to_screen(tx, ty, ox, oy);
            self.blit_shuriken(sx, sy, pose);
        }
    }

    fn blit_shuriken(&mut self, sx: i32, sy: i32, pose_1based: u32) {
        let Some(table) = self.assets.shuriken.as_ref() else {
            return;
        };
        let idx = pose_1based.saturating_sub(1) as usize;
        let Some(frame) = table.get(idx) else {
            return;
        };
        // Lua `setCenter(0.5, -2.6)` on 12×10 cells — lifts the star above the tile.
        let bx = sx - ((frame.width as f32) * 0.5).round() as i32;
        let by = sy - ((frame.height as f32) * -2.6).round() as i32;
        frame.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
    }

    /// Twinstep `Images/parry` spark (NXOR). `behind` selects N/W vs S/E z order.
    fn blit_parry_sparks_at(&mut self, gx: i32, gy: i32, sx: i32, sy: i32, behind: bool) {
        let poses: Vec<u32> = self
            .parry_spark_fx
            .iter()
            .filter(|s| s.x == gx && s.y == gy && s.draw_behind == behind)
            .map(|s| s.anim.pose_1based())
            .collect();
        for pose in poses {
            self.blit_parry_spark(sx, sy, pose);
        }
    }

    fn blit_parry_spark(&mut self, sx: i32, sy: i32, pose_1based: u32) {
        let Some(table) = self.assets.parry.as_ref() else {
            return;
        };
        // `parry.pdt` is a short oneshot strip (not a 4-facing isosprite).
        let idx = pose_1based.saturating_sub(1) as usize;
        if let Some(frame) = table.get(idx) {
            let (bx, by) = actor_blit_pos(sx, sy);
            // Lua `setCenter(0.5, -0.1)` lifts the spark slightly above the body.
            frame.blit_mode(&mut self.fb, bx, by - 3, DrawMode::Nxor);
        }
    }

    /// Spirit-room `door` blit (`door.lua` extends `isotile` — tile center).
    fn blit_spirit_door_at(&mut self, gx: i32, gy: i32, sx: i32, sy: i32) {
        let Some(door) = self.spirit_doors.iter().find(|d| d.x == gx && d.y == gy) else {
            return;
        };
        let cell = door.blit_cell();
        let Some(table) = self.assets.spirit_door.as_ref() else {
            // Fallback diamond so closed doors are still visible in tests.
            draw_diamond(&mut self.fb, sx, sy, true);
            return;
        };
        if let Some(frame) = table.get(cell) {
            let (bx, by) = tile_blit_pos(sx, sy);
            frame.blit(&mut self.fb, bx, by);
        }
    }

    fn blit_enemy_kind_pose(
        &mut self,
        sx: i32,
        sy: i32,
        facing: Facing,
        pose_1based: u32,
        kind: crate::level::EnemyKind,
        inverted: bool,
    ) {
        use crate::level::EnemyKind;
        let table = match kind {
            EnemyKind::Ninja => self
                .assets
                .ninja
                .as_ref()
                .or(self.assets.enemy.as_ref()),
            EnemyKind::Pikeman => self
                .assets
                .pikeman
                .as_ref()
                .or(self.assets.enemy.as_ref()),
            EnemyKind::Twinstep => self
                .assets
                .twinstep
                .as_ref()
                .or(self.assets.enemy.as_ref()),
            EnemyKind::King => self
                .assets
                .king
                .as_ref()
                .or(self.assets.enemy.as_ref()),
            EnemyKind::Spirit => self
                .assets
                .spirit
                .as_ref()
                .or(self.assets.enemy.as_ref()),
            EnemyKind::LeftSouthEntrance | EnemyKind::LeftEastEntrance => {
                self.assets.leftdoor.as_ref()
            }
            EnemyKind::RightSouthEntrance | EnemyKind::RightEastEntrance => {
                self.assets.rightdoor.as_ref()
            }
            _ => self
                .assets
                .enemy
                .as_ref()
                .or(self.assets.ninja.as_ref()),
        };
        if let Some(table) = table {
            let per = (table.len() / 4).max(1);
            let idx = isosprite_frame_index(facing, pose_1based, per);
            if let Some(frame) = table.get(idx) {
                let (bx, by) = actor_blit_pos(sx, sy);
                let mode = if inverted {
                    DrawMode::Inverted
                } else {
                    DrawMode::Copy
                };
                frame.blit_mode(&mut self.fb, bx, by, mode);
                return;
            }
        }
        self.fb.fill_rect(sx - 3, sy - 12, 7, 12, true);
    }

    /// `piketip` child — same screen point as the body (`enemy:setVisible` / `setPikeUp`).
    fn blit_piketip_pose(
        &mut self,
        sx: i32,
        sy: i32,
        facing: Facing,
        pose_1based: u32,
        inverted: bool,
    ) {
        let Some(table) = self.assets.piketip.as_ref() else {
            return;
        };
        let per = (table.len() / 4).max(1);
        let idx = isosprite_frame_index(facing, pose_1based, per);
        if let Some(frame) = table.get(idx) {
            let (bx, by) = actor_blit_pos(sx, sy);
            let mode = if inverted {
                DrawMode::Inverted
            } else {
                DrawMode::Copy
            };
            frame.blit_mode(&mut self.fb, bx, by, mode);
        }
    }

    /// `chester.chestSprite` — `Images/chest` as an `isotile` (32×64, same center).
    fn blit_chest(&mut self, sx: i32, sy: i32) {
        let Some(chest) = self.assets.chest.as_ref() else {
            // Fallback diamond so a missing asset is still visible in tests / boot.
            draw_diamond(&mut self.fb, sx, sy, true);
            return;
        };
        let (bx, by) = tile_blit_pos(sx, sy);
        chest.blit(&mut self.fb, bx, by);
    }

    /// `samurai:trailBlood` stamp at this tile (`Images/trail`, NXOR, tile center).
    fn blit_trail_at(&mut self, gx: i32, gy: i32, sx: i32, sy: i32) {
        let cells: Vec<u8> = self
            .trail_stamps
            .iter()
            .filter(|t| t.x == gx && t.y == gy)
            .map(|t| t.cell)
            .collect();
        if cells.is_empty() {
            return;
        }
        let Some(table) = self.assets.trail.as_ref() else {
            return;
        };
        let (bx, by) = tile_blit_pos(sx, sy);
        for cell in cells {
            if let Some(frame) = table.get(cell as usize) {
                // Lua: NXOR into the strip; GID 3 briefly flips to Inverted first
                // then immediately overwrites with NXOR — net effect is NXOR.
                frame.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
            }
        }
    }

    fn blit_floor_blood_at(&mut self, gx: i32, gy: i32, sx: i32, sy: i32) {
        let stamps: Vec<(Option<usize>, i32)> = self
            .floor_bloods
            .iter()
            .filter(|b| b.x == gx && b.y == gy && b.delay <= 0)
            .map(|b| (b.floorspray_idx, b.delay))
            .collect();
        for (floorspray_idx, _) in stamps {
            match floorspray_idx {
                None => {
                    if let Some(bmp) = self.assets.hereblood.as_ref() {
                        let (bx, by) = tile_blit_pos(sx, sy);
                        bmp.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
                    }
                }
                Some(idx) => {
                    if let Some(table) = self.assets.floorspray.as_ref() {
                        if let Some(cell) = table.get(idx) {
                            let (bx, by) = floor_blood_blit_pos(sx, sy);
                            cell.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
                        }
                    }
                }
            }
        }
    }

    fn blit_espray(
        &mut self,
        sx: i32,
        sy: i32,
        facing: Facing,
        pose_1based: u32,
        table_i: u8,
    ) {
        let Some(table) = self.assets.espray.get(table_i as usize).and_then(|t| t.as_ref())
        else {
            return;
        };
        let per = (table.len() / 4).max(1);
        let idx = isosprite_frame_index(facing, pose_1based, per);
        if let Some(frame) = table.get(idx) {
            let (bx, by) = actor_blit_pos(sx, sy);
            frame.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }

    /// Pierce kill spray (`Images/isospray`, same 4-facing layout as espray).
    fn blit_isospray(&mut self, sx: i32, sy: i32, facing: Facing, pose_1based: u32) {
        let Some(table) = self.assets.isospray.as_ref() else {
            return;
        };
        let per = (table.len() / 4).max(1);
        let idx = isosprite_frame_index(facing, pose_1based, per);
        if let Some(frame) = table.get(idx) {
            let (bx, by) = actor_blit_pos(sx, sy);
            // Lua `samurai:bleed` pierce uses XOR; NXOR matches our other sprays.
            frame.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }

    /// Player-death drip cell (`dripsC/N/S/E/W`, oneshot, NXOR, drip center).
    fn blit_drip(&mut self, sx: i32, sy: i32, which: u8, pose_1based: u32) {
        let Some(table) = self.assets.drips.get(which as usize).and_then(|t| t.as_ref()) else {
            return;
        };
        let idx = pose_1based.saturating_sub(1) as usize;
        if let Some(frame) = table.get(idx) {
            let (bx, by) = drip_blit_pos(sx, sy);
            frame.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }

    /// Draw active floor-smoke puffs (`smoker:update` image selection + NXOR).
    fn draw_floor_smoke(&mut self, ox: i32, oy: i32) {
        let puffs: Vec<SmokePuff> = self.floorsmoke.puffs.iter().copied().collect();
        for p in puffs {
            if !p.active() {
                continue;
            }
            let anim = p.anim();
            // Visible only while `0 < frame <= #anim` (Lua gate).
            if p.frame <= 0 || p.frame as usize > anim.len() {
                continue;
            }
            let cell_1based = anim[(p.frame as usize) - 1];
            let idx = cell_1based.saturating_sub(1);
            // N/W → smoke, S/E → smoke2; S/W flip X (`smoker:update`).
            let use_smoke2 = matches!(p.dir, Facing::South | Facing::East);
            let flip_x = matches!(p.dir, Facing::South | Facing::West);
            let table = if use_smoke2 {
                self.assets.smoke2.as_ref()
            } else {
                self.assets.smoke.as_ref()
            };
            let Some(table) = table else {
                continue;
            };
            let Some(cell) = table.get(idx) else {
                continue;
            };
            let (sx, sy) = grid_to_screen(p.tile_x, p.tile_y, ox, oy);
            let (bx, by) = smoke_blit_pos(sx, sy);
            if flip_x {
                cell.blit_mode_flip_x(&mut self.fb, bx, by, DrawMode::Nxor);
            } else {
                cell.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
            }
        }
    }

    /// Place walk/kill icons along the aim path from readybar segments.
    /// Mirrors `selector:update`: for segment index `i` (1-based Lua), intermediate
    /// cells use `player + dir * (i-1)` when `i` is not the last segment and `i > 1`.
    fn draw_aim_path_icons(&mut self, ox: i32, oy: i32) {
        let segs = self.aim_plan.segments.clone();
        if segs.is_empty() {
            return;
        }
        let (dir_x, dir_y) = self.selector.step_dir();
        if dir_x == 0 && dir_y == 0 {
            return;
        }
        // Only draw intermediate icons when |extent| > 1 (Lua gate).
        if self.selector.extent_x.abs() <= 1 && self.selector.extent_y.abs() <= 1 {
            return;
        }
        // Path icons on cells between player and tip. After the leading bonus walk
        // from `movebar:addSegment`, real path segments sit at indices 1..=extent;
        // cell `c` uses `segs[c]`.
        let path_len = self.selector.extent_x.abs().max(self.selector.extent_y.abs()) as usize;
        let px = self.player_x;
        let py = self.player_y;
        for cell_i in 1..path_len {
            let seg = segs.get(cell_i).copied().unwrap_or(PathSegment::Walk);
            if seg == PathSegment::Stab {
                continue;
            }
            let gx = px + dir_x * cell_i as i32;
            let gy = py + dir_y * cell_i as i32;
            let (sx, sy) = grid_to_screen(gx, gy, ox, oy);
            if seg.is_slash() {
                self.blit_killicon(sx, sy);
            } else {
                self.blit_passicon(sx, sy);
            }
        }
    }

    fn blit_passicon(&mut self, sx: i32, sy: i32) {
        if let Some(icon) = &self.assets.passicon {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }

    fn blit_killicon(&mut self, sx: i32, sy: i32) {
        if let Some(icon) = &self.assets.killicon {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        } else {
            self.blit_passicon(sx, sy);
        }
    }

    fn blit_moveicon(&mut self, sx: i32, sy: i32) {
        if let Some(icon) = &self.assets.moveicon {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
            return;
        }
        // Fallback if icons not loaded yet — still prefer NXOR diamond outline.
        if let Some(icon) = &self.assets.passicon {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }

    fn blit_stabicon(&mut self, sx: i32, sy: i32) {
        if let Some(icon) = &self.assets.stabicon {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
            return;
        }
        self.blit_moveicon(sx, sy);
    }

    /// `selector:update` exit tip — `exiticon[onExit]` with `kDrawModeCopy` (not NXOR).
    fn blit_exiticon(&mut self, sx: i32, sy: i32, dir: Facing) {
        let idx = (dir as u8 as usize).saturating_sub(1);
        if let Some(table) = &self.assets.exiticon {
            if let Some(icon) = table.get(idx) {
                let (bx, by) = selector_icon_blit_pos(sx, sy);
                icon.blit_mode(&mut self.fb, bx, by, DrawMode::Copy);
                return;
            }
        }
        self.blit_moveicon(sx, sy);
    }

    fn blit_centerdot(&mut self, sx: i32, sy: i32) {
        if let Some(icon) = &self.assets.centerdot {
            let (bx, by) = selector_icon_blit_pos(sx, sy);
            icon.blit_mode(&mut self.fb, bx, by, DrawMode::Nxor);
        }
    }
}

/// Floor division matching Lua `math.floor(a/b)` for negative numerators.
fn floor_div(a: i32, b: i32) -> i32 {
    let q = a / b;
    let r = a % b;
    if r != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

fn draw_diamond(fb: &mut Framebuffer, sx: i32, sy: i32, filled: bool) {
    for t in 0..=16 {
        fb.set_pixel((sx - 16 + t) as u32, (sy + t / 2) as u32, true);
        fb.set_pixel((sx + 16 - t) as u32, (sy + t / 2) as u32, true);
        fb.set_pixel((sx - 16 + t) as u32, (sy + 16 - t / 2) as u32, true);
        fb.set_pixel((sx + 16 - t) as u32, (sy + 16 - t / 2) as u32, true);
    }
    if filled {
        for y in 1..15 {
            let half = if y < 8 { y } else { 16 - y };
            for x in (16 - half * 2)..(16 + half * 2) {
                fb.set_pixel((sx - 16 + x) as u32, (sy + y) as u32, true);
            }
        }
    }
}
