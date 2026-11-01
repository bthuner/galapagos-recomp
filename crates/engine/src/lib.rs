//! WebAssembly entry points.
//!
//! We deliberately avoid wasm-bindgen: the ABI is a handful of `extern "C"`
//! functions exchanging pointers into linear memory. It is small enough to
//! read in one sitting, which is the point of this project.
//!
//! Ownership rule: when JavaScript hands us a file it first calls [`alloc`],
//! fills the buffer, then passes `(ptr, len)` to an `*_open` function, which
//! takes ownership. JavaScript must not touch or free that buffer afterwards.

mod creature;
mod sim;
mod world;

use galapagos_formats::creature::{overlay_for_age, Brain, CreatureBody};
use galapagos_formats::smacker::{Smacker, YScale};
use galapagos_formats::worlds::Archive;
use std::cell::RefCell;

/// Allocate `len` bytes the host (JavaScript) can write a file into.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// Free a buffer from [`alloc`] that was never handed to an `*_open` call.
#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    unsafe { drop(Vec::from_raw_parts(ptr, 0, len)) }
}

/// Turn an [`alloc`]ated buffer back into an owned `Vec` without copying.
unsafe fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    Vec::from_raw_parts(ptr, len, len)
}

/// Returns the number of world sections, or -1 if the archive is malformed.
#[no_mangle]
pub extern "C" fn worlds_section_count(ptr: *const u8, len: usize) -> i32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    match Archive::parse(data) {
        Ok(a) => a.sections.len() as i32,
        Err(_) => -1,
    }
}

// ——— Movies (Smacker) ————————————————————————————————————————————————
//
// One movie plays at a time, like in the original game. WebAssembly here is
// single-threaded, so a thread-local RefCell is a safe home for that state.

struct Movie {
    smk: Smacker,
    /// The latest frame as RGBA at its stored size (640x240 for the intro).
    rgba: Vec<u8>,
    /// Audio track 0 for the whole movie, decoded up front.
    audio: Vec<i16>,
    /// Header facts, laid out for JavaScript to read as a Uint32Array.
    info: [u32; 8],
}

thread_local! {
    static MOVIE: RefCell<Option<Movie>> = const { RefCell::new(None) };
}

/// Open a Smacker file. Returns 0 on success, -1 if it isn't valid Smacker.
#[no_mangle]
pub extern "C" fn movie_open(ptr: *mut u8, len: usize) -> i32 {
    let Ok(smk) = Smacker::from_vec(unsafe { take(ptr, len) }) else {
        return -1;
    };
    let i = smk.info();
    let (rate, channels) = i.audio.first().map_or((0, 0), |t| (t.sample_rate, t.channels as u32));
    let y_scale = match i.y_scale {
        YScale::None => 0,
        YScale::Doubled => 1,
        YScale::Interlaced => 2,
    };
    let info = [i.width, i.height, i.display_height(), i.frame_count, i.us_per_frame, rate, channels, y_scale];
    MOVIE.with(|m| *m.borrow_mut() = Some(Movie { smk, rgba: Vec::new(), audio: Vec::new(), info }));
    0
}

/// Pointer to eight u32s: width, stored height, display height, frames,
/// µs per frame, audio sample rate, audio channels, and the Y mode
/// (0 = as stored, 1 = line-doubled, 2 = interlaced with black lines).
#[no_mangle]
pub extern "C" fn movie_info() -> *const u32 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.info.as_ptr()))
}

/// Decode the entire soundtrack (all frames) and rewind. Returns the number
/// of i16 samples (interleaved if stereo), readable at [`movie_audio_ptr`].
///
/// Why up front? The browser plays an AudioBuffer with sample-accurate timing,
/// so we let the audio clock drive which video frame is shown. Picture
/// follows sound and the two cannot drift.
#[no_mangle]
pub extern "C" fn movie_decode_audio() -> i32 {
    MOVIE.with(|m| {
        let mut guard = m.borrow_mut();
        let Some(movie) = guard.as_mut() else { return -1 };
        let mut audio = Vec::new();
        while movie.smk.frames_remaining() > 0 {
            match movie.smk.decode_next_frame() {
                Ok(f) => audio.extend_from_slice(&f.audio[0]),
                Err(_) => return -1,
            }
        }
        movie.smk.rewind();
        movie.audio = audio;
        movie.audio.len() as i32
    })
}

#[no_mangle]
pub extern "C" fn movie_audio_ptr() -> *const i16 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.audio.as_ptr()))
}

