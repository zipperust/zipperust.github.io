# Zipper-Rust

**Play it here: <https://zipperust.github.io>**

A from-scratch engine for **Zipper** (Bennett Foddy / Panic, Playdate): the game's rules, rendering, and audio reimplemented in Rust and compiled to WebAssembly.

You bring your own `Zipper.pdx` (Data Disk / sideload copy); the browser reads its art, audio, fonts, and outdoor map (from `main.pdz`) locally.

## What this is

- Reverse-engineered Rust → WASM browser host for Zipper’s rules and rendering
- **Bring your own assets (BYOA)**: zip or folder drop → `loadPdi` / `loadPdt` / `loadPda` / `loadPft` / `loadWorldmap`
- Map extract via vendored `lib/pdz.js` + `lib/worldmap_from_luac.js` (no Java unluac in the client)

## Provenance

The Rust engine is an independent reimplementation of Zipper's observable behavior, including none of the original game's code or content. Zipper and its content remain © Bennett Foddy / Panic.

## How to use

1. On a Playdate with Zipper installed, enable Data Disk and copy the `Zipper.pdx` folder to your computer.
2. Optionally zip that folder (`Zipper.pdx.zip`).
3. Open <https://zipperust.github.io>, drop the zip or folder, wait for ingest.

## Prerequisites

- [Rust + rustup](https://rustup.rs) — the exact channel and the
  `wasm32-unknown-unknown` target are pinned in `rust-toolchain.toml`, so rustup
  installs them on first use.
- `python3` — `scripts/build.sh` uses it to stamp the shell assets
  (`index.html`, `main.js`, `sw.js`), and it serves the site locally.
- Network access on the first build: `cargo` fetches crates and `wasm-pack`
  downloads `wasm-opt`.

## Build

The Rust engine (`crates/zipper-core`, `crates/zipper-wasm`) compiles to `./pkg`
(untracked) with `wasm-pack`. `pkg/` is **build output and is not committed**:
the published site is built from this source by
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) and deployed to
GitHub Pages.

```bash
# Fresh clone without your own Zipper.pdx (no golden fixtures yet):
SKIP_TESTS=1 NO_BUMP=1 ./scripts/build.sh

# With local fixtures present, this also runs the parity tests and advances the
# version letter (use it only when you mean to ship):
./scripts/build.sh

GOD=1 ./scripts/build.sh      # dev God tools (hidden unless #god)
```

Parity tests need golden fixtures derived from your own `Zipper.pdx` (see
`crates/zipper-core/data/README.md`); they are gitignored and never published,
so a plain `./scripts/build.sh` aborts on a fresh clone — use the
`SKIP_TESTS=1` form above. Feature landings hand-bump the minor in
`crates/zipper-core/PORT_VERSION` and reset the letter to `a`.

### Verify the published wasm

Nothing compiled is shipped in the repository — the site's wasm is built from
the Rust in this repo on every push. To reproduce that build locally:

```bash
cargo install wasm-pack --version 0.13.1 --locked
SKIP_TESTS=1 NO_BUMP=1 CACHE_TOKEN=local ./scripts/build.sh
```

(rustup installs the pinned `wasm32-unknown-unknown` target automatically from
`rust-toolchain.toml`, so no separate `rustup target add` is needed.)

Vendored third-party code is listed in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Local serve

Serve the **repository root**: `main.js` imports `./pkg/zipper_wasm.js`, so the
shell and the build output must share an origin (module workers also need a real
origin, not `file://`):

```bash
python3 -m http.server 8080   # any free port works
# open http://127.0.0.1:8080
```

## License / rights

Zipper-Rust's own source is MIT-licensed (see [LICENSE](LICENSE)). Zipper and its content (art, audio, fonts, Lua, map/dialog data) remain © Bennett Foddy / Panic and are not covered by that license: this repository ships only the runtime and extract helpers, and use requires a copy of Zipper you already own. Vendored third-party code is listed in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Disclaimer

100% vibe coded.
