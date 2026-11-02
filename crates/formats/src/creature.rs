//! Mendel: the creature record, body, sensors and NERM brain.
//! See docs/re/mendel.md for the full, pedagogic spec.
//!
//! Three things in the `worlds` archive belong to Mendel:
//!
//! ```text
//! header.creature            0x3601c6  CreatureBody  (mesh + 128-frame walk cycle)
//! header.creature_overlays   0x37505e  5 x Brain     (pre-trained NERM brains, 28,242 B each)
//! header.creature_sounds     0x3977f8  sound set     (11 sounds, see worlds.rs)
//! ```
//!
//! and one thing at the end of every world section belongs to the camera
//! that follows him: the 468-byte trailer ([`CameraProfiles`]).
//!
//! A `.mnd` file ("Export Mendel NERM") is byte-for-byte the same as one
//! overlay: three NERM controllers back to back ([`Brain`]).
//!
//! Function names/addresses below are the ones applied in the REA session
//! and listed in docs/re/mendel.md.

use crate::bytes::{Cursor, OutOfBounds};

/// Number of key frames stored per body part (one full walk cycle).
pub const FRAMES: usize = 128;
/// Mendel's polygons use texture slots `16 + texture` of the current world
/// section: every section carries its own copy of his four skins, repainted
/// in that section's palette.
pub const TEXTURE_BASE: u16 = 16;

fn i16le(c: &mut Cursor) -> Result<i16, OutOfBounds> {
    Ok(c.u16()? as i16)
}

fn f32le(c: &mut Cursor) -> Result<f32, OutOfBounds> {
    Ok(f32::from_bits(c.u32()?))
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

/// Per-polygon render attributes, read last in `CreatureBody_ReadPart` @ 0x426c50.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolyAttr {
    /// Texture, relative to [`TEXTURE_BASE`] (the engine adds 16 on load).
    pub texture: u16,
    /// Index of a section colour ramp (re-registered with the renderer per
    /// section by `Creature_ReadWorldSection` @ 0x4177d0). Values 4 and 8.
    pub color_ramp: u16,
    /// Texel offset `(u, v)` at polygon +0x5c / +0x5e: the same field a
    /// world polygon's appearance frame sets (`0x402420`). The Direct3D
    /// draw adds `(offset & 0x1ff) / 128` to the texture coordinates
    /// (`0x40e850`), the software one `offset & 0x7f` texels (`0x404100`):
    /// with wrapping textures, the same shift.
    pub offset: [i16; 2],
}

/// One of the three body parts: the shell, or one stance variant of the legs.
///
/// On disk (`CreatureBody_ReadPart` @ 0x426c50):
///
/// ```text
/// i32 offset[3]                       origin shift applied to every frame
/// u32 polygon_count
/// polygon_count x { u32 n; u16 index[n] }
/// polygon_count x f32[6]              (u,v) at three reference vertices
/// 128 x { u32 vertex_count; vertex_count x i16[3] }   key frames
/// polygon_count x { u16 texture, color_ramp, unknown[2] }
/// ```
#[derive(Debug, Clone)]
pub struct BodyPart {
    pub offset: [i32; 3],
    /// Vertex indices in *file* order (see [`CreatureBody::pose`] for winding).
    pub polygons: Vec<Vec<u16>>,
    /// Texture coordinates in texels (128 = one texture width), as
    /// `(u0,v0,u1,v1,u2,v2)` for the file vertices 0, 1, 2 of the polygon
    /// (for the reversed parts) — vertex 1 is always (0,0).
    pub uv: Vec<[f32; 6]>,
    /// `FRAMES` key frames of raw (untransformed) vertices.
    pub frames: Vec<Vec<[i16; 3]>>,
    pub attrs: Vec<PolyAttr>,
}

