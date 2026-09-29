//! Zipper `pathfinding.lua` + a Playdate-shaped graph / A* core.
//!
//! Layer 1: SDK-like `PathGraph` (`find_path` = Manhattan A*).
//! Layer 2: 28×28×4 room graph, weights, `pf` / `step_enemies` (real + ghost).
//! Room fence: graph nodes exist only for culled `roomtiles` (`cullroomtiles`);
//! door GIDs 125–128 also cost weight 1000 via `checkPathBlocked`.
//! Crank ghost preview (**v0.15**) shares `GraphBuildCtx { ghost: true }` —
//! see `docs/crank-preview-0.15.md`.

use crate::level::Enemy;
use crate::worldmap::{Facing, WorldMap};
use std::collections::HashSet;

/// `pathfinding.lua` `graphCols` / `graphRows`.
pub const GRAPH_COLS: i32 = 28;
pub const GRAPH_ROWS: i32 = 28;
pub const GRAPH_NODE_COUNT: usize = (GRAPH_COLS * GRAPH_ROWS * 4) as usize;

const WEIGHT_STEP: i32 = 14;
const WEIGHT_BLOCKED: i32 = 1000;

/// One node in the Playdate-style pathfinder graph (1-based ids externally).
#[derive(Debug, Clone)]
struct GraphNode {
    /// World tile (set during weight reset).
    x: i32,
    y: i32,
    /// Outgoing edges: (to_id_1based, weight).
    edges: Vec<(u32, i32)>,
}

impl GraphNode {
    fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            edges: Vec::new(),
        }
    }
}

/// Playdate `pathfinder.graph` stand-in: nodes `1..=GRAPH_NODE_COUNT`.
#[derive(Debug, Clone)]
pub struct PathGraph {
    nodes: Vec<GraphNode>,
}

impl PathGraph {
    pub fn new() -> Self {
        Self {
            nodes: (0..GRAPH_NODE_COUNT).map(|_| GraphNode::new()).collect(),
        }
    }

    fn node_mut(&mut self, id: u32) -> Option<&mut GraphNode> {
        let i = id.checked_sub(1)? as usize;
        self.nodes.get_mut(i)
    }

    fn node(&self, id: u32) -> Option<&GraphNode> {
        let i = id.checked_sub(1)? as usize;
        self.nodes.get(i)
    }

    pub fn set_xy(&mut self, id: u32, x: i32, y: i32) {
        if let Some(n) = self.node_mut(id) {
            n.x = x;
            n.y = y;
        }
    }

    pub fn remove_all_connections_from(&mut self, id: u32) {
        if let Some(n) = self.node_mut(id) {
            n.edges.clear();
        }
    }

    pub fn add_connection(&mut self, from: u32, to: u32, weight: i32, reciprocal: bool) {
        if let Some(n) = self.node_mut(from) {
            if let Some(e) = n.edges.iter_mut().find(|(t, _)| *t == to) {
                e.1 = weight;
            } else {
                n.edges.push((to, weight));
            }
        }
        if reciprocal {
            self.add_connection(to, from, weight, false);
        }
    }

    pub fn has_connections(&self, id: u32) -> bool {
        self.node(id).map(|n| !n.edges.is_empty()).unwrap_or(false)
    }

    /// Outgoing neighbour ids (1-based) — tests / debug.
    pub fn outgoing_ids(&self, id: u32) -> Vec<u32> {
        self.node(id)
            .map(|n| n.edges.iter().map(|(t, _)| *t).collect())
            .unwrap_or_default()
    }

    /// Weight of the edge `from → to`, if present.
    pub fn edge_weight(&self, from: u32, to: u32) -> Option<i32> {
        self.node(from)
            .and_then(|n| n.edges.iter().find(|(t, _)| *t == to).map(|(_, w)| *w))
    }

    pub fn xy(&self, id: u32) -> Option<(i32, i32)> {
        self.node(id).map(|n| (n.x, n.y))
    }

