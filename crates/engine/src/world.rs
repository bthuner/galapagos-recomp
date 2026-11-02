//! Turning a parsed world section into buffers a GPU can draw.
//!
//! The 1997 engine rasterised polygons itself, in 256 colours. We hand the
//! same geometry to WebGL2 instead, so this module flattens the object graph
//! into one interleaved vertex array and one stack of textures:
//!
//! ```text
//! vertex = x, y, z, u, v, layer, light     (7 × f32)
//! ```
//!
//! * Polygons on disk are convex n-gons; we fan them into triangles.
//! * There are no texture coordinates on disk. The engine's polygon
//!   initialiser (0x42320d) gives the first three vertices the texel
//!   coordinates (0,127), (0,0), (127,0): v1 is the origin, u runs along
//!   v1→v2 and v runs along v1→v0. That defines an affine map over the
//!   polygon's plane; a quad's fourth vertex lands on (127,127).
//! * `layer` indexes a TEXTURE_2D_ARRAY; -1 means "untextured" (no
//!   appearance data) and -2 "not drawn in the current appearance state"
//!   (a polygon whose frames never gave it a texture: invisible collision
//!   geometry), kept in the buffer so a state change can reveal it by
//!   rewriting one float per vertex. -4 marks *glass* (a `-4` frame, see
//!   [`GLASS`]): drawn untextured and additively, after everything else.
//! * Vertices are *object-local*. Each object carries a 4×4 transform, so
//!   the simulation can move a platform by changing 16 floats instead of
//!   rebuilding geometry. The renderer draws one range per object.
//! * `light` is a flat Lambert term from the section's light direction.
//!   Distance darkening (the other half of the 32×32×256 shade table's job)
//!   happens in the fragment shader, using the section's far-clip distance.

use galapagos_formats::worlds::{Object, PolyFrame, Section, WorldObject};

pub const FLOATS_PER_VERTEX: usize = 7;
const TEX: f32 = 128.0;

/// The slice of the vertex array belonging to one object.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DrawRange {
    pub first: u32,
    pub count: u32,
}

/// The `layer` of a glass polygon. A `-4` frame sets the polygon's glass
/// flag (`0x423791`: flag = texture == -4, cleared by any other frame;
/// stored by `0x4023b0` at polygon `+0x62`). The Direct3D renderer draws
/// those polygons in a bucket of their own (bucket `0x27` of the draw loop
/// `0x40f000`) with depth writes off, alpha blending on, no texture and no
/// culling; the blend factors set at start-up (`0x40a640`) are `ONE, ONE`,
/// so glass *adds* a grey of half the polygon's light (`0x40c330`) to
/// whatever is behind it. The software renderer (`0x404660`) gets the same
/// look its own way: glass never hides a span, it raises the light level
/// of the surface behind it. The intro capsule (#3 of section 0) is glass.
///
/// How bright the veil is depends on the polygon's *material* (the frame's
/// shade number, see [`glass_grey`]); glass needs no texture coordinates,
/// so the vertex `u` carries that grey to the shader.
pub const GLASS: f32 = -4.0;

/// The grey a glass polygon adds, 0..1, for one material (a section
/// "colour ramp", read as six `u16 / 65535` by `0x42ece0`; `m[0]` base
/// light, `m[1]` over-brightening, `m[2]` ambient factor, `m[3]`
/// directional factor, `m[5]` fog switch).
///
/// * Light level (`0x401b70`): `t = min((m0 + m2·ambient − directional) / 2,
///   0.5) + m1`, at most 1, then `level = trunc(t · 31)` (32 rows,
///   `pow(2, 5)` at `0x40346f`). The ambient is `trunc(u16 / 65535)`
///   (`0x42e940`), so 0 for every section on the CD, and the glass
///   materials (17–19, 49–61 in section 0) have no directional term.
/// * Direct3D colour (`0x40c3e2`): below the middle row the grey is
///   `level / 16 · 255`, then halved for glass, without the brightness
///   preference's highlight.
/// * The draw loop skips glass whose level is 0 (`0x40f000`): the docked
///   capsule (material 49, base 0) shows no glass at all; on its ride
///   (states 2–4, materials 17–19) it turns into the grey veil of the
///   footage.
pub fn glass_grey(m: [u16; 6]) -> f32 {
    let f = m.map(|x| x as f32 / 65535.0);
    let t = ((f[0] / 2.0).min(0.5) + f[1]).min(1.0);
    let level = (t * 31.0) as i32;
    if level <= 0 {
        return 0.0;
    }
    (level as f32 / 16.0).min(1.0) / 2.0
}

