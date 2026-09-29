//! Full outdoor world extracted from `main.pdz` → `worldmap.luac` (Tiled 1.2.4).
//!
//! Loaded at runtime from `worldmap.bin` (`tools/worldmap_to_bin.py`) — never baked
//! into the library. Browser host fetches `www/assets/demo/worldmap.bin`; native
//! tests/examples read `data/worldmap.bin` via [`WorldMap::data_file_path`].
//! Movement classification mirrors `main.lua` `initbackground` + `Globals.walkabletiles`.

use crate::level::{Enemy, EnemyKind, Terrain};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Tile GIDs the original treats as walkable floors (`Globals.walkabletiles`).
pub const WALKABLE_TILES: &[u16] = &[
    2, 3, 4, 5, 6, 7, 8, 9, 257, 258, 259, 260, 261, 262, 263, 264,
];

/// Door tiles: `roomtiles = -1` in `main.lua` — selectable / zip-through, then exit.
pub const DOOR_TILES: &[u16] = &[127, 128, 381, 382];

/// Monk / NPC pad: `roomtiles = -2`.
pub const NPC_TILE: u16 = 253;

/// Playdate screen (`Globals.lua` / `dsp.getWidth/Height`).
const LUA_SCREEN_W: i32 = 400;
const LUA_SCREEN_H: i32 = 240;
/// `numRows = floor(screenHeight / 8) + 8`, `numCols = floor(screenWidth / 32) + 3`.
const LUA_NUM_ROWS: i32 = LUA_SCREEN_H / 8 + 8; // 38
const LUA_NUM_COLS: i32 = LUA_SCREEN_W / 32 + 3; // 15
/// `utils.lua` `cullroomtiles` BFS cap.
const CULL_MAX_ITERS: i32 = 392;

/// Boot spawn after title (`main.lua` startGame → `moveToTile(109, 182)`).
pub const START_PLAY: (i32, i32) = (109, 182);
/// Title-card spawn.
pub const START_TITLE: (i32, i32) = (109, 188);
/// Camera origin at outdoor start (`main.lua` sets `cameratile_x/y = 110, 180`).
pub const START_CAMERA: (i32, i32) = (110, 180);

/// `Globals.maxSteps` — selector extent cap.
pub const MAX_STEPS: i32 = 20;

/// Cardinal facing as in `Globals`: kNorth=1, kSouth=2, kEast=3, kWest=4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Facing {
    North = 1,
    South = 2,
    East = 3,
    West = 4,
}

impl Facing {
    pub fn offsets(self) -> (i32, i32) {
        match self {
            Facing::North => (0, -1),
            Facing::South => (0, 1),
            Facing::East => (1, 0),
            Facing::West => (-1, 0),
        }
    }

    /// `Globals` cardinals: kNorth=1 … kWest=4.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Facing::North),
            2 => Some(Facing::South),
            3 => Some(Facing::East),
            4 => Some(Facing::West),
            _ => None,
        }
    }

    pub fn from_delta(dx: i32, dy: i32) -> Option<Self> {
        let ax = dx.abs();
        let ay = dy.abs();
        if ax == 0 && ay == 0 {
            return None;
        }
        if ax > ay {
            if dx < 0 {
                Some(Facing::West)
            } else {
                Some(Facing::East)
            }
        } else if dy > 0 {
            Some(Facing::South)
        } else {
            Some(Facing::North)
        }
    }

    /// `utils.lua` `oppositeFacing`.
    pub fn opposite(self) -> Self {
        match self {
            Facing::North => Facing::South,
            Facing::South => Facing::North,
            Facing::East => Facing::West,
            Facing::West => Facing::East,
        }
    }
}

/// `utils.lua` `facePlayer(ex, ey)` — dominant-axis facing toward the player.
pub fn face_player(ex: i32, ey: i32, px: i32, py: i32) -> Facing {
    let dx = px - ex;
    let dy = py - ey;
    let absx = dx.abs();
    let absy = dy.abs();
    if absx > absy {
        if dx > 0 {
            Facing::East
        } else {
            Facing::West
        }
    } else if dy > 0 {
        Facing::South
    } else {
        Facing::North
    }
}

/// `enemy.lua` `faceforblock` — twinstep room-attach / spawn facing.
///
/// If the player is on an adjacent row **or** column (`abs(dx)==1` or
/// `abs(dy)==1`), face along the matching axis (East/West checked before
/// South/North, so a diagonal-adjacent player faces on X). Otherwise
/// [`face_player`].
pub fn face_for_block(ex: i32, ey: i32, px: i32, py: i32) -> Facing {
    let dx = px - ex;
    let dy = py - ey;
    if dx.abs() == 1 || dy.abs() == 1 {
        if dx == 1 {
            Facing::East
        } else if dx == -1 {
            Facing::West
        } else if dy == 1 {
            Facing::South
        } else if dy == -1 {
            Facing::North
        } else {
            // abs==1 on one axis with dx/dy==0 on that branch is unreachable for ints;
            // keep a deterministic fallback matching facePlayer.
            face_player(ex, ey, px, py)
        }
    } else {
        face_player(ex, ey, px, py)
    }
}

/// Exit / NPC object from the Tiled Exits layer.
#[derive(Debug, Clone)]
pub struct MapExit {
    pub id: i32,
    pub x: i32,
    pub y: i32,
    pub nx: i32,
    pub ny: i32,
    pub sx: i32,
    pub sy: i32,
    pub entrance: i32,
    pub face: i32,
    pub heal: i32,
    pub reverse: bool,
    /// 0 = exit, 1 = npc, 2 = other
    pub kind: u8,
    pub dialog: String,
    pub teleports: Vec<i32>,
}

