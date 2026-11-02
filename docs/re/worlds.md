# `worlds`: the one file that holds Galapagos

> Source: `crates/formats/src/worlds.rs` (parser + tests). Reverse engineered
> from `galapagos.exe` (Ghidra via REA, plus a raw `objdump` listing for code
> Ghidra had not discovered) and checked byte-for-byte against the retail CD.
> No game data lives in this repository.

## Why one 126 MB file?

`install/data/worlds` is 126,483,197 bytes, and apart from the Smacker
movies/music (`0`..`10`, see `smacker.md`) it *is* the game: eight playable
sections, every texture, every sound effect, the 3D meshes, the scripted
behaviours of clickable objects, the creature Mendel, the UI pictures and the
font. In 1997 a CD-ROM seeks slowly, so you keep the file open and jump around
inside it. The engine opens it **exactly once** (`UFileStream_Open` @
`0x414760`, a 4 KB-buffered wrapper over CRT `fopen`/`fseek`/`fread`) and
every loader below just *seeks and reads* through that stream.

The file has no magic number, no compression and no checksums. It is a raw
dump of C++ objects, each class reading its own members in order. Three things
betray that the authoring tools ran on a **Macintosh** while the game itself is
a Win95/x86 build:

* all integers are little-endian (converted at export), **but**
* colours are QuickDraw `RGBColor`s — three `u16`s where the 8-bit value is
  doubled (`0xf9f9`) — and the engine only keeps the high byte;
* sample rates are Mac `Fixed` 16.16 numbers (`0x2b110000` = 11025.0 Hz) with
  a "base note" (60 = middle C), straight out of the Sound Manager;
* object types are FourCCs (`'Objt'`, `'Fldr'`, `'Cmra'`...), stored as a
  little-endian `u32`, so they read backwards in a hex dump (`cnEM` = `'MEnc'`).

Confidence labels used below: **observed** = read in the disassembly *and*
verified on the data; **inferred** = consistent with code and data but the
meaning is a guess; **unknown** = bytes we skip.

## How the loader was found

The string `"Worlds"` (`0x46826c`) has three references. One is a
file-presence check (`CountInstalledDataFiles` @ `0x4366b0`); the other two
sit inside a function Ghidra never created, because it is only ever reached
through a vtable. Disassembling `0x41d370` by hand revealed the archive
reader. From there: the archive object keeps its stream at `+0x5c`
(`WorldsArchive_GetStream` @ `0x4225b0`), so every caller of that getter is a
loader. Seven call sites, seven stories.

## Map of the file

```text
0x00000000  header + global table            WorldsArchive_ReadHeader   0x41d370
0x000000fc  font #0                          LoadFonts                  0x41cce0
0x00001984  39 UI pictures                   LoadPicture_Pal48_8bpp     0x41aa10
0x003601c6  creature (Mendel) master record  LoadCreature               0x41cde0
0x0037505e  5 creature per-world overlays    LoadCreatureWorldOverlay   0x41ced0
0x003977f8  creature sound set (11 sounds)   SoundServices_ReadSoundSet 0x42c6a0
0x004409b5  predator sound set (2 sounds)    (Pred class)               0x42a5fb
0x004601b5  world section 0                  Game_ReadWorldSection      0x41cc20
   ...      world sections 1..7
```

Every global block ends exactly where the next one begins, which is how we know
the map has no holes.

## 1. Header — observed

Read by `WorldsArchive_ReadHeader` @ `0x41d370`, field by field, into the
archive object (offsets in that object in the last column).

| Offset | Type        | Value (EN CD)        | Meaning                                   | obj  |
|--------|-------------|----------------------|-------------------------------------------|------|
| 0x00   | u16         | 8                    | section count                             | +4   |
| 0x02   | u16         | 0                    | default section (used when asked for -1)  | +6   |
| 0x04   | u32[count]  | 0x4601b5, ...        | absolute section offsets                  | +8   |
| 0x24   | u16         | 1                    | font count                                | +0x60 |
| 0x26   | u32[5]      | 0xfc, 0, ...         | font offsets (engine asserts < 5 fonts)   | +0x64 |
| 0x3a   | u16         | 39                   | picture count                             | +0x78 |
| 0x3c   | u32[40]     | 0x1984, ...          | picture offsets (slot 39 unused/garbage)  | +0x7c |
| 0xdc   | u32         | 0x3601c6             | creature master record                    | +0x11c |
| 0xe0   | u32[5]      | 0x37505e, ...        | creature overlays, `[i]` used for world i+1 | +0x120 |
| 0xf4   | u32         | 0x3977f8             | creature sound set                        | +0x134 |
| 0xf8   | u32         | 0x4409b5             | predator sound set                        | +0x138 |

