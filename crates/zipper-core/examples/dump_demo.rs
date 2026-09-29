use std::path::PathBuf;
use zipper_core::{decode_pdi, decode_pdt, Buttons, DemoAssets, Game, Input, WorldMap};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ref/Zipper.pdx/Images");
    let mut g = Game::new();
    g.load_worldmap(&std::fs::read(WorldMap::data_file_path()).expect("data/worldmap.bin"))
        .expect("load worldmap");
    let mut assets = DemoAssets::default();
    if let Ok(d) = std::fs::read(root.join("Launcher/card.pdi")) {
        assets.card = Some(decode_pdi(&d).unwrap());
    }
    // Prefer Lua's `Images/unifiedtiles` (512 cells; castle GIDs 257+).
    let tiles_path = ["unifiedtiles.pdt", "tileset.pdt"]
        .iter()
        .map(|n| root.join(n))
        .find(|p| p.exists());
    if let Some(p) = tiles_path {
        if let Ok(d) = std::fs::read(&p) {
            assets.tiles = Some(decode_pdt(&d).unwrap());
        }
    }
    if let Ok(d) = std::fs::read(root.join("ninja.pdt")) {
        assets.ninja = Some(decode_pdt(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("player.pdt")) {
        assets.player = Some(decode_pdt(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("enemy.pdt")) {
        assets.enemy = Some(decode_pdt(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("passicon.pdi")) {
        assets.passicon = Some(decode_pdi(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("moveicon.pdi")) {
        assets.moveicon = Some(decode_pdi(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("centerdot.pdi")) {
        assets.centerdot = Some(decode_pdi(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("killicon.pdi")) {
        assets.killicon = Some(decode_pdi(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("stabicon.pdi")) {
        assets.stabicon = Some(decode_pdi(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("exiticon.pdt")) {
        assets.exiticon = Some(decode_pdt(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("smoke.pdt")) {
        assets.smoke = Some(decode_pdt(&d).unwrap());
    }
    if let Ok(d) = std::fs::read(root.join("smoke2.pdt")) {
        assets.smoke2 = Some(decode_pdt(&d).unwrap());
    }
    g.set_demo_assets(assets);

    g.update(0.0, Input::default());
    write_fb("lvl-boot.ppm", &g);

    // leave boot → outdoor world at real spawn (109, 182)
    let mut input = Input::default();
    input.buttons.a = true;
    g.update(0.016, input);
    input.buttons = Buttons::default();
    g.update(0.016, input);
    write_fb("lvl-start.ppm", &g);

    // extend selector north 8 steps (original: one tile per press)
    for _ in 0..8 {
        input.buttons.up = true;
        g.update(0.016, input);
        input.buttons = Buttons::default();
        g.update(0.016, input);
    }
    write_fb("lvl-start-aim.ppm", &g);

    // commit zip (A)
    input.buttons.a = true;
    g.update(0.016, input);
    input.buttons = Buttons::default();
    g.update(0.016, input);
    write_fb("lvl-after-zip.ppm", &g);

    eprintln!(
        "state={:?} player=({},{}) sel={:?} cam={:?} enemies={} map={}x{}",
        g.state,
        g.player_x,
        g.player_y,
        g.selector_tile(),
        g.camera,
        g.room.enemies.iter().filter(|e| e.alive).count(),
        g.world.width,
        g.world.height,
    );
}

fn write_fb(name: &str, g: &Game) {
    use zipper_core::{SCREEN_HEIGHT, SCREEN_WIDTH};
    let mut out = String::new();
    out.push_str(&format!("P3\n{} {}\n255\n", SCREEN_WIDTH, SCREEN_HEIGHT));
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            if g.fb.get_pixel(x, y) {
                out.push_str("26 24 21 ");
            } else {
                out.push_str("177 175 168 ");
            }
        }
        out.push('\n');
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.shots")
        .join(name);
    std::fs::create_dir_all(p.parent().unwrap()).ok();
    std::fs::write(&p, out).unwrap();
    eprintln!("wrote {}", p.display());
}
