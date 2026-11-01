//! RAD Game Tools **Smacker** decoder (`SMK2`, plus the `SMK4` variant),
//! written from scratch for the Galapagos recomp.
//!
//! Every cinematic *and* every music / voice track on the Galapagos CD
//! (`install/data/0` .. `10`) is a Smacker file. In 1997 Smacker was *the*
//! way to put video on a CD-ROM: it decodes with nothing but table lookups
//! and byte copies, so a 486 could play 640x240 at 24 fps.
//!
//! # The big picture
//!
//! ```text
//! +-------------------+  104 bytes: size, frame count, rate, flags,
//! | header            |  audio track descriptions, Huffman tree sizes
//! +-------------------+
//! | frame sizes  u32  |  one per frame; bit 0 = keyframe
//! | frame types  u8   |  one per frame; bit 0 = palette, bits 1..7 = audio
//! +-------------------+
//! | Huffman trees     |  four trees, bit-packed: MMAP, MCLR, FULL, TYPE
//! +-------------------+
//! | frame 0           |  [palette chunk] [audio chunk]* [video bitstream]
//! | frame 1           |
//! | ...               |
//! +-------------------+
//! ```
//!
//! * **Video** is 8-bit indexed colour, cut into 4x4 blocks. Each block is
//!   one of four kinds — *mono* (2 colours + a 16-bit mask), *full* (16
//!   explicit pixels), *void* (unchanged from last frame) or *solid* (one
//!   colour) — and runs of identical kinds are coded once.
//! * All the numbers above come out of **Huffman trees** whose shapes are
//!   stored once in the file header. Their 16-bit symbols are themselves
//!   compressed with two small 8-bit trees, and a three-slot "recently used"
//!   cache makes repeated symbols cheap.
//! * **Palettes** are sent as deltas against the previous frame's palette.
//! * **Audio** is either raw PCM or Huffman-coded *DPCM* (each sample is
//!   stored as the difference from the previous one).
//!
//! # Bits are read LSB-first
//!
//! Every bitstream in Smacker is little-endian *at the bit level*: the first
//! bit of a byte is its lowest bit (`byte & 1`), and multi-bit fields are
//! assembled with the first bit read as their least significant bit.
//!
//! # Usage
//!
//! ```no_run
//! # fn demo(bytes: &[u8]) -> Result<(), galapagos_formats::smacker::Error> {
//! use galapagos_formats::smacker::Smacker;
//! let mut smk = Smacker::parse(bytes)?;
//! while smk.frames_remaining() > 0 {
//!     let frame = smk.decode_next_frame()?;
//!     let rgba = frame.to_rgba();          // ready for a <canvas>
//!     let pcm = &frame.audio[0];           // i16 interleaved, track 0
//!     # let _ = (rgba, pcm);
//! }
//! # Ok(()) }
//! ```