    /// A* with Manhattan heuristic on node `x/y` (SDK default when heuristic is nil).
    /// Returns path of 1-based node ids including start and goal, or `None`.
    ///
    /// Note: Zipper passes `nil` heuristic (`pathfinding.lua` `findPath(..., nil)`),
    /// so the SDK uses unscaled Manhattan on node x/y. Edge weights are still 14
    /// (walks + same-tile turns). No outcome-tuned bias — see Known gaps for the
    /// castle-twin east-cut delta vs Playdate.
    pub fn find_path(&self, start_id: u32, goal_id: u32) -> Option<Vec<u32>> {
        if start_id == 0 || goal_id == 0 {
            return None;
        }
        if start_id == goal_id {
            return Some(vec![start_id]);
        }
        let start = self.node(start_id)?;
        let goal = self.node(goal_id)?;
        let (gx, gy) = (goal.x, goal.y);

        let n = self.nodes.len();
        let mut g_score = vec![i32::MAX; n];
        let mut came_from = vec![0u32; n];
        let mut open: Vec<(i32, u32)> = Vec::new(); // (f_score, id)

        let si = (start_id - 1) as usize;
        g_score[si] = 0;
        let h0 = (start.x - gx).abs() + (start.y - gy).abs();
        open.push((h0, start_id));

        while let Some((_, current)) = pop_min_f(&mut open) {
            if current == goal_id {
                return Some(reconstruct_path(&came_from, current));
            }
            let ci = (current - 1) as usize;
            let Some(node) = self.node(current) else {
                continue;
            };
            for &(to, w) in &node.edges {
                let ti = (to - 1) as usize;
                if ti >= n {
                    continue;
                }
                let tentative = g_score[ci].saturating_add(w);
                if tentative >= g_score[ti] {
                    continue;
                }
                came_from[ti] = current;
                g_score[ti] = tentative;
                let (tx, ty) = self.node(to).map(|n| (n.x, n.y)).unwrap_or((gx, gy));
                let f = tentative + (tx - gx).abs() + (ty - gy).abs();
                if let Some(e) = open.iter_mut().find(|(_, id)| *id == to) {
                    e.0 = f;
                } else {
                    open.push((f, to));
                }
            }
        }
        None
    }
}

impl Default for PathGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn pop_min_f(open: &mut Vec<(i32, u32)>) -> Option<(i32, u32)> {
    if open.is_empty() {
        return None;
    }
    let mut best_i = 0;
    for i in 1..open.len() {
        if open[i].0 < open[best_i].0 {
            best_i = i;
        }
    }
    Some(open.swap_remove(best_i))
}

fn reconstruct_path(came_from: &[u32], mut current: u32) -> Vec<u32> {
    let mut path = vec![current];
    while came_from[(current - 1) as usize] != 0 {
        current = came_from[(current - 1) as usize];
        path.push(current);
    }
    path.reverse();
    path
}

/// `facingForID` — `floor((id - 1) % 4 + 1)` → Facing.
pub fn facing_for_id(id: u32) -> Facing {
    match ((id - 1) % 4) + 1 {
        1 => Facing::North,
        2 => Facing::South,
        3 => Facing::East,
        _ => Facing::West,
    }
}

/// `graphindexforxyf(_x, _y, _f)` — room col/row 1-based, facing 1..=4 (or 0 base).
pub fn graph_index_xyf(col: i32, row: i32, facing: u8) -> u32 {
    let base = ((row - 1) * GRAPH_COLS + (col - 1)) * 4;
    (base + facing as i32) as u32
}

/// Camera-centered room↔world tables (`setupRoomToWorldTileTables`).
#[derive(Debug, Clone)]
pub struct RoomTables {
    pub camera: (i32, i32),
    pub world_x: [i32; GRAPH_COLS as usize],
    pub world_y: [i32; GRAPH_ROWS as usize],
}

impl RoomTables {
    pub fn for_camera(camera: (i32, i32)) -> Self {
        let (cx, cy) = camera;
        let mut world_x = [0i32; GRAPH_COLS as usize];
        let mut world_y = [0i32; GRAPH_ROWS as usize];
        // Lua: initalVal = cameratile_x - ceil(graphCols/2); roomToWorldTileX[i] = i + initalVal
        // ceil(28/2)=14.
        let x0 = cx - 14;
        let y0 = cy - 14;
        for i in 0..GRAPH_COLS as usize {
            world_x[i] = (i as i32 + 1) + x0;
        }
        for i in 0..GRAPH_ROWS as usize {
            world_y[i] = (i as i32 + 1) + y0;
        }
        Self {
            camera,
            world_x,
            world_y,
        }
    }

