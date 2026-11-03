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
288x211 and 144x108 (the same panels at two smaller sizes), 504x30, 256x15,
248x15, 128x8 (bars and strips).

Pictures 0 / 10 / 20 (8-bit) and 30 / 33 / 36 (16-bit) are the chrome
**GALAPAGOS title strip** at three sizes, on a flat violet background.
The game shows it over the 3-D view at the start of world 0 through the
`Over` object class (objects.md, "The title overlay"): the picture's
top-left pixel is its colour key, so only the letters are drawn. The
pause menu of the port reuses picture 0 as its title.

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
| 0   | u32     | 999999 / 60000     | far clip distance (→ renderer `+0x45a0`; Direct3D z = depth / far) |
| 4   | u16[3]  | 0,0,0 / ffff x3    | background = fog colour (48-bit; observed, see §5.3) |
| 10  | u16     | 0 / 1              | stored at `+0x128668` and never read again (observed: no other reference) |
| 12  | u16     | 50                 | ambient light: `trunc(u16 / 65535)` (`0x42e940`, `_ftol`) → `+0x128684`, so 0 on the CD |
| 14  | i32[3]  | -128,-128,-128     | directional light: the direction light travels, normalised (`0x401470`) |
| 26  | u16     | 65535              | light intensity `/65535` (`0x401510`, light `+0x38`) |

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

**Direct3D upload** (observed). With Direct3D on, the reader hands each
texture to the renderer's vtable `+0x20` = `0x40adc0` together with a
palette built *once, from the raw section palette* (`0x42eba0`: bytes 1,
3, 5 of each 6-byte entry, the high bytes of red, green, blue in that
order, into `+0x17e958`, 4 bytes an entry). The brightness preference's
working palette (`+0x127a50`, `0x403130`) is not used there, so in
Direct3D the preference does not touch the 3-D textures (the port's
shader still applies it: a known deviation, harmless at "Standard").
`0x40adc0` creates a 128 × 128 system-memory texture surface (64 × 64,
every other texel, when `0x40ad00` found fewer than ten 128 × 128 video
textures) in the display's 16-bit pixel format and fills it texel by
texel: `(R >> 3) << rshift | (G >> 2 or 3) << gshift | (B >> 3) <<
bshift`, masks and shifts read from the primary surface by `0x40a940`
(5-6-5 or 5-5-5; which one depends on the card). That truncation is the
only colour change on the way: at most −7/255 per channel, no channel
swap, no rounding that turns a hue (Mendel's leg skin, averaged: hue 237° → 235°).
`0x40b0f0` pages the surfaces into a small cache of video textures.

### 5.3 Colour ramps are materials; how a polygon is lit — decoded

`Renderer_ReadColorRamps` @ `0x42ece0`: `u16 count` (62 in every section),
then `count x u16[6]`, each divided by 65535 and passed in order to
`0x405620`, which appends them to the renderer's material table (`+0x1864`,
24 bytes each). A frame's shade number (`+6`, stored at polygon `+0x60` by
`0x402450`; `-1` keeps the current one) selects the polygon's material.