use core::fmt;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong while reading a Smacker file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file does not start with `SMK2` or `SMK4`.
    BadSignature([u8; 4]),
    /// A structure claims to extend past the end of the data it lives in.
    Truncated(&'static str),
    /// The data is self-inconsistent (impossible tree, bad palette op...).
    Corrupt(&'static str),
    /// The audio track uses Bink audio compression, which we do not decode.
    UnsupportedAudio,
    /// `decode_next_frame` was called after the last frame.
    EndOfStream,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadSignature(s) => write!(f, "not a Smacker file (signature {s:?})"),
            Error::Truncated(what) => write!(f, "truncated Smacker data: {what}"),
            Error::Corrupt(what) => write!(f, "corrupt Smacker data: {what}"),
            Error::UnsupportedAudio => write!(f, "Bink-compressed audio is not supported"),
            Error::EndOfStream => write!(f, "no more frames"),
        }
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------------------
// The bit reader
// ---------------------------------------------------------------------------

/// Reads bits LSB-first out of a byte slice. Smacker's encoder wrote bits
/// into the low end of each byte first, so bit `n` of the stream is
/// `(data[n / 8] >> (n % 8)) & 1`.
struct BitReader<'a> {
    data: &'a [u8],
    /// Position in *bits* from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<bool, Error> {
        let byte = *self
            .data
            .get(self.pos >> 3)
            .ok_or(Error::Truncated("bitstream ended early"))?;
        let b = (byte >> (self.pos & 7)) & 1;
        self.pos += 1;
        Ok(b != 0)
    }

    /// Reads `n` (<= 16) bits; the first bit read becomes bit 0 of the result.
    fn bits(&mut self, n: u32) -> Result<u32, Error> {
        let mut v = 0;
        for i in 0..n {
            v |= (self.bit()? as u32) << i;
        }
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Huffman trees
// ---------------------------------------------------------------------------

/// High bit of a tree slot: "this is an inner node, not a leaf".
///
/// Trees are stored *flattened in pre-order*: an inner node at index `i` is
/// followed by its left (bit 0) subtree at `i + 1`, and its right (bit 1)
/// subtree starts `left_size` slots later. The low bits of a node slot hold
/// that `left_size`, so walking the tree is pure arithmetic — exactly the
/// layout RAD's own decoder used.
const NODE: u32 = 0x8000_0000;

/// A flattened Huffman tree. Leaves hold the decoded symbol.
#[derive(Clone, Debug)]
struct Tree {
    slots: Vec<u32>,
}

impl Tree {
    /// A tree that is just one leaf: decoding consumes no bits and always
    /// yields `value`. Used when the file says "this tree is absent".
    fn constant(value: u32) -> Self {
        Self { slots: vec![value] }
    }

    /// Walk from the root following one bit per inner node; returns the
    /// index of the leaf reached.
    fn walk(&self, bits: &mut BitReader) -> Result<usize, Error> {
        let mut i = 0;
        loop {
            let slot = self.slots[i];
            if slot & NODE == 0 {
                return Ok(i);
            }
            if bits.bit()? {
                i += (slot & !NODE) as usize;
            }
            i += 1;
            if i >= self.slots.len() {
                return Err(Error::Corrupt("Huffman walk left the tree"));
            }
        }
    }

    fn decode(&self, bits: &mut BitReader) -> Result<u32, Error> {
        Ok(self.slots[self.walk(bits)?])
    }
}

/// Reads an **8-bit tree** (symbols 0..=255). Used inside the video trees
/// and for every audio channel.
///
/// On disk: one "present" bit. If 0, the tree is absent and always decodes
/// to 0. If 1, a recursive pre-order description follows — bit 1 = inner
/// node (left then right subtree follow), bit 0 = leaf followed by its
/// 8-bit symbol — and then one terminating 0 bit.
fn read_byte_tree(bits: &mut BitReader) -> Result<Tree, Error> {
    if !bits.bit()? {
        return Ok(Tree::constant(0));
    }
    let mut tree = Tree { slots: Vec::new() };
    read_byte_tree_node(bits, &mut tree, 0)?;
    bits.bit()?; // terminator, always 0
    Ok(tree)
}

/// Returns the number of slots the subtree occupies.
fn read_byte_tree_node(bits: &mut BitReader, tree: &mut Tree, depth: u32) -> Result<u32, Error> {
    // 256 leaves can't need codes longer than 32 bits in a sane file.
    if depth > 32 || tree.slots.len() > 512 {
        return Err(Error::Corrupt("8-bit Huffman tree too deep"));
    }
    if !bits.bit()? {
        tree.slots.push(bits.bits(8)?);
        return Ok(1);
    }
    let me = tree.slots.len();
    tree.slots.push(NODE);
    let left = read_byte_tree_node(bits, tree, depth + 1)?;
    tree.slots[me] = NODE | left;
    let right = read_byte_tree_node(bits, tree, depth + 1)?;
    Ok(1 + left + right)
}

/// A **big tree**: a Huffman tree of 16-bit symbols with a three-entry
/// "recently used" cache. All video data (block types, colours, masks and
/// pixels) is coded with four of these.
///
/// The trick: three particular symbol values, the *escapes*, are stored in
/// the tree in place of real values. When the decoder lands on such a leaf
/// it returns whatever value currently sits in that cache slot. After every
/// decode the cache is shifted (`c2 = c1; c1 = c0; c0 = value`) so codes for
/// "same as last time", "same as the time before" etc. can be very short.
#[derive(Clone, Debug)]
struct BigTree {
    tree: Tree,
    /// Slot indices of the three cache leaves inside `tree.slots`.
    cache: [usize; 3],
}

impl BigTree {
    /// Reads one big tree from the header bitstream. `size_hint` is the
    /// size in bytes the header reserved for it, used as a sanity bound.
    fn read(bits: &mut BitReader, size_hint: u32) -> Result<BigTree, Error> {
        if !bits.bit()? {
            // Absent tree: always decodes 0. Point all cache slots at the
            // single leaf so the cache logic stays harmless.
            return Ok(BigTree { tree: Tree::constant(0), cache: [0; 3] });
        }
        // The 16-bit symbols are sent as (low byte, high byte), each coded
        // with its own 8-bit tree.
        let low = read_byte_tree(bits)?;
        let high = read_byte_tree(bits)?;
        // Escape values are plain 16-bit fields.
        let escapes = [bits.bits(16)?, bits.bits(16)?, bits.bits(16)?];

        let mut builder = BigTreeBuilder {
            tree: Tree { slots: Vec::new() },
            low,
            high,
            escapes,
            cache: [None; 3],
            max_slots: (size_hint as usize).div_ceil(4) + 4,
        };
        builder.node(bits, 0)?;
        bits.bit()?; // terminator, always 0

        let BigTreeBuilder { mut tree, cache, .. } = builder;
        // An escape that never appeared in the tree still needs a slot for
        // the cache shuffle to write into; give it a private one that the
        // walk can never reach.
        let cache = cache.map(|c| {
            c.unwrap_or_else(|| {
                tree.slots.push(0);
                tree.slots.len() - 1
            })
        });
        Ok(BigTree { tree, cache })
    }

    /// Clears the cache values. Done at the start of every video frame.
    fn reset(&mut self) {
        for &c in &self.cache {
            self.tree.slots[c] = 0;
        }
    }

    fn decode(&mut self, bits: &mut BitReader) -> Result<u32, Error> {
        let leaf = self.tree.walk(bits)?;
        let value = self.tree.slots[leaf];
        let [c0, c1, c2] = self.cache;
        let s = &mut self.tree.slots;
        if value != s[c0] {
            s[c2] = s[c1];
            s[c1] = s[c0];
            s[c0] = value;
        }
        Ok(value)
    }
}

struct BigTreeBuilder {
    tree: Tree,
    low: Tree,
    high: Tree,
    escapes: [u32; 3],
    cache: [Option<usize>; 3],
    max_slots: usize,
}

impl BigTreeBuilder {
    /// Same pre-order shape as the 8-bit tree, but a leaf's value is coded
    /// as `low_tree symbol | high_tree symbol << 8`.
    fn node(&mut self, bits: &mut BitReader, depth: u32) -> Result<u32, Error> {
        if depth > 500 || self.tree.slots.len() >= self.max_slots {
            return Err(Error::Corrupt("big Huffman tree too large"));
        }
        if !bits.bit()? {
            let lo = self.low.decode(bits)?;
            let hi = self.high.decode(bits)?;
            let mut value = lo | (hi << 8);
            let here = self.tree.slots.len();
            // Leaves carrying an escape value become cache slots
            // (starting out as 0).
            if let Some(k) = self.escapes.iter().position(|&e| e == value) {
                self.cache[k] = Some(here);
                value = 0;
            }
            self.tree.slots.push(value);
            return Ok(1);
        }
        let me = self.tree.slots.len();
        self.tree.slots.push(NODE);
        let left = self.node(bits, depth + 1)?;
        self.tree.slots[me] = NODE | left;
        let right = self.node(bits, depth + 1)?;
        Ok(1 + left + right)
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Which flavour of the bitstream this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    /// `SMK2`: the original format. Galapagos only uses this one.
    Smk2,
    /// `SMK4`: adds two cheaper "full block" sub-modes (see `decode_video`).
    Smk4,
}

/// How the stored frame maps to the screen vertically (header flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YScale {
    /// Show the frame as stored.
    None,
    /// Flag bit 1: each stored line is followed by a *black* line. Half the
    /// data, a "TV scanline" look — the Galapagos intro uses this.
    Interlaced,
    /// Flag bit 2: each stored line is shown twice.
    Doubled,
}

/// How an audio track's chunks are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCompression {
    /// Plain PCM: unsigned 8-bit or signed 16-bit little-endian.
    Raw,
    /// Smacker's Huffman-coded DPCM (`SMK_AUD_PACKED`).
    Dpcm,
    /// Bink audio (later RAD codec) — recognised, not decoded.
    Bink,
}

