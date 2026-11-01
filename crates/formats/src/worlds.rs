//! The `worlds` archive (`install/data/worlds`, 126 MB): every level, the
//! creature, the UI pictures and the font of Galapagos in one file.
//!
//! The full, pedagogic spec lives in `docs/re/worlds.md`; this module is the
//! executable version of it. Everything here was confirmed against the
//! retail EN CD *and* against the loader code in `galapagos.exe` (the
//! function addresses quoted below are the ones in that spec).
//!
//! # The big picture
//!
//! ```text
//! +----------------------------+ 0x00
//! | u16 section_count  (8)     |   WorldsArchive_ReadHeader @ 0x41d370
//! | u16 default_section (0)    |
//! | u32 section_offset[8]      |   absolute file offsets
//! +----------------------------+ 0x24   "global block" header
//! | u16 font_count + 5 x u32   |
//! | u16 picture_count + 40 x u32
//! | u32 creature, 5 x u32 overlays, u32 creature sounds, u32 predator sounds
//! +----------------------------+ 0xfc
//! | fonts, pictures, creature, |   reached through the offsets above
//! | sound sets ...             |
//! +----------------------------+ section_offset[0]
//! | world section 0            |   environment, palette, shade table,
//! |   ...                      |   textures, colour ramps, sound set,
//! | world section 7            |   object list (meshes!), trailer
//! +----------------------------+ EOF
//! ```
//!
//! The format is a straight serialisation of the engine's C++ objects: each
//! class reads its own fields in declaration order with a buffered
//! `fread`. There is no compression and no magic number. A few conventions
//! hold everywhere:
//!
//! * integers are little-endian (it is an x86 build), but
//! * colours are Mac-style 48-bit `RGBColor`s (`u16` per channel with the
//!   8-bit value doubled, `0xf9f9`), sample rates are Mac `Fixed` 16.16 and
//!   object types are Mac-style FourCCs (`'Objt'`, `'Fldr'` ...) stored as
//!   little-endian `u32`s — strong hints the tools ran on a Macintosh.
//! * world objects carry a table of seven *absolute block-end offsets*, which
//!   lets a reader skip any sub-block it does not understand.

use crate::bytes::{Cursor, OutOfBounds};
use core::ops::Range;

/// Everything that can go wrong while walking the archive.
#[derive(Debug)]
pub enum Error {
    /// A read ran past the end of the data.
    OutOfBounds(OutOfBounds),
    /// A structural check failed (what, at which file offset).
    Malformed(&'static str, usize),
}

impl From<OutOfBounds> for Error {
    fn from(e: OutOfBounds) -> Self {
        Error::OutOfBounds(e)
    }
}

/// An 8-bit RGB triple, already reduced from the 48-bit Mac colour.
pub type Rgb = [u8; 3];

/// Number of slots the header reserves for fonts (the engine asserts `< 5`).
pub const FONT_SLOTS: usize = 5;
/// Number of slots the header reserves for pictures.
pub const PICTURE_SLOTS: usize = 40;
/// Textures are always 128 x 128 texels, one byte (palette index) each.
pub const TEXTURE_SIZE: usize = 128;
/// The software renderer's colour look-up table: 32 x 32 rows of 256
/// palette indices (`1 << 5` and `1 << 5` are hard-coded in the renderer
/// constructor at 0x403300).
pub const SHADE_TABLE_LEN: usize = 32 * 32 * 256;
/// Bytes of the fixed-size environment record at the start of a section.
pub const ENVIRONMENT_LEN: usize = 28;

fn i16(c: &mut Cursor) -> Result<i16, OutOfBounds> {
    Ok(c.u16()? as i16)
}

/// Reads a 256-entry palette of 48-bit colours. The engine keeps only the
/// high byte of each channel (`LoadPicture` @ 0x41aa10 reads `[esp+0x13]`).
pub fn read_palette48(c: &mut Cursor) -> Result<[Rgb; 256], OutOfBounds> {
    let mut pal = [[0u8; 3]; 256];
    for entry in pal.iter_mut() {
        for ch in entry.iter_mut() {
            *ch = (c.u16()? >> 8) as u8;
        }
    }
    Ok(pal)
}

/// The archive header (file offsets 0x00 .. 0xfc).
#[derive(Debug, Clone)]
pub struct Header {
    /// Section loaded when the game asks for section `-1`.
    pub default_section: u16,
    pub section_offsets: Vec<u32>,
    /// Offsets of the bitmap fonts actually in use.
    pub font_offsets: Vec<u32>,
    /// Offsets of the full-colour UI pictures actually in use.
    pub picture_offsets: Vec<u32>,
    /// The creature (Mendel) master record, read by `LoadCreature` @ 0x41cde0.
    pub creature: u32,
    /// Per-world creature overlays; overlay `i` is applied for world `i + 1`.
    pub creature_overlays: [u32; 5],
    /// Sound set of the creature (11 sounds).
    pub creature_sounds: u32,
    /// Sound set of the predator (2 sounds).
    pub predator_sounds: u32,
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::new(data);
        let count = c.u16()? as usize;
        let default_section = c.u16()?;
        let mut section_offsets = Vec::with_capacity(count);
        for _ in 0..count {
            section_offsets.push(c.u32()?);
        }
        let font_count = c.u16()? as usize;
        let mut fonts = [0u32; FONT_SLOTS];
        for f in fonts.iter_mut() {
            *f = c.u32()?;
        }
        let picture_count = c.u16()? as usize;
        let mut pictures = [0u32; PICTURE_SLOTS];
        for p in pictures.iter_mut() {
            *p = c.u32()?;
        }
        let creature = c.u32()?;
        let mut creature_overlays = [0u32; 5];
        for o in creature_overlays.iter_mut() {
            *o = c.u32()?;
        }
        Ok(Self {
            default_section,
            section_offsets,
            font_offsets: fonts[..font_count.min(FONT_SLOTS)].to_vec(),
            picture_offsets: pictures[..picture_count.min(PICTURE_SLOTS)].to_vec(),
            creature,
            creature_overlays,
            creature_sounds: c.u32()?,
            predator_sounds: c.u32()?,
        })
    }
}