impl BodyPart {
    fn read(c: &mut Cursor, polygon_count: usize, vertex_count: usize) -> Result<Self, OutOfBounds> {
        let offset = [c.i32()?, c.i32()?, c.i32()?];
        let n = c.u32()? as usize;
        if n != polygon_count {
            return Err(OutOfBounds { at: c.pos, want: polygon_count });
        }
        let mut polygons = Vec::with_capacity(n);
        for _ in 0..n {
            let k = c.u32()? as usize;
            let mut p = Vec::with_capacity(k);
            for _ in 0..k {
                p.push(c.u16()?);
            }
            polygons.push(p);
        }
        let mut uv = Vec::with_capacity(n);
        for _ in 0..n {
            let mut q = [0f32; 6];
            for v in q.iter_mut() {
                *v = f32le(c)?;
            }
            uv.push(q);
        }
        let mut frames = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            let k = c.u32()? as usize;
            if k != vertex_count {
                return Err(OutOfBounds { at: c.pos, want: vertex_count });
            }
            let mut f = Vec::with_capacity(k);
            for _ in 0..k {
                f.push([i16le(c)?, i16le(c)?, i16le(c)?]);
            }
            frames.push(f);
        }
        let mut attrs = Vec::with_capacity(n);
        for _ in 0..n {
            attrs.push(PolyAttr { texture: c.u16()?, color_ramp: c.u16()?, offset: [c.u16()? as i16, c.u16()? as i16] });
        }
        Ok(Self { offset, polygons, uv, frames, attrs })
    }

    /// The engine's load-time vertex transform (0x426f1b): an axis swap plus
    /// the part offset. Output is in the creature's model space.
    pub fn transform(&self, v: [i16; 3]) -> [i32; 3] {
        let [x, y, z] = v.map(i32::from);
        let o = self.offset;
        [140 - o[2] - z, -(o[1] + y), -(o[0] + x)]
    }
}

/// Mendel's body, read by `CreatureBody_Read` @ 0x426a70 from `header.creature`.
///
/// ```text
/// u32 body_polygons (34)  u32 leg_polygons (22)
/// u32 body_vertices (41)  u32 leg_vertices (32)
/// BodyPart shell          (body counts)
/// BodyPart legs_a         (leg counts)   stance variant "a"
/// BodyPart legs_b         (leg counts)   stance variant "b"
/// ```
///
/// There is no skeleton: each part is a vertex-animated mesh with 128 key
/// frames forming one walk cycle. A leg part holds the two legs of one side;
/// the engine builds the other side by mirroring (z -> -z) and playing it
/// half a cycle later. `legs_a`/`legs_b` are two stances blended by the
/// brain's posture output.
#[derive(Debug, Clone)]
pub struct CreatureBody {
    pub shell: BodyPart,
    pub legs_a: BodyPart,
    pub legs_b: BodyPart,
    /// File offset just after the record (== first overlay on the CD).
    pub end: usize,
}

/// A posed, renderable mesh.
#[derive(Debug, Clone)]
pub struct PosedMesh {
    pub vertices: Vec<[i32; 3]>,
    pub faces: Vec<Face>,
}

#[derive(Debug, Clone)]
pub struct Face {
    /// Indices into [`PosedMesh::vertices`], in engine winding.
    pub indices: Vec<u16>,
    /// Per-vertex texture coordinates in texels.
    pub uv: Vec<[f32; 2]>,
    /// Absolute texture slot in the current section (16..=19).
    pub texture: u16,
    /// The section colour ramp (material) of the polygon (4 or 8).
    pub color_ramp: u16,
    /// Texel offset added to `uv` when drawn ([`PolyAttr::offset`]).
    pub offset: [i16; 2],
}

