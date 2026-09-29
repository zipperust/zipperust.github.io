//! Isometric helpers matching Zipper 1.10 Lua.
//!
//! Shared screen point `(sx, sy)` is the value of `tileToScreen(tile_x, tile_y)`
//! (`Globals.lua`): both floor tiles and actors `moveTo` that point, then
//! Playdate `setCenter` places the image relative to it.
//!
//! | Sprite     | Lua `setCenter`            | Source            |
//! |------------|----------------------------|-------------------|
//! | `isotile`  | `(0.4375, 0.203125)`       | `isotile.lua`     |
//! | `isosprite`| `(0.5, 0.27083334)`        | `isosprite.lua`   |
//! | selector   | `(0.4375, -2.1875)`        | `selector.lua`    |
//! | smoke      | `(0.4375, -1.38)`          | `smoker.lua`      |

use crate::worldmap::Facing;

/// Half-width of a floor diamond in pixels (`tileWidth / 2`).
pub const TILE_HALF_W: i32 = 16;
/// Half-height of a floor diamond in pixels (`tileHeight / 2`).
pub const TILE_HALF_H: i32 = 8;

/// Pixel size of tileset cells.
pub const TILE_CELL_W: i32 = 32;
pub const TILE_CELL_H: i32 = 64;

/// Character cell size (`player.pdt`, `ninja.pdt`, …).
pub const ACTOR_CELL: i32 = 96;

/// `isotile:setCenter(0.4375, 0.203125)`.
pub const TILE_CENTER_X: f32 = 0.4375;
pub const TILE_CENTER_Y: f32 = 0.203125;

/// `isosprite:setCenter(0.5, 0.27083334)`.
pub const ACTOR_CENTER_X: f32 = 0.5;
pub const ACTOR_CENTER_Y: f32 = 0.27083334;

/// `selector` / walkicon / centerdot: `setCenter(0.4375, -2.1875)` on 32×16.
pub const SELECTOR_ICON_W: i32 = 32;
pub const SELECTOR_ICON_H: i32 = 16;
pub const SELECTOR_CENTER_X: f32 = 0.4375;
pub const SELECTOR_CENTER_Y: f32 = -2.1875;

/// Floor zip smoke (`smoker.lua`): `setCenter(0.4375, -1.38)` on 42×24 cells.
pub const SMOKE_CELL_W: i32 = 42;
pub const SMOKE_CELL_H: i32 = 24;
pub const SMOKE_CENTER_X: f32 = 0.4375;
pub const SMOKE_CENTER_Y: f32 = -1.38;

/// Directional floor blood splatters (`enemy:bleed` → `floorspray`):
/// `setCenter(0.4375, -2.2)` on 32×16 cells.
pub const FLOOR_BLOOD_CELL_W: i32 = 32;
pub const FLOOR_BLOOD_CELL_H: i32 = 16;
pub const FLOOR_BLOOD_CENTER_X: f32 = 0.4375;
pub const FLOOR_BLOOD_CENTER_Y: f32 = -2.2;

/// Player death drips (`samurai:sploosh` → `dripsC/N/S/E/W`):
/// `setCenter(0.5, 0.171875)` on 32×64 cells.
pub const DRIP_CELL_W: i32 = 32;
pub const DRIP_CELL_H: i32 = 64;
pub const DRIP_CENTER_X: f32 = 0.5;
pub const DRIP_CENTER_Y: f32 = 0.171875;

/// Grid → `tileToScreen`-style point (before camera origin is added by the caller).
///
/// Full Lua: `px = (x-cam_x-(y-cam_y))*16 + 200`, `py = (x-cam_x+(y-cam_y))*8 + 80`
/// (`tileHeight * 5` = 80). Callers pass `origin_x/y` that fold in camera + bias
/// (`Game::origin` uses `self.camera` as `cameratile`, not the player).
#[inline]
pub fn grid_to_screen(gx: i32, gy: i32, origin_x: i32, origin_y: i32) -> (i32, i32) {
    let sx = origin_x + (gx - gy) * TILE_HALF_W;
    let sy = origin_y + (gx + gy) * TILE_HALF_H;
    (sx, sy)
}

/// Playdate `setCenter(cx, cy)` → image top-left when the sprite is at `(sx, sy)`.
#[inline]
pub fn sprite_blit_pos(
    sx: i32,
    sy: i32,
    width: i32,
    height: i32,
    center_x: f32,
    center_y: f32,
) -> (i32, i32) {
    let dx = (center_x * width as f32).round() as i32;
    let dy = (center_y * height as f32).round() as i32;
    (sx - dx, sy - dy)
}