| field | read by | meaning (section 0's values) |
| --- | --- | --- |
| `m0` | `0x401b70` | base light: 0.35 … 1.0 in steps |
| `m1` | `0x401b70` | over-brightening past the middle row, towards white (0 … 0.23) |
| `m2` | `0x401b70` | ambient factor (1 or 0); the ambient is 0 on the CD (§5.1) |
| `m3` | `0x401b70` | directional-light factor (1 for materials 0–16, else 0) |
| `m4` | — | not read by the lighting or by the Direct3D path |
| `m5` | `0x40c502` | fog amount (0 for 34–48: lamps that never fade) |

Materials 49–61 (base 0 … 0.41, unlit) are glass materials, see `-4` in §5.5.

**When.** The mesh draw `0x402490` runs every frame for every object; for
each polygon that survives clipping it calls `0x401b70`, so lighting is
recomputed every frame, not at load. Polygon normals are object-local
(`0x4019a0`: `(v2 − v1) × (v0 − v1)`, normalised, recomputed only when the
mesh changes) and turned into camera space with the object's current
matrix (`0x401640`); the light vector is turned into camera space by the
same camera rotation every frame (`0x401520`, called for each light from the
frame start `0x40f68a`). The dot product is therefore the world-space one:
a turning object is relit, and a flip-book that changes a polygon's shade
(capsule #3's lamps, polygon 2: materials 35, 38, 27 → levels 8, 12, 16)
blinks.

**Level** (`0x401b70`, observed; constants `0x45f030` = 0.0, `0x45f034` = 2.0,
`0x45f038` = 0.5, `0x45f040` = 1.0; rows = `+0x128680` = 32, `pow(2,5)` at
`0x40346f`):

```text
L     = m0 + m2·ambient
for each light (one per section, +0x1778 / +0x177c):
  d = n·l;  if d < 0:  L = L − m3·intensity·d        0x401bc3..0x401c0e
t     = min(L / 2, 0.5) + m1;  if t > 1: t = 1          0x401c10..0x401c53
level = trunc(t · (rows − 1))                           0x401c77 (_ftol) → polygon +0x66
```

Light travels along `l` (section 0: `(−1, −1, −1)/√3`), so a face is lit
when it faces against it, and never from behind. With `m0 ≥ 0.43` any
lit face caps at the middle row 15; turned away it falls back to
`trunc(m0/2 · 31)`; `m1` pushes past 15 towards 22.

**Direct3D colours** (`0x40c330`, observed `0x40c3e2..0x40c5b9`; constant
`0x45f390` = 255.0). Each `D3DTLVERTEX` gets a flat diffuse grey and a flat
specular grey from the polygon's level, and a per-vertex fog factor in the
specular alpha:

```text
level <  16:  diffuse = min(trunc(level / 15 · 255), 255)   specular = 0
level >= 16:  diffuse = 255   specular = min(trunc((level − 16) / 16 · 255), 255)
glass (+0x62):  diffuse = diffuse / 2,  specular = 0
specular += DAT_0048b628, at most 255
fog (alpha) = trunc(255 − min(trunc(1/rhw), 24000) / 24000 · m5 · 255)   per vertex
```

Note the divisor 15: row 15 is already the plain texture. The texture stage
is Direct3D's default `MODULATE`; the specular is added (render state
`SPECULARENABLE` is never set, so it keeps its DirectX 5 default, inferred
TRUE: the footage shows the white wash-out of the bright panels). `1/rhw`
is the vertex's view depth (`0x40c3bb`: `rhw = 1 / z`), clamped to
`0x5dc0` = 24000 (`0x45f394`), whatever the section's far clip.

`DAT_0048b628` is not a preference: `0x434730` writes the screen ramp's red
`[0]` into it each time a palette flash or fade is applied (`0x436e00`,
`0x4371c0`, `0x437350`, `0x4374e0`, `0x437690`): the Direct3D stand-in for
the palette effects of the 8-bit renderer. It is 0 outside flashes.

**Fog** (observed). `FOGENABLE` is set at start-up (`0x40a640`, render
state `0x1c`); the frame start sets `FOGCOLOR` (`0x22`, at `0x40f6a6`) from
`+0x17a560`, which the Direct3D override of the background setter
(`0x40aa80`, vtable `+0xc`, called by the environment reader) makes white
(`0xffffff`) when the background's red is non-zero, else black. The frame
clear (`0x40f5d7`) makes the same choice. Section 1 (background white,
far clip 75000) is the only white world on the CD.

**Brightness preference** (observed). The help text says the setting
"controls the ambient light of the scene", but the code does not touch the
ambient: `0x42ea40` maps the Preferences index 1..7 to the renderer's
vtable `+0x14` = `0x403a90` (mode, amount): Darkest (2, 0.3), Darker
(2, 0.2), Dark (2, 0.1), Standard (0, 0), Light (1, 0.05), Lighter (1, 0.1),
Lightest (1, 0.15). `0x403130` then rebuilds the working palette
(`+0x127a50`) from the section palette: each 16-bit channel moves towards
white (mode 1) or black (mode 2), `c + (target − c)·amount`, except a
channel already at the opposite end. The software shade table (`0x402c91`)
and the screen palette (`0x42ead9`) are built from that palette (inferred:
the Direct3D textures too).

The port: `crates/engine/src/world.rs` (`light_level`, `d3d_colour`,
`fog_factor`, `fog_white`) is the Rust reference; the vertex shader in
`web/src/world-view.js` evaluates it per frame with each object's model
matrix. Mendel's polygons use the section materials their records name
(4 or 8, see mendel.md §1.3); an affliction's brightness `v` rewrites the first two
terms of his per-polygon copy (`0x406110`: below 0x8000 `m0 = 2v/65535,
m1 = 0`, else `m0 = 1, m1 = (v − 0x7fff)/2/65535`), restored every tick by
`0x405fc0`.

**Which faces are drawn** (observed). The polygon normal of `0x4019a0`,
`(v2 − v1) × (v0 − v1)` from the on-disk vertex order, marks the front,
and two tests keep only the front:

* the mesh draw `0x402490` (both renderers): when the mesh's flag `+0x2a`
  is set, a polygon is skipped if `n · v0 ≥ 0` in camera space, `v0` its
  first vertex seen from the eye at the origin (`0x402508..0x4025c6`).
  The flag is 1 from the mesh constructor (`0x402150`); `0x4022f0` sets
  it. The world-object frame appliers set it to `glass == 0` for the
  frame they applied **last**, i.e. the object's last polygon
  (`0x4237e8`, `0x4239c0`): an object ending with glass (the intro capsule
  #3) loses the test entirely. Four effect set-ups save it at their
  `+0x38` and override it (`0x411640`, `0x416850`, `0x429bc0` clear it,
  `0x40fa50` sets it to its flags `& 4`; inferred: the break-up and
  materialise effects); `0x429cf0` restores the saved value.
* Direct3D: the draw loop `0x40f000` sets `CULLMODE` (render state
  `0x16`) to 3 (counter-clockwise) for the texture buckets and 1 (none)
  for the glass bucket `0x27`. It fans a polygon backwards from its last
  vertex, triangles `(n−1, k+1, k)` (`0x40f325..0x40f390`), and projects
  camera x to the right and camera z up with screen y down (`0x401640`).
  inferred: the camera basis is a rotation of the world's (right-handed),
  so a front polygon, counter-clockwise on screen in its own order, is
  clockwise once reversed, and `CULL_CCW` removes back faces only — as it
  must, since the world shows on the footage.

So every textured polygon is one-sided, whatever the flag, and glass is
one-sided unless its object ends with glass (3 objects in section 0, 18
in section 7, none elsewhere; one glass polygon of section 5 sits in a
culled mesh). Panels meant to be seen from both sides are modelled
twice: 84 polygons on the CD have a reversed twin in their own mesh
(8 in section 0, 66 in section 5, 10 in section 7). Mendel's body is a
mesh like the others (flag 1) and is culled too. The port enables
WebGL's back-face culling with counter-clockwise fronts (our fans
`(0, k, k+1)` keep the polygon's order and our view is right-handed);
`WorldMesh::cull` carries the flag for the glass pass. Verified: Mendel
always stands on the drawn side of his floor polygon (test
`mendel_stands_on_the_drawn_side_of_his_floors`, section 0 and section
2's upside-down start), the pad #83 winds counter-clockwise seen from
above, and frame-by-frame comparisons of the original camera with and
without culling differ only where the camera looks through the back of
a wall: in the intro the camera rides behind the white light panels and
now sees the capsule through them, as the footage does (0:55).

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
in the mesh**. The in-memory polygon (0x68 bytes) starts with default texture
coordinates (0,127), (0,0), (127,0) for its first three vertices
(`0x42320d`: floats at `+0x44..+0x58`, `0x42fe0000` = 127.0), but the
object's shape frames overwrite them (objects.md §4, "Vertex morphs"),
and on the CD that happens to every object at level load; see "Texture
coordinates" below. `GameObject_ComputePolyTextureBasis` @ `0x41d6d0`
derives a plane basis from those texels for scrolling textures.

#### Texture coordinates and sampling — observed

The texels of a polygon are the floats at `+0x44..+0x5b` of its record:
`(u, v)` of vertices 0, 1, 2. The initialiser writes (0,127), (0,0),
(127,0) (`0x42320d`), and **the shape morph overwrites them**: slot
`0x70` `0x423530` (on every shape action) and slot `0x74` `0x4235d0`
(every tick of a playing morph) copy the current shape frame's six floats
per polygon there (`0x4236be`, stride 0x68, `+0x44`). Every object's
initial state selects a shape (1,678 of 1,679; objects.md §4), so by
the time the level-load pass is over no polygon of the world keeps the
initialiser's texels. The Direct3D renderer turns the texels into one
`(u, v)` per vertex when it first meets a mesh (`0x40c130` → `0x40b2f0`,
again whenever the mesh is marked changed, which `0x402280` does): it
rotates the polygon into its plane (`0x431ea0` cross product, `0x430ff0`
rotation, `0x431a60`), solves the affine map that sends vertices 0, 1, 2
to the three texels, and evaluates it at **every** vertex, divided by 128
(`0x40b62e..0x40b6b1`, constant `0x45f368` = 128.0); the same map for
any `n` (concave polygons are ear-clipped into triangles by `0x40be00`,
`+0x168/+0x16c` of the per-polygon record). Polygons crossing the screen
edge or the near plane are clipped with a camera-space copy of the same
map (`0x40b750`, used by `0x40e850` / `0x40e020`). `0x40e850` adds the
frame offset, `(u & 0x1ff) / 128` (an integer number of textures away
from the raw `u / 128` the port adds, so the same picture), and
`0x40c330` copies the result into the `D3DTLVERTEX` unchanged. The
software renderer reads the same texels (`0x404100`, offset `& 0x7f`).

Port: `world.rs` `polygon_uvs` (the affine map) and `WorldMesh::set_shape`
(vertices, normals and texels of a shape frame), driven by `sim.rs`
(`select_shape`, the morph step) and kept in saves (format version 5).

**What the footage showed, explained.** Before shape frames were ported
the port drew every polygon with the initialiser's texels: one copy of
the texture stretched over the triangle of its first three vertices,
whatever its size. The 1997 footage disagreed everywhere, and this
section used to conclude that it came from another build. It does not:
the birth room's white light panels (#60, #61, #63, #65, texture 9, a
white tile cut in four by a dark cross) carry about 4 × 3 copies with
their shape texels (test
`birth_room_light_panels_are_a_grid_of_texture_9_when_data_present`):
the grid of white squares of the footage at 0:10 and 0:20; the pillar
under the start pad and the capsule's flame skin repeat the same way.
Over the CD's 8,512 textured polygons, world units per texel go from
"whatever size the polygon is" (90th percentile 73, 966 polygons above
64) to a scale the artists held (35 % at 16..23 units, 90 % below 35, 98
above 64; test `shape_texels_give_one_texel_density_when_data_present`):
the "≈ 16 units a texel" measured on the footage frames. The texels
belong to vertices 0, 1, 2 in file order: with the reversed order
Mendel's own UVs use (n−1, n−2, n−3) coplanar neighbours lose their
continuity instead of gaining it (measured, not kept as a test).

#### Concave polygons and rings — observed

The mesh's polygons are **not all convex**: the CD has 816 concave
ones (test `triangles_tile_every_polygon_when_data_present` lists them),
among them *rings*: an outer outline and an inner one joined by a
zero-width bridge, two vertices repeated. Section 0's save pad is one:
`#87` polygon 5 (texture 1, the dark circuit board) is a hexagonal ring
(14 vertices) 37 units above polygon 7, the pool (texture 21, the blue
clouds, a looping 32-frame flip-book sliding the offset (+4, +4) texels a
tick); `#84` above them is the invisible `0x22` pad itself. The
Direct3D renderer triangulates each polygon once, when it first meets
the mesh (`0x40c130` → `0x40be00`, right after the texel map
`0x40b2f0`):

1. vertices are moved by `−v1` and rotated so that the normal `(v2 − v1)
   × (v0 − v1)` (`0x431ea0`) becomes `+z` (`0x430ff0` builds the matrix,
   `0x431a60` applies it); only `(x, y)` is kept;
2. a corner is convex when `(n.y − c.y)(p.x − c.x) − (n.x − c.x)(p.y −
   c.y) < 800` in `f32` (`0x40bab0`, with its previous and next remaining
   vertices): flat and zero-area corners (the bridge) count as convex;
3. all convex → `+0x168 = 0`, the draw loop fans the polygon; otherwise
   ear clipping from vertex 0, always moving on to the next remaining
   vertex: `cur` is cut off as `(prev, cur, next)` when it is convex and
   no other remaining vertex is inside that triangle (`0x40bd10` →
   `0x40bb50`: inclusive bounding box, then a crossing count over the
   three edges taken from their lower end, `lo.y <= p.y < hi.y`, counted
   when `ftol(x on the edge) <= ftol(p.x)`), until `n − 2` triangles are
   stored at `+0x16c` (`(next, cur, prev)` each, mesh indices and polygon
   positions).

The port fanned every polygon, so `#87`'s ring became a filled hexagon:
a grey lid over the pool, and the "shimmer" ran unseen underneath (the
bug report "the save pad's animation is wrong"). `world.rs`
`triangulate` reproduces `0x40be00` (inferred: if no ear is ever found,
where the exe would loop forever, it fans the rest; no CD polygon needs
it). Every concave polygon of the CD comes out tiled exactly (triangle
areas add up to the polygon's own). Clicks (`World_Pick` → `0x4076d0` →
`0x407830`) test the polygon's own edges, so a ring's hole is a hole
there too; `Sim::pick_hit` now uses the same triangles, and a click in
the pool reaches the save pad `#84` instead of the ring.

**Walls with doorways are rings too** — observed. Section 0's octagonal
hall is walled by `#100`, `#101`, `#102` (texture 2, flags `0x340`:
drawn and solid) and `#148`, `#149` (texture 22), each one keyhole
polygon round a window, with a second keyhole (texture 27, the grey
panels) as the window's frame: `#100` polygon 2 is `[12, 13, 9, 0, 3, 2,
1, 0, 9, 8]`, the window at x −2220..0, z 2960..5920 of the wall y =
−15170, where the walkway `#94` meets the white-ceilinged corridor `#79`;
`#101` polygon 2 holds both a window near the floor (y −2960..0, z
−25900..−23680, the footage's 2:45, Mendel climbing past it) and the
hexagonal hole high up that the intro capsule flies in by (y ≈
−5153..5153, z 11840..16280). Fanned,
every one of them was bricked up: a dark wall with the grey frame
z-fighting on it where the footage shows the lattice-lit corridor beyond
and, worse, **solid**:

Collision never triangulates. `World_Cast` (`0x4089e0` → `0x408730` →
`0x4076d0`) first crosses the polygon's plane (`0x407740`, integer
normal, either way), then counts the crossings of a ray from the hit
point over the polygon's own edges in a coordinate plane picked by the
record's `+0x24` (`0x407830`; its state machine counts a vertex the ray
passes exactly once): an even-odd test of the outline, for which a
bridge drawn twice cancels and a ring's hole is a hole. The port's casts
(`walker.rs` `Near::gather`, every probe and hull ray of Mendel, the
camera's collision and the predators' probes) fanned each polygon from
its first vertex, so the hull stopped Mendel 450 units short of every
doorway. They now cast against `triangulate`'s triangles (`LocalPoly::tris`),
which cover exactly the even-odd area (the tiling test). Consequences,
all tests: Mendel walks off `#94` through `#100`'s window into `#79`
(`section0_walkway_leads_through_the_window_when_data_present`, stopped
only by `#77` standing across the corridor); the intro capsule's path
crosses no drawn solid polygon until it has dropped him
(`intro_capsule_flies_through_openings_when_data_present`); section 2's
belt `#197` carries him through the opening of wall `#201` onto the next
belt `#177` instead of pinning him to a filled hole; section 2's `Strt`
`#194` stands him on its own polygon 1 (the hexagonal plate, recessed 74
units inside the ring polygon 6) rather than on the ring's filled hole.
inferred: triangles and the exe's edge count can disagree only on a
polygon's boundary (a ray through a vertex), a set of measure zero.