impl CreatureBody {
    pub fn parse(data: &[u8], offset: usize) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::at(data, offset);
        let (bp, lp, bv, lv) = (c.u32()? as usize, c.u32()? as usize, c.u32()? as usize, c.u32()? as usize);
        let shell = BodyPart::read(&mut c, bp, bv)?;
        let legs_a = BodyPart::read(&mut c, lp, lv)?;
        let legs_b = BodyPart::read(&mut c, lp, lv)?;
        Ok(Self { shell, legs_a, legs_b, end: c.pos })
    }

    /// Walk-cycle phase (0..=255, the high byte of the engine's 16-bit phase
    /// accumulator) to key frame. The engine's table at body+0x6a8 is
    /// `frame = phase / 2`.
    pub fn frame_for_phase(phase: u8) -> usize {
        phase as usize / 2
    }

    /// Builds the mesh exactly like `CreatureBody_SetPose` @ 0x426410 for
    /// straight walking (`turn = 0`):
    ///
    /// * shell = frame(phase)
    /// * side A legs = lerp(legs_b, legs_a, stance/256) at frame(phase)
    /// * side B legs = same at frame(phase + 128), mirrored z -> -z
    ///
    /// Polygon winding: the shell and side A are reversed on load
    /// (`CreatureBody_RemapIndices` @ 0x427040 with flag 1); side B, being
    /// mirrored, keeps file order.
    pub fn pose(&self, phase: u8, stance: u8) -> PosedMesh {
        self.pose_turning(phase, stance as i32, 0, true)
    }

    /// The full `CreatureBody_SetPose` @ 0x426410, turning included.
    ///
    /// Turning is "tank style": the legs of one side play the walk cycle
    /// backwards, blended in proportionally to `turn` (0..=256). `side`
    /// (the brain's turn output >= 128) picks which side reverses: side B
    /// when `side` is set, side A otherwise.
    ///
    /// ```text
    /// legs(ph)   = lerp(legs_b, legs_a, stance/256) at frame(ph)
    /// back(ph)   = lerp(legs(ph), legs(-ph), turn/256)
    /// side A     = side ? legs(ph)             : back(ph)
    /// side B     = mirror_z(side ? back(ph+128) : legs(ph+128))
    /// ```
    pub fn pose_turning(&self, phase: u8, stance: i32, turn: i32, side: bool) -> PosedMesh {
        let t = stance.clamp(0, 256);
        let turn = turn.clamp(0, 256);
        let frame = |p: &BodyPart, ph: u8| -> Vec<[i32; 3]> {
            p.frames[Self::frame_for_phase(ph)].iter().map(|&v| p.transform(v)).collect()
        };
        let lerp = |a: &[[i32; 3]], b: &[[i32; 3]], w: i32| -> Vec<[i32; 3]> {
            a.iter().zip(b).map(|(a, b)| core::array::from_fn(|k| a[k] + (((b[k] - a[k]) * w) >> 8))).collect()
        };
        let legs = |ph: u8| -> Vec<[i32; 3]> { lerp(&frame(&self.legs_b, ph), &frame(&self.legs_a, ph), t) };
        let back = |ph: u8| -> Vec<[i32; 3]> {
            let fwd = legs(ph);
            if turn == 0 {
                return fwd;
            }
            lerp(&fwd, &legs(ph.wrapping_neg()), turn)
        };
        let half = phase.wrapping_add(128);
        let (side_a, side_b) = if side { (legs(phase), back(half)) } else { (back(phase), legs(half)) };
        let mut vertices = frame(&self.shell, phase);
        let base_a = vertices.len();
        vertices.extend(side_a);
        let base_b = vertices.len();
        vertices.extend(side_b.into_iter().map(|[x, y, z]| [x, y, -z]));

        let mut faces = Vec::new();
        for (part, base, reverse) in [(&self.shell, 0, true), (&self.legs_a, base_a, true), (&self.legs_b, base_b, false)] {
            for (i, poly) in part.polygons.iter().enumerate() {
                let mut idx: Vec<u16> = poly.iter().map(|&v| v + base as u16).collect();
                if reverse {
                    idx.reverse();
                }
                let uv = polygon_uvs(&vertices, &idx, &part.uv[i]);
                let a = part.attrs[i];
                faces.push(Face { indices: idx, uv, texture: a.texture + TEXTURE_BASE, color_ramp: a.color_ramp, offset: a.offset });
            }
        }
        PosedMesh { vertices, faces }
    }
}