pub struct Archive<'a> {
    pub data: &'a [u8],
    pub sections: Vec<Range<usize>>,
    /// The bytes between the section table and the first section.
    pub global: Range<usize>,
    pub header: Header,
}

impl<'a> Archive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, OutOfBounds> {
        let header = Header::parse(data)?;
        let header_end = 4 + 4 * header.section_offsets.len();
        let mut offsets: Vec<usize> = header.section_offsets.iter().map(|&o| o as usize).collect();
        offsets.push(data.len());
        if offsets.windows(2).any(|w| w[0] > w[1]) || offsets[0] < header_end {
            return Err(OutOfBounds { at: header_end, want: offsets[0] });
        }
        let sections = offsets.windows(2).map(|w| w[0]..w[1]).collect();
        Ok(Self { data, sections, global: header_end..offsets[0], header })
    }

    pub fn section(&self, i: usize) -> &'a [u8] {
        &self.data[self.sections[i].clone()]
    }

    /// Fully parses world section `i`.
    pub fn world(&self, i: usize) -> Result<Section<'a>, Error> {
        Section::parse(self.data, self.sections[i].clone())
    }

    /// UI picture `i` of the global block.
    pub fn picture(&self, i: usize) -> Result<Picture<'a>, Error> {
        Picture::parse(self.data, self.header.picture_offsets[i] as usize)
    }

    /// Bitmap font `i` of the global block.
    pub fn font(&self, i: usize) -> Result<Font<'a>, OutOfBounds> {
        Font::parse(self.data, self.header.font_offsets[i] as usize)
    }
}

/// A full-colour (8-bit indexed) UI picture, read by `LoadPicture` @ 0x41aa10.
///
/// ```text
/// RGB48[256] palette | u32 byte_count | u8[byte_count] pixels | u16 width
/// ```
///
/// Curiously the width comes *after* the pixels: the engine already knows
/// the size of the surface it is filling and only uses it as a check.
pub struct Picture<'a> {
    pub palette: [Rgb; 256],
    pub width: u16,
    pub height: u16,
    pub pixels: &'a [u8],
}

