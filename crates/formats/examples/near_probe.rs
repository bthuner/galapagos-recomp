//! Lists the objects of a section near a given object: kind, position,
//! mesh size, shape-morph states, initial state and first textures.
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let mut args = std::env::args().skip(1);
    let data = std::fs::read(args.next().unwrap()).unwrap();
    let section: usize = args.next().unwrap().parse().unwrap();
    let id: usize = args.next().unwrap().parse().unwrap();
    let radius: f64 = args.next().unwrap().parse().unwrap();
    let a = Archive::parse(&data).unwrap();
    let sec = a.world(section).unwrap();
    let Object::World(c) = &sec.objects[id - 1] else { return };
    let c = c.position;
    for (i, o) in sec.objects.iter().enumerate() {
        let Object::World(w) = o else { continue };
        let d = (0..3).map(|k| (w.position[k] - c[k]) as f64).map(|x| x * x).sum::<f64>().sqrt();
        if d > radius {
            continue;
        }
        let (np, nv, zr) = w.mesh.as_ref().map_or((0, 0, (0, 0)), |m| {
            let z = m.vertices.iter().map(|v| v[2]);
            (m.polygons.len(), m.vertices.len(), (z.clone().min().unwrap_or(0), z.max().unwrap_or(0)))
        });
        let shape: Vec<String> = w.logic.shape.iter().flat_map(|s| s.states.iter()).map(|s| format!("{}f/r{}", s.frames.len(), s.polygon_count)).collect();
        let tex: Vec<i16> = (0..np.min(6)).filter_map(|p| w.polygon_texture(p)).collect();
        println!(
            "#{} {} folder {} at {:?} d {:.0} polys {} verts {} z {:?} init {} shape {:?} tex {:?} motions {}",
            i + 1,
            String::from_utf8_lossy(&w.kind),
            w.folder,
            w.position,
            d,
            np,
            nv,
            zr,
            w.initial_state(),
            shape,
            tex,
            w.logic.motions.len()
        );
    }
}
