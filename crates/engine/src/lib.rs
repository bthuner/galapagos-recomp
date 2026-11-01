//! WebAssembly entry points.
//!
//! We deliberately avoid wasm-bindgen: the ABI is a handful of `extern "C"`
//! functions exchanging pointers into linear memory. It is small enough to
//! read in one sitting, which is the point of this project.
//!
//! Ownership rule: when JavaScript hands us a file it first calls [`alloc`],
//! fills the buffer, then passes `(ptr, len)` to an `*_open` function, which
//! takes ownership. JavaScript must not touch or free that buffer afterwards.

use galapagos_formats::smacker::Smacker;
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
    /// The latest frame, expanded to RGBA at display size (640x480 for the intro).
    rgba: Vec<u8>,
    /// Audio track 0 for the whole movie, decoded up front.
    audio: Vec<i16>,
    /// Header facts, laid out for JavaScript to read as a Uint32Array.
    info: [u32; 6],
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
    let info = [i.width, i.display_height(), i.frame_count, i.us_per_frame, rate, channels];
    MOVIE.with(|m| *m.borrow_mut() = Some(Movie { smk, rgba: Vec::new(), audio: Vec::new(), info }));
    0
}

/// Pointer to six u32s: width, display height, frames, µs per frame,
/// audio sample rate, audio channels.
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
                movie.rgba = f.to_rgba();
                1
            }
            Err(_) => -1,
        }
    })
}

/// The current frame as width × display-height RGBA bytes.
#[no_mangle]
pub extern "C" fn movie_frame_ptr() -> *const u8 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.rgba.as_ptr()))
}

#[no_mangle]
pub extern "C" fn movie_close() {
    MOVIE.with(|m| *m.borrow_mut() = None);
}
