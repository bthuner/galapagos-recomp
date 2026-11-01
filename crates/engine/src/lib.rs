//! WebAssembly entry points.
//!
//! We deliberately avoid wasm-bindgen: the ABI is a handful of `extern "C"`
//! functions exchanging pointers into linear memory. It is small enough to
//! read in one sitting, which is the point of this project.
//!
//! Ownership rule: when JavaScript hands us a file it first calls [`alloc`],
//! fills the buffer, then passes `(ptr, len)` to an `*_open` function, which
//! takes ownership. JavaScript must not touch or free that buffer afterwards.

mod world;

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
    WORLDS.with(|w| *w.borrow_mut() = Some(Worlds { data, mesh: None }));
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
        worlds.mesh = Some(mesh);
        n
    })
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
