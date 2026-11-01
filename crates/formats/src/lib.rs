//! Readers for the file formats shipped on the Galapagos CD.
//!
//! Everything here is `no_std`-friendly in spirit: plain byte slices in,
//! plain structs out, so the same code runs natively (for tests and tools)
//! and inside the browser as WebAssembly.

pub mod bytes;
pub mod creature;
pub mod logic;
pub mod smacker;
pub mod worlds;