impl<'a> Picture<'a> {
    pub fn parse(data: &'a [u8], offset: usize) -> Result<Self, Error> {
        let mut c = Cursor::at(data, offset);
        let palette = read_palette48(&mut c)?;
        let n = c.u32()? as usize;
        let pixels = c.take(n)?;
        let width = c.u16()?;
        if width == 0 || n % width as usize != 0 {
            return Err(Error::Malformed("picture size is not a multiple of its width", offset));
        }
        Ok(Self { palette, width, height: (n / width as usize) as u16, pixels })
    }
}

/// A bitmap font: one strip of 95 glyphs (ASCII 32..126), read by
/// `LoadFonts` @ 0x41cce0 and registered by `Renderer_AddFont` @ 0x405750.
/// Pixel value `0xff` is transparent.
pub struct Font<'a> {
    pub pixels: &'a [u8],
    /// Bytes per row of the strip (= 95 * `cell_width`).
    pub stride: u16,
    pub cell_width: u16,
    pub height: u16,
}

impl<'a> Font<'a> {
    pub fn parse(data: &'a [u8], offset: usize) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::at(data, offset);
        let n = c.u32()? as usize;
        let pixels = c.take(n)?;
        Ok(Self { pixels, stride: c.u16()?, cell_width: c.u16()?, height: c.u16()? })
    }
}

/// The 28-byte record that opens every section (`Renderer_ReadEnvironment`
/// @ 0x42e880). Fractions are stored as `u16 / 65535`.
#[derive(Debug, Clone, Copy)]
pub struct Environment {
    /// Far clipping distance (999999, 75000, 110000, 60000 ...).
    pub far_clip: u32,
    /// Background / fog colour, 48-bit Mac style.
    pub background: [u16; 3],
    pub fog_enabled: u16,
    /// Stored `/ 65535` then truncated to an integer by the engine (unknown).
    pub unknown_level: u16,
    /// Direction of the single directional light, world units.
    pub light_direction: [i32; 3],
    /// Light intensity, `u16 / 65535`.
    pub light_intensity: u16,
}

impl Environment {
    fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        Ok(Self {
            far_clip: c.u32()?,
            background: [c.u16()?, c.u16()?, c.u16()?],
            fog_enabled: c.u16()?,
            unknown_level: c.u16()?,
            light_direction: [c.i32()?, c.i32()?, c.i32()?],
            light_intensity: c.u16()?,
        })
    }
}

/// One sound sample (`SoundServices_CreateSoundSample` @ 0x43a2b0):
/// mono PCM, 8-bit unsigned or 16-bit signed little-endian.
#[derive(Debug, Clone, Copy)]
pub struct Sample<'a> {
    pub bits: u16,
    /// Mac `Fixed` 16.16, e.g. `0x2b110000` = 11025 Hz.
    pub rate_fixed: u32,
    /// MIDI note the sample was recorded at (60 = middle C), used to pitch it.
    pub base_note: u8,
    pub pcm: &'a [u8],
}

impl Sample<'_> {
    pub fn sample_rate(&self) -> u32 {
        self.rate_fixed >> 16
    }
}

/// A sound set (`SoundServices_ReadSoundSet` @ 0x42c6a0). The same sounds
/// are stored four times, once per quality setting (1 = 8-bit 11 kHz ...
/// 4 = 16-bit 44 kHz), and the header points at each copy.
///
/// ```text
/// u16 count | u32 end | i32 quality_offset[4]          (22 bytes)
/// at quality_offset[q]:  count x { u16 present;
///     if present: u16 bits, u32 rate (16.16), u8 base_note, u32 len, u8[len] }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SoundSet {
    pub count: u16,
    /// Absolute offset just past the whole set (the engine seeks there).
    pub end: u32,
    /// Absolute offsets of the four quality copies (`-1` = absent).
    pub quality_offsets: [i32; 4],
}