    pub fn world_to_room_x(&self, wx: i32) -> i32 {
        // wx - cameratile_x + ceil(graphCols/2)
        wx - self.camera.0 + 14
    }

    pub fn world_to_room_y(&self, wy: i32) -> i32 {
        wy - self.camera.1 + 14
    }

    pub fn index_world_facing(&self, wx: i32, wy: i32, facing: Facing) -> Option<u32> {
        let col = self.world_to_room_x(wx);
        let row = self.world_to_room_y(wy);
        if col < 1 || col > GRAPH_COLS || row < 1 || row > GRAPH_ROWS {
            return None;
        }
        Some(graph_index_xyf(col, row, facing as u8))
    }
}

/// `utils.lua` `xyfmanhattandistance` (sort / pikeman target scoring).
pub fn xyf_manhattan(
    start_x: i32,
    start_y: i32,
    start_f: Facing,
    target_x: i32,
    target_y: i32,
    target_f: Facing,
) -> i32 {
    let dx = target_x - start_x;
    let dy = target_y - start_y;
    let mut score = dx.abs() + dy.abs();
    // Match Lua `utils.lua:9` literally (including the duplicated `dy < 0` north check).
    let turned_around = (dx > 0 && start_f == Facing::West)
        || (dx < 0 && start_f == Facing::East)
        || (dy < 0 && start_f == Facing::South)
        || (dy < 0 && start_f == Facing::North);
    let off_axis = (dx.abs() > 0 && matches!(start_f, Facing::North | Facing::South))
        || (dy.abs() > 0 && matches!(start_f, Facing::East | Facing::West));
    let new_face = if turned_around {
        start_f.opposite()
    } else if off_axis {
        if matches!(start_f, Facing::North | Facing::South) {
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
    } else {
        start_f
    };
    if turned_around {
        score += 2;
    } else if off_axis {
        score += 1;
    }
    if new_face.opposite() == target_f {
        score += 2;
    } else if new_face == target_f {
        // +0
    } else {
        score += 1;
    }
    score
}

/// Snapshot of world state the graph weight reset needs.
pub struct GraphBuildCtx<'a> {
    pub world: &'a WorldMap,
    pub enemies: &'a [Enemy],
    pub player: (i32, i32),
    pub cursor: (i32, i32),
    pub chest: Option<(i32, i32)>,
    pub has_key: bool,
    /// Ghost mode (`pathfinding.lua` `pf(..., ghost=true)` / crank preview).
    pub ghost: bool,
    /// Per-local ghost tiles (`enemy.ghost.tile_x/y`); length matches `enemies`
    /// when `ghost` is true. Ignored in real mode.
    pub ghost_xy: &'a [(i32, i32)],
    /// Aim `deathlist` indices — bodies block ghost path tiles.
    pub deathlist: &'a [usize],
    /// Culled `roomtiles` membership after `cullroomtiles` (`~= nil`).
    /// Doors stay; floors past a door are absent — that is the enemy room fence.
    pub roomtiles: &'a HashSet<(i32, i32)>,
}

