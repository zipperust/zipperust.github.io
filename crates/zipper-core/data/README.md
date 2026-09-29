# Local golden fixtures (gitignored)

The parity test suite reads game-derived fixtures from this directory. They are
**not** committed: Zipper-Rust ships no Zipper content. Generate them locally from
your own `Zipper.pdx` (Data Disk / sideload build):

| File | Used by | Source |
|---|---|---|
| `worldmap.bin` | `worldmap.rs`, `lib.rs` tests | `main.pdz` → `worldmap` luac (ZMAP) |
| `dialogs.json` | `dialog_scripts.rs` tests | `main.pdz` → `script` luac |
| `introchord.json` | `lib.rs` intro tests | `main.pdz` → `Globals` luac |

Without these, `./scripts/build.sh` stops with instructions. On a data-free
machine (e.g. CI) run `SKIP_TESTS=1 ./scripts/build.sh`.
