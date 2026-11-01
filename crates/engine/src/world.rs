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
//! * There are no texture coordinates on disk. The engine stretches each
//!   128×128 texture across the polygon, using its first edge (v0→v1) and
//!   last edge (v0→vn) as the texture axes. We do the same, which is exact
//!   for the quads that make up most of a world.
//! * `layer` indexes a TEXTURE_2D_ARRAY; -1 means "untextured".
//! * `light` is a flat Lambert term from the section's light direction,
//!   the job the 32×32×256 shade table did in software.

use galapagos_formats::worlds::{Object, Section, WorldObject};

pub const FLOATS_PER_VERTEX: usize = 7;
const TEX: f32 = 128.0;

pub struct WorldMesh {
    pub vertices: Vec<f32>,
    /// `textures.len() / (128*128*4)` RGBA layers.
    pub textures: Vec<u8>,
    pub texture_count: u32,
    /// min x, y, z, max x, y, z.
    pub bounds: [f32; 6],
    pub objects_drawn: u32,
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
    pub fn build(section: &Section) -> Self {
        let mut vertices = Vec::new();
        let mut bounds = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
        let light_dir = {
            let l = section.environment.light_direction;
            normalize([l[0] as f32, l[1] as f32, l[2] as f32])
        };
        let mut objects_drawn = 0;

        for object in &section.objects {
            let Object::World(obj) = object else { continue };
            if obj.mesh.is_some() {
                objects_drawn += 1;
                emit_object(obj, light_dir, &mut vertices, &mut bounds);
            }
        }

        let texture_count = section.textures.len() as u32;
        let mut textures = Vec::with_capacity(section.textures.len() * 128 * 128 * 4);
        for i in 0..section.textures.len() {
            for rgb in section.texture_rgb(i).chunks_exact(3) {
                textures.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        WorldMesh { vertices, textures, texture_count, bounds, objects_drawn }
    }
}

fn emit_object(obj: &WorldObject, light_dir: V3, out: &mut Vec<f32>, bounds: &mut [f32; 6]) {
    let mesh = obj.mesh.as_ref().unwrap();
    let origin = obj.position.map(|c| c as f32);
    let world: Vec<V3> = mesh
        .vertices
        .iter()
        .map(|v| [origin[0] + v[0] as f32, origin[1] + v[1] as f32, origin[2] + v[2] as f32])
        .collect();

    for (pi, poly) in mesh.polygons.iter().enumerate() {
        let pts: Vec<V3> = poly.iter().filter_map(|&i| world.get(i as usize).copied()).collect();
        if pts.len() < 3 {
            continue;
        }
        let frame = obj
            .appearance
            .as_ref()
            .and_then(|states| states.first())
            .and_then(|s| s.polygons.get(pi))
            .and_then(|frames| frames.first());
        // Invisible in the initial state: skip (e.g. hidden until triggered).
        if frame.is_some_and(|f| f.enabled == 0) {
            continue;
        }
        let layer = frame.map_or(-1, |f| f.texture);
        let layer = if layer >= 0 { layer as f32 } else { -1.0 };
        let (du, dv) = frame.map_or((0.0, 0.0), |f| (f.u as f32 / TEX, f.v as f32 / TEX));

        let e1 = sub(pts[1], pts[0]);
        let e2 = sub(pts[pts.len() - 1], pts[0]);
        let normal = normalize(cross(e1, e2));
        // Two-sided: polygons may face either way, as in a software renderer
        // without back-face culling.
        let light = 0.45 + 0.55 * dot(normal, light_dir).abs();

        let uv: Vec<(f32, f32)> = pts.iter().map(|&p| affine_st(sub(p, pts[0]), e1, e2)).collect();
        for k in 1..pts.len() - 1 {
            for &i in &[0, k, k + 1] {
                let p = pts[i];
                for a in 0..3 {
                    bounds[a] = bounds[a].min(p[a]);
                    bounds[a + 3] = bounds[a + 3].max(p[a]);
                }
                out.extend_from_slice(&[p[0], p[1], p[2], uv[i].0 + du, uv[i].1 + dv, layer, light]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::worlds::Archive;

    #[test]
    fn affine_maps_a_quad_to_unit_square() {
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
}