/// Decode the next frame into the RGBA buffer. Returns 1 when a frame was
/// decoded, 0 at the end of the movie, -1 on a decoding error.
#[no_mangle]
pub extern "C" fn movie_next_frame() -> i32 {
    MOVIE.with(|m| {
        let mut guard = m.borrow_mut();
        let Some(movie) = guard.as_mut() else { return -1 };
        if movie.smk.frames_remaining() == 0 {
            return 0;
        }
        match movie.smk.decode_next_frame() {
            Ok(f) => {
                movie.rgba = f.to_rgba_stored();
                1
            }
            Err(_) => -1,
        }
    })
}

/// The current frame as width × stored-height RGBA bytes. Scaling to the
/// display height is the browser's job (see web/src/movie.js).
#[no_mangle]
pub extern "C" fn movie_frame_ptr() -> *const u8 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.rgba.as_ptr()))
}

#[no_mangle]
pub extern "C" fn movie_close() {
    MOVIE.with(|m| *m.borrow_mut() = None);
}

// ——— Worlds ——————————————————————————————————————————————————————————
//
// The 126 MB archive is adopted once; a section is parsed and flattened into
// GPU buffers on request (see world.rs). Parsing is fast because the format
// is plain offsets into the same bytes: nothing is decompressed.

struct Worlds {
    data: Vec<u8>,
    mesh: Option<world::WorldMesh>,
    sim: Option<sim::Sim>,
    /// Host events handed out by the last `sim_tick`.
    events: Vec<sim::HostEvent>,
}

thread_local! {
    static WORLDS: RefCell<Option<Worlds>> = const { RefCell::new(None) };
}

/// Adopt the `worlds` file. Returns the section count, or -1 if malformed.
#[no_mangle]
pub extern "C" fn worlds_open(ptr: *mut u8, len: usize) -> i32 {
    let data = unsafe { take(ptr, len) };
    let n = match Archive::parse(&data) {
        Ok(a) => a.sections.len() as i32,
        Err(_) => return -1,
    };
    WORLDS.with(|w| {
        *w.borrow_mut() = Some(Worlds { data, mesh: None, sim: None, events: Vec::new() })
    });
    n
}

/// Build render buffers for section `i`. Returns the number of vertices
/// (7 floats each, see world.rs), or -1 on a parse error.
#[no_mangle]
pub extern "C" fn world_build(i: usize) -> i32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -1 };
        let Ok(archive) = Archive::parse(&worlds.data) else { return -1 };
        let Ok(section) = archive.world(i) else { return -1 };
        let mesh = world::WorldMesh::build(&section);
        let n = (mesh.vertices.len() / world::FLOATS_PER_VERTEX) as i32;
        let mut sim = sim::Sim::new(&section, &mesh);
        // Mendel carries his brain from section to section; the first time,
        // it comes from the CD (see `default_brain`).
        let brain = worlds.sim.take().and_then(|s| s.creature).map(|c| c.brain).or_else(|| default_brain(&archive));
        let body = CreatureBody::parse(&worlds.data, archive.header.creature as usize).ok();
        if let (Some(body), Some(brain)) = (body, brain) {
            sim.creature = Some(creature::Creature::new(&section, &mesh, body, brain));
        }
        worlds.sim = Some(sim);
        worlds.mesh = Some(mesh);
        n
    })
}

/// The age preference slider decides which pre-trained brain a new game
/// starts with (`0x4171ad`). We pick the middle of the slider, level 3.
/// (not in the original: the preferences dialog, so the age is fixed)
pub const DEFAULT_AGE: u8 = 128;

fn default_brain(archive: &Archive) -> Option<Brain> {
    let i = overlay_for_age(DEFAULT_AGE)?;
    Brain::parse(archive.data, archive.header.creature_overlays[i] as usize).ok()
}

fn with_mesh<T>(f: impl FnOnce(&world::WorldMesh) -> T, default: T) -> T {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.mesh.as_ref()).map_or(default, f))
}

#[no_mangle]
pub extern "C" fn world_vertices_ptr() -> *const f32 {
    with_mesh(|m| m.vertices.as_ptr(), core::ptr::null())
}

/// Number of 128×128 RGBA texture layers at [`world_textures_ptr`].
#[no_mangle]
pub extern "C" fn world_texture_count() -> u32 {
    with_mesh(|m| m.texture_count, 0)
}

#[no_mangle]
pub extern "C" fn world_textures_ptr() -> *const u8 {
    with_mesh(|m| m.textures.as_ptr(), core::ptr::null())
}