The flip-book itself was right: it runs while the pad's folder chain
(`#81` < `#80` < `#74`, the second room) is awake. `#74` switches its
97 descendants off on tick 2 while Mendel is elsewhere and back on when
he touches it, so far from him the pool shows a still frame, as every
object of a sleeping folder does.

Scrolling floors: `0x41d9e0` puts the object's current shape frame on the
mesh (slot `0x70`; at build, state 0, frame 0) right before `0x41d6d0`
reads `+0x44`, so the conveyor basis comes from the shape texels too.
Section 2's belt #197 has `t` running along its length (v1→v2), not
across it as the initialiser's texels had it: Mendel rides it to its end
(walker test `section2_belt_carries_him_at_the_exes_speed`). inferred:
`0x41d6d0`'s x87 tail is read as the inverse of the texel map
(`E · T⁻¹`), the only map consistent with both texel sets.

Sampling (`0x40a640`, render states): `TEXTUREMAG` (0x11) and `TEXTUREMIN`
(0x12) both `D3DFILTER_LINEAR` (2), i.e. bilinear **without mip-maps**;
`TEXTUREPERSPECTIVE` (4) on; `TEXTUREADDRESS` (3) is set nowhere (not
there, not in the draw loop `0x40f000`), so it stays Direct3D's `WRAP`. The port built mip-maps (`LINEAR_MIPMAP_LINEAR`) and wrapped with
`fract()` in the shader; at every repeat boundary `fract` jumps, the
derivative explodes, the smallest mip level is picked, and a dashed grey
seam appeared (across the light panels, floors). It now samples like the
exe: `LINEAR`, no mip-maps, the sampler's `REPEAT`.

