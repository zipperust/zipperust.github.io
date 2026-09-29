//! Dump first/second interior frames for spawn alignment checks.
use std::path::PathBuf;
use zipper_core::{
    decode_pdi, decode_pdt, Buttons, DemoAssets, Game, Input, WorldMap, SCREEN_HEIGHT,
    SCREEN_WIDTH,
};

fn write_fb(name: &str, g: &Game) {
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

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ref/Zipper.pdx/Images");
    let mut g = Game::new();
    g.load_worldmap(&std::fs::read(WorldMap::data_file_path()).expect("data/worldmap.bin"))
        .expect("load worldmap");
    let mut assets = DemoAssets::default();
    let load_pdt = |name: &str| {
        std::fs::read(root.join(name))
            .ok()
            .and_then(|d| decode_pdt(&d).ok())
    };
    let load_pdi = |name: &str| {
        std::fs::read(root.join(name))
            .ok()
            .and_then(|d| decode_pdi(&d).ok())
    };
    assets.tiles = load_pdt("unifiedtiles.pdt").or_else(|| load_pdt("tileset.pdt"));
    assets.player = load_pdt("player.pdt");
    assets.enemy = load_pdt("enemy.pdt");
    assets.ninja = load_pdt("ninja.pdt");
    assets.moveicon = load_pdi("moveicon.pdi");
    assets.passicon = load_pdi("passicon.pdi");
    assets.centerdot = load_pdi("centerdot.pdi");
    g.set_demo_assets(assets);

    let mut input = Input::default();
    input.buttons.a = true;
    g.update(0.05, input);
    input = Input::default();
    g.update(0.05, input);

    // Door at (109,170)
    let steps = g.player_y - 170;
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
    eprintln!(
        "room1 cam={:?} player={:?} enemies={:?}",
        g.camera,
        (g.player_x, g.player_y),
        g.room
            .enemies
            .iter()
            .map(|e| (e.x, e.y))
            .collect::<Vec<_>>()
    );
    write_fb("room1.ppm", &g);

    // North door on player's column
    let (tx, ty) = (g.player_x, 156);
    input.buttons.b = true;
    g.update(0.05, input);
    input = Input::default();
    g.update(0.05, input);
    let _ = tx;
    while g.selector_tile().1 != ty {
        input.buttons.up = true;
        g.update(0.05, input);
        input = Input::default();
        g.update(0.05, input);
    }
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
            .map(|e| (e.x, e.y))
            .collect::<Vec<_>>()
    );
    write_fb("room2.ppm", &g);
}