/// Six f32: min x, y, z, max x, y, z.
#[no_mangle]
pub extern "C" fn world_bounds_ptr() -> *const f32 {
    with_mesh(|m| m.bounds.as_ptr(), core::ptr::null())
}

/// How many objects with geometry the current section contains.
#[no_mangle]
pub extern "C" fn world_object_count() -> u32 {
    with_mesh(|m| m.objects_drawn, 0)
}

/// Far-clip distance of the current section's environment, in world units.
#[no_mangle]
pub extern "C" fn world_far_clip() -> f32 {
    with_mesh(|m| m.far_clip, 0.0)
}

/// Number of drawn objects; each has a [`world::DrawRange`] (two u32:
/// first vertex, vertex count) and a 4×4 column-major transform.
#[no_mangle]
pub extern "C" fn world_draw_count() -> u32 {
    with_mesh(|m| m.draws.len() as u32, 0)
}

#[no_mangle]
pub extern "C" fn world_draws_ptr() -> *const u32 {
    with_mesh(|m| m.draws.as_ptr() as *const u32, core::ptr::null())
}

#[no_mangle]
pub extern "C" fn world_transforms_ptr() -> *const f32 {
    with_mesh(|m| m.transforms.as_ptr(), core::ptr::null())
}

// ——— Simulation ——————————————————————————————————————————————————————
//
// JavaScript owns the clock: it calls `sim_tick` as many times as 24 Hz
// demands, then reads back what changed (transforms are re-read every frame;
// vertex changes come as one dirty range; side effects come as events).

/// Run `n` ticks of the world simulation. Returns the number of host events
/// produced (read them at [`sim_events_ptr`], 3 × i32 each).
#[no_mangle]
pub extern "C" fn sim_tick(n: u32) -> u32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return 0 };
        let (Some(sim), Some(mesh)) = (worlds.sim.as_mut(), worlds.mesh.as_mut()) else { return 0 };
        for _ in 0..n {
            sim.step(mesh);
        }
        worlds.events = std::mem::take(&mut sim.events);
        worlds.events.len() as u32
    })
}

#[no_mangle]
pub extern "C" fn sim_events_ptr() -> *const sim::HostEvent {
    WORLDS.with(|w| w.borrow().as_ref().map_or(core::ptr::null(), |w| w.events.as_ptr()))
}

/// Takes the vertex range patched since the last call. Returns -1 if
/// nothing changed; otherwise 0, with `[first, end)` in the two u32 at
/// [`sim_dirty_ptr`].
#[no_mangle]
pub extern "C" fn sim_take_dirty() -> i32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -1 };
        let Some(sim) = worlds.sim.as_mut() else { return -1 };
        match sim.dirty.take() {
            Some((a, b)) => {
                DIRTY.with(|d| d.set([a, b]));
                0
            }
            None => -1,
        }
    })
}

thread_local! {
    static DIRTY: std::cell::Cell<[u32; 2]> = const { std::cell::Cell::new([0, 0]) };
}

#[no_mangle]
pub extern "C" fn sim_dirty_ptr() -> *const u32 {
    DIRTY.with(|d| d.as_ptr() as *const u32)
}

/// Pick along a world-space ray; a hit counts as a click on the next tick.
/// Returns the object id hit, or 0.
#[no_mangle]
pub extern "C" fn sim_click(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> u32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return 0 };
        let Some(sim) = worlds.sim.as_mut() else { return 0 };
        match sim.pick([ox, oy, oz], [dx, dy, dz]) {
            Some(id) => {
                sim.click(id);
                id as u32
            }
            None => 0,
        }
    })
}

/// The simulation rate. 24 Hz is inferred: the time screen divides the
/// tick counters by 24 (docs/re/objects.md §7).
#[no_mangle]
pub extern "C" fn sim_ticks_per_second() -> u32 {
    sim::TICKS_PER_SECOND
}

thread_local! {
    static SCRATCH: std::cell::Cell<[i32; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Position (3 × i32) and current state (1 × i32) of object `id`, for
/// debugging and the making-of's "poke it from the console" examples.
#[no_mangle]
pub extern "C" fn sim_object_info(id: u32) -> *const i32 {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow().as_ref().and_then(|w| w.sim.as_ref()) {
            let p = sim.position_of(id as u16);
            SCRATCH.with(|s| s.set([p[0], p[1], p[2], sim.state_of(id as u16) as i32]));
        }
    });
    SCRATCH.with(|s| s.as_ptr() as *const i32)
}