impl GraphBuildCtx<'_> {
    /// `roomtiles[i] ~= nil` after iso footprint + `cullroomtiles`.
    fn in_roomtiles(&self, x: i32, y: i32) -> bool {
        self.roomtiles.contains(&(x, y))
    }

    /// Corpse at tile (alive == false). Spirit corpses use `deadenemies == 2`
    /// and still count as blockers for path tiles (`deadenemies ~= nil` in Lua).
    fn dead_enemy_at(&self, x: i32, y: i32) -> bool {
        self.enemies.iter().any(|e| !e.alive && e.x == x && e.y == y)
    }

    fn on_deathlist(&self, i: usize) -> bool {
        self.deathlist.iter().any(|&d| d == i)
    }

    /// `checkPathBlocked` (`pathfinding.lua:396–425`).
    pub fn path_blocked(&self, x: i32, y: i32) -> bool {
        if self.ghost {
            if (x, y) == self.player {
                return false;
            }
            if (x, y) == self.cursor {
                return true;
            }
            for (i, e) in self.enemies.iter().enumerate() {
                if e.kind.is_inanimate() {
                    continue;
                }
                if e.alive {
                    if let Some(&(gx, gy)) = self.ghost_xy.get(i) {
                        if (gx, gy) == (x, y) {
                            return true;
                        }
                    }
                }
                if self.on_deathlist(i) && e.x == x && e.y == y {
                    return true;
                }
            }
        } else if (x, y) == self.player {
            return true;
        } else {
            for e in self.enemies {
                if e.alive && e.x == x && e.y == y {
                    return true;
                }
            }
        }
        let gid = self.world.tile(x, y);
        // Lua: thistile > 124 and thistile < 129 (door GIDs in outdoor tileset).
        gid > 124 && gid < 129
    }

    /// `checkBlockedByEntity` (`myself` = enemy index).
    pub fn blocked_by_entity(&self, x: i32, y: i32, myself: usize) -> bool {
        if !self.ghost && (x, y) == self.player {
            return true;
        }
        if self.ghost && (x, y) == self.cursor {
            return true;
        }
        for (j, e) in self.enemies.iter().enumerate() {
            if j == myself {
                continue;
            }
            if self.ghost {
                if let Some(&(gx, gy)) = self.ghost_xy.get(j) {
                    if (gx, gy) == (x, y) {
                        return true;
                    }
                }
                continue;
            }
            if e.alive && e.x == x && e.y == y {
                return true;
            }
            // Lowered tip: Lua may raise pike; we treat as blocked for movers.
            if e.alive && e.tip_blocks(x, y) {
                return true;
            }
        }
        false
    }
}

/// Rebuild all node XY + connections for the camera window (`resetGraphWeightsAndConnections`).
pub fn reset_graph_weights(graph: &mut PathGraph, tables: &RoomTables, ctx: &GraphBuildCtx<'_>) {
    for row in 1..=GRAPH_ROWS {
        for col in 1..=GRAPH_COLS {
            let tile_x = tables.world_x[(col - 1) as usize];
            let tile_y = tables.world_y[(row - 1) as usize];
            let base = graph_index_xyf(col, row, 0);
            let north = base + Facing::North as u32;
            let south = base + Facing::South as u32;
            let east = base + Facing::East as u32;
            let west = base + Facing::West as u32;
            for id in [north, south, east, west] {
                graph.set_xy(id, tile_x, tile_y);
            }

            let chest_block = ctx.chest == Some((tile_x, tile_y)) && !ctx.has_key;
            if !ctx.in_roomtiles(tile_x, tile_y)
                || ctx.dead_enemy_at(tile_x, tile_y)
                || chest_block
            {
                for id in [north, south, east, west] {
                    graph.remove_all_connections_from(id);
                }
                continue;
            }

            // Cardinal steps (Lua always adds; weight 1000 if blocked / edge).
            let w_n = if row > 1 {
                let ty = tables.world_y[(row - 2) as usize];
                if ctx.path_blocked(tile_x, ty) {
                    WEIGHT_BLOCKED
                } else {
                    WEIGHT_STEP
                }
            } else {
                WEIGHT_BLOCKED
            };
            if row > 1 {
                graph.add_connection(north, north - (GRAPH_COLS as u32) * 4, w_n, false);
            }

            let w_e = if col < GRAPH_COLS {
                let tx = tables.world_x[col as usize];
                if ctx.path_blocked(tx, tile_y) {
                    WEIGHT_BLOCKED
                } else {
                    WEIGHT_STEP
                }
            } else {
                WEIGHT_BLOCKED
            };
            if col < GRAPH_COLS {
                graph.add_connection(east, east + 4, w_e, false);
            }

            let w_s = if row < GRAPH_ROWS {
                let ty = tables.world_y[row as usize];
                if ctx.path_blocked(tile_x, ty) {
                    WEIGHT_BLOCKED
                } else {
                    WEIGHT_STEP
                }
            } else {
                WEIGHT_BLOCKED
            };
            if row < GRAPH_ROWS {
                graph.add_connection(south, south + (GRAPH_COLS as u32) * 4, w_s, false);
            }

            let w_w = if col > 1 {
                let tx = tables.world_x[(col - 2) as usize];
                if ctx.path_blocked(tx, tile_y) {
                    WEIGHT_BLOCKED
                } else {
                    WEIGHT_STEP
                }
            } else {
                WEIGHT_BLOCKED
            };
            if col > 1 {
                graph.add_connection(west, west - 4, w_w, false);
            }

            // Same-cell facing turns (reciprocal).
            graph.add_connection(north, east, WEIGHT_STEP, true);
            graph.add_connection(north, west, WEIGHT_STEP, true);
            graph.add_connection(south, east, WEIGHT_STEP, true);
            graph.add_connection(south, west, WEIGHT_STEP, true);
        }
    }
}