/// One of the up-to-seven audio tracks a Smacker file can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTrack {
    /// 0..=6, the track number (frames flag tracks by this index).
    pub index: usize,
    pub sample_rate: u32,
    /// 1 = mono, 2 = stereo.
    pub channels: u8,
    /// 8 or 16 as stored. Decoded samples are always handed out as `i16`.
    pub bits: u8,
    pub compression: AudioCompression,
    /// Largest decoded chunk in bytes (the header's buffer-size hint).
    pub max_chunk_bytes: u32,
}

/// Everything the 104-byte header tells us.
#[derive(Clone, Debug)]
pub struct Info {
    pub version: Version,
    /// Stored frame size, in pixels. Always a multiple of 4 in practice.
    pub width: u32,
    pub height: u32,
    /// Number of frames to play (excluding the optional "ring frame").
    pub frame_count: u32,
    /// Microseconds per frame (see [`Info::fps`]).
    pub us_per_frame: u32,
    /// The file has an extra trailing frame that turns frame N back into
    /// frame 0, for seamless looping.
    pub ring_frame: bool,
    pub y_scale: YScale,
    pub audio: Vec<AudioTrack>,
}

impl Info {
    pub fn fps(&self) -> f64 {
        1_000_000.0 / self.us_per_frame as f64
    }

    /// Height once the Y-interlace / Y-double flag is applied.
    pub fn display_height(&self) -> u32 {
        match self.y_scale {
            YScale::None => self.height,
            _ => self.height * 2,
        }
    }

    pub fn duration_us(&self) -> u64 {
        self.frame_count as u64 * self.us_per_frame as u64
    }
}

/// Converts the header's signed frame-rate field to microseconds per frame.
///
/// * `> 0`: milliseconds per frame (`100` = 10 fps).
/// * `< 0`: *hundredths* of a millisecond, i.e. units of 10 µs
///   (`-4166` = 41.66 ms = 24 fps). This is how non-integer-ms rates such
///   as 24 fps were expressed.
/// * `0`: the default, 10 fps.
pub fn rate_to_us_per_frame(rate: i32) -> u32 {
    match rate {
        r if r > 0 => r as u32 * 1000,
        r if r < 0 => r.unsigned_abs() * 10,
        _ => 100_000,
    }
}