/// Texture coordinates are an affine map on the polygon plane. The file gives
/// it at the engine-order vertices n-1, n-2, n-3 (`CreatureBody_ConvertUVs`
/// @ 0x4270e0 re-expresses it at vertices 0, 1, 2); we evaluate it at every
/// vertex so a GPU can use plain per-vertex UVs.
fn polygon_uvs(v: &[[i32; 3]], idx: &[u16], uv: &[f32; 6]) -> Vec<[f32; 2]> {
    let n = idx.len();
    let p = |i: usize| v[idx[i] as usize].map(|c| c as f32);
    let (r0, o, r2) = (p(n - 1), p(n - 2), p(n - 3));
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let (e1, e2) = (sub(r0, o), sub(r2, o));
    let (g11, g12, g22) = (dot(e1, e1), dot(e1, e2), dot(e2, e2));
    let det = g11 * g22 - g12 * g12;
    let t = [[uv[0], uv[1]], [uv[2], uv[3]], [uv[4], uv[5]]];
    (0..n)
        .map(|i| {
            if det.abs() < 1e-6 {
                return t[1];
            }
            let r = sub(p(i), o);
            let (r1, r2) = (dot(r, e1), dot(r, e2));
            let a = (r1 * g22 - r2 * g12) / det;
            let b = (r2 * g11 - r1 * g12) / det;
            core::array::from_fn(|k| t[1][k] + a * (t[0][k] - t[1][k]) + b * (t[2][k] - t[1][k]))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// NERM brain
// ---------------------------------------------------------------------------

/// Serialised size of one controller (`Nerm_Read` @ 0x428070).
pub const NERM_LEN: usize = 9414;
/// Size of a brain = one overlay = one `.mnd` file.
pub const BRAIN_LEN: usize = 3 * NERM_LEN;
pub const STATES: usize = 10;
pub const POPULATION: usize = 30;
pub const KNOTS: usize = 9;

/// The engine's random generator (global at 0x468640): the classic
/// `seed = seed * 1103515245 + 12345`, using bits 16..30.
#[derive(Debug, Clone, Copy)]
pub struct Lcg(pub u32);

impl Lcg {
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(0x41c6_4e6d).wrapping_add(0x3039);
        (self.0 >> 16) & 0x7fff
    }
}

/// One NERM controller: a 10-state Markov chain whose transition
/// probabilities are piecewise-linear functions of the inputs, evolved by a
/// tiny genetic algorithm (population 30). See docs/re/mendel.md §4.
///
/// On disk, in this order (sizes in bytes):
///
/// | field          | size | meaning |
/// |----------------|------|---------|
/// | `state`        | 2    | current chain state 0..9 |
/// | `transitions`  | 60   | per state: next state for {stay, down, up} |
/// | `outputs`      | 20   | output level of each state (0 or 256) |
/// | `probs_cache`  | 60   | last transition probabilities (x 32767) |
/// | `genome`       | 8100 | `u8[30 individuals][10 states][3 transitions][9 knots]` |
/// | `knots`        | 18   | input breakpoints 0,32,..,256 |
/// | `exp_table`    | 1028 | `floor(2^(i/24))`, 256 used + 1 spare |
/// | `fitness`      | 120  | cost of each evaluated individual (lower = better) |
/// | `individual`   | 2    | individual currently driving the chain |
/// | `input_count`  | 2    | 3 |
/// | `samples`      | 2    | chain steps per decision (200) |
#[derive(Debug, Clone, PartialEq)]
pub struct Nerm {
    pub state: i16,
    pub transitions: [[i16; 3]; STATES],
    pub outputs: [i16; STATES],
    pub probs_cache: [[i16; 3]; STATES],
    pub genome: Vec<u8>,
    pub knots: [i16; KNOTS],
    pub exp_table: Vec<i32>,
    pub fitness: [i32; POPULATION],
    pub individual: i16,
    pub input_count: i16,
    pub samples: i16,
}

impl Nerm {
    pub fn read(c: &mut Cursor) -> Result<Self, OutOfBounds> {
        let state = i16le(c)?;
        let mut transitions = [[0i16; 3]; STATES];
        for t in transitions.iter_mut().flatten() {
            *t = i16le(c)?;
        }
        let mut outputs = [0i16; STATES];
        for o in outputs.iter_mut() {
            *o = i16le(c)?;
        }
        let mut probs_cache = [[0i16; 3]; STATES];
        for p in probs_cache.iter_mut().flatten() {
            *p = i16le(c)?;
        }
        let genome = c.take(POPULATION * STATES * 3 * KNOTS)?.to_vec();
        let mut knots = [0i16; KNOTS];
        for k in knots.iter_mut() {
            *k = i16le(c)?;
        }
        let mut exp_table = Vec::with_capacity(257);
        for _ in 0..257 {
            exp_table.push(c.i32()?);
        }
        let mut fitness = [0i32; POPULATION];
        for f in fitness.iter_mut() {
            *f = c.i32()?;
        }
        Ok(Self {
            state,
            transitions,
            outputs,
            probs_cache,
            genome,
            knots,
            exp_table,
            fitness,
            individual: i16le(c)?,
            input_count: i16le(c)?,
            samples: i16le(c)?,
        })
    }

    /// Serialises in `Nerm_Write` @ 0x428110 order (same as the reader).
    pub fn write(&self, out: &mut Vec<u8>) {
        let w16 = |v: i16, out: &mut Vec<u8>| out.extend_from_slice(&v.to_le_bytes());
        w16(self.state, out);
        self.transitions.iter().flatten().for_each(|&v| w16(v, out));
        self.outputs.iter().for_each(|&v| w16(v, out));
        self.probs_cache.iter().flatten().for_each(|&v| w16(v, out));
        out.extend_from_slice(&self.genome);
        self.knots.iter().for_each(|&v| w16(v, out));
        self.exp_table.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        self.fitness.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        w16(self.individual, out);
        w16(self.input_count, out);
        w16(self.samples, out);
    }

    /// Genome byte for (individual, state, transition, knot).
    pub fn gene(&self, ind: usize, state: usize, tr: usize, knot: usize) -> u8 {
        self.genome[((ind * STATES + state) * 3 + tr) * KNOTS + knot]
    }

    /// `Nerm_ComputeTransitionProbs` @ 0x427c50. Transition `j` of every
    /// state reads input `(3*state + j) % input_count` (= input `j`). The
    /// input (0..=256) picks a segment between two knots, the gene values at
    /// both ends are linearly interpolated to a level 0..255, and the level
    /// goes through `exp_table` to a positive weight. Weights are normalised
    /// per state to sum to 32767.
    pub fn transition_probs(&self, inputs: [i32; 3]) -> [[i16; 3]; STATES] {
        let ind = self.individual.rem_euclid(POPULATION as i16) as usize;
        let n = self.input_count.max(1) as usize;
        let mut out = [[0i16; 3]; STATES];
        for s in 0..STATES {
            let mut w = [0i32; 3];
            for (j, wj) in w.iter_mut().enumerate() {
                let x = inputs[(3 * s + j) % n];
                let b = ((x * 8) >> 8).clamp(0, KNOTS as i32 - 1) as usize;
                let k0 = self.knots[b] as i32;
                let g0 = self.gene(ind, s, j, b) as i32;
                let level = if k0 == x {
                    g0
                } else {
                    let g1 = self.gene(ind, s, j, b + 1) as i32;
                    (x - k0) * (g1 - g0) / (self.knots[b + 1] as i32 - k0) + g0
                };
                *wj = self.exp_table[level as usize];
            }
            let total: u32 = w.iter().map(|&v| v as u32).sum();
            for j in 0..3 {
                out[s][j] = ((w[j] as u32 * 0x7fff) / total.max(1)) as i16;
            }
        }
        out
    }

    /// `Nerm_RunMarkovChain` @ 0x427de0: walk the chain `samples` times and
    /// return the mean output level (0..=256). With outputs alternating
    /// 0/256 this is "how often the walk sat in an odd state".
    pub fn run_chain(&mut self, probs: &[[i16; 3]; STATES], rng: &mut Lcg) -> i32 {
        let mut acc = 0i32;
        for _ in 0..self.samples {
            let r = rng.next() as i16;
            let s = self.state as usize;
            let mut cum = 0i16;
            for j in 0..3 {
                cum = cum.wrapping_add(probs[s][j]);
                if r < cum || j == 2 {
                    self.state = self.transitions[s][j];
                    break;
                }
            }
            acc += self.outputs[self.state as usize] as i32;
        }
        acc / self.samples.max(1) as i32
    }

    /// `Nerm_ReportFitnessAndEvolve` @ 0x427ea0: record the cost of the
    /// current individual and move on; after all 30 have been tried, rank
    /// them (lowest cost first), keep the best 15 and rebuild the worst 15
    /// gene by gene: each gene comes from a random elite parent, plus a
    /// uniform mutation in -10..=10, clamped to 0..=255.
    pub fn report_fitness(&mut self, cost: i32, rng: &mut Lcg) {
        let i = self.individual as usize % POPULATION;
        self.fitness[i] = cost;
        self.individual += 1;
        if self.individual as usize != POPULATION {
            return;
        }
        let mut f = self.fitness;
        let mut rank = [0usize; POPULATION];
        for r in rank.iter_mut() {
            let (mut best, mut v) = (0, 0x0fff_ffff);
            for (k, &c) in f.iter().enumerate() {
                if c < v {
                    v = c;
                    best = k;
                }
            }
            *r = best;
            f[best] = 0x0fff_ffff;
        }
        let per = STATES * 3 * KNOTS;
        for &child in &rank[15..] {
            for g in 0..per {
                let parent = rank[(rng.next() % 15) as usize];
                let delta = (rng.next() % 21) as i32 - 10;
                let v = (self.genome[parent * per + g] as i32 + delta).clamp(0, 255);
                self.genome[child * per + g] = v as u8;
            }
        }
        self.individual = 0;
    }
}

/// Mendel's brain: three NERM controllers driving, in order, posture
/// (`stance`), walking speed and turning. Read by `Creature_ReadBrain_Mnd`
/// @ 0x419bd0 both from the overlays and from `.mnd` files.
#[derive(Debug, Clone, PartialEq)]
pub struct Brain {
    pub nerms: [Nerm; 3],
}

impl Brain {
    pub fn parse(data: &[u8], offset: usize) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::at(data, offset);
        Ok(Self { nerms: [Nerm::read(&mut c)?, Nerm::read(&mut c)?, Nerm::read(&mut c)?] })
    }

    /// A newborn's brain (age level 1). The original runs `Nerm_Init` @
    /// 0x427ac0, filling the genome from its LCG. inferred: we keep the
    /// structure of a shipped brain (knots, exp table, chain layout) and
    /// replace the genome with LCG bytes, forgetting all fitness: a mind
    /// with Mendel's anatomy and none of his experience.
    pub fn newborn(template: &Brain, seed: u32) -> Brain {
        let mut rng = Lcg(seed);
        let mut b = template.clone();
        for n in &mut b.nerms {
            for g in n.genome.iter_mut() {
                *g = rng.next() as u8;
            }
            n.fitness = [0; POPULATION];
            n.individual = 0;
            n.state = 0;
        }
        b
    }

    /// The bytes of a `.mnd` file.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(BRAIN_LEN);
        self.nerms.iter().for_each(|n| n.write(&mut v));
        v
    }
}