/// Refresh one world tile’s four facing nodes (`resetGraphWeightAndConnectionsForXY`).
pub fn reset_graph_xy(
    graph: &mut PathGraph,
    tables: &RoomTables,
    ctx: &GraphBuildCtx<'_>,
    tile_x: i32,
    tile_y: i32,
) {
    let col = tables.world_to_room_x(tile_x);
    let row = tables.world_to_room_y(tile_y);
    if col < 1 || col > GRAPH_COLS || row < 1 || row > GRAPH_ROWS {
        return;
    }
    let base = graph_index_xyf(col, row, 0);
    let north = base + Facing::North as u32;
    let south = base + Facing::South as u32;
    let east = base + Facing::East as u32;
    let west = base + Facing::West as u32;
    for id in [north, south, east, west] {
        graph.set_xy(id, tile_x, tile_y);
    }

    if !ctx.in_roomtiles(tile_x, tile_y) || ctx.dead_enemy_at(tile_x, tile_y) {
        for id in [north, south, east, west] {
            graph.remove_all_connections_from(id);
        }
        return;
    }

    let neighbor = |dx: i32, dy: i32| (tile_x + dx, tile_y + dy);
    let weight_to = |x: i32, y: i32| {
        if ctx.path_blocked(x, y) {
            WEIGHT_BLOCKED
        } else {
            WEIGHT_STEP
        }
    };

    let (nx, ny) = neighbor(0, -1);
    graph.add_connection(
        north,
        north.wrapping_sub((GRAPH_COLS as u32) * 4),
        weight_to(nx, ny),
        false,
    );
    let (ex, ey) = neighbor(1, 0);
    graph.add_connection(east, east + 4, weight_to(ex, ey), false);
    let (sx, sy) = neighbor(0, 1);
    graph.add_connection(
        south,
        south + (GRAPH_COLS as u32) * 4,
        weight_to(sx, sy),
        false,
    );
    let (wx, wy) = neighbor(-1, 0);
    graph.add_connection(west, west - 4, weight_to(wx, wy), false);

    graph.add_connection(north, east, WEIGHT_STEP, true);
    graph.add_connection(north, west, WEIGHT_STEP, true);
    graph.add_connection(south, east, WEIGHT_STEP, true);
    graph.add_connection(south, west, WEIGHT_STEP, true);
}

