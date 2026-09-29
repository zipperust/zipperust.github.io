//! Extract `introchord` from Playdate `Globals.luac` → JSON for demo / native tests.
//!
//! ```text
//! cargo run -p zipper-core --example extract_introchord -- \
//!   ref/extracted/main/Globals.luac
//! # → crates/zipper-core/data/introchord.json
//! # → www/assets/demo/introchord.json  (only when the private `www/` tree exists)
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;

use zipper_core::introchord_from_globals_luac;

fn main() {
    let args: Vec<String> = env::args().collect();
    let luac = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ref/extracted/main/Globals.luac")
    };
    let bytes = fs::read(&luac).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", luac.display());
        std::process::exit(1);
    });
    let notes = introchord_from_globals_luac(&bytes).unwrap_or_else(|e| {
        eprintln!("extract failed: {e}");
        std::process::exit(1);
    });
    let json = format!(
        "[{}]\n",
        notes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let data_out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/introchord.json");
    fs::write(&data_out, &json).expect("write data/introchord.json");

    // The `www/` demo tree only exists on the private branch; skip it in the
    // public/BYOA tree so fixture generation doesn't create stray dirs.
    let www_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../www");
    let demo_out = www_dir.join("assets/demo/introchord.json");
    if www_dir.is_dir() {
        if let Some(parent) = demo_out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&demo_out, &json).expect("write www/assets/demo/introchord.json");
        eprintln!("wrote {}", demo_out.display());
    }
    eprintln!("introchord {:?}\nwrote {}", notes, data_out.display());
}
