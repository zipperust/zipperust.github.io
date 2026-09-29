//! Viewport helpers on the extracted outdoor map.
//!
//! Behaviour contracts come from decompiled Lua (`main.lua` roomtiles,
//! `selector.lua` trymove / moveByTile). Do not invent alternate control schemes.

use crate::iso::depth_key;
use crate::worldmap::{WorldMap, START_CAMERA, START_PLAY};

/// Terrain in one grid cell — mirrors roomtiles classification + draw GID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terrain {
    Void,
    Floor { tile: u16 },
    Wall { tile: u16 },
    /// Door GID (127/128/381/382): roomtiles=-1, selectable, triggers exit.
    Door { tile: u16 },
    /// NPC pad (253): roomtiles=-2.
    Npc { tile: u16 },
}

impl Terrain {
    /// Selector may land here (`roomtiles <= 0`).
    pub fn selector_ok(self) -> bool {
        matches!(
            self,
            Terrain::Floor { .. } | Terrain::Door { .. } | Terrain::Npc { .. }
        )
    }

    pub fn walkable(self) -> bool {
        self.selector_ok()
    }

    pub fn tile(self) -> Option<u16> {
        match self {
            Terrain::Void => None,
            Terrain::Floor { tile }
            | Terrain::Wall { tile }
            | Terrain::Door { tile }
            | Terrain::Npc { tile } => Some(tile),
        }
    }

    pub fn blocks_zip(self) -> bool {
        matches!(self, Terrain::Void | Terrain::Wall { .. })
    }