impl SoundSet {
    pub fn parse(data: &[u8], offset: usize) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::at(data, offset);
        Ok(Self {
            count: c.u16()?,
            end: c.u32()?,
            quality_offsets: [c.i32()?, c.i32()?, c.i32()?, c.i32()?],
        })
    }

    /// Decodes the samples of quality `q` (1..=4, falling back to 1 when that
    /// copy is missing, like the engine does). `None` marks an empty slot.
    pub fn samples<'a>(&self, data: &'a [u8], q: usize) -> Result<Vec<Option<Sample<'a>>>, OutOfBounds> {
        let q = q.clamp(1, 4);
        let mut at = self.quality_offsets[q - 1];
        if at < 0 {
            at = self.quality_offsets[0];
        }
        let mut c = Cursor::at(data, at as usize);
        let mut out = Vec::with_capacity(self.count as usize);
        for _ in 0..self.count {
            if c.u16()? == 0 {
                out.push(None);
                continue;
            }
            let bits = c.u16()?;
            let rate_fixed = c.u32()?;
            let base_note = c.u8()?;
            let len = c.u32()? as usize;
            out.push(Some(Sample { bits, rate_fixed, base_note, pcm: c.take(len)? }));
        }
        Ok(out)
    }
}

/// A polygon mesh (`WorldObject_ReadMesh` @ 0x423150). Vertices are in the
/// object's local space; add [`WorldObject::position`] for world space.
///
/// ```text
/// u32 polygon_count, polygon_count x { u32 n, u16 index[n] }
/// u32 vertex_count,  vertex_count  x { i16 x, i16 y, i16 z }
/// ```
///
/// There are no UVs on disk: the engine maps texel (0,127), (0,0), (127,0)
/// of the 128x128 texture onto the first three vertices of each polygon.
#[derive(Debug, Clone)]
pub struct Mesh {
    pub polygons: Vec<Vec<u16>>,
    pub vertices: Vec<[i16; 3]>,
}

impl Mesh {
    pub fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let pc = c.u32()? as usize;
        let mut polygons = Vec::with_capacity(pc.min(1 << 16));
        for _ in 0..pc {
            let n = c.u32()? as usize;
            let mut p = Vec::with_capacity(n.min(256));
            for _ in 0..n {
                p.push(c.u16()?);
            }
            polygons.push(p);
        }
        let vc = c.u32()? as usize;
        let mut vertices = Vec::with_capacity(vc.min(1 << 16));
        for _ in 0..vc {
            vertices.push([i16(c)?, i16(c)?, i16(c)?]);
        }
        Ok(Self { polygons, vertices })
    }
}

/// One frame of a polygon's look (`PolyAppearance_Read` @ 0x42e180,
/// applied by `WorldObject_ApplyAppearance` @ 0x423730).
///
/// On disk: `i16 texture, i16 shade, u8 enabled, u32 flags, i16 u, i16 v`
/// (13 bytes).
#[derive(Debug, Clone, Copy)]
pub struct PolyFrame {
    /// Texture slot (0..40) or a negative special value (-2..-5; -5 and -4
    /// switch the object's visibility / a render mode — semantics inferred).
    pub texture: i16,
    /// Colour ramp / shade index (`-1` = keep).
    pub shade: i16,
    pub enabled: u8,
    /// Bits 2..3 select texture scrolling; other bits unknown.
    pub flags: u32,
    /// Texture origin offset in texels.
    pub u: i16,
    pub v: i16,
}

/// One appearance state: a list of frames for every polygon of the mesh.
#[derive(Debug, Clone)]
pub struct AppearanceState {
    pub looping: u16,
    pub polygons: Vec<Vec<PolyFrame>>,
}

fn read_appearance(c: &mut Cursor) -> Result<Vec<AppearanceState>, OutOfBounds> {
    let n = c.u16()? as usize;
    let mut states = Vec::with_capacity(n);
    for _ in 0..n {
        let looping = c.u16()?;
        let pc = c.u16()? as usize;
        let mut polygons = Vec::with_capacity(pc);
        for _ in 0..pc {
            let fc = c.u16()? as usize;
            let mut frames = Vec::with_capacity(fc);
            for _ in 0..fc {
                frames.push(PolyFrame {
                    texture: i16(c)?,
                    shade: i16(c)?,
                    enabled: c.u8()?,
                    flags: c.u32()?,
                    u: i16(c)?,
                    v: i16(c)?,
                });
            }
            polygons.push(frames);
        }
        states.push(AppearanceState { looping, polygons });
    }
    Ok(states)
}