The first guess, "u32 count = 8", was half right: it is two `u16`s, and the
second one is the default section. The "ascending offset table" seen in the
global block is the picture table. Getters: `0x41d4b0` .. `0x41d520`.

Save games (`.gal`) reuse the archive object: `0x41d590`/`0x41d5c0` write and
read the current section (`u16`) plus a 40-byte array (`+0x13c`).

## 2. Pictures — observed

`LoadPicture_Pal48_8bpp` @ `0x41aa10` (seek to `picture_offsets[i]`):

| Size        | Field                                                     |
|-------------|-----------------------------------------------------------|
| 256 x 6     | palette: `u16 r, g, b` per entry, keep the high byte      |
| 4           | `u32 n` = width x height                                  |
| n           | 8-bit palette indices, row-major                          |
| 2           | `u16 width` — *after* the pixels, used only as a check    |

On a 16-bit display each index goes through the palette into the surface's
pixel format (`0x40aa20`); in 8-bit mode the bytes are copied directly. Sizes on
the CD: 560x421 (full-screen panels such as the "LEVEL / TOTAL" score screen),
288x211 and 144x108 (save-slot thumbnails at two sizes), 504x30, 256x15,
248x15, 128x8 (bars and strips).

## 3. Font — observed

`LoadFonts` @ `0x41cce0` → `Renderer_AddFont` @ `0x405750`:

| Size | Field |
|------|-------|
| 4    | `u32 n` pixel byte count |
| n    | pixels; `0xff` = transparent, `0x00` = ink |
| 2    | `u16 stride` (570) |
| 2    | `u16 cell_width` (6) |
| 2    | `u16 height` (11) |

The strip holds 95 glyphs, ASCII 32..126, glyph *i* at x = *i* x cell_width.

## 4. Sound sets — observed

Used three times (creature, predator, and once per world) by
`SoundServices_ReadSoundSet` @ `0x42c6a0`. The neat trick: the *same* sounds
are stored four times, one copy per audio-quality preference, and the header
lets the engine seek straight to the right one.

| Size | Field |
|------|-------|
| 2    | `u16 count` (an empty set triggers `"No sounds in set"`) |
| 4    | `u32 end` — absolute offset after the whole set; the engine seeks there at the end |
| 16   | `i32 quality_offset[4]` — absolute; `-1` falls back to quality 1 |

At `quality_offset[q]`, `count` entries:

| Size | Field |
|------|-------|
| 2    | `u16 present` (0 = empty slot, nothing follows) |
| 2    | `u16 bits` — 8 (unsigned) or 16 (signed, little-endian, verified by waveform smoothness) |
| 4    | `u32 rate`, Mac `Fixed` 16.16 |
| 1    | `u8 base_note` (MIDI; 60 for most, 36..74 for pitched instruments) |
| 4    | `u32 len` |
| len  | mono PCM |

Quality 1..4 = 8-bit/11 kHz, 16-bit/11 kHz, 16-bit/22 kHz, 16-bit/44 kHz.
Handed to `SoundServices_CreateSoundSample` @ `0x43a2b0`.

## 5. A world section

`Game_LoadWorldSection` @ `0x422180` seeks to `section_offset[i]`
(`WorldsArchive_SeekSection` @ `0x4225c0`) and calls the game's virtual
`ReadSection` = `Game_ReadWorldSection` @ `0x41cc20`, which chains:

1. `WorldBase_ReadSection` @ `0x4222d0` (via `World_ReadSection` @ `0x408d90`):
   renderer data, sounds, objects;
