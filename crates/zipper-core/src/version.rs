//! Port build stamp (baked into wasm; shown in the host menu, not on the canvas).
//!
//! # Scheme
//!
//! ```text
//! v{major}.{minor}{letter}     e.g.  v0.2b
//! ```
//!
//! - **major** — hand-bump for milestones (still `0`).
//! - **minor** — hand-bump when a feature lands; reset letter to `a`
//!   (edit `PORT_VERSION`, e.g. `0.3a`).
//! - **letter** — advanced by `scripts/build.sh` on every wasm compile
//!   (`a`→`b`→…→`z`). Proves the browser loaded a fresh build.
//!
//! Full write-up: repo `README.md` § Port versioning.
//!
//! Source of truth: one line in `crates/zipper-core/PORT_VERSION`.

/// Full stamp including the `v` prefix, baked at compile time from `PORT_VERSION`.
pub const PORT_VERSION: &str = concat!("v", include_str!("../PORT_VERSION"));

/// Trimmed stamp (no trailing newline from the include).
pub fn stamp() -> &'static str {
    PORT_VERSION.trim()
}