pub struct WorldMesh {
    pub vertices: Vec<f32>,
    /// One range per drawn object, in section object order.
    pub draws: Vec<DrawRange>,
    /// For each section object (by list index): its draw index, if it has a mesh.
    pub draw_of_object: Vec<Option<usize>>,
    /// For each draw: the vertex range of every mesh polygon (count 0 if the
    /// polygon was degenerate), so appearance changes can patch in place.
    pub poly_ranges: Vec<Vec<DrawRange>>,
    /// For each draw and polygon: is its current frame's texture -5? One
    /// such polygon hides the whole object (see `apply_frame`).
    hides_object: Vec<Vec<bool>>,
    /// For each draw: hidden by its class (a predator blinking through
    /// its materialise effect, predator.rs), whatever its frames say.
    blinking: Vec<bool>,
    /// One byte per draw: 1 = drawn, 0 = hidden by a -5 frame.
    pub visible: Vec<u8>,
    /// For each draw and polygon: its current material (the last frame
    /// shade number `>= 0`; `0x402450` stores it at polygon `+0x60`).
    material: Vec<Vec<i16>>,
    /// [`glass_grey`] of every section material.
    glass_greys: Vec<f32>,
    /// Texture coordinates before any per-frame offset, 2 per vertex.
    pub base_uv: Vec<f32>,
    /// One column-major 4×4 matrix per drawn object (16 f32 each).
    pub transforms: Vec<f32>,
    /// `textures.len() / (128*128*4)` RGBA layers.
    pub textures: Vec<u8>,
    pub texture_count: u32,
    /// min x, y, z, max x, y, z.
    pub bounds: [f32; 6],
    pub objects_drawn: u32,
    /// Section environment far clip, in world units (drives depth cueing).
    pub far_clip: f32,
}

type V3 = [f32; 3];
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l == 0.0 { a } else { [a[0] / l, a[1] / l, a[2] / l] }
}

/// Coordinates (s, t) of `p` in the basis (e1, e2) anchored at the
/// polygon's first vertex, solved by least squares (2×2 normal equations).
fn affine_st(p: V3, e1: V3, e2: V3) -> (f32, f32) {
    let (a, b, c) = (dot(e1, e1), dot(e1, e2), dot(e2, e2));
    let (d, e) = (dot(p, e1), dot(p, e2));
    let det = a * c - b * b;
    if det.abs() < 1e-6 {
        return (0.0, 0.0);
    }
    ((d * c - e * b) / det, (e * a - d * b) / det)
}

impl WorldMesh {
    fn refresh_visible(&mut self, draw: usize) {
        self.visible[draw] = !(self.blinking[draw] || self.hides_object[draw].iter().any(|&x| x)) as u8;
    }

    /// Hide (or show again) a whole object for its class's own reasons.
    pub fn set_blinking(&mut self, draw: usize, hidden: bool) {
        if let Some(b) = self.blinking.get_mut(draw) {
            *b = hidden;
            self.refresh_visible(draw);
        }
    }