2. `Creature_ReadWorldSection` @ `0x4177d0` and camera tail `0x414060`.

### 5.1 Environment — observed layout, inferred meaning

`Renderer_ReadEnvironment` @ `0x42e880`, 28 bytes:

| Off | Type    | Example            | Meaning |
|-----|---------|--------------------|---------|
| 0   | u32     | 999999 / 60000     | far clip distance (inferred) |
| 4   | u16[3]  | 0,0,0 / ffff x3    | background/fog colour (48-bit) |
| 10  | u16     | 0 / 1              | fog on/off (inferred) |
| 12  | u16     | 50                 | `/65535` then truncated — unknown |
| 14  | i32[3]  | -128,-128,-128     | directional light vector (→ `0x401470`) |
| 26  | u16     | 65535              | light intensity `/65535` (→ `0x401510`) |

### 5.2 Palette, shade table, textures — observed

`Renderer_ReadPaletteShadeTextures` @ `0x42eba0`:

| Size           | Field |
|----------------|-------|
| 256 x 6        | section palette (48-bit colours) |
| 262,144        | shade table: `32 x 32 x 256` palette indices |
| 2              | `u16 texture_count` (22..38) |
| count x 16,384 | textures, 128 x 128, 8-bit indices into the section palette |

The 262,144-byte block was the trap: its size is computed from two
constructor constants (`pow(2,5)` twice, @ `0x403300`) times 256. Exported as
an image it is a stack of colour gradients, not a texture: it is the software
renderer's **colour look-up table** — for each of 32 x 32 (light, fog?) levels,
where every palette colour ends up. That is how a 1997 8-bit renderer does
lighting without arithmetic. The 16 KB blocks that follow are the real
textures (marble, lava, water, alien skins...); each goes to
`Renderer_AddTexture128` @ `0x405580`, which allows at most 40 — matching
`"Not a vaild texture index"` (`0x40f4ca`, asserts `index < 0x28`).

### 5.3 Colour ramps are materials — decoded

`Renderer_ReadColorRamps` @ `0x42ece0`: `u16 count` (62 in every section),
then `count x u16[6]`, each divided by 65535 and passed in order to
`0x405620`, which appends them to the renderer's material table (`+0x1864`,
24 bytes each). A frame's shade number (`+6`, stored at polygon `+0x60` by
`0x402450`) selects the polygon's material. `0x401b70` turns it into a light
level (polygon `+0x66`, a row of the 32-row lighting table, `pow(2, 5)` at
`0x40346f`):

```text
L     = m0 + m2·ambient − Σ lights m3·intensity·(n·l)   (only n·l < 0 counts)
t     = min(L / 2, 0.5) + m1, at most 1
level = trunc(t · 31)
```