#[derive(Debug, Clone, Copy)]
pub struct MapChest {
    pub x: i32,
    pub y: i32,
    pub gid: i32,
}

/// 256×256 outdoor map + exits + chests.
#[derive(Debug, Clone)]
pub struct WorldMap {
    pub width: u16,
    pub height: u16,
    /// Row-major tile GIDs (`Outside Layer`).
    pub tiles: Vec<u16>,
    /// Row-major enemy spawn markers (after the game's `gid - 512` fix).
    pub enemies: Vec<u8>,
    pub exits: Vec<MapExit>,
    pub chests: Vec<MapChest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldMapError {
    BadMagic,
    UnsupportedVersion(u32),
    Truncated(&'static str),
    SizeMismatch,
    /// Disk read failed (`from_path`).
    Io,
}

impl std::fmt::Display for WorldMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldMapError::BadMagic => write!(f, "worldmap: bad magic"),
            WorldMapError::UnsupportedVersion(v) => {
                write!(f, "worldmap: unsupported version {v}")
            }
            WorldMapError::Truncated(w) => write!(f, "worldmap: truncated ({w})"),
            WorldMapError::SizeMismatch => write!(f, "worldmap: size mismatch"),
            WorldMapError::Io => write!(f, "worldmap: failed to read file"),
        }
    }
}

impl std::error::Error for WorldMapError {}