/// Everything in the object list that is not the camera
/// (`WorldObject_Read` @ 0x423b50).
///
/// ```text
/// u32 fourcc
/// u32 block_end[7]        absolute offsets, see below
/// i16 folder              parent object id (1-based list index), -1 = none
/// u32 flags               low u16 = initial state (StateMachine_SetInitialState @ 0x42ce50)
/// i32 position[3]         world position
/// u16 has_mesh   + Mesh                      -> ends at block_end[1]
/// u16 has_states + state machine (0x42d580)  -> ends at block_end[2]
/// u16 flag + vertex-morph ShapeAnim (+0xbc)  -> ends at block_end[3]
/// u16 has_appearance + appearance            -> ends at block_end[4]
/// u16 flag + SoundEmitters (0x42b9e0)        -> ends at block_end[5]
/// u16 flag + MotionPaths (0x428cf0)          -> ends at block_end[6]
/// u16 flag + ObjectParams (0x40a0d0)         -> ends at block_end[0]
/// ```
///
/// The state machine, shape animation, sounds, motion paths and params are
/// decoded by [`crate::logic`] into [`WorldObject::logic`]; no class appends
/// bytes after the params.
#[derive(Debug, Clone)]
pub struct WorldObject {
    /// FourCC in reading order, e.g. `*b"Objt"`.
    pub kind: [u8; 4],
    /// File range of the whole record.
    pub range: Range<usize>,
    pub block_end: [u32; 7],
    /// Parent object: 1-based index into the section's object list (the
    /// object registry id, `ObjectRegistry_GetObject` @ 0x4282b0), usually a
    /// `'Fldr'`; `-1` = top level. Events posted to an object bubble up to
    /// its parents, and a disabled folder skips the update of its subtree.
    pub folder: i16,
    /// Low `u16` = initial state of the state machine; the high half is 0
    /// on the CD. See [`WorldObject::initial_state`].
    pub flags: u32,
    pub position: [i32; 3],
    pub mesh: Option<Mesh>,
    /// Appearance states; state 0 is the initial look.
    pub appearance: Option<Vec<AppearanceState>>,
    /// Behaviour: state machine, morphs, sounds, motion paths, class params.
    pub logic: crate::logic::ObjectLogic,
}

impl WorldObject {
    /// Texture of polygon `p` in the initial appearance state, if any.
    pub fn polygon_texture(&self, p: usize) -> Option<i16> {
        let first = self.appearance.as_ref()?.first()?;
        first.polygons.get(p)?.first().map(|f| f.texture)
    }

    /// State the object starts in (queued on load, so its entry actions run
    /// on the first tick).
    pub fn initial_state(&self) -> u16 {
        self.flags as u16
    }
}

/// One camera key: 20 bytes. Keys are **per polygon**: path `i` of the
/// camera record belongs to object `i` of the section and holds one key per
/// mesh polygon (verified: 689 non-empty paths, each with exactly
/// `polygon_count` keys). When Mendel stands on polygon `p` of object `i`,
/// `Camera_Update` @ 0x412d50 switches to `paths[i][p]` if its `a` is set.
#[derive(Debug, Clone, Copy)]
pub struct CameraKey {
    /// 1 = this polygon defines a camera; 0 = keep the current one.
    pub a: u8,
    /// Camera style: index into [`Section::camera_styles`] (0..32).
    pub b: u8,
    /// Camera position: absolute world position, or an offset from the
    /// reference chosen by the style's `mode`.
    pub position: [i32; 3],
    /// Camera orientation, 1024ths of a turn (inferred).
    pub c: [u16; 3],
}

impl CameraKey {
    fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        Ok(Self {
            a: c.u8()?,
            b: c.u8()?,
            position: [c.i32()?, c.i32()?, c.i32()?],
            c: [c.u16()?, c.u16()?, c.u16()?],
        })
    }
}

/// The `'Cmra'` record (`Camera_ReadPaths` @ 0x413e80) does not use the
/// block-end table: `u32 n, n x { u16 k, k x CameraKey }, CameraKey`.
/// `n` equals the number of objects in the world (one path per object).
#[derive(Debug, Clone)]
pub struct Camera {
    pub range: Range<usize>,
    pub paths: Vec<Vec<CameraKey>>,
    pub default: CameraKey,
}

#[derive(Debug, Clone)]
pub enum Object {
    Camera(Camera),
    World(WorldObject),
}