/// Maps the "age" preference slider (0..=255) to a starting brain, as the
/// new-game code at 0x4171ad does: 0 -> newborn (random genome, `None`),
/// 1..=84 -> overlay 1, 85..=169 -> overlay 2, 170..=254 -> overlay 3,
/// 255 -> overlay 4. Overlay 0 is never selected by this path.
pub fn overlay_for_age(slider: u8) -> Option<usize> {
    let age = match slider {
        0 => 1,
        1..=84 => 2,
        85..=169 => 3,
        170..=254 => 4,
        255 => 5,
    };
    (age > 1).then(|| age - 1)
}

// ---------------------------------------------------------------------------
// Section trailer
// ---------------------------------------------------------------------------

/// One camera motion profile (16 bytes in memory, 14 on disk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraProfile {
    /// 16.16 fraction (0.6 .. 0.95): smoothing factor (inferred).
    pub damping_a: i32,
    /// 16.16 fraction: second smoothing factor (inferred).
    pub damping_b: i32,
    pub c: i16,
    pub d: i16,
    /// 0..11; 0 and 7 make the camera jump straight to its target.
    pub mode: i16,
}

/// The 468-byte trailer of every world section, read by
/// `Camera_ReadMotionProfiles` @ 0x414060 (it is *not* creature data):
/// `u32 unknown (1520); u16 n (33); n x { i32, i32, i16, i16, i16 }`.
/// A camera key's second byte indexes this table. Identical in all 8
/// sections of the CD.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraProfiles {
    pub unknown: u32,
    pub profiles: Vec<CameraProfile>,
}