/// Top-left blit for a 32×64 tile (`isotile:setCenter`).
#[inline]
pub fn tile_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        TILE_CELL_W,
        TILE_CELL_H,
        TILE_CENTER_X,
        TILE_CENTER_Y,
    )
}

/// Top-left blit for a 96×96 actor (`isosprite:setCenter`).
#[inline]
pub fn actor_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        ACTOR_CELL,
        ACTOR_CELL,
        ACTOR_CENTER_X,
        ACTOR_CENTER_Y,
    )
}

/// Top-left blit for selector / passicon / centerdot.
#[inline]
pub fn selector_icon_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        SELECTOR_ICON_W,
        SELECTOR_ICON_H,
        SELECTOR_CENTER_X,
        SELECTOR_CENTER_Y,
    )
}

/// Top-left blit for floor smoke puffs (`smoker` sprites).
#[inline]
pub fn smoke_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        SMOKE_CELL_W,
        SMOKE_CELL_H,
        SMOKE_CENTER_X,
        SMOKE_CENTER_Y,
    )
}

/// Top-left blit for directional floor blood (`floorspray` cells).
#[inline]
pub fn floor_blood_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        FLOOR_BLOOD_CELL_W,
        FLOOR_BLOOD_CELL_H,
        FLOOR_BLOOD_CENTER_X,
        FLOOR_BLOOD_CENTER_Y,
    )
}

/// Top-left blit for player-death drip sprays (`samurai:sploosh`).
#[inline]
pub fn drip_blit_pos(sx: i32, sy: i32) -> (i32, i32) {
    sprite_blit_pos(
        sx,
        sy,
        DRIP_CELL_W,
        DRIP_CELL_H,
        DRIP_CENTER_X,
        DRIP_CENTER_Y,
    )
}

/// Floor-diamond top in screen space for a tile whose `tileToScreen` is `(sx, sy)`.
///
/// Floor ink sits at image y≈48 in the 32×64 cell; with `isotile` center y=13,
/// that is `48 - 13 = 35` px below the sprite anchor.
#[inline]
pub fn floor_diamond_top(sx: i32, sy: i32) -> (i32, i32) {
    (sx, sy + 35)
}

/// `isosprite:setFrame` — imagetable is 4 facing rows (W, S, E, N),
/// `numFrames = length/4`, idle pose `n = 1` (1-based) → 0-based cell index.
#[inline]
pub fn isosprite_frame_index(
    facing: Facing,
    pose_1based: u32,
    num_frames_per_facing: usize,
) -> usize {
    let row = match facing {
        Facing::West => 0,
        Facing::South => 1,
        Facing::East => 2,
        Facing::North => 3,
    };
    let n = pose_1based.saturating_sub(1) as usize;
    row * num_frames_per_facing + n
}

/// Painter's sort key: draw far tiles first (smaller gx+gy first).
#[inline]
pub fn depth_key(gx: i32, gy: i32) -> i32 {
    gx + gy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn north_idle_is_fourth_row_first_pose() {
        assert_eq!(isosprite_frame_index(Facing::North, 1, 16), 48);
        assert_eq!(isosprite_frame_index(Facing::West, 1, 16), 0);
        assert_eq!(isosprite_frame_index(Facing::South, 1, 16), 16);
        assert_eq!(isosprite_frame_index(Facing::East, 1, 16), 32);
    }

    #[test]
    fn actor_center_matches_isosprite_lua() {
        // setCenter(0.5, 0.27083334) on 96×96 → offset (48, 26)
        let (bx, by) = actor_blit_pos(200, 100);
        assert_eq!(bx, 200 - 48);
        assert_eq!(by, 100 - 26);
    }

    #[test]
    fn tile_center_matches_isotile_lua() {
        // setCenter(0.4375, 0.203125) on 32×64 → offset (14, 13)
        let (bx, by) = tile_blit_pos(200, 100);
        assert_eq!(bx, 200 - 14);
        assert_eq!(by, 100 - 13);
    }

    #[test]
    fn selector_icon_center_from_lua() {
        let (bx, by) = selector_icon_blit_pos(200, 100);
        assert_eq!(bx, 200 - 14);
        assert_eq!(by, 100 - (-35));
    }

    #[test]
    fn actor_feet_sit_on_floor_diamond() {
        // Relative geometry from the shared tileToScreen point:
        // floor top at sy+35; feet (~image y 72) at sy+(72-26)=sy+46 → 11px into diamond.
        let (sx, sy) = (200, 100);
        let (_tx, floor_y) = floor_diamond_top(sx, sy);
        let (_ax, actor_top) = actor_blit_pos(sx, sy);
        let feet_y = actor_top + 72;
        assert_eq!(floor_y, sy + 35);
        assert_eq!(feet_y, sy + 46);
        assert_eq!(feet_y - floor_y, 11);
    }
}
