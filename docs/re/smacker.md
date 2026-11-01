# Smacker: the format behind Galapagos' intro and soundtrack

> Source: `crates/formats/src/smacker.rs` (decoder), `crates/formats/examples/smk_dump.rs`
> (PNG/WAV dumper). Written from scratch from public descriptions of the format;
> no RAD code and no third-party crates involved.

## Why Smacker?

In 1997, if your game shipped video on a CD-ROM, odds are it was a **RAD Game
Tools Smacker** file. It was designed for the hardware of the time: a 486 or
early Pentium, a 2x CD drive (300 KB/s), and an 8-bit 256-colour display.
Decoding is nothing but Huffman lookups and byte copies — no DCT, no motion
vectors, no floating point.

Galapagos' `install/data/` folder holds eleven files named `0` .. `10` with no
extension. They all begin with the four bytes `SMK2`. Only one of them is a
video, though: the other ten are **audio-only** Smacker files used as a
convenient streaming-audio container.

## What's on the disc

All numbers below come out of our decoder (`smk_dump`), and audio durations
were measured by actually decoding every sample.

| File | Size     | Frame (stored → shown)         | Frames | Rate      | Audio track 0                | Duration |
|------|----------|--------------------------------|--------|-----------|------------------------------|----------|
| `0`  | 3.6 MB   | 640x240 → 640x480 (Y-interlace) | 386    | 24.004 fps | 22050 Hz, mono, 8-bit, DPCM  | 16.08 s  |
| `1`  | 4.5 MB   | 4x4 (dummy)                    | 1382   | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 138.20 s |
| `2`  | 1.2 MB   | 4x4 (dummy)                    | 360    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 36.00 s  |
| `3`  | 1.8 MB   | 4x4 (dummy)                    | 526    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 52.60 s  |
| `4`  | 1.8 MB   | 4x4 (dummy)                    | 517    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 51.70 s  |
| `5`  | 1.2 MB   | 4x4 (dummy)                    | 344    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 34.40 s  |
| `6`  | 1.5 MB   | 4x4 (dummy)                    | 449    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 44.90 s  |
| `7`  | 1.2 MB   | 4x4 (dummy)                    | 350    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 35.00 s  |
| `8`  | 1.5 MB   | 4x4 (dummy)                    | 423    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 42.30 s  |
| `9`  | 1.4 MB   | 4x4 (dummy)                    | 420    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 42.00 s  |
| `10` | 0.6 MB   | 4x4 (dummy)                    | 172    | 10 fps    | 22050 Hz, mono, 16-bit, DPCM | 17.20 s  |

* **`0` is the intro**: 16 seconds of fiery particles that coalesce into the
  green, arched logo of **Anark**, the studio that made Galapagos, with the
  letters `A N A R K` underneath. It is stored at half height and shown with
  black lines in between — the "Y-interlace" header flag — a classic trick to
  halve the data rate while still filling a 640x480 screen.
* **`1`..`10` are the soundtrack**: about 8.5 minutes in total, each file a
  single 1-colour 4x4 "video" (one block!) carrying 10 audio chunks per second.
  Every frame flags audio only (frame type `0x02`); none carries a palette.
  Which piece of music plays in which world is decided by the game executable
  and is not yet mapped (see "Open questions").

Why wrap music in a video container? Because the engine already linked RAD's
player, which streams from CD, double-buffers, and keeps audio in sync — the
same code path as cinematics, for free.

## Anatomy of a Smacker file

```text
+-------------------+  0x00  "SMK2" / "SMK4"
| header (104 B)    |  0x04  width, 0x08 height, 0x0C frame count
|                   |  0x10  frame rate (signed!), 0x14 flags
|                   |  0x18  7 x audio buffer size
|                   |  0x34  size of the Huffman tree block
|                   |  0x38  MMAP / MCLR / FULL / TYPE tree sizes
|                   |  0x48  7 x audio rate + flags
+-------------------+  0x68
| frame sizes  u32  |  one per frame (+1 if ring frame); low 2 bits are flags
| frame types  u8   |  bit 0: palette chunk; bit 1+n: audio track n chunk
+-------------------+
| Huffman trees     |  one bitstream: MMAP, MCLR, FULL, TYPE
+-------------------+
| frame data ...    |  [palette] [audio track 0..6] [video]
+-------------------+
```

### The frame rate field

A signed 32-bit integer with three meanings:

| value  | meaning                                  | Galapagos            |
|--------|------------------------------------------|----------------------|
| `> 0`  | milliseconds per frame                   | `100` → 10 fps       |
| `< 0`  | **hundredths** of a millisecond per frame | `-4166` → 41.66 ms → 24.004 fps |
| `0`    | default, 10 fps                          |                      |

The negative form exists because 24 fps is not a whole number of milliseconds.

### Flags

| bit | meaning |
|-----|---------|
| 0   | ring frame: one extra frame at the end that morphs the last frame back into the first, for seamless loops |
| 1   | Y-interlace: show each line followed by a black line |
| 2   | Y-double: show each line twice |

### Audio track descriptors