impl CameraProfiles {
    pub fn parse(trailer: &[u8]) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::new(trailer);
        let unknown = c.u32()?;
        let n = c.u16()? as usize;
        let mut profiles = Vec::with_capacity(n);
        for _ in 0..n {
            profiles.push(CameraProfile {
                damping_a: c.i32()?,
                damping_b: c.i32()?,
                c: i16le(&mut c)?,
                d: i16le(&mut c)?,
                mode: i16le(&mut c)?,
            });
        }
        Ok(Self { unknown, profiles })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worlds::Archive;

    fn data() -> Option<Vec<u8>> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        std::fs::read(p).ok()
    }

    fn synthetic_nerm() -> Nerm {
        let mut transitions = [[0i16; 3]; STATES];
        let mut outputs = [0i16; STATES];
        for s in 0..STATES as i16 {
            transitions[s as usize] = [s, if s == 0 { 1 } else { s - 1 }, if s == 9 { 8 } else { s + 1 }];
            outputs[s as usize] = if s % 2 == 0 { 0 } else { 256 };
        }
        Nerm {
            state: 0,
            transitions,
            outputs,
            probs_cache: [[0; 3]; STATES],
            genome: (0..8100).map(|i| (i * 7 % 256) as u8).collect(),
            knots: core::array::from_fn(|i| (i * 32) as i16),
            exp_table: (0..257).map(|i| if i < 256 { 2f64.powf(i as f64 / 24.0) as i32 } else { 0 }).collect(),
            fitness: [0; POPULATION],
            individual: 0,
            input_count: 3,
            samples: 200,
        }
    }

    #[test]
    fn nerm_roundtrip_and_chain() {
        let n = synthetic_nerm();
        let mut b = Vec::new();
        n.write(&mut b);
        assert_eq!(b.len(), NERM_LEN);
        let back = Nerm::read(&mut Cursor::new(&b)).unwrap();
        assert_eq!(back, n);
        let mut m = n.clone();
        let p = m.transition_probs([0, 128, 256]);
        for row in p {
            assert!(row.iter().map(|&v| v as i32).sum::<i32>() <= 0x7fff);
        }
        let mut rng = Lcg(1);
        let out = m.run_chain(&p, &mut rng);
        assert!((0..=256).contains(&out));
        for i in 0..30 {
            m.report_fitness(i, &mut rng);
        }
        assert_eq!(m.individual, 0);
        assert_eq!(&m.genome[..270], &n.genome[..270], "the best individual survives untouched");
    }

    #[test]
    fn age_slider() {
        assert_eq!(overlay_for_age(0), None);
        assert_eq!(overlay_for_age(84), Some(1));
        assert_eq!(overlay_for_age(85), Some(2));
        assert_eq!(overlay_for_age(255), Some(4));
    }

    #[test]
    fn body_matches_retail_cd() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let body = CreatureBody::parse(&d, a.header.creature as usize).unwrap();
        assert_eq!((body.shell.polygons.len(), body.shell.frames[0].len()), (34, 41));
        assert_eq!((body.legs_a.polygons.len(), body.legs_a.frames[0].len()), (22, 32));
        assert_eq!(body.legs_a.polygons, body.legs_b.polygons);
        // the record ends exactly where the first brain overlay begins
        assert_eq!(body.end, a.header.creature_overlays[0] as usize);
        let mesh = body.pose(0, 128);
        assert_eq!((mesh.vertices.len(), mesh.faces.len()), (105, 78));
        for f in &mesh.faces {
            assert!(f.indices.iter().all(|&i| (i as usize) < mesh.vertices.len()));
            assert!((16..=19).contains(&f.texture));
            assert!(f.uv.iter().flatten().all(|v| v.is_finite()));
        }
        // the four skins exist in every section
        for i in 0..a.sections.len() {
            assert!(a.world(i).unwrap().textures.len() > 19);
        }
    }

    /// His materials and texel offsets (`0x426c50` → polygon +0x60, +0x5c,
    /// +0x5e). The head is the shell's polygons 0..=6 (texture 18, the skin
    /// with the glowing stripe); its four front faces 2..=5 are the only
    /// polygons on material 8 (brighter base) and 61 of 78 polygons carry
    /// an offset: the head's first is
    /// (134, −2974), i.e. (6, 98) texels into the 128-texel skin, which is
    /// what puts the stripe in view.
    #[test]
    fn body_materials_and_offsets() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let body = CreatureBody::parse(&d, a.header.creature as usize).unwrap();
        let ramps: Vec<u16> = [&body.shell, &body.legs_a, &body.legs_b].iter().flat_map(|p| p.attrs.iter().map(|a| a.color_ramp)).collect();
        assert!(ramps.iter().all(|&r| r == 4 || r == 8));
        assert_eq!(ramps.iter().filter(|&&r| r == 8).count(), 4);
        assert_eq!(body.shell.attrs[0].offset, [134, -2974]);
        assert_eq!(body.shell.attrs[0].texture + TEXTURE_BASE, 18);
        let with_offset = [&body.shell, &body.legs_a, &body.legs_b].iter().flat_map(|p| p.attrs.iter()).filter(|a| a.offset != [0, 0]).count();
        assert_eq!(with_offset, 61);
        // every section gives materials 4 and 8 the same values (m1 = 0:
        // never brighter than the plain texture)
        for i in 0..a.sections.len() {
            let r = a.world(i).unwrap().color_ramps;
            assert_eq!((r[4], r[8]), ([41574, 0, 65535, 65535, 0, 65535], [62873, 0, 65535, 65535, 0, 65535]));
        }
    }

    #[test]
    fn brains_match_retail_cd() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let ov = a.header.creature_overlays;
        for (i, &o) in ov.iter().enumerate() {
            let b = Brain::parse(&d, o as usize).unwrap();
            let next = if i + 1 < ov.len() { ov[i + 1] } else { a.header.creature_sounds };
            assert_eq!(next as usize - o as usize, BRAIN_LEN);
            assert_eq!(b.to_bytes(), &d[o as usize..next as usize], "writer is the inverse of the reader");
            for n in &b.nerms {
                assert_eq!((n.input_count, n.samples), (3, 200));
                assert_eq!(n.knots, [0, 32, 64, 96, 128, 160, 192, 224, 256]);
                assert_eq!(n.outputs, [0, 256, 0, 256, 0, 256, 0, 256, 0, 256]);
                for k in 0..256 {
                    assert_eq!(n.exp_table[k], 2f64.powf(k as f64 / 24.0) as i32);
                }
            }
            // posture and turning are trained on the same cost
            assert_eq!(b.nerms[0].fitness, b.nerms[2].fitness);
        }
        // the cached probabilities of brain 0 are reproduced from its genome
        let b = Brain::parse(&d, ov[0] as usize).unwrap();
        for n in &b.nerms {
            assert_eq!(n.transition_probs([32, 0, 0]), n.probs_cache);
        }
    }

    #[test]
    fn trailer_is_camera_profiles() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        for i in 0..a.sections.len() {
            let s = a.world(i).unwrap();
            let t = CameraProfiles::parse(&d[s.trailer.clone()]).unwrap();
            assert_eq!((t.unknown, t.profiles.len()), (1520, 33));
            assert_eq!(t.profiles[0], CameraProfile { damping_a: 55705, damping_b: 62259, c: 15, d: 300, mode: 0 });
        }
    }

    #[test]
    fn newborn_brain_is_new_and_round_trips() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let a = crate::worlds::Archive::parse(&data).unwrap();
        let template = Brain::parse(&data, a.header.creature_overlays[1] as usize).unwrap();
        let baby = Brain::newborn(&template, 7);
        assert_ne!(baby.nerms[0].genome, template.nerms[0].genome, "a new genome");
        assert_eq!(baby.nerms[0].genome.len(), template.nerms[0].genome.len());
        let bytes = baby.to_bytes();
        assert_eq!(bytes.len(), BRAIN_LEN, "exactly one .mnd");
        assert_eq!(Brain::parse(&bytes, 0).unwrap(), baby, "a .mnd round-trips");
    }
}