| field | meaning (section 0's values) |
| --- | --- |
| `m0` | base light: 0.35 … 1.0 in steps, so `t` reaches the middle row (0.5) at `m0 = 1` |
| `m1` | over-brightening past the middle row, towards white (0 … 0.23) |
| `m2` | ambient factor (1 or 0); the ambient itself is `trunc(u16 / 65535)` (`0x42e940`), 0 on the CD |
| `m3` | directional-light factor (1 for materials 0–16, else 0) |
| `m5` | read as a per-span switch by the software renderer (`0x404660`); 0 for 34–48 (inferred: fog off) |

Materials 49–61 (base 0 … 0.41, unlit) are glass materials, see `-4` in §5.5.
In Direct3D (`0x40c330`) a level below the middle row becomes a grey of
`level / 16 · 255` that modulates the texture; above it the grey is 255 and
the rest goes to the specular highlight.

### 5.4 The world sound set — observed

A sound set (section 4), immediately after the ramps; 75..113 sounds per world.
The engine then seeks to its `end` field.

### 5.5 Object list — observed

`u32 count`, then `count` records. For each, `WorldBase_ReadSection` reads a
`u32` FourCC, asks the factory (`Game_CreateObjectByFourCC` @ `0x41bf90`,
falling back to `WorldBase_CreateObjectByFourCC` @ `0x421d80`) for a C++
object, and calls its virtual `Read(stream)` (vtable slot `+0x44`).

| FourCC | Seen | Notes |
|--------|------|-------|
| `Objt` | 60..416 per world | generic scenery object |
| `Fldr` | 17..125 | folder / group |
| `Tele` | 1..23 | teleporter (inferred from name) |
| `Rend` | 0..18 | |
| `MEnc` | 1 | |
| `Strt` | 1 | start position (inferred) |
| `Voic` | 1..2 | |
| `Cmra` | 1 | camera — **different layout**, below |
| `Over`, `STel`, `Save`, `PBal`, `PFrk`, `PMan`, `PPol`, `Pred` | rare | `Save` = save pads, `Pred` = predator (inferred) |

#### The common object record — `WorldObject_Read` @ `0x423b50`

| Size | Field | Confidence |
|------|-------|------------|
| 4    | FourCC | observed |
| 28   | `u32 block_end[7]` — absolute offsets | observed (engine reads and ignores them) |
| 2    | `i16 folder` (resolved via `0x4282b0` when > -1) | layout observed, meaning inferred |
| 4    | `u32 flags` (→ `0x42ce50`) | unknown |
| 12   | `i32 position[3]` — world position | observed (meshes line up) |
| 2+…  | `u16 has_mesh` + **mesh** → ends at `block_end[1]` | observed |
| 2+…  | `u16 has_states` + state machine (`StateMachine_Read` @ `0x42d580`) → `block_end[2]` | layout observed |
| 2+…  | `u16` + block of the `+0xbc` helper (`0x42acc0` class) → `block_end[3]` | unknown |
| 2+…  | `u16 has_appearance` + **appearance** → `block_end[4]` | observed |
| 2+…  | `u16` + block read by `0x42b9e0` → `block_end[5]` | unknown |
| 2+…  | `u16` + block read by `0x428cf0` → `block_end[6]` | unknown |
| 2+…  | `u16` + block read by `0x40a0d0`, then subclass fields → `block_end[0]` | unknown |

The `block_end` table is a gift: whatever the exporter wrote, a reader can jump
over it. Our parser decodes the mesh and the appearance and skips the rest;
walking all 2,097 objects of the eight sections this way lands every time on a
valid FourCC, and each section ends with the same 468-byte trailer.

#### Mesh — `WorldObject_ReadMesh` @ `0x423150` — observed

```text
u32 polygon_count
polygon_count x { u32 n; u16 vertex_index[n] }      n = 3, 4, ... (convex fans)
u32 vertex_count
vertex_count x { i16 x, i16 y, i16 z }               local space
```

World space = `position + vertex` (verified by rendering a wireframe of every
object of section 1: towers, walkways and bridges line up). There are **no UVs
on disk**. The in-memory polygon (0x68 bytes) starts with default texture
coordinates (0,127), (0,0), (127,0) for its first three vertices — the whole
128x128 texture stretched across the polygon (inferred from the initialiser at
`0x42320d`); `GameObject_ComputePolyTextureBasis` @ `0x41d6d0` derives a plane
basis from those vertices for scrolling textures.

#### Appearance — `PolyAppearance_Read` @ `0x42e180` — observed

Per-polygon look, with optional flip-book animation and several states (an
object can change look when clicked):

```text
u16 state_count
state_count x {
  u16 looping
  u16 polygon_count                    == mesh polygon count (verified, 1679 blocks)
  polygon_count x {
    u16 frame_count
    frame_count x { i16 texture; i16 shade; u8 enabled; u32 flags; i16 u; i16 v }   13 bytes
  }
}
```

`WorldObject_ApplyAppearance` @ `0x423730` shows what the frame fields do:
`texture >= 0` → set the polygon's texture slot (all values on the CD are
below the section's texture count); `-5` / `-4` toggle object-level modes
(visibility?); `shade >= 0` → polygon colour/shade index (`+0x60`);
`u, v` → texture origin offset. `flags & 0xc` marks scrolling textures
(`GameObject_BuildTextureScroll` @ `0x41d9e0`); other bits unknown.

In memory a frame is 16 bytes (`0x42e180` reads the file fields into
`+4 texture, +6 shade, +0xc enabled, +0 flags, +8 u, +0xa v`). **`enabled`
only gates the flags**: `0x42df50` (state change) and `0x42e050`
(flip-book step) copy a frame's flags into the per-polygon flag array
(`+0x14`) when `enabled > 0`; the frame appliers `0x423730` and
`0x423810` set texture (slot 0x14, `0x41daf0`), shade (`0x402450`) and
offset (slot 0x18, `0x41dbc0`) of every frame, enabled or not. Flip-books
are typically one enabled frame followed by disabled `-2`/`-3` frames that
only slide the offset (section 1's pad #49 polygon 0: `(8, e1)` then
`(-3, e0, +4/+4) × 31`): the polygon keeps texture 8 all along. `-4` is
**glass**: every applied frame sets the polygon's flag `+0x62` to
`texture == -4` (`0x423791` → `0x4023b0`) and leaves the texture alone.
Direct3D draws glass in bucket `0x27` of `0x40f000` with depth writes off,
alpha blending on with the start-up factors `ONE, ONE` (`0x40a640`), no
texture and no culling, skipping polygons whose level is 0; its grey is
half the usual (`0x40c330`). The software renderer (`0x404660`) never lets
glass hide a span: it adds the glass level to the light level of the
surface behind. Section 0's intro capsule #3 is glass: material 49 (level
0, invisible) while docked, 17–19 during the ride. `-5` hides the
object (`0x4022e0`). The port once read `enabled == 0` as "not drawn",
which blanked every flip-book polygon on all but its first frame (the
"invisible pad under a floating Mendel" of section 1).