/// Debug/teaching switch: run every state machine, ignoring folder gating.
#[no_mangle]
pub extern "C" fn sim_set_wake_all(on: u32) {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow_mut().as_mut().and_then(|w| w.sim.as_mut()) {
            sim.wake_all = on != 0;
        }
    });
}

/// Hold (1) or release (0) the "skip" key: Space in the original, which
/// makes every `STel` object count as clicked (section 0: skip the intro).
#[no_mangle]
pub extern "C" fn sim_hold_skip(on: u32) {
    with_sim_mut(|sim| sim.skip_held = on != 0);
}

fn with_sim_mut(f: impl FnOnce(&mut sim::Sim)) {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow_mut().as_mut().and_then(|w| w.sim.as_mut()) {
            f(sim);
        }
    });
}

// ——— Mendel ——————————————————————————————————————————————————————————
//
// His posed mesh is rebuilt every tick in world space (same 7-float vertex
// layout as the world, texture layers 16..19), so the renderer draws it
// with an identity model matrix.

fn with_creature<T>(f: impl FnOnce(&creature::Creature) -> T, default: T) -> T {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).and_then(|s| s.creature.as_ref()).map_or(default, f))
}

#[no_mangle]
pub extern "C" fn creature_vertices_ptr() -> *const f32 {
    with_creature(|c| c.vertices.as_ptr(), core::ptr::null())
}

/// Number of vertices at [`creature_vertices_ptr`] (0 while he is absent).
#[no_mangle]
pub extern "C" fn creature_vertex_count() -> u32 {
    with_creature(|c| (c.vertices.len() / world::FLOATS_PER_VERTEX) as u32, 0)
}

thread_local! {
    static CREATURE_STATE: std::cell::Cell<[f32; 16]> = const { std::cell::Cell::new([0.0; 16]) };
}

/// Sixteen f32 describing Mendel, for the camera and the HUD:
///
/// ```text
/// 0 mode (0 absent, 1 appearing, 2 alive, 3 vanishing)   1..3 x, y, z
/// 4 heading (1024ths)   5 ground object id (0 = airborne)   6 ground polygon
/// 7 left eye/touch   8 right   9 posture   10 speed   11 turn  (actuators, 0..256)
/// 12 distance walked   13 touch events posted   14 deaths   15 generations evolved
/// ```
#[no_mangle]
pub extern "C" fn creature_state() -> *const f32 {
    let s = with_creature(
        |c| {
            let mode = match c.mode {
                creature::Mode::Absent => 0.0,
                creature::Mode::Appearing(_) => 1.0,
                creature::Mode::Alive => 2.0,
                creature::Mode::Vanishing(..) => 3.0,
            };
            let (gobj, gpoly) = c.ground().map_or((0.0, 0.0), |(o, p)| (o as f32 + 1.0, p as f32));
            let [a0, a1, a2] = c.outputs().map(|v| v as f32);
            [
                mode, c.pos[0], c.pos[1], c.pos[2], c.heading() as f32, gobj, gpoly, c.left as f32, c.right as f32, a0, a1, a2,
                c.stats.walked, c.stats.touches_posted as f32, c.stats.deaths as f32, c.stats.evolutions as f32,
            ]
        },
        [0.0; 16],
    );
    CREATURE_STATE.with(|c| c.set(s));
    CREATURE_STATE.with(|c| c.as_ptr() as *const f32)
}

/// Right click: ray against Mendel. Returns 1 if his left side was touched,
/// 2 for the right, 0 for a miss. A touch is felt for 4 ticks.
#[no_mangle]
pub extern "C" fn creature_touch_ray(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> u32 {
    let mut out = 0;
    with_sim_mut(|sim| {
        if let Some(c) = sim.creature.as_mut() {
            if let Some(left) = c.pick([ox, oy, oz], [dx, dy, dz]) {
                c.touch(left);
                out = if left { 1 } else { 2 };
            }
        }
    });
    out
}

/// Chase-camera collision: the clear fraction (0..1) of the segment from
/// `(fx, fy, fz)` to the eye `(ex, ey, ez)` (see `Creature::camera_clearance`).
#[no_mangle]
pub extern "C" fn creature_camera_clearance(fx: f32, fy: f32, fz: f32, ex: f32, ey: f32, ez: f32) -> f32 {
    WORLDS.with(|w| {
        let guard = w.borrow();
        let Some(sim) = guard.as_ref().and_then(|w| w.sim.as_ref()) else { return 1.0 };
        sim.creature.as_ref().map_or(1.0, |c| c.camera_clearance(sim, [fx, fy, fz], [ex, ey, ez]))
    })
}