Each of the 7 rate words packs the sample rate in its low 24 bits and flags in
the top byte: `0x80` packed (DPCM), `0x40` track present, `0x20` 16-bit,
`0x10` stereo, `0x0C` Bink audio (a later RAD codec we don't need). Galapagos
uses `0xC0` (packed 8-bit mono, the intro) and `0xE0` (packed 16-bit mono,
the music). `0x5622` = 22050 Hz.

## Bits, LSB first

Every Smacker bitstream is read from the *low* bit of each byte upwards, and a
multi-bit field takes its first bit as its least significant bit. Getting this
backwards produces perfect-looking garbage, so it is the first unit test in
`smacker.rs`.

## Huffman trees, three layers deep

Smacker never stores literal pixel values: everything is a Huffman code. The
tree *shapes* are sent once, in the header block.

1. **8-bit trees.** A recursive pre-order description: bit `1` = inner node
   (left subtree, then right subtree, follow), bit `0` = leaf followed by its
   8-bit symbol. A leading "present" bit (0 = tree absent, always decodes 0)
   and a trailing `0` bit frame the tree.
2. **Big trees** (16-bit symbols). Same shape encoding, but each leaf's 16-bit
   value is itself coded as *(low byte via one 8-bit tree, high byte via
   another)*. Those two small trees, plus three 16-bit **escape values**, are
   stored right before the big tree.
3. **The cache.** Leaves whose value equals one of the three escapes are not
   values at all: they are slots of a tiny most-recently-used cache. After
   every decode the cache shifts (`c2 ← c1 ← c0 ← value`, skipped if the value
   is already `c0`), and it is zeroed at the start of each frame. So "same
   colour pair as the previous block" can cost a single bit.

We store each tree as a flat array in pre-order: an inner node holds the size
of its left subtree, so a 1 bit means "jump over the left subtree". Cache
slots are simply leaves whose value we overwrite in place.

The four big trees:

| tree   | symbols                                                  |
|--------|----------------------------------------------------------|
| `MMAP` | 16-bit masks for mono blocks                             |
| `MCLR` | colour pairs for mono blocks (`high << 8 \| low`)         |
| `FULL` | two pixels at a time for full blocks                     |
| `TYPE` | block kind (2 bits), run-length index (6 bits), extra byte |

## Video: 4x4 blocks

The picture is a grid of 4x4 blocks, decoded in raster order on top of the
previous frame. A `TYPE` symbol gives a kind and a run: `BLOCK_RUNS[index]`
consecutive blocks of that kind, where the table is `1..=59` then `128, 256,
512, 1024, 2048`.

| kind | name  | data per block                                                       |
|------|-------|----------------------------------------------------------------------|
| 0    | mono  | `MCLR` (two colours) + `MMAP` (16 bits: 1 = high colour, LSB = top-left) |
| 1    | full  | 8 `FULL` symbols, two per row, **right half first**                    |
| 2    | void  | nothing: block unchanged since last frame                            |
| 3    | solid | nothing: fill with the colour in the `TYPE` symbol's high byte         |

`SMK4` files read 1-2 extra bits before a full-block run choosing between
normal, "2x2 fat pixels" (2 symbols per block) and "double-height pixels"
(4 symbols). Galapagos has no `SMK4` files, so that path is implemented but
untested on real data.

## Palette deltas

A frame with bit 0 set in its type byte starts with a palette chunk: a length
byte (in 4-byte units, itself included), then operations walking the 256
entries:

| byte(s)              | effect                                              |
|----------------------|-----------------------------------------------------|
| `1nnnnnnn`           | keep the next `n+1` entries                         |
| `01nnnnnn s`         | copy `n+1` entries from the *previous* palette at `s` |
| `00rrrrrr gg bb`     | one new colour                                       |

Colours are 6-bit VGA DAC values. We widen them with `v << 2 | v >> 4`, which
reproduces RAD's `palmap` table exactly (0 → 0, 16 → 0x41, 63 → 0xFF). The
Galapagos intro sends a single palette, in frame 0.

## Audio: Huffman-coded DPCM

Each flagged track contributes a chunk: a `u32` length (itself included), then
data. Raw chunks are plain PCM. Packed chunks start with the decoded size in
bytes, then a bitstream:

1. flags: *has data*, *stereo*, *16-bit*;
2. one 8-bit tree per byte lane: 1 (8-bit mono), 2 (8-bit stereo / 16-bit
   mono), 4 (16-bit stereo: L-lo, L-hi, R-lo, R-hi);
3. each channel's first sample verbatim (right channel first; 16-bit values
   high byte first);
4. every next sample = previous sample + a Huffman-decoded difference,
   wrapping on overflow.

Audio changes slowly, so differences cluster near zero and compress well: the
music files hold 16-bit 22 kHz audio in about 32 KB per second, ~74% of raw.

## Verification

* All 386 intro frames decode; dumped PNGs show coherent images (fire
  particles, the Anark logo) rather than noise.
* For all 11 files, decoded audio length equals `frames / fps` to the sample
  block (e.g. file `1`: 138.20 s of audio for 1382 frames at 10 fps).
* The music decodes to smooth waveforms: the RMS of sample-to-sample
  differences is 3-6x smaller than the signal RMS (white noise would be ~1.4x
  larger), i.e. real, band-limited sound.
* Unit tests (`cargo test -p galapagos-formats`) re-check these properties
  whenever the original disc is mounted at `originals/mounted`.

## Open questions

* Which music file plays where (menu, each world, credits) lives in the game
  executable, which we have not traced yet.
* The keyframe bit (bit 0 of each frame size) is never set in these files,
  and bit 1 of the frame size is unused; we parse but don't rely on them.
* `SMK4` sub-modes and raw/stereo audio paths are implemented from the
  format description but have no test data on this disc.