    pub fn is_door(self) -> bool {
        matches!(self, Terrain::Door { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomId {
    World,
}

impl RoomId {
    pub fn name(self) -> &'static str {
        "OUTDOOR"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnemyKind {
    Swordsman,
    Pikeman,
    Ninja,
    Twinstep,
    King,
    Spirit,
    Reverse,
    /// Castle / outdoor gate halves (`Globals` `kLeftSouthEntrance`…`kRightEastEntrance`).
    /// `enemy.inanimate`; block the selector until the key opens them.
    LeftSouthEntrance,
    RightSouthEntrance,
    LeftEastEntrance,
    RightEastEntrance,
}

impl EnemyKind {
    /// Lua `enemy.etype` / `kSwordsman`… (`Globals.lua`).
    pub fn etype(self) -> usize {
        match self {
            Self::Swordsman => 1,
            Self::Pikeman => 2,
            Self::Ninja => 3,
            Self::Twinstep => 4,
            Self::King => 5,
            Self::Spirit => 6,
            Self::Reverse => 7,
            Self::LeftSouthEntrance => 10,
            Self::RightSouthEntrance => 11,
            Self::LeftEastEntrance => 12,
            Self::RightEastEntrance => 13,
        }
    }

    /// Inverse of [`Self::etype`] for save restore (`deadenemies`).
    pub fn from_etype(etype: usize) -> Option<Self> {
        Some(match etype {
            1 => Self::Swordsman,
            2 => Self::Pikeman,
            3 => Self::Ninja,
            4 => Self::Twinstep,
            5 => Self::King,
            6 => Self::Spirit,
            7 => Self::Reverse,
            10 => Self::LeftSouthEntrance,
            11 => Self::RightSouthEntrance,
            12 => Self::LeftEastEntrance,
            13 => Self::RightEastEntrance,
            _ => return None,
        })
    }

    /// `enemy.inanimate` — gate props, not combatants (`enemy.lua` entrance init).
    pub fn is_inanimate(self) -> bool {
        matches!(
            self,
            Self::LeftSouthEntrance
                | Self::RightSouthEntrance
                | Self::LeftEastEntrance
                | Self::RightEastEntrance
        )
    }

    /// Left vs right gate art (`leftdoorTable` / `rightdoorTable`).
    pub fn is_left_entrance(self) -> bool {
        matches!(self, Self::LeftSouthEntrance | Self::LeftEastEntrance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enemy {
    pub kind: EnemyKind,
    pub x: i32,
    pub y: i32,
    pub facing: u8,
    pub alive: bool,
    /// `enemy.stunned` — turns of AI skipped after blood-blind (`enemy:stun`).
    /// Set to 2 on stun; decremented in `braintwo` each enemy phase.
    pub stunned: i32,
    /// Index into `Game::global_enemies` (`enemy.globalindex` in Lua).
    /// `None` for test-injected locals that are not tracked globally.
    pub global_index: Option<usize>,
    /// Pikeman tip tile (`enemy.child_x` / `child_y`). Equals body when raised
    /// or for non-pikemen.
    pub child_x: i32,
    pub child_y: i32,
    /// `enemy.pikeup` — tip stowed on the body when true.
    pub pike_up: bool,
    /// `enemy.justdodged` — set on successful dodge/parry; cleared first `braintwo`.
    pub just_dodged: bool,
    /// `enemy.dodgestate` — skip path AI while set; second `braintwo` clears + idles.
    pub dodge_state: bool,
}

impl Enemy {
    /// Combatant with tip stowed on the body (Lua `enemy:init` → `setPikeUp(true)`
    /// for pikemen; other kinds ignore tip fields).
    pub fn new(kind: EnemyKind, x: i32, y: i32, facing: u8) -> Self {
        let mut e = Self {
            kind,
            x,
            y,
            facing,
            alive: true,
            stunned: 0,
            global_index: None,
            child_x: x,
            child_y: y,
            pike_up: false,
            just_dodged: false,
            dodge_state: false,
        };
        if matches!(kind, EnemyKind::Pikeman) {
            e.pike_up = true;
        }
        e
    }

    /// Keep `child_*` consistent after a body move / facing change (`moveToTile`).
    pub fn sync_pike_tip(&mut self) {
        if !matches!(self.kind, EnemyKind::Pikeman) {
            self.child_x = self.x;
            self.child_y = self.y;
            return;
        }
        if self.pike_up {
            self.child_x = self.x;
            self.child_y = self.y;
        } else if let Some(f) = crate::worldmap::Facing::from_u8(self.facing) {
            let (ox, oy) = f.offsets();
            self.child_x = self.x + ox;
            self.child_y = self.y + oy;
        } else {
            self.child_x = self.x;
            self.child_y = self.y;
        }
    }

    /// Living tip occupies `(tx, ty)` (`selector` / path block).
    pub fn tip_blocks(&self, tx: i32, ty: i32) -> bool {
        self.alive
            && matches!(self.kind, EnemyKind::Pikeman)
            && !self.pike_up
            && self.child_x == tx
            && self.child_y == ty
    }
}

/// Viewport onto the outdoor world (camera-sized cut for draw + local enemies).
#[derive(Debug, Clone)]
pub struct Room {
    pub id: RoomId,
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: i32,
    pub height: i32,
    pub cells: Vec<Terrain>,
    pub enemies: Vec<Enemy>,
    pub default_spawn: (i32, i32),
}

impl Room {
    /// World-space cell lookup only (no local/world ambiguity).
    pub fn cell(&self, wx: i32, wy: i32) -> Terrain {
        let lx = wx - self.origin_x;
        let ly = wy - self.origin_y;
        if lx < 0 || ly < 0 || lx >= self.width || ly >= self.height {
            return Terrain::Void;
        }
        self.cells[(ly * self.width + lx) as usize]
    }

    pub fn walkable(&self, wx: i32, wy: i32) -> bool {
        self.cell(wx, wy).walkable()
    }

    /// Living combatant at this tile (blocks selector / pathing).
    pub fn enemy_at(&self, wx: i32, wy: i32) -> Option<usize> {
        self.enemies
            .iter()
            .position(|e| e.alive && e.x == wx && e.y == wy)
    }

    /// Living pikeman tip at this tile (`enemy.child` while `pikeup == false`).
    pub fn pike_tip_at(&self, wx: i32, wy: i32) -> Option<usize> {
        self.enemies
            .iter()
            .position(|e| e.tip_blocks(wx, wy))
    }

    /// Any enemy sprite at this tile, including corpses (`alive == false`).
    /// Lua keeps dead enemies in `localenemies` / `globalenemies` and still draws them.
    pub fn any_enemy_at(&self, wx: i32, wy: i32) -> Option<usize> {
        self.enemies
            .iter()
            .position(|e| e.x == wx && e.y == wy)
    }

    pub fn paint_order(&self) -> Vec<(i32, i32)> {
        let mut v = Vec::with_capacity((self.width * self.height) as usize);
        for ly in 0..self.height {
            for lx in 0..self.width {
                let t = self.cells[(ly * self.width + lx) as usize];
                if !matches!(t, Terrain::Void) {
                    v.push((self.origin_x + lx, self.origin_y + ly));
                }
            }
        }
        v.sort_by_key(|&(x, y)| depth_key(x, y));
        v
    }
}

pub fn viewport_around(map: &WorldMap, camera: (i32, i32), half_w: i32, half_h: i32) -> Room {
    if !map.is_loaded() {
        return Room {
            id: RoomId::World,
            origin_x: 0,
            origin_y: 0,
            width: 0,
            height: 0,
            cells: Vec::new(),
            enemies: Vec::new(),
            default_spawn: START_PLAY,
        };
    }
    let x0 = (camera.0 - half_w).max(0);
    let y0 = (camera.1 - half_h).max(0);
    let x1 = (camera.0 + half_w).min(map.width as i32 - 1);
    let y1 = (camera.1 + half_h).min(map.height as i32 - 1);
    let width = x1 - x0 + 1;
    let height = y1 - y0 + 1;

    let mut cells = Vec::with_capacity((width * height) as usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            cells.push(map.terrain_at(x, y));
        }
    }

    // Enemies are filled by `find_local_enemies` / `locals_from_globals` on the
    // Game side (`utils.lua` findlocalenemies) — terrain-only here.
    Room {
        id: RoomId::World,
        origin_x: x0,
        origin_y: y0,
        width,
        height,
        cells,
        enemies: Vec::new(),
        default_spawn: START_PLAY,
    }
}

pub fn load_start_room(map: &WorldMap) -> Room {
    // Camera at outdoor start; generous window so selector can reach the north door.
    viewport_around(map, START_CAMERA, 16, 16)
}

/// Path cells from player toward cursor (exclusive of start, inclusive of end).
pub fn path_cells(px: i32, py: i32, cx: i32, cy: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let mut x = px;
    let mut y = py;
    let dx = (cx - px).signum();
    let dy = (cy - py).signum();
    // Axis-aligned only (selector is cardinal).
    if dx != 0 && dy != 0 {
        return out;
    }
    while x != cx || y != cy {
        x += dx;
        y += dy;
        out.push((x, y));
        if out.len() > 64 {
            break;
        }
    }
    out
}
