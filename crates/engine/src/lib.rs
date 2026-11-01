//! WebAssembly entry points.
//!
//! We deliberately avoid wasm-bindgen: the ABI is a handful of `extern "C"`
//! functions exchanging pointers into linear memory. It is small enough to
//! read in one sitting, which is the point of this project.

use galapagos_formats::worlds::Archive;

/// Allocate `len` bytes the host (JavaScript) can write a file into.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
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
