# Galapagos, recompiled for the web

*Galapagos: Mendel's Escape* (Anark Game Studios / Electronic Arts, 1997) is a
puzzle game whose hero you cannot control. Mendel, a synthetic creature with a
learning "NERM" brain, explores hostile worlds while you manipulate the
environment around him.

This repository is a **clean-room recomp**: we reverse engineer the original
executable and data formats, then reimplement the engine in Rust compiled to
WebAssembly, so the game runs in a browser tab with nothing to install.

**Read the story:** [`web/making-of/`](web/making-of/index.html) is a
pedagogic, chapter-by-chapter logbook of the reverse engineering.

## Bring your own CD

No original game data is in this repository, and none ever will be. You drop
your own `Galapagos PC.iso` onto the page; it is read locally with ISO 9660
parsing in JavaScript and cached in your browser's private storage (OPFS).

## Layout

| Path | What |
| --- | --- |
| `crates/formats` | Pure-Rust readers for the game's files (`worlds` archive, Smacker video). Tested natively against real data when present. |
| `crates/engine` | The runtime, built as a `cdylib` for `wasm32-unknown-unknown`. Plain `extern "C"` ABI, no bindings generator. |
| `web/` | The static site: player shell (`index.html`), ISO reader, OPFS cache, WebGL/WebAudio host. |
| `web/making-of/` | The making-of. |
| `docs/re/` | Reverse-engineering notes: one Markdown spec per format, with exe addresses. |
| `tools/` | `wise_extract.py` unpacks the CD installer (to recover `galapagos.exe` for study); `build.sh` builds the WASM. |

## Build and run

```sh
rustup target add wasm32-unknown-unknown
./tools/build.sh
python3 -m http.server -d web 8000   # open http://localhost:8000
```

Tests that need game data look for it under `originals/` (gitignored) and
skip themselves when it's absent:

```sh
mkdir -p originals && cp "/path/to/Galapagos PC.iso" originals/
# mount it or extract it to originals/mounted/
cargo test --workspace
```

## Status

Playable as a living world: the Anark intro and the soundtrack play; all
eight worlds render, and their 2,089 object state machines run at 24 Hz with
3-D sound; Mendel spawns, walks with his real NERM brain, wakes the world with
his footsteps and reacts to touch; the original menus and preferences work.
In progress: save pads and the time screen, and the original camera. See the
journal at the end of the making-of.

---

Co-authored with AI. Galapagos © 1997 Anark Corporation / Electronic Arts.
This is a non-commercial preservation project.