impl WorldMap {
    /// 0×0 placeholder until the host loads a real `worldmap.bin`.
    pub fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            tiles: Vec::new(),
            enemies: Vec::new(),
            exits: Vec::new(),
            chests: Vec::new(),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Checkout path used by native tests / examples: `crates/zipper-core/data/worldmap.bin`.
    pub fn data_file_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/worldmap.bin")
    }

    /// Read and parse a `ZMAP` file from disk (tests, examples, native hosts).
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, WorldMapError> {
        let data = std::fs::read(path.as_ref()).map_err(|_| WorldMapError::Io)?;
        Self::from_bytes(&data)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, WorldMapError> {
        if data.len() < 12 {
            return Err(WorldMapError::Truncated("header"));
        }
        if &data[0..4] != b"ZMAP" {
            return Err(WorldMapError::BadMagic);
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != 1 {
            return Err(WorldMapError::UnsupportedVersion(version));
        }
        let width = u16::from_le_bytes(data[8..10].try_into().unwrap());
        let height = u16::from_le_bytes(data[10..12].try_into().unwrap());
        let n = (width as usize).checked_mul(height as usize).unwrap_or(0);
        let mut off = 12;

        let tiles_bytes = n * 2;
        if data.len() < off + tiles_bytes {
            return Err(WorldMapError::Truncated("tiles"));
        }
        let mut tiles = Vec::with_capacity(n);
        for i in 0..n {
            let b = off + i * 2;
            tiles.push(u16::from_le_bytes([data[b], data[b + 1]]));
        }
        off += tiles_bytes;

        if data.len() < off + n {
            return Err(WorldMapError::Truncated("enemies"));
        }
        let enemies = data[off..off + n].to_vec();
        off += n;

        let n_exits = read_u32(data, &mut off)?;
        let mut exits = Vec::with_capacity(n_exits as usize);
        for _ in 0..n_exits {
            exits.push(read_exit(data, &mut off)?);
        }

        let n_chests = read_u32(data, &mut off)?;
        let mut chests = Vec::with_capacity(n_chests as usize);
        for _ in 0..n_chests {
            let x = read_i32(data, &mut off)?;
            let y = read_i32(data, &mut off)?;
            let gid = read_i32(data, &mut off)?;
            chests.push(MapChest { x, y, gid });
        }

        if tiles.len() != n || enemies.len() != n {
            return Err(WorldMapError::SizeMismatch);
        }

        Ok(Self {
            width,
            height,
            tiles,
            enemies,
            exits,
            chests,
        })
    }

    #[inline]
    pub fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some((y as usize) * (self.width as usize) + (x as usize))
    }

    pub fn tile(&self, x: i32, y: i32) -> u16 {
        self.index(x, y).map(|i| self.tiles[i]).unwrap_or(0)
    }

    pub fn enemy_marker(&self, x: i32, y: i32) -> u8 {
        self.index(x, y).map(|i| self.enemies[i]).unwrap_or(0)
    }

    pub fn is_walkable_tile(gid: u16) -> bool {
        WALKABLE_TILES.contains(&gid)
    }

    pub fn is_door_tile(gid: u16) -> bool {
        DOOR_TILES.contains(&gid)
    }

    /// Mirror of `main.lua` roomtiles assignment for one cell.
    /// `Some(0)` floor, `Some(-1)` door, `Some(-2)` npc, `None` = not in room / blocked.
    pub fn roomtile_flag(&self, x: i32, y: i32) -> Option<i8> {
        let gid = self.tile(x, y);
        if gid == 0 {
            return None;
        }
        if Self::is_door_tile(gid) {
            return Some(-1);
        }
        if gid == NPC_TILE {
            return Some(-2);
        }
        if Self::is_walkable_tile(gid) {
            return Some(0);
        }
        None
    }

    /// Selector may step onto a cell when `roomtiles[i] ~= nil and roomtiles[i] <= 0`.
    pub fn selector_can_step(&self, x: i32, y: i32) -> bool {
        matches!(self.roomtile_flag(x, y), Some(f) if f <= 0)
    }

    /// Draw / classify terrain. Doors stay drawable and **walkable** (exit trigger is separate).
    pub fn terrain_at(&self, x: i32, y: i32) -> Terrain {
        let gid = self.tile(x, y);
        if gid == 0 {
            return Terrain::Void;
        }
        if Self::is_door_tile(gid) {
            return Terrain::Door { tile: gid };
        }
        if gid == NPC_TILE {
            return Terrain::Npc { tile: gid };
        }
        if Self::is_walkable_tile(gid) {
            Terrain::Floor { tile: gid }
        } else {
            Terrain::Wall { tile: gid }
        }
    }

    pub fn exit_at(&self, x: i32, y: i32) -> Option<&MapExit> {
        self.exits
            .iter()
            .find(|e| e.kind == 0 && e.x == x && e.y == y)
    }

    /// NPC pad object (`type = "npc"`, kind == 1) at tile `(x, y)`.
    pub fn npc_at(&self, x: i32, y: i32) -> Option<&MapExit> {
        self.exits
            .iter()
            .find(|e| e.kind == 1 && e.x == x && e.y == y)
    }

    pub fn exit_by_id(&self, id: i32) -> Option<&MapExit> {
        self.exits.iter().find(|e| e.id == id)
    }

    /// Camera destination after stepping onto an exit, per `samurai:playerMoved`.
    pub fn exit_camera_for(&self, exit: &MapExit, facing: Facing) -> (i32, i32) {
        match facing {
            Facing::North | Facing::East => (exit.nx, exit.ny),
            Facing::South | Facing::West => (exit.sx, exit.sy),
        }
    }

    /// Iso screen footprint + `cullroomtiles` for one camera / player pose
    /// (`main.lua` `initbackground` → `utils.lua` `cullroomtiles`).
    ///
    /// This — not a rectangle around the camera — is what `findlocalenemies`
    /// iterates. Enemies outside this set stay in `globalenemies` with
    /// updates disabled (they do not path or attack).
    pub fn roomtiles_for(&self, camera: (i32, i32), player: (i32, i32)) -> HashSet<(i32, i32)> {
        let (cam_x, cam_y) = camera;
        let mut raw: HashSet<(i32, i32)> = HashSet::new();
        let mut floors: HashSet<(i32, i32)> = HashSet::new();
        let mut doors_npcs: HashSet<(i32, i32)> = HashSet::new();

        for row in 1..=LUA_NUM_ROWS {
            for col in 1..=LUA_NUM_COLS {
                // Lua: screencell_x = -14 + col + floor(row/2)
                //      screencell_y = 0 - col + ceil(row/2)
                //      mapcell = screencell + cameratile - 2
                let screencell_x = -14 + col + row / 2;
                let screencell_y = -col + (row + 1) / 2; // ceil(row/2)
                let mapcell_x = screencell_x + cam_x - 2;
                let mapcell_y = screencell_y + cam_y - 2;
                // Lua skips mapcell < 1 or >= map size (1-based in the dump;
                // our map is 0-based — keep the same numeric bounds).
                if mapcell_x < 1
                    || mapcell_y < 1
                    || mapcell_x >= self.width as i32
                    || mapcell_y >= self.height as i32
                {
                    continue;
                }
                match self.roomtile_flag(mapcell_x, mapcell_y) {
                    Some(0) => {
                        raw.insert((mapcell_x, mapcell_y));
                        floors.insert((mapcell_x, mapcell_y));
                    }
                    Some(-1) | Some(-2) => {
                        raw.insert((mapcell_x, mapcell_y));
                        doors_npcs.insert((mapcell_x, mapcell_y));
                    }
                    _ => {}
                }
            }
        }

        // BFS from the player over floor cells (`roomtiles >= 0`), max 392 iters.
        let mut visited: HashSet<(i32, i32)> = HashSet::new();
        let mut frontier: Vec<(i32, i32)> = vec![player];
        let mut iters = CULL_MAX_ITERS;
        while let Some((cx, cy)) = frontier.pop() {
            if iters <= 0 {
                break;
            }
            iters -= 1;
            for (nx, ny) in [(cx + 1, cy), (cx - 1, cy), (cx, cy + 1), (cx, cy - 1)] {
                if floors.contains(&(nx, ny)) && !visited.contains(&(nx, ny)) {
                    frontier.push((nx, ny));
                }
            }
            visited.insert((cx, cy));
        }

        // Keep visited floors + every door/npc that was in the iso footprint
        // (Lua: `visited[i] == nil and roomtiles[i] ~= -1 and ~= -2` → drop).
        let mut kept = visited;
        kept.extend(doors_npcs);
        // Only cells that were actually in the iso footprint.
        kept.retain(|c| raw.contains(c));
        kept
    }

    /// Deterministic placed enemies on the given `roomtiles` set.
    /// Marker `> 9` → `type = marker - 9` (`utils.lua`). Skips `kLeftDoor`/`kRightDoor`
    /// (8/9 → markers 17/18), which spawn the separate `door` class.
    pub fn fixed_enemies_in_tiles(&self, tiles: &HashSet<(i32, i32)>) -> Vec<Enemy> {
        let mut out = Vec::new();
        for &(x, y) in tiles {
            let m = self.enemy_marker(x, y);
            if m <= 9 {
                continue;
            }
            let etype = m as i32 - 9;
            if let Some(kind) = enemy_kind_from_type(etype) {
                let facing = entrance_facing(kind).unwrap_or(Facing::South);
                out.push(Enemy::new(kind, x, y, facing as u8));
            }
        }
        out
    }

    /// Spirit-room door markers in `roomtiles` (`kLeftDoor=8` / `kRightDoor=9`).
    /// Returns `(x, y, left)` where `left == 0` is kLeftDoor, `1` is kRightDoor
    /// (`utils.lua` `door(0|1, …)`).
    pub fn spirit_door_markers_in_tiles(&self, tiles: &HashSet<(i32, i32)>) -> Vec<(i32, i32, u8)> {
        let mut out = Vec::new();
        for &(x, y) in tiles {
            let m = self.enemy_marker(x, y);
            if m <= 9 {
                continue;
            }
            let etype = m as i32 - 9;
            match etype {
                8 => out.push((x, y, 0)),
                9 => out.push((x, y, 1)),
                _ => {}
            }
        }
        out.sort_by_key(|&(x, y, _)| (y, x));
        out
    }

    /// Weight spawn tiles (`1..=9`) on the given `roomtiles` set.
    /// Sorted by world index so spawn RNG stays deterministic across HashSet order.
    pub fn weight_spawn_tiles_in_tiles(
        &self,
        tiles: &HashSet<(i32, i32)>,
    ) -> Vec<(i32, i32, u8)> {
        let mut out = Vec::new();
        for &(x, y) in tiles {
            let m = self.enemy_marker(x, y);
            if (1..=9).contains(&m) {
                out.push((x, y, m));
            }
        }
        out.sort_by_key(|&(x, y, _)| y.wrapping_mul(self.width as i32).wrapping_add(x));
        out
    }
}