impl Object {
    pub fn kind(&self) -> [u8; 4] {
        match self {
            Object::Camera(_) => *b"Cmra",
            Object::World(o) => o.kind,
        }
    }

    fn read(data: &[u8], c: &mut Cursor) -> Result<Self, Error> {
        let start = c.pos;
        let kind = c.u32()?.to_be_bytes();
        if kind == *b"Cmra" {
            let n = c.u32()? as usize;
            let mut paths = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                let k = c.u16()? as usize;
                let mut keys = Vec::with_capacity(k);
                for _ in 0..k {
                    keys.push(CameraKey::read(c)?);
                }
                paths.push(keys);
            }
            let default = CameraKey::read(c)?;
            return Ok(Object::Camera(Camera { range: start..c.pos, paths, default }));
        }
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err(Error::Malformed("object type is not a FourCC", start));
        }
        let mut block_end = [0u32; 7];
        for b in block_end.iter_mut() {
            *b = c.u32()?;
        }
        let end = block_end[0] as usize;
        if end <= start || end > data.len() {
            return Err(Error::Malformed("object end offset out of range", start));
        }
        let folder = i16(c)?;
        let flags = c.u32()?;
        let position = [c.i32()?, c.i32()?, c.i32()?];
        let mesh = if c.u16()? != 0 { Some(Mesh::read(c)?) } else { None };
        if c.pos != block_end[1] as usize {
            return Err(Error::Malformed("mesh does not end at block_end[1]", start));
        }
        // The state machine and the +0xbc block are skipped via the table.
        let mut a = Cursor::at(data, block_end[3] as usize);
        let appearance = if a.u16()? != 0 { Some(read_appearance(&mut a)?) } else { None };
        if a.pos != block_end[4] as usize {
            return Err(Error::Malformed("appearance does not end at block_end[4]", start));
        }
        c.pos = end;
        Ok(Object::World(WorldObject {
            kind,
            range: start..end,
            block_end,
            folder,
            flags,
            position,
            mesh,
            appearance,
            logic: crate::logic::ObjectLogic::read(data, &block_end)?,
        }))
    }
}

/// A world section, read by `Game_ReadWorldSection` @ 0x41cc20.
pub struct Section<'a> {
    pub range: Range<usize>,
    pub environment: Environment,
    pub palette: [Rgb; 256],
    /// 32 x 32 x 256 palette-index look-up table of the software renderer
    /// (shading / fog / translucency; exact axes unknown).
    pub shade_table: &'a [u8],
    /// 128 x 128 palette-indexed textures, slot order = texture index.
    pub textures: Vec<&'a [u8]>,
    /// Six `u16 / 65535` fractions each (two colours?), use unknown.
    pub color_ramps: Vec<[u16; 6]>,
    pub sounds: SoundSet,
    pub objects: Vec<Object>,
    /// 468 bytes = the camera style table (`Camera_ReadStyles` @ 0x414060).
    pub trailer: Range<usize>,
    /// The 33 camera styles referenced by [`CameraKey::b`].
    pub camera_styles: Vec<crate::logic::CameraStyle>,
}