The flip-book step (`0x423810(frac)`) advances every polygon one frame per
call (`0x42e050`) and, for `frac < 0x100`, draws the offset part way from
the old frame to the new one (`old + (new − old)·frac >> 8` when the jump is
under 64 texels); `Game_Tick` passes `0x100` while catching up and the
sub-tick fraction on the tick it renders (`0x422230`), so it still steps
exactly once per tick.

#### Camera — `Camera_ReadPaths` @ `0x413e80` — layout observed

`'Cmra'` has no `block_end` table:

```text
u32 n                           == number of objects in the section
n x { u16 k; k x key }          one camera path per object (most empty)
key                             default camera
key = u8 a; u8 b; i32 x, y, z; u16 c[3]     20 bytes, meaning of a/b/c unknown
```

### 5.6 Trailer — unknown

468 bytes, identical layout in every section, read by
`Creature_ReadWorldSection` @ `0x4177d0` and the camera's second reader
`0x414060`. The creature reader also seeks back into the global creature
sound set.

## Proof

A Python reimplementation (scratch, not in the repo) exported every UI
picture, the font, all world textures as PNGs, a WAV of a world sound, and
section 1 as an OBJ + MTL with the textures applied. The Rust tests
(`cargo test -p galapagos-formats`) re-check the same invariants whenever the
CD image is mounted: header values, all 39 pictures, the font geometry, eight
sections with their texture/object counts, mesh indices in range, appearance
polygon counts equal to mesh polygon counts, and the four sound qualities.

## Open questions

* The creature master record (`0x417570`) and its 5 overlays (`0x419bd0`).
* The state-machine opcodes (`StateMachine_Read`; strings
  `"Unknown OpCode in CStateMachine"`) — this is the clickable-object logic.
* The four unknown object blocks (`0x42acc0` class, `0x42b9e0`, `0x428cf0`,
  `0x40a0d0`) and the subclass tails (`Fldr` `0x4287a0`, `PBal/PFrk/PMan`
  `0x416590`, `Pred` `0x42a740`, game objects `0x409070`).
* What the 32 x 32 axes of the shade table are (light x fog? light x depth?).
* Exact UV rule for polygons with more than 4 vertices, and the meaning of the
  appearance `flags` bits.
* The `folder` and `flags` object fields, the camera key
  fields, the 468-byte trailer.
* Why there are 8 sections when the readme talks about 5 worlds (the overlay
  table has 5 entries — maybe 3 sections are hub/tutorial spaces).

Note for future REA sessions: the function names above were applied in the REA
session but REA's Ghidra annotations are ephemeral; re-apply them from this
document after reopening the binary.