/// `Globals.enemySets` — per-difficulty lists of encounter compositions (`kSwordsman`=1…).
/// Indexed by difficulty 1..=6 (Lua `currentDifficulty`).
pub const ENEMY_SETS: &[&[&[u8]]] = &[
    // difficulty 1
    &[&[1], &[1, 1], &[1, 1], &[1, 1]],
    // 2
    &[&[2], &[1, 1], &[1, 1], &[1, 1, 1]],
    // 3
    &[&[1, 2], &[1, 2], &[1, 1, 1], &[2, 1], &[2, 1, 1]],
    // 4
    &[&[2, 2], &[4], &[2, 1, 2], &[2, 1, 1, 1], &[4, 1]],
    // 5
    &[
        &[4, 2, 1],
        &[2, 2, 1, 1],
        &[2, 1, 2],
        &[1, 1, 1, 1, 1],
        &[4, 2, 1],
        &[4, 4],
    ],
    // 6
    &[
        &[4, 4, 4, 4],
        &[4, 1, 2, 2],
        &[4, 1, 1, 1],
        &[2, 2, 2],
        &[3, 1],
        &[3, 2],
    ],
];

/// `Globals.scorePerLevel` — kills needed to advance `currentDifficulty`.
pub const SCORE_PER_LEVEL: &[i32] = &[5, 20, 45, 80, 120, 160];

fn enemy_kind_from_type(etype: i32) -> Option<EnemyKind> {
    match etype {
        1 => Some(EnemyKind::Swordsman),
        2 => Some(EnemyKind::Pikeman),
        3 => Some(EnemyKind::Ninja),
        4 => Some(EnemyKind::Twinstep),
        5 => Some(EnemyKind::King),
        6 => Some(EnemyKind::Spirit),
        7 => Some(EnemyKind::Reverse),
        // 8/9 = `kLeftDoor` / `kRightDoor` (spirit-room `door` class) — separate path.
        10 => Some(EnemyKind::LeftSouthEntrance),
        11 => Some(EnemyKind::RightSouthEntrance),
        12 => Some(EnemyKind::LeftEastEntrance),
        13 => Some(EnemyKind::RightEastEntrance),
        _ => None,
    }
}

/// Facing set in `enemy:init` for entrance props (not `cameFromDir`).
fn entrance_facing(kind: EnemyKind) -> Option<Facing> {
    match kind {
        EnemyKind::LeftSouthEntrance | EnemyKind::RightSouthEntrance => Some(Facing::South),
        EnemyKind::LeftEastEntrance | EnemyKind::RightEastEntrance => Some(Facing::East),
        _ => None,
    }
}

/// Seedable gameplay RNG (`math.random` / `randomseed` stand-in).
///
/// xoshiro256** (Blackman/Vigna), seeded like Lua 5.4 `math.randomseed(n)`:
/// state `[n, 0xff, 0, 0]` then 16 discarded steps so nearby integer seeds
/// diversify (the old Numerical Recipes LCG made consecutive `#seed=N` values
/// alternate between only two chest pads).
///
/// Deterministic and sync-friendly for wasm — not a CSPRNG, and not claimed
/// bit-identical to a physical Playdate.
#[derive(Debug, Clone)]
pub struct SpawnRng {
    s: [u64; 4],
}

