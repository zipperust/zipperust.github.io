//! Runtime dialog corpus (`script.lua` `dialogs[1..=31]`).
//!
//! Creative prose is **not** baked into the binary. Hosts load it via
//! [`load_dialogs`] / wasm `loadDialogs` (from `script.luac` or JSON).
//! Until then the table is empty and dialog shows nothing.

use std::sync::RwLock;

/// Process-wide dialog scripts (0-based; Lua index = rust + 1).
static DIALOGS: RwLock<Vec<Vec<String>>> = RwLock::new(Vec::new());

/// Replace the dialog corpus. Empty input clears (BYOA until user assets load).
pub fn load_dialogs(scripts: Vec<Vec<String>>) {
    *DIALOGS.write().expect("dialogs lock") = scripts;
}

/// Number of loaded scripts (0 until [`load_dialogs`]).
pub fn dialog_script_count() -> usize {
    DIALOGS.read().expect("dialogs lock").len()
}

/// Copy of script `index` (0-based). Empty if missing / unloaded.
pub fn dialog_script(index: usize) -> Vec<String> {
    DIALOGS
        .read()
        .expect("dialogs lock")
        .get(index)
        .cloned()
        .unwrap_or_default()
}

/// True after a non-empty [`load_dialogs`].
pub fn dialogs_loaded() -> bool {
    !DIALOGS.read().expect("dialogs lock").is_empty()
}

#[cfg(test)]
pub fn load_test_dialogs() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/dialogs.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e} (extract from script.luac → dialogs.json)",
            path.display()
        )
    });
    let scripts: Vec<Vec<String>> =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("dialogs.json: {e}"));
    load_dialogs(scripts);
}