/// Result of one `pf` step for a living enemy (real mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PfStep {
    pub x: i32,
    pub y: i32,
    pub facing: Facing,
    /// True when path was empty / already at goal (facing-only update possible).
    pub already_there: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_for_id_cycles() {
        assert_eq!(facing_for_id(1), Facing::North);
        assert_eq!(facing_for_id(2), Facing::South);
        assert_eq!(facing_for_id(3), Facing::East);
        assert_eq!(facing_for_id(4), Facing::West);
        assert_eq!(facing_for_id(5), Facing::North);
    }

    #[test]
    fn graph_index_matches_lua() {
        // col=1,row=1,f=North → id 1
        assert_eq!(graph_index_xyf(1, 1, Facing::North as u8), 1);
        assert_eq!(graph_index_xyf(1, 1, Facing::West as u8), 4);
        assert_eq!(graph_index_xyf(2, 1, Facing::North as u8), 5);
    }

    #[test]
    fn find_path_straight_line() {
        let mut g = PathGraph::new();
        // Tiny chain: 1 → 2 → 3
        g.set_xy(1, 0, 0);
        g.set_xy(2, 1, 0);
        g.set_xy(3, 2, 0);
        g.add_connection(1, 2, 14, false);
        g.add_connection(2, 3, 14, false);
        let path = g.find_path(1, 3).expect("path");
        assert_eq!(path, vec![1, 2, 3]);
    }

    #[test]
    fn room_tables_start_camera() {
        let t = RoomTables::for_camera((110, 180));
        // world_to_room: 110 - 110 + 14 = 14
        assert_eq!(t.world_to_room_x(110), 14);
        assert_eq!(t.world_to_room_y(180), 14);
        assert_eq!(t.world_x[13], 110); // index 13 = col 14
    }

    /// Same-cell ±90° turns are graph hops; no 180° edge (`pathfinding.lua`).
    #[test]
    fn turn_edges_are_90_deg_only() {
        let mut g = PathGraph::new();
        let n = graph_index_xyf(5, 5, Facing::North as u8);
        let s = graph_index_xyf(5, 5, Facing::South as u8);
        let e = graph_index_xyf(5, 5, Facing::East as u8);
        let w = graph_index_xyf(5, 5, Facing::West as u8);
        for id in [n, s, e, w] {
            g.set_xy(id, 100, 100);
        }
        // Mirror Lua reciprocal turn pairs only.
        g.add_connection(n, e, WEIGHT_STEP, true);
        g.add_connection(n, w, WEIGHT_STEP, true);
        g.add_connection(s, e, WEIGHT_STEP, true);
        g.add_connection(s, w, WEIGHT_STEP, true);

        let outs = g.outgoing_ids(n);
        assert!(outs.contains(&e) && outs.contains(&w));
        assert!(!outs.contains(&s), "no direct 180° N→S edge");

        let path_ne = g.find_path(n, e).expect("90° turn");
        assert_eq!(path_ne, vec![n, e], "one hop for 90°");

        let path_ns = g.find_path(n, s).expect("180° via two 90°");
        assert_eq!(path_ns.len(), 3, "about-face needs two turn hops");
        assert_eq!(path_ns[0], n);
        assert_eq!(*path_ns.last().unwrap(), s);
        // First hop is ±90°, not the goal.
        assert!(path_ns[1] == e || path_ns[1] == w);
    }

    /// Culled roomtiles disconnect the far side of a door; door edges cost 1000.
    #[test]
    fn culled_roomtiles_fence_doors() {
        use crate::worldmap::{WorldMap, START_CAMERA, START_PLAY};

        let world = WorldMap::from_path(WorldMap::data_file_path()).expect("worldmap.bin");
        let roomtiles = world.roomtiles_for(START_CAMERA, START_PLAY);
        assert!(roomtiles.contains(&(109, 170)));
        assert!(!roomtiles.contains(&(109, 168)));

        let tables = RoomTables::for_camera(START_CAMERA);
        let enemies: Vec<Enemy> = Vec::new();
        let ghost_xy: Vec<(i32, i32)> = Vec::new();
        let ctx = GraphBuildCtx {
            world: &world,
            enemies: &enemies,
            player: START_PLAY,
            cursor: START_PLAY,
            chest: None,
            has_key: false,
            ghost: false,
            ghost_xy: &ghost_xy,
            deathlist: &[],
            roomtiles: &roomtiles,
        };
        let mut graph = PathGraph::new();
        reset_graph_weights(&mut graph, &tables, &ctx);

        let far_col = tables.world_to_room_x(109);
        let far_row = tables.world_to_room_y(168);
        let far_id = graph_index_xyf(far_col, far_row, Facing::South as u8);
        assert!(
            !graph.has_connections(far_id),
            "far-side floor must have no graph edges"
        );

        let door_col = tables.world_to_room_x(109);
        let door_row = tables.world_to_room_y(170);
        let south_row = tables.world_to_room_y(171);
        let from_id = graph_index_xyf(door_col, south_row, Facing::North as u8);
        let door_id = graph_index_xyf(door_col, door_row, Facing::North as u8);
        assert!(graph.has_connections(from_id), "this-room floor stays linked");
        assert_eq!(
            graph.edge_weight(from_id, door_id),
            Some(WEIGHT_BLOCKED),
            "step onto outdoor door GID 128 costs 1000"
        );

        let start_id = graph_index_xyf(
            tables.world_to_room_x(109),
            tables.world_to_room_y(171),
            Facing::North as u8,
        );
        assert!(
            graph.find_path(start_id, far_id).is_none(),
            "no path into the next room past the door"
        );
    }
}