impl<'a> Section<'a> {
    pub fn parse(data: &'a [u8], range: Range<usize>) -> Result<Self, Error> {
        let mut c = Cursor::at(data, range.start);
        let environment = Environment::read(&mut c)?;
        let palette = read_palette48(&mut c)?;
        let shade_table = c.take(SHADE_TABLE_LEN)?;
        let tex_count = c.u16()? as usize;
        let mut textures = Vec::with_capacity(tex_count);
        for _ in 0..tex_count {
            textures.push(c.take(TEXTURE_SIZE * TEXTURE_SIZE)?);
        }
        let ramp_count = c.u16()? as usize;
        let mut color_ramps = Vec::with_capacity(ramp_count);
        for _ in 0..ramp_count {
            let mut r = [0u16; 6];
            for v in r.iter_mut() {
                *v = c.u16()?;
            }
            color_ramps.push(r);
        }
        let sounds = SoundSet::parse(data, c.pos)?;
        c.pos = sounds.end as usize;
        let n = c.u32()? as usize;
        let mut objects = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            objects.push(Object::read(data, &mut c)?);
        }
        if c.pos > range.end {
            return Err(Error::Malformed("objects overrun the section", c.pos));
        }
        let trailer = c.pos..range.end;
        let camera_styles = crate::logic::CameraStyle::read_table(data, trailer.start)?;
        Ok(Self { range, environment, palette, shade_table, textures, color_ramps, sounds, objects, trailer, camera_styles })
    }

    /// Converts texture `i` to RGB using the section palette.
    pub fn texture_rgb(&self, i: usize) -> Vec<u8> {
        self.textures[i].iter().flat_map(|&p| self.palette[p as usize]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Option<Vec<u8>> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        std::fs::read(p).ok()
    }

    #[test]
    fn mesh_reader_on_synthetic_bytes() {
        // one triangle (0,1,2) and three vertices
        let mut b = vec![1, 0, 0, 0, 3, 0, 0, 0, 0, 0, 1, 0, 2, 0, 3, 0, 0, 0];
        for v in [1i16, 2, 3, -4, 5, 6, 7, 8, -9] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let m = Mesh::read(&mut Cursor::new(&b)).unwrap();
        assert_eq!(m.polygons, vec![vec![0, 1, 2]]);
        assert_eq!(m.vertices[1], [-4, 5, 6]);
    }

    #[test]
    fn header_matches_retail_cd() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let h = &a.header;
        assert_eq!(h.section_offsets.len(), 8);
        assert_eq!(h.section_offsets[0], 0x4601b5);
        assert_eq!(h.default_section, 0);
        assert_eq!(h.font_offsets, vec![0xfc]);
        assert_eq!(h.picture_offsets.len(), 39);
        assert_eq!(h.creature, 0x3601c6);
        assert_eq!(h.predator_sounds, 0x4409b5);
        // the predator sound set ends exactly where the first world begins
        let ps = SoundSet::parse(&d, h.predator_sounds as usize).unwrap();
        assert_eq!(ps.end, h.section_offsets[0]);
        let cs = SoundSet::parse(&d, h.creature_sounds as usize).unwrap();
        assert_eq!((cs.count, cs.end), (11, h.predator_sounds));
    }

    #[test]
    fn pictures_and_font() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let p = a.picture(1).unwrap();
        assert_eq!((p.width, p.height), (560, 421));
        for i in 0..a.header.picture_offsets.len() {
            let p = a.picture(i).unwrap();
            assert_eq!(p.pixels.len(), p.width as usize * p.height as usize);
        }
        let f = a.font(0).unwrap();
        assert_eq!((f.stride, f.cell_width, f.height), (570, 6, 11));
        assert_eq!(f.stride as usize, 95 * f.cell_width as usize);
    }

    #[test]
    fn every_world_section_parses() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let textures = [38, 24, 33, 24, 28, 35, 22, 36];
        let objects = [171, 284, 214, 523, 121, 486, 177, 121];
        for i in 0..a.sections.len() {
            let s = a.world(i).unwrap();
            assert_eq!(s.textures.len(), textures[i], "section {i}");
            assert_eq!(s.objects.len(), objects[i], "section {i}");
            assert_eq!(s.color_ramps.len(), 62);
            assert_eq!(s.trailer.len(), 468);
            assert_eq!(s.objects.iter().filter(|o| o.kind() == *b"Cmra").count(), 1);
            for o in &s.objects {
                let Object::World(o) = o else { continue };
                let (Some(m), Some(app)) = (&o.mesh, &o.appearance) else { continue };
                assert_eq!(app[0].polygons.len(), m.polygons.len());
                for p in &m.polygons {
                    assert!(p.iter().all(|&v| (v as usize) < m.vertices.len()));
                }
                for t in 0..m.polygons.len() {
                    assert!(o.polygon_texture(t).unwrap() < textures[i] as i16);
                }
            }
        }
    }

    #[test]
    fn sound_qualities() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let s = a.world(0).unwrap();
        let expect = [(8, 11025), (16, 11025), (16, 22050), (16, 44100)];
        for (q, &(bits, rate)) in (1..=4).zip(expect.iter()) {
            let samples = s.sounds.samples(&d, q).unwrap();
            assert_eq!(samples.len(), 93);
            let first = samples.iter().flatten().next().unwrap();
            assert_eq!((first.bits, first.sample_rate(), first.base_note), (bits, rate, 60));
        }
    }
}