(An earlier version of this section measured the footage's texel density
against the initialiser's texels and concluded that the footage came
from another build; the shape frames, ported since, explain it. See
above.)

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
texture and no culling (render state `CULLMODE` 1; every other bucket
uses 3 = counter-clockwise), skipping polygons whose level is 0; its grey
is half the usual diffuse, with no highlight (`0x40c478`), and it is
fogged like any polygon. Glass loses its back faces only through the
mesh's own test, which is off when the object's last polygon is glass
(see "Which faces are drawn", §5.3). The software renderer (`0x404660`) never lets
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
* What the 32 x 32 axes of the shade table are: one is the light level
  (§5.3); the other is presumably the fog step the software renderer
  applies when a span's material has `m5` (`0x404660`), towards the
  background colour (`0x402d82` reads it while building the table).
* The meaning of the appearance `flags` bits. (The UV rule for polygons of
  more than 4 vertices is settled: "Texture coordinates and sampling".)
* (Settled: the footage's texel density is this CD's, once the shape
  frames' texels are applied; see "Texture coordinates and sampling".)
* The `folder` and `flags` object fields, the camera key
  fields, the 468-byte trailer.
* Why there are 8 sections when the readme talks about 5 worlds (the overlay
  table has 5 entries — maybe 3 sections are hub/tutorial spaces).

Note for future REA sessions: the function names above were applied in the REA
session but REA's Ghidra annotations are ephemeral; re-apply them from this
document after reopening the binary.