    pub fn build(section: &Section) -> Self {
        let mut vertices = Vec::new();
        let mut draws = Vec::new();
        let mut transforms = Vec::new();
        let mut draw_of_object = Vec::new();
        let mut poly_ranges = Vec::new();
        let mut bounds = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
        let light_dir = {
            let l = section.environment.light_direction;
            normalize([l[0] as f32, l[1] as f32, l[2] as f32])
        };
        let mut objects_drawn = 0;

        for object in &section.objects {
            let Object::World(obj) = object else {
                draw_of_object.push(None);
                continue;
            };
            draw_of_object.push(obj.mesh.as_ref().map(|_| draws.len()));
            if obj.mesh.is_some() {
                objects_drawn += 1;
                let first = (vertices.len() / FLOATS_PER_VERTEX) as u32;
                poly_ranges.push(emit_object(obj, light_dir, &mut vertices, &mut bounds));
                let count = (vertices.len() / FLOATS_PER_VERTEX) as u32 - first;
                draws.push(DrawRange { first, count });
                let [x, y, z] = obj.position.map(|c| c as f32);
                transforms.extend_from_slice(&[1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.]);
            }
        }

        let texture_count = section.textures.len() as u32;
        let mut textures = Vec::with_capacity(section.textures.len() * 128 * 128 * 4);
        for i in 0..section.textures.len() {
            for rgb in section.texture_rgb(i).chunks_exact(3) {
                textures.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let far_clip = section.environment.far_clip as f32;
        // emit_object wrote un-offset UVs; keep a copy so frame offsets can
        // be re-applied whenever a polygon's appearance frame changes.
        let base_uv = vertices.chunks_exact(FLOATS_PER_VERTEX).flat_map(|v| [v[3], v[4]]).collect();
        let hides_object = poly_ranges.iter().map(|p| vec![false; p.len()]).collect();
        let visible = vec![1; draws.len()];
        let blinking = vec![false; draws.len()];
        let material = poly_ranges.iter().map(|p| vec![0; p.len()]).collect();
        let glass_greys = section.color_ramps.iter().map(|&m| glass_grey(m)).collect();
        let mut mesh = WorldMesh {
            vertices, draws, draw_of_object, poly_ranges, hides_object, blinking, visible, material, glass_greys,
            base_uv, transforms, textures, texture_count, bounds, objects_drawn, far_clip,
        };
        // Apply each polygon's initial appearance frame (layer, UV offset).
        for object in &section.objects {
            let Object::World(obj) = object else { continue };
            let Some(states) = &obj.appearance else { continue };
            let Some(state) = states.first() else { continue };
            let draw = mesh.draw_of_object[obj_index(section, obj)].unwrap_or(usize::MAX);
            if draw == usize::MAX {
                continue;
            }
            for (p, frames) in state.polygons.iter().enumerate() {
                mesh.apply_frame(draw, p, frames, 0);
            }
        }
        mesh
    }

    /// Writes a polygon's appearance frame into the vertex buffer: texture
    /// layer (or hidden / untextured) and the texel offset that makes
    /// surfaces scroll. Returns the patched vertex range.
    pub fn apply_frame(&mut self, draw: usize, poly: usize, frames: &[PolyFrame], index: usize) -> Option<DrawRange> {
        let r = *self.poly_ranges.get(draw)?.get(poly)?;
        let frame = frames.get(index).or(frames.first());
        // What a frame's texture number does, exactly as the exe's frame
        // applier (0x423730) reads it:
        //   >= 0   use that texture;
        //   -5     hide the *whole object* (a flag passed to 0x4022e0 after
        //          the polygon loop): start pad #57 and the birth tube #54
        //          carry it, so their stacks are never drawn;
        //   -4     glass (see `GLASS`): the texture is left alone, the
        //          polygon is drawn as an additive grey veil;
        //   -2/-3  change nothing: the polygon keeps its previous texture.
        // On the CD, -2/-3 frames sit inside flip-books (e.g. pad #83:
        // 9, -3, -3), so "previous" is the last earlier frame of the same
        // list with a real texture. inferred: resolving it from the list
        // rather than from render history keeps snapshots reproducible.
        // A list with none (section 1's pad #163, states 6 and 7: only
        // -2/-3 frames) keeps the texture the polygon shows now, as the
        // exe does; only a polygon that never had one stays hidden.
        //
        // The frame's `enabled` byte plays no part here: 0x423730 and the
        // flip-book step 0x423810 set texture, shade and texel offset of
        // every frame, and only the flag copies (0x42df50, 0x42e050) test
        // `enabled > 0`. Flip-books are mostly one enabled frame followed
        // by disabled `-2`/`-3` frames that just slide the offset (pad #49
        // of section 1: `(8, e1), (-3, e0, +4/+4) × 31`), so reading
        // "disabled" as "hidden" blanked them 31 ticks out of 32.
        let previous = frames[..index.min(frames.len())].iter().rev().find(|f| f.texture >= 0).map(|f| f.texture as f32);
        let layer = match frame {
            Some(f) if f.texture >= 0 => f.texture as f32,
            Some(f) if f.texture == -4 => GLASS,
            Some(_) => previous.unwrap_or_else(|| {
                let now = self.vertices.get(r.first as usize * FLOATS_PER_VERTEX + 5).copied().unwrap_or(-2.0);
                if r.count > 0 && now >= 0.0 { now } else { -2.0 }
            }),
            None => -1.0,
        };
        if let Some(h) = self.hides_object.get_mut(draw).and_then(|h| h.get_mut(poly)) {
            *h = frame.is_some_and(|f| f.texture == -5);
            self.refresh_visible(draw);
        }
        if let (Some(f), Some(m)) = (frame, self.material.get_mut(draw).and_then(|m| m.get_mut(poly))) {
            if f.shade >= 0 {
                *m = f.shade;
            }
        }
        let grey = self.material[draw][poly];
        let grey = self.glass_greys.get(grey as usize).copied().unwrap_or(0.0);
        let (du, dv) = frame.map_or((0.0, 0.0), |f| (f.u as f32 / TEX, f.v as f32 / TEX));
        for v in r.first..r.first + r.count {
            let i = v as usize;
            let o = i * FLOATS_PER_VERTEX;
            if layer == GLASS {
                // Glass is untextured: its u carries the grey it adds.
                self.vertices[o + 3] = grey;
                self.vertices[o + 4] = 0.0;
            } else {
                self.vertices[o + 3] = self.base_uv[i * 2] + du;
                self.vertices[o + 4] = self.base_uv[i * 2 + 1] + dv;
            }
            self.vertices[o + 5] = layer;
        }
        Some(r)
    }
}

fn obj_index(section: &Section, obj: &WorldObject) -> usize {
    section
        .objects
        .iter()
        .position(|o| matches!(o, Object::World(w) if core::ptr::eq(w, obj)))
        .unwrap()
}

fn emit_object(obj: &WorldObject, light_dir: V3, out: &mut Vec<f32>, bounds: &mut [f32; 6]) -> Vec<DrawRange> {
    let mut ranges = Vec::new();
    let mesh = obj.mesh.as_ref().unwrap();
    let origin = obj.position.map(|c| c as f32);
    let local: Vec<V3> = mesh.vertices.iter().map(|v| v.map(|c| c as f32)).collect();

    for poly in &mesh.polygons {
        let pts: Vec<V3> = poly.iter().filter_map(|&i| local.get(i as usize).copied()).collect();
        let first = (out.len() / FLOATS_PER_VERTEX) as u32;
        if pts.len() < 3 {
            ranges.push(DrawRange { first, count: 0 });
            continue;
        }
        // Layer and texel offset come from the appearance frame and are
        // written afterwards by `WorldMesh::apply_frame`.
        // Texel axes from the first three vertices, as the original does.
        let eu = sub(pts[2], pts[1]);
        let ev = sub(pts[0], pts[1]);
        let normal = normalize(cross(eu, ev));
        // Two-sided (no back-face culling in the original). The footage shows
        // surfaces near full brightness up close, so the Lambert term is gentle.
        let light = 0.7 + 0.3 * dot(normal, light_dir).abs();

        const EDGE: f32 = 127.0 / TEX; // (0,127) is the last texel, not 128
        let uv: Vec<(f32, f32)> = pts
            .iter()
            .map(|&p| {
                let (s, t) = affine_st(sub(p, pts[1]), eu, ev);
                (s * EDGE, t * EDGE)
            })
            .collect();
        for k in 1..pts.len() - 1 {
            for &i in &[0, k, k + 1] {
                let p = pts[i];
                for a in 0..3 {
                    bounds[a] = bounds[a].min(origin[a] + p[a]);
                    bounds[a + 3] = bounds[a + 3].max(origin[a] + p[a]);
                }
                out.extend_from_slice(&[p[0], p[1], p[2], uv[i].0, uv[i].1, -1.0, light]);
            }
        }
        let count = (out.len() / FLOATS_PER_VERTEX) as u32 - first;
        ranges.push(DrawRange { first, count });
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::worlds::Archive;

    #[test]
    fn affine_maps_a_quad_to_unit_square() {
        // eu, ev span the polygon plane; the opposite corner is (1, 1).
        let (e1, e2) = ([10.0, 0.0, 0.0], [0.0, 0.0, 10.0]);
        assert_eq!(affine_st([10.0, 0.0, 10.0], e1, e2), (1.0, 1.0));
        assert_eq!(affine_st([0.0, 0.0, 10.0], e1, e2), (0.0, 1.0));
    }

    /// Prints per-section stats; run with `-- --nocapture` to see them.
    #[test]
    fn builds_every_section_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        for i in 0..archive.sections.len() {
            let m = WorldMesh::build(&archive.world(i).unwrap());
            let tris = m.vertices.len() / FLOATS_PER_VERTEX / 3;
            assert!(tris > 0, "section {i} has no geometry");
            println!(
                "section {i}: {} objects, {tris} triangles, {} textures, bounds {:?}",
                m.objects_drawn, m.texture_count, m.bounds
            );
        }
    }

    /// `-4` frames are glass (0x423791): the intro capsule #3 of section 0
    /// has twelve glass polygons (20..=31), drawn, not hidden.
    #[test]
    fn minus_four_is_glass_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        let m = WorldMesh::build(&archive.world(0).unwrap());
        let d = m.draw_of_object[3 - 1].unwrap();
        assert_eq!(m.visible[d], 1, "the capsule is drawn");
        let layer = |p: usize| m.vertices[m.poly_ranges[d][p].first as usize * FLOATS_PER_VERTEX + 5];
        for p in 20..=31 {
            assert_eq!(layer(p), GLASS, "capsule polygon {p} is glass");
        }
        assert!(layer(0) >= 0.0, "the capsule's frame stays textured");
        // Docked (material 49, base 0) the glass adds nothing; on the ride
        // (materials 17..=19) it is a grey veil.
        let u = |p: usize| m.vertices[m.poly_ranges[d][p].first as usize * FLOATS_PER_VERTEX + 3];
        assert_eq!(u(20), 0.0);
        let s = archive.world(0).unwrap();
        let ride: Vec<f32> = (17..=19).map(|i| glass_grey(s.color_ramps[i])).collect();
        assert!(ride.iter().all(|&g| g > 0.1 && g < 0.25), "ride glass greys {ride:?}");
    }

    /// A `-5` frame hides the whole object (0x423730): section 0's start pad
    /// #57 and birth tube #54 are never drawn, though Mendel stands in them.
    #[test]
    fn minus_five_hides_whole_objects_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        for s in 0..archive.sections.len() {
            let m = WorldMesh::build(&archive.world(s).unwrap());
            let hidden = m.visible.iter().filter(|&&v| v == 0).count();
            println!("section {s}: {hidden} of {} objects hidden by -5 frames", m.visible.len());
            if s == 0 {
                for id in [57usize, 54] {
                    let d = m.draw_of_object[id - 1].unwrap();
                    assert_eq!(m.visible[d], 0, "object #{id} must be hidden");
                }
            }
        }
    }
}