impl SpawnRng {
    /// `math.randomseed(seed)` with the optional second seed left at 0.
    pub fn new(seed: u32) -> Self {
        let mut rng = Self {
            s: [u64::from(seed), 0xff, 0, 0],
        };
        for _ in 0..16 {
            let _ = rng.next_u64();
        }
        rng
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Unbiased projection into `[0, n]` (Lua 5.4 `project`).
    fn project(&mut self, mut ran: u32, n: u32) -> u32 {
        if n & n.wrapping_add(1) == 0 {
            return ran & n;
        }
        let mut lim = n;
        lim |= lim >> 1;
        lim |= lim >> 2;
        lim |= lim >> 4;
        lim |= lim >> 8;
        lim |= lim >> 16;
        loop {
            ran &= lim;
            if ran <= n {
                return ran;
            }
            ran = self.next_u32();
        }
    }

    /// Inclusive `math.random(1, n)`.
    pub fn gen_1_to(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let ran = self.next_u32();
        1 + self.project(ran, n - 1)
    }

    /// `math.random()` — float in [0, 1).
    pub fn gen_unit(&mut self) -> f32 {
        // Top 24 bits → f32 mantissa; matches common [0,1) construction.
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        if items.len() < 2 {
            return;
        }
        for i in (1..items.len()).rev() {
            let ran = self.next_u32();
            let j = self.project(ran, i as u32) as usize;
            items.swap(i, j);
        }
    }
}

/// Port of `utils.lua` `findlocalenemies(cameFromDir)`.
///
/// Membership is the Lua `roomtiles` set (iso strip + `cullroomtiles`), **not**
/// a rectangle around the camera. Globals outside that set stay dormant.
///
/// 1. Reuse any `global_enemies` whose tile is in `roomtiles`.
/// 2. Else spawn fixed markers (`> 9`) and roll weight tiles (`1..=9`) via `enemySets`.
///
/// Twinstep facing on attach/spawn uses [`face_for_block`] (`enemy.lua`), not
/// `cameFromDir`. Weight-tile facing also copies a Lua quirk: the phase-3
/// `if type == kTwinstep` check reads the leftover fixed-marker `type` from
/// the phase-2 scan (not `newenemy.etype`) — see the phase-3 comment below.
///
/// Newly created enemies are appended to `global_enemies`. Returns the local list.
pub fn find_local_enemies(
    map: &WorldMap,
    roomtiles: &HashSet<(i32, i32)>,
    global_enemies: &mut Vec<Enemy>,
    random_seed: u32,
    camera: (i32, i32),
    difficulty: usize,
    came_from: Facing,
    player: (i32, i32),
    // Shared process RNG (`math.random` stand-in). Reseeded here like Lua
    // `math.randomseed(random_seed + cameratile_x + cameratile_y)` so later
    // callers (`chester:reset`) see an advanced stream.
    rng: &mut SpawnRng,
) -> Vec<Enemy> {
    // Lua: math.randomseed(random_seed + cameratile_x + cameratile_y)
    *rng = SpawnRng::new(
        random_seed
            .wrapping_add(camera.0 as u32)
            .wrapping_add(camera.1 as u32),
    );

    // --- Phase 1: already-spawned globals that sit in this room ---
    // Lua inserts the *same* enemy object into localenemies; facing/alive mutate
    // both lists. We copy into locals but write facing back onto the global slot.
    let mut local: Vec<Enemy> = Vec::new();
    let mut found_spawned = false;
    let mut ninja_count = 0i32;
    let mut phase1_idxs: Vec<usize> = Vec::new();
    for (gi, g) in global_enemies.iter().enumerate() {
        if !roomtiles.contains(&(g.x, g.y)) {
            continue;
        }
        found_spawned = true;
        phase1_idxs.push(gi);
        if matches!(g.kind, EnemyKind::Ninja) {
            ninja_count += 1;
        }
    }
    for gi in phase1_idxs {
        let g = &mut global_enemies[gi];
        if g.alive {
            if g.kind.is_inanimate() {
                // Entrance facing is fixed at init (`enemy:init`); never rewrite.
            } else if matches!(g.kind, EnemyKind::Twinstep) {
                g.facing = face_for_block(g.x, g.y, player.0, player.1) as u8;
            } else if !matches!(g.kind, EnemyKind::King) {
                // `enemy:setFacing(cameFromDir)` — also rotates a lowered pike tip.
                g.facing = came_from as u8;
            }
        }
        if matches!(g.kind, EnemyKind::King | EnemyKind::Spirit) {
            // King stays south; Spirit `setFacing` always forces south + idle.
            g.facing = Facing::South as u8;
        }
        // Match Lua `setFacing` / `setVisible` re-attach: tip tile follows facing.
        // Without this, body frame faces `came_from` while a stale tip still blocks
        // the old offset (visual south, collision still west).
        g.sync_pike_tip();
        // Ensure the slot knows its own index (older saves / tests may omit it).
        g.global_index = Some(gi);
        local.push(*g);
    }
    if found_spawned {
        let _ = ninja_count; // used only when spawning new rooms
        return local;
    }

    // --- Phase 2: fixed markers ---
    // Lua: `local type = 0` then `type = layers[2].data[i] - 9` for every cell
    // with marker `> 7`. The final value leaks into phase 3's twinstep check.
    // Lua's `next, roomtiles` order is hash-unstable; we walk markers sorted by
    // world index so the leftover is deterministic while keeping the same leak.
    let mut last_marker_etype: i32 = 0;
    {
        let mut marker_cells: Vec<(i32, i32, u8)> = Vec::new();
        for &(x, y) in roomtiles {
            let m = map.enemy_marker(x, y);
            if m > 7 {
                marker_cells.push((x, y, m));
            }
        }
        marker_cells
            .sort_by_key(|&(x, y, _)| y.wrapping_mul(map.width as i32).wrapping_add(x));
        for &(_, _, m) in &marker_cells {
            last_marker_etype = m as i32 - 9;
        }
    }

    // Lua: `numCreated` only increments for `type < kLeftDoor` combatants.
    let mut num_created = 0i32;
    for e in map.fixed_enemies_in_tiles(roomtiles) {
        let mut e = e;
        if matches!(e.kind, EnemyKind::Ninja) {
            ninja_count += 1;
        }
        if e.kind.is_inanimate() {
            // Facing already set in `fixed_enemies_in_tiles` / `enemy:init`.
        } else if matches!(e.kind, EnemyKind::Twinstep) {
            e.facing = face_for_block(e.x, e.y, player.0, player.1) as u8;
        } else {
            e.facing = came_from as u8;
            num_created += 1;
        }
        if matches!(e.kind, EnemyKind::King | EnemyKind::Spirit) {
            // King stays south; Spirit `setFacing` always forces south + idle.
            e.facing = Facing::South as u8;
        }
        e.sync_pike_tip();
        // Lua: `newenemy.globalindex = #globalenemies` after insert (1-based length).
        // Rust stores 0-based index into `global_enemies`.
        e.global_index = Some(global_enemies.len());
        global_enemies.push(e);
        local.push(e);
    }

    // --- Phase 3: weight tiles + enemySets ---
    let mut weight_tiles = map.weight_spawn_tiles_in_tiles(roomtiles);
    if weight_tiles.is_empty() {
        return local;
    }
    let num_random_tiles = weight_tiles.len() as i32;

    let diff_i = difficulty.saturating_sub(1).min(ENEMY_SETS.len() - 1);
    let sets = ENEMY_SETS[diff_i];
    let set_i = (rng.gen_1_to(sets.len() as u32) as usize) - 1;
    let composition = sets[set_i];
    let num_needed = (composition.len() as i32 - num_created).max(0);
    let take = num_needed.min(num_random_tiles).max(0) as usize;

    let mut spawn_types: Vec<u8> = Vec::new();
    for i in 0..take {
        let mut en_index = composition[i];
        if en_index == 3 {
            // kNinja — at most one per room (`utils.lua`).
            if ninja_count > 0 {
                en_index = rng.gen_1_to(2) as u8; // swordsman or pikeman
                if rng.gen_unit() < 0.3 {
                    en_index = 4; // twinstep
                }
            } else {
                ninja_count += 1;
            }
        }
        spawn_types.push(en_index);
    }

    rng.shuffle(&mut weight_tiles);
    let mut used = vec![false; weight_tiles.len()];
    let mut shuffle_index = 0usize;

    // Lua always constructs with `set[1]` (first composition entry), once per set slot.
    let spawn_kind = spawn_types
        .first()
        .copied()
        .and_then(|t| enemy_kind_from_type(t as i32));
    let Some(kind) = spawn_kind else {
        return local;
    };

    for _ in 0..spawn_types.len() {
        // Keep rolling until a free weight tile accepts (`c*c/30`), matching Lua's
        // unbounded `while keeplooking`. Cap iterations so a pathological seed cannot hang.
        let mut placed = false;
        for _attempt in 0..(weight_tiles.len() * 64).max(64) {
            if !used[shuffle_index] {
                let (tx, ty, c) = weight_tiles[shuffle_index];
                let threshold = (c as f32) * (c as f32) / 30.0;
                if rng.gen_unit() < threshold {
                    // Lua quirk (`utils.lua` ~345): `if type == kTwinstep` uses the
                    // leftover phase-2 `type` (last marker>7's etype), NOT
                    // `newenemy.etype`. So weight spawns face via faceforblock only
                    // when that leftover was a twinstep — even if this spawn is a
                    // swordsman/pikeman, and *not* when this spawn is a twinstep but
                    // the room's last fixed marker was something else.
                    let facing = if last_marker_etype == 4 {
                        face_for_block(tx, ty, player.0, player.1) as u8
                    } else if matches!(kind, EnemyKind::King) {
                        Facing::South as u8
                    } else {
                        came_from as u8
                    };
                    let mut e = Enemy::new(kind, tx, ty, facing);
                    e.sync_pike_tip();
                    e.global_index = Some(global_enemies.len());
                    used[shuffle_index] = true;
                    global_enemies.push(e);
                    local.push(e);
                    placed = true;
                    shuffle_index = (shuffle_index + 1) % weight_tiles.len();
                    break;
                }
            }
            shuffle_index = (shuffle_index + 1) % weight_tiles.len();
        }
        if !placed {
            break;
        }
    }

    local
}

/// Locals whose tile is in the current `roomtiles` set — phase-1 of
/// `findlocalenemies` only (no new weight rolls). Used for outdoor streaming
/// without a room change; off-screen globals stay out of AI.
pub fn locals_from_globals(
    roomtiles: &HashSet<(i32, i32)>,
    global_enemies: &[Enemy],
) -> Vec<Enemy> {
    let mut local = Vec::new();
    for g in global_enemies {
        if roomtiles.contains(&(g.x, g.y)) {
            local.push(*g);
        }
    }
    local
}

fn read_u32(data: &[u8], off: &mut usize) -> Result<u32, WorldMapError> {
    if *off + 4 > data.len() {
        return Err(WorldMapError::Truncated("u32"));
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_i32(data: &[u8], off: &mut usize) -> Result<i32, WorldMapError> {
    Ok(read_u32(data, off)? as i32)
}

fn read_u16(data: &[u8], off: &mut usize) -> Result<u16, WorldMapError> {
    if *off + 2 > data.len() {
        return Err(WorldMapError::Truncated("u16"));
    }
    let v = u16::from_le_bytes(data[*off..*off + 2].try_into().unwrap());
    *off += 2;
    Ok(v)
}

fn read_u8(data: &[u8], off: &mut usize) -> Result<u8, WorldMapError> {
    if *off >= data.len() {
        return Err(WorldMapError::Truncated("u8"));
    }
    let v = data[*off];
    *off += 1;
    Ok(v)
}

fn read_exit(data: &[u8], off: &mut usize) -> Result<MapExit, WorldMapError> {
    let id = read_i32(data, off)?;
    let x = read_i32(data, off)?;
    let y = read_i32(data, off)?;
    let nx = read_i32(data, off)?;
    let ny = read_i32(data, off)?;
    let sx = read_i32(data, off)?;
    let sy = read_i32(data, off)?;
    let entrance = read_i32(data, off)?;
    let face = read_i32(data, off)?;
    let heal = read_i32(data, off)?;
    let reverse = read_u8(data, off)? != 0;
    let kind = read_u8(data, off)?;
    let dlg_len = read_u16(data, off)? as usize;
    if *off + dlg_len > data.len() {
        return Err(WorldMapError::Truncated("dialog"));
    }
    let dialog = String::from_utf8_lossy(&data[*off..*off + dlg_len]).into_owned();
    *off += dlg_len;
    let n_tp = read_u16(data, off)? as usize;
    let mut teleports = Vec::with_capacity(n_tp);
    for _ in 0..n_tp {
        teleports.push(read_i32(data, off)?);
    }
    Ok(MapExit {
        id,
        x,
        y,
        nx,
        ny,
        sx,
        sy,
        entrance,
        face,
        heal,
        reverse,
        kind,
        dialog,
        teleports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_data_map() -> WorldMap {
        WorldMap::from_path(WorldMap::data_file_path()).expect("data/worldmap.bin")
    }

    #[test]
    fn data_file_map_loads() {
        let m = load_data_map();
        assert_eq!(m.width, 256);
        assert_eq!(m.height, 256);
        assert!(WorldMap::is_walkable_tile(m.tile(START_PLAY.0, START_PLAY.1)));
        assert!(m.selector_can_step(START_PLAY.0, START_PLAY.1));
        // First outdoor door north of spawn.
        assert!(WorldMap::is_door_tile(m.tile(109, 170)));
        assert!(m.selector_can_step(109, 170));
    }

    /// `cullroomtiles` keeps the start-room door but drops floors past it
    /// (enemy path fence). Door `(109,170)` stays; far floor `(109,168)` goes.
    #[test]
    fn start_roomtiles_drop_floors_past_door() {
        let m = load_data_map();
        let tiles = m.roomtiles_for(START_CAMERA, START_PLAY);
        assert!(
            tiles.contains(&(109, 170)),
            "north door pad must stay in roomtiles (-1)"
        );
        assert!(
            tiles.contains(&(110, 170)),
            "north door pad twin must stay"
        );
        assert!(
            !tiles.contains(&(109, 168)),
            "floor past the door must be culled (next room)"
        );
        assert!(
            !tiles.contains(&(109, 169)),
            "floor immediately north of door must be culled"
        );
        assert!(
            tiles.contains(&(109, 171)),
            "floor south of door (this room) must remain"
        );
    }

    /// Consecutive integer seeds must diversify the first `gen_1_to(10)` draw
    /// (the old LCG only alternated between two residues).
    #[test]
    fn spawn_rng_consecutive_seeds_diversify() {
        let mut seen = [false; 10];
        let mut distinct = 0usize;
        for seed in 0u32..64 {
            let mut rng = SpawnRng::new(seed);
            let idx = (rng.gen_1_to(10) - 1) as usize;
            if !seen[idx] {
                seen[idx] = true;
                distinct += 1;
            }
        }
        assert!(
            distinct > 2,
            "expected >2 distinct first rolls over seeds 0..63, got {distinct}"
        );
    }

    /// Outdoor start: Lua `roomtiles` has no combatants. A rectangular half-w=16
    /// window falsely includes weight tiles at (95,164) that are off-screen —
    /// those must NOT enter `globalenemies` (that poisoned later rooms).
    #[test]
    fn start_roomtiles_have_no_combatants() {
        let m = load_data_map();
        let tiles = m.roomtiles_for(START_CAMERA, START_PLAY);
        assert!(
            !tiles.is_empty(),
            "start roomtiles footprint must include the player pad"
        );
        assert!(tiles.contains(&START_PLAY));
        assert!(
            m.weight_spawn_tiles_in_tiles(&tiles).is_empty(),
            "start screen has no weight spawns inside the iso footprint"
        );
        assert!(
            m.fixed_enemies_in_tiles(&tiles).is_empty(),
            "start screen has no fixed combatants"
        );
        // Off-screen weight that the old rectangle wrongly included:
        assert!(
            !tiles.contains(&(95, 164)),
            "(95,164) is outside Lua roomtiles and must not spawn"
        );
    }

    /// First interior camera (`exit` 35 → cam 108,162): fixed swordsman at
    /// (108,160) is on the player's connected floor component.
    #[test]
    fn first_interior_spawns_fixed_swordsman() {
        let m = load_data_map();
        let cam = (108, 162);
        let player = (109, 169); // one step north of the door
        let tiles = m.roomtiles_for(cam, player);
        let fixed = m.fixed_enemies_in_tiles(&tiles);
        assert!(
            fixed.iter().any(|e| e.x == 108 && e.y == 160),
            "fixed swordsman at (108,160) must be in roomtiles, got {fixed:?}"
        );

        let mut globals = Vec::new();
        let mut rng = SpawnRng::new(0x5EED_2026);
        let locals = find_local_enemies(
            &m,
            &tiles,
            &mut globals,
            0x5EED_2026,
            cam,
            1,
            Facing::South,
            player,
            &mut rng,
        );
        assert_eq!(locals.len(), 1);
        assert_eq!(locals[0].x, 108);
        assert_eq!(locals[0].y, 160);
        assert!(locals[0].alive);
        assert_eq!(locals[0].global_index, Some(0));

        // Off-screen / other-component globals must not be treated as local.
        // Plant a fake global on a weight tile that is in the iso strip but
        // culled away from the player — it must not early-return / path.
        let mut dormant = Enemy::new(EnemyKind::Swordsman, 109, 150, Facing::South as u8);
        dormant.global_index = Some(1);
        globals.push(dormant);
        let again = find_local_enemies(
            &m,
            &tiles,
            &mut globals,
            0x5EED_2026,
            cam,
            1,
            Facing::North,
            player,
            &mut rng,
        );
        assert_eq!(
            again.len(),
            1,
            "only the on-screen fixed enemy is local; culled-component globals stay dormant"
        );
        assert_eq!(again[0].x, 108);
        assert_eq!(again[0].y, 160);
    }

    /// `enemy.lua` `faceforblock`: adjacent axis → face that way; else `facePlayer`.
    #[test]
    fn face_for_block_matches_lua() {
        // Adjacent column → East / West (checked before Y).
        assert_eq!(face_for_block(10, 10, 11, 10), Facing::East);
        assert_eq!(face_for_block(10, 10, 9, 10), Facing::West);
        // Adjacent row → South / North.
        assert_eq!(face_for_block(10, 10, 10, 11), Facing::South);
        assert_eq!(face_for_block(10, 10, 10, 9), Facing::North);
        // Diagonal-adjacent: X branch wins (`dx == ±1` before `dy`).
        assert_eq!(face_for_block(10, 10, 11, 11), Facing::East);
        assert_eq!(face_for_block(10, 10, 9, 11), Facing::West);
        // Far: dominant-axis facePlayer.
        assert_eq!(face_for_block(10, 10, 20, 12), Facing::East);
        assert_eq!(face_for_block(10, 10, 12, 0), Facing::North);
        assert_eq!(face_for_block(10, 10, 10, 20), Facing::South);
    }

    /// Castle twin room (teleport 1104 → cam 117,24): four fixed twinsteps face
    /// via `faceforblock` toward the landing pad, not `cameFromDir`.
    #[test]
    fn castle_twins_face_for_block_on_spawn() {
        let m = load_data_map();
        let cam = (117, 24);
        // Landing tile of exit 1104 (outdoor gate teleport 641 → 1104).
        let player = (117, 32);
        let tiles = m.roomtiles_for(cam, player);
        let expected = [
            ((112, 24), Facing::South),
            ((114, 22), Facing::South),
            ((118, 21), Facing::West),
            ((123, 24), Facing::South),
        ];
        for &((x, y), _) in &expected {
            assert!(
                tiles.contains(&(x, y)),
                "twin marker ({x},{y}) must be in roomtiles"
            );
            assert_eq!(m.enemy_marker(x, y), 13, "marker 13 = kTwinstep");
        }

        let mut globals = Vec::new();
        let mut rng = SpawnRng::new(0x5EED_2026);
        // came_from deliberately wrong (East) so a stub would disagree with faceforblock.
        let locals = find_local_enemies(
            &m,
            &tiles,
            &mut globals,
            0x5EED_2026,
            cam,
            4,
            Facing::East,
            player,
            &mut rng,
        );
        assert_eq!(locals.len(), 4, "exactly four fixed twins, no weight packs");
        for &((x, y), face) in &expected {
            let e = locals
                .iter()
                .find(|e| e.x == x && e.y == y)
                .unwrap_or_else(|| panic!("missing twin at ({x},{y})"));
            assert!(matches!(e.kind, EnemyKind::Twinstep));
            assert_eq!(
                Facing::from_u8(e.facing),
                Some(face),
                "twin ({x},{y}) facing"
            );
        }

        // Phase-1 re-attach also re-runs faceforblock (player may have moved).
        let player2 = (117, 24); // now on-axis with (112,24) → East
        let tiles2 = m.roomtiles_for(cam, player2);
        let again = find_local_enemies(
            &m,
            &tiles2,
            &mut globals,
            0x5EED_2026,
            cam,
            4,
            Facing::North,
            player2,
            &mut rng,
        );
        let west = again
            .iter()
            .find(|e| e.x == 112 && e.y == 24)
            .expect("reattach twin");
        assert_eq!(Facing::from_u8(west.facing), Some(Facing::East));
        let east = again
            .iter()
            .find(|e| e.x == 123 && e.y == 24)
            .expect("reattach twin");
        assert_eq!(Facing::from_u8(east.facing), Some(Facing::West));
    }
}