/// Where a frame lives in the file and what it contains.
#[derive(Clone, Copy, Debug)]
struct FrameEntry {
    offset: usize,
    size: usize,
    keyframe: bool,
    /// Bit 0: palette chunk present. Bit `1 + n`: audio track `n` present.
    contents: u8,
}

const HEADER_SIZE: usize = 104;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// A 256-entry RGB palette, 8 bits per channel.
pub type Palette = [[u8; 3]; 256];

/// Smacker stores colours as 6-bit VGA DAC values (0..=63). This expands
/// them to 8 bits by copying the top bits into the bottom ones
/// (`v << 2 | v >> 4`), so 0 maps to 0 and 63 maps to 255 exactly. The
/// result is identical to RAD's published `palmap` table
/// (0x00, 0x04, ... 0x3C, 0x41, 0x45, ... 0xFB, 0xFF).
pub const PALMAP: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = ((i << 2) | (i >> 4)) as u8;
        i += 1;
    }
    t
};

/// Applies one palette chunk (without its leading length byte) to `pal`.
///
/// The chunk is a list of operations walking the 256 entries in order:
///
/// | first byte  | meaning                                                   |
/// |-------------|-----------------------------------------------------------|
/// | `1nnnnnnn`  | keep the next `n + 1` entries unchanged                   |
/// | `01nnnnnn`, `s` | copy `n + 1` entries from the *previous* palette, starting at index `s` |
/// | `00rrrrrr`, `g`, `b` | one new colour (6-bit components)                 |
fn apply_palette_chunk(pal: &mut Palette, chunk: &[u8]) -> Result<(), Error> {
    let old = *pal;
    let mut i = 0; // palette entry being written
    let mut p = 0; // position in chunk
    let byte = |p: usize| chunk.get(p).copied().ok_or(Error::Truncated("palette chunk"));
    while i < 256 && p < chunk.len() {
        let op = chunk[p];
        p += 1;
        if op & 0x80 != 0 {
            i += (op & 0x7F) as usize + 1;
        } else if op & 0x40 != 0 {
            let count = (op & 0x3F) as usize + 1;
            let src = byte(p)? as usize;
            p += 1;
            if src + count > 256 {
                return Err(Error::Corrupt("palette copy out of range"));
            }
            for k in 0..count {
                if i >= 256 {
                    break;
                }
                pal[i] = old[src + k];
                i += 1;
            }
        } else {
            let g = byte(p)?;
            let b = byte(p + 1)?;
            p += 2;
            pal[i] = [PALMAP[op as usize & 0x3F], PALMAP[g as usize & 0x3F], PALMAP[b as usize & 0x3F]];
            i += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// A run length for each of the 64 values of the 6-bit run field in a block
/// type symbol: 1..=59 literally, then 128, 256, 512, 1024, 2048 for long
/// stretches of identical blocks (typically "unchanged").
const BLOCK_RUNS: [usize; 64] = {
    let mut t = [0usize; 64];
    let mut i = 0;
    while i < 59 {
        t[i] = i + 1;
        i += 1;
    }
    t[59] = 128;
    t[60] = 256;
    t[61] = 512;
    t[62] = 1024;
    t[63] = 2048;
    t
};

/// The four video trees, in the order they are stored in the header.
struct VideoTrees {
    /// MMAP: 16-bit masks for mono blocks (which of the 16 pixels use the
    /// "high" colour).
    mono_map: BigTree,
    /// MCLR: colour pairs for mono blocks (`high << 8 | low`).
    mono_colors: BigTree,
    /// FULL: pairs of pixels for full blocks.
    full: BigTree,
    /// TYPE: block type + run length (+ colour for solid blocks).
    block_type: BigTree,
}

/// Decodes one frame's video bitstream on top of the previous picture.
///
/// The frame is visited as a raster of 4x4 blocks, left to right, top to
/// bottom. Each TYPE symbol is laid out as:
///
/// ```text
///  15            8 7          2 1  0
/// +---------------+------------+----+
/// | extra (colour)| run index  |kind|
/// +---------------+------------+----+
/// ```
///
/// and covers `BLOCK_RUNS[run index]` consecutive blocks of that kind:
///
/// * `0` **mono**: per block, one MCLR symbol (two colours) and one MMAP
///   symbol (16-bit mask, LSB = top-left, 4 bits per row).
/// * `1` **full**: per block, 8 FULL symbols = 16 explicit pixels. Each
///   row is two symbols, *right half first*. `SMK4` adds two cheaper
///   sub-modes chosen by 1-2 bits before the run (see below).
/// * `2` **void**: leave the blocks as they were in the previous frame.
/// * `3` **solid**: fill each block with colour `extra`.
fn decode_video(
    bits: &mut BitReader,
    trees: &mut VideoTrees,
    pixels: &mut [u8],
    width: usize,
    height: usize,
    version: Version,
) -> Result<(), Error> {
    for t in [&mut trees.mono_map, &mut trees.mono_colors, &mut trees.full, &mut trees.block_type] {
        t.reset();
    }
    let blocks_wide = width / 4;
    let total_blocks = blocks_wide * (height / 4);
    let stride = width;
    let mut block = 0;

    while block < total_blocks {
        let ty = trees.block_type.decode(bits)?;
        let run = BLOCK_RUNS[((ty >> 2) & 0x3F) as usize];
        let kind = ty & 3;

        // SMK4 full blocks: "1" = double-wide-and-tall pixels (2 symbols
        // per block), "01" = double-tall pixels (4 symbols), "00" = normal.
        let mut full_mode = 0;
        if kind == 1 && version == Version::Smk4 {
            if bits.bit()? {
                full_mode = 1;
            } else if bits.bit()? {
                full_mode = 2;
            }
        }

        let end = (block + run).min(total_blocks);
        while block < end {
            let x = (block % blocks_wide) * 4;
            let y = (block / blocks_wide) * 4;
            let at = y * stride + x;
            match kind {
                0 => {
                    let colors = trees.mono_colors.decode(bits)?;
                    let mut map = trees.mono_map.decode(bits)?;
                    let hi = (colors >> 8) as u8;
                    let lo = colors as u8;
                    for row in 0..4 {
                        let line = &mut pixels[at + row * stride..at + row * stride + 4];
                        for px in line.iter_mut() {
                            *px = if map & 1 != 0 { hi } else { lo };
                            map >>= 1;
                        }
                    }
                }
                1 => match full_mode {
                    0 => {
                        for row in 0..4 {
                            let o = at + row * stride;
                            let right = trees.full.decode(bits)?;
                            let left = trees.full.decode(bits)?;
                            pixels[o + 2] = right as u8;
                            pixels[o + 3] = (right >> 8) as u8;
                            pixels[o] = left as u8;
                            pixels[o + 1] = (left >> 8) as u8;
                        }
                    }
                    1 => {
                        for half in 0..2 {
                            let pix = trees.full.decode(bits)?;
                            let (a, b) = (pix as u8, (pix >> 8) as u8);
                            for row in half * 2..half * 2 + 2 {
                                let o = at + row * stride;
                                pixels[o..o + 4].copy_from_slice(&[a, a, b, b]);
                            }
                        }
                    }
                    _ => {
                        for half in 0..2 {
                            let right = trees.full.decode(bits)?;
                            let left = trees.full.decode(bits)?;
                            let row4 = [left as u8, (left >> 8) as u8, right as u8, (right >> 8) as u8];
                            for row in half * 2..half * 2 + 2 {
                                let o = at + row * stride;
                                pixels[o..o + 4].copy_from_slice(&row4);
                            }
                        }
                    }
                },
                2 => {}
                _ => {
                    let color = (ty >> 8) as u8;
                    for row in 0..4 {
                        let o = at + row * stride;
                        pixels[o..o + 4].fill(color);
                    }
                }
            }
            block += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Decodes one audio chunk to interleaved `i16` samples (8-bit audio is
/// widened: `(u8 - 128) << 8`).
fn decode_audio(track: &AudioTrack, chunk: &[u8], out: &mut Vec<i16>) -> Result<(), Error> {
    match track.compression {
        AudioCompression::Bink => Err(Error::UnsupportedAudio),
        AudioCompression::Raw => {
            if track.bits == 16 {
                out.extend(chunk.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
            } else {
                out.extend(chunk.iter().map(|&b| u8_to_i16(b)));
            }
            Ok(())
        }
        AudioCompression::Dpcm => decode_dpcm(chunk, out),
    }
}

fn u8_to_i16(b: u8) -> i16 {
    ((b as i16) - 128) << 8
}

/// Smacker's packed audio ("DPCM"): a 32-bit decoded size, then a
/// bitstream holding
///
/// 1. three flag bits: *has data*, *stereo*, *16-bit*;
/// 2. one 8-bit Huffman tree per byte lane — 1 (8-bit mono), 2 (8-bit
///    stereo or 16-bit mono) or 4 (16-bit stereo: L-low, L-high, R-low,
///    R-high);
/// 3. the first sample of each channel stored verbatim (right channel
///    first; 16-bit values big-endian);
/// 4. then, for every following sample, a Huffman-coded *difference* that
///    is added to that channel's previous sample.
///
/// Differences of real-world sound are small and cluster around zero, so
/// they Huffman-code far better than the samples themselves.
fn decode_dpcm(chunk: &[u8], out: &mut Vec<i16>) -> Result<(), Error> {
    if chunk.len() < 4 {
        return Err(Error::Truncated("packed audio chunk"));
    }
    let unpacked_bytes = u32::from_le_bytes(chunk[0..4].try_into().unwrap()) as usize;
    let mut bits = BitReader::new(&chunk[4..]);
    if !bits.bit()? {
        return Ok(()); // "no data" chunk: silence
    }
    let stereo = bits.bit()? as usize;
    let sixteen = bits.bit()?;
    let lanes = 1 << (sixteen as usize + stereo);
    let mut trees = Vec::with_capacity(lanes);
    for _ in 0..lanes {
        trees.push(read_byte_tree(&mut bits)?);
    }

    let channels = stereo + 1;
    out.reserve(if sixteen { unpacked_bytes / 2 } else { unpacked_bytes });

    if sixteen {
        let samples = unpacked_bytes / 2;
        let mut pred = [0i16; 2];
        for c in (0..channels).rev() {
            let hi = bits.bits(8)?;
            let lo = bits.bits(8)?;
            pred[c] = ((hi << 8) | lo) as u16 as i16;
        }
        out.extend_from_slice(&pred[..channels]);
        for i in channels..samples {
            let c = i & stereo; // odd samples are the right channel
            let lo = trees[c * 2].decode(&mut bits)?;
            let hi = trees[c * 2 + 1].decode(&mut bits)?;
            pred[c] = pred[c].wrapping_add((lo | (hi << 8)) as u16 as i16);
            out.push(pred[c]);
        }
    } else {
        let samples = unpacked_bytes;
        let mut pred = [0u8; 2];
        for c in (0..channels).rev() {
            pred[c] = bits.bits(8)? as u8;
        }
        out.extend(pred[..channels].iter().map(|&b| u8_to_i16(b)));
        for i in channels..samples {
            let c = i & stereo;
            let delta = trees[c].decode(&mut bits)? as u8; // two's-complement i8
            pred[c] = pred[c].wrapping_add(delta);
            out.push(u8_to_i16(pred[c]));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The decoder
// ---------------------------------------------------------------------------

/// One decoded frame. Borrowed from the decoder: copy what you need before
/// decoding the next one.
pub struct Frame<'a> {
    /// 0-based frame number.
    pub index: usize,
    pub keyframe: bool,
    /// True when this frame carried a palette chunk.
    pub palette_changed: bool,
    /// Stored dimensions (before Y-interlace / Y-double).
    pub width: u32,
    pub height: u32,
    pub y_scale: YScale,
    /// `width * height` palette indices, row-major.
    pub pixels: &'a [u8],
    pub palette: &'a Palette,
    /// Samples for each of the 7 possible tracks (empty when the frame has
    /// none for that track), interleaved L,R,L,R for stereo.
    pub audio: [Vec<i16>; 7],
}

impl Frame<'_> {
    /// Height after applying the Y-scale flag.
    pub fn display_height(&self) -> u32 {
        match self.y_scale {
            YScale::None => self.height,
            _ => self.height * 2,
        }
    }

    /// RGBA at the *stored* size, ignoring the Y flags. A browser can then
    /// stretch it to the display height with smooth filtering. For the
    /// interlaced intro that's line doubling with interpolation, which is closer
    /// to what a blurry 1997 CRT showed than hard black gaps on a sharp LCD.
    pub fn to_rgba_stored(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for &idx in self.pixels {
            let [r, g, b] = self.palette[idx as usize];
            out.extend_from_slice(&[r, g, b, 255]);
        }
        out
    }

    /// Converts to RGBA8 (`width * display_height * 4` bytes), applying
    /// Y-interlace (black odd lines) or Y-double. This is the buffer you
    /// hand to `ImageData` / `putImageData` in the browser.
    pub fn to_rgba(&self) -> Vec<u8> {
        let w = self.width as usize;
        let mut out = vec![0u8; w * self.display_height() as usize * 4];
        let line_bytes = w * 4;
        for (y, row) in self.pixels.chunks_exact(w).enumerate() {
            let dst_y = match self.y_scale {
                YScale::None => y,
                _ => y * 2,
            };
            let dst = &mut out[dst_y * line_bytes..(dst_y + 1) * line_bytes];
            for (px, &idx) in dst.chunks_exact_mut(4).zip(row) {
                let [r, g, b] = self.palette[idx as usize];
                px.copy_from_slice(&[r, g, b, 255]);
            }
            match self.y_scale {
                YScale::Doubled => out.copy_within(dst_y * line_bytes..(dst_y + 1) * line_bytes, (dst_y + 1) * line_bytes),
                YScale::Interlaced => {
                    // Black, but opaque.
                    for px in out[(dst_y + 1) * line_bytes..(dst_y + 2) * line_bytes].chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                }
                YScale::None => {}
            }
        }
        out
    }
}

/// A Smacker file ready to be played frame by frame.
pub struct Smacker {
    data: Vec<u8>,
    info: Info,
    frames: Vec<FrameEntry>,
    trees: VideoTrees,
    pixels: Vec<u8>,
    palette: Palette,
    next_frame: usize,
}

impl Smacker {
    /// Parses a Smacker file held in memory (the bytes are copied).
    pub fn parse(data: &[u8]) -> Result<Smacker, Error> {
        Self::from_vec(data.to_vec())
    }

    /// Like [`Smacker::parse`] but takes ownership, avoiding the copy.
    pub fn from_vec(data: Vec<u8>) -> Result<Smacker, Error> {
        if data.len() < HEADER_SIZE {
            return Err(Error::Truncated("header"));
        }
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let signature: [u8; 4] = data[0..4].try_into().unwrap();
        let version = match &signature {
            b"SMK2" => Version::Smk2,
            b"SMK4" => Version::Smk4,
            _ => return Err(Error::BadSignature(signature)),
        };
        // Header layout (all u32 little-endian):
        //   0x00 signature      0x04 width      0x08 height     0x0C frames
        //   0x10 frame rate (i32)               0x14 flags
        //   0x18 audio buffer size x7           0x34 trees size
        //   0x38 MMAP size  0x3C MCLR size  0x40 FULL size  0x44 TYPE size
        //   0x48 audio rate+flags x7            0x64 reserved
        let width = u32_at(0x04);
        let height = u32_at(0x08);
        let frame_count = u32_at(0x0C);
        let us_per_frame = rate_to_us_per_frame(u32_at(0x10) as i32);
        let flags = u32_at(0x14);
        let trees_size = u32_at(0x34) as usize;
        let tree_sizes = [u32_at(0x38), u32_at(0x3C), u32_at(0x40), u32_at(0x44)];

        if width == 0 || height == 0 || width > 4096 || height > 4096 || width % 4 != 0 || height % 4 != 0 {
            return Err(Error::Corrupt("frame dimensions"));
        }

        let ring_frame = flags & 1 != 0;
        let y_scale = if flags & 4 != 0 {
            YScale::Doubled
        } else if flags & 2 != 0 {
            YScale::Interlaced
        } else {
            YScale::None
        };

        // Audio rate word: low 24 bits = sample rate, high byte = flags.
        //   0x80 packed (DPCM)  0x40 track present  0x20 16-bit  0x10 stereo
        //   0x0C Bink audio variants
        let mut audio = Vec::new();
        for index in 0..7 {
            let word = u32_at(0x48 + index * 4);
            if word & 0x4000_0000 == 0 {
                continue;
            }
            let compression = if word & 0x0C00_0000 != 0 {
                AudioCompression::Bink
            } else if word & 0x8000_0000 != 0 {
                AudioCompression::Dpcm
            } else {
                AudioCompression::Raw
            };
            audio.push(AudioTrack {
                index,
                sample_rate: word & 0x00FF_FFFF,
                channels: if word & 0x1000_0000 != 0 { 2 } else { 1 },
                bits: if word & 0x2000_0000 != 0 { 16 } else { 8 },
                compression,
                max_chunk_bytes: u32_at(0x18 + index * 4),
            });
        }

        // Per-frame tables. A ring frame adds one entry to each.
        let stored_frames = frame_count as usize + ring_frame as usize;
        let sizes_at = HEADER_SIZE;
        let types_at = sizes_at + stored_frames * 4;
        let trees_at = types_at + stored_frames;
        let frames_at = trees_at + trees_size;
        if frames_at > data.len() {
            return Err(Error::Truncated("frame tables / trees"));
        }
        let mut frames = Vec::with_capacity(stored_frames);
        let mut offset = frames_at;
        for i in 0..stored_frames {
            let raw = u32_at(sizes_at + i * 4);
            // The low two bits are flags (bit 0 = keyframe); sizes are
            // always multiples of 4.
            let size = (raw & !3) as usize;
            if offset + size > data.len() {
                return Err(Error::Truncated("frame data"));
            }
            frames.push(FrameEntry { offset, size, keyframe: raw & 1 != 0, contents: data[types_at + i] });
            offset += size;
        }

        // The four video trees, one after another in a single bitstream.
        let mut bits = BitReader::new(&data[trees_at..frames_at]);
        let trees = VideoTrees {
            mono_map: BigTree::read(&mut bits, tree_sizes[0])?,
            mono_colors: BigTree::read(&mut bits, tree_sizes[1])?,
            full: BigTree::read(&mut bits, tree_sizes[2])?,
            block_type: BigTree::read(&mut bits, tree_sizes[3])?,
        };

        let info = Info { version, width, height, frame_count, us_per_frame, ring_frame, y_scale, audio };
        Ok(Smacker {
            pixels: vec![0; (width * height) as usize],
            palette: [[0; 3]; 256],
            data,
            info,
            frames,
            trees,
            next_frame: 0,
        })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Frames left before [`Smacker::decode_next_frame`] returns
    /// [`Error::EndOfStream`]. The ring frame (if any) is not counted: it
    /// is only useful when looping, and [`Smacker::rewind`] restarts from
    /// the keyframe anyway.
    pub fn frames_remaining(&self) -> usize {
        self.info.frame_count as usize - self.next_frame.min(self.info.frame_count as usize)
    }

    /// Restart playback from frame 0 (clears picture and palette).
    pub fn rewind(&mut self) {
        self.next_frame = 0;
        self.pixels.fill(0);
        self.palette = [[0; 3]; 256];
    }

    /// Decodes the next frame: palette delta, audio for every track, video.
    pub fn decode_next_frame(&mut self) -> Result<Frame<'_>, Error> {
        if self.next_frame >= self.info.frame_count as usize {
            return Err(Error::EndOfStream);
        }
        let index = self.next_frame;
        self.next_frame += 1;
        let entry = self.frames[index];
        let chunk = &self.data[entry.offset..entry.offset + entry.size];
        let mut p = 0;

        // 1. Palette: a length byte counting 4-byte units (itself included).
        let palette_changed = entry.contents & 1 != 0;
        if palette_changed {
            let len = *chunk.first().ok_or(Error::Truncated("palette length"))? as usize * 4;
            if len == 0 || len > chunk.len() {
                return Err(Error::Truncated("palette chunk"));
            }
            apply_palette_chunk(&mut self.palette, &chunk[1..len])?;
            p = len;
        }

        // 2. Audio: for each flagged track, a u32 length (itself included)
        //    then the track's data.
        let mut audio: [Vec<i16>; 7] = Default::default();
        for (track, samples) in audio.iter_mut().enumerate() {
            if entry.contents & (2 << track) == 0 {
                continue;
            }
            let len_bytes = chunk.get(p..p + 4).ok_or(Error::Truncated("audio length"))?;
            let len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
            if len < 4 || p + len > chunk.len() {
                return Err(Error::Truncated("audio chunk"));
            }
            if let Some(desc) = self.info.audio.iter().find(|t| t.index == track) {
                decode_audio(desc, &chunk[p + 4..p + len], samples)?;
            }
            p += len;
        }

        // 3. Video: everything left is the block bitstream.
        let mut bits = BitReader::new(&chunk[p..]);
        decode_video(
            &mut bits,
            &mut self.trees,
            &mut self.pixels,
            self.info.width as usize,
            self.info.height as usize,
            self.info.version,
        )?;

        Ok(Frame {
            index,
            keyframe: entry.keyframe,
            palette_changed,
            width: self.info.width,
            height: self.info.height,
            y_scale: self.info.y_scale,
            pixels: &self.pixels,
            palette: &self.palette,
            audio,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palmap_matches_rad_table() {
        assert_eq!(&PALMAP[..4], &[0x00, 0x04, 0x08, 0x0C]);
        assert_eq!(PALMAP[15], 0x3C);
        assert_eq!(PALMAP[16], 0x41);
        assert_eq!(PALMAP[32], 0x82);
        assert_eq!(PALMAP[63], 0xFF);
    }

    #[test]
    fn frame_rate_semantics() {
        assert_eq!(rate_to_us_per_frame(100), 100_000);
        assert_eq!(rate_to_us_per_frame(-4166), 41_660);
        assert_eq!(rate_to_us_per_frame(0), 100_000);
    }

    #[test]
    fn bits_are_lsb_first() {
        let mut b = BitReader::new(&[0b1010_0001, 0xFF]);
        assert!(b.bit().unwrap());
        assert!(!b.bit().unwrap());
        assert_eq!(b.bits(6).unwrap(), 0b101000);
        assert_eq!(b.bits(8).unwrap(), 0xFF);
        assert!(b.bit().is_err());
    }

    #[test]
    fn palette_ops() {
        let mut pal = [[0u8; 3]; 256];
        pal[10] = [1, 2, 3];
        // new colour (63,0,32) at 0; skip 2; copy 1 entry from old index 10.
        apply_palette_chunk(&mut pal, &[63, 0, 32, 0x81, 0x40, 10]).unwrap();
        assert_eq!(pal[0], [255, 0, 0x82]);
        assert_eq!(pal[3], [1, 2, 3]);
    }

    /// Real-data tests: only run when the original CD is mounted.
    fn original(name: &str) -> Option<Vec<u8>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/");
        std::fs::read(format!("{path}{name}")).ok()
    }

    #[test]
    fn intro_decodes_every_frame() {
        let Some(data) = original("0") else { return };
        let mut smk = Smacker::parse(&data).unwrap();
        let info = smk.info().clone();
        assert_eq!((info.width, info.height, info.frame_count), (640, 240, 386));
        assert_eq!(info.y_scale, YScale::Interlaced);
        assert_eq!(info.us_per_frame, 41_660);
        let mut samples = 0;
        let mut distinct = std::collections::HashSet::new();
        for i in 0..info.frame_count {
            let f = smk.decode_next_frame().unwrap();
            samples += f.audio[0].len();
            if i == 200 {
                distinct.extend(f.pixels.iter().copied());
            }
        }
        // A real picture uses many palette entries; noise or black wouldn't.
        assert!(distinct.len() > 32, "only {} colours in frame 200", distinct.len());
        let secs = samples as f64 / info.audio[0].sample_rate as f64;
        assert!((secs - info.duration_us() as f64 / 1e6).abs() < 1.0, "audio {secs}s");
    }

    #[test]
    fn audio_tracks_decode() {
        for name in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"] {
            let Some(data) = original(name) else { return };
            let mut smk = Smacker::parse(&data).unwrap();
            let info = smk.info().clone();
            let mut samples = 0usize;
            let mut peak = 0i32;
            while smk.frames_remaining() > 0 {
                let f = smk.decode_next_frame().unwrap();
                samples += f.audio[0].len();
                peak = peak.max(f.audio[0].iter().map(|&s| (s as i32).abs()).max().unwrap_or(0));
            }
            let track = &info.audio[0];
            let secs = samples as f64 / (track.sample_rate as f64 * track.channels as f64);
            let expected = info.duration_us() as f64 / 1e6;
            assert!((secs - expected).abs() < 1.5, "file {name}: audio {secs}s vs video {expected}s");
            assert!(peak > 1000, "file {name}: suspiciously quiet");
        }
    }
}
