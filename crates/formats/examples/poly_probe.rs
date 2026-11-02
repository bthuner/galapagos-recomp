//! Prints the polygons of objects (1-based ids) of a section in world
//! space: texture, flags of the initial appearance state, vertices.
//! `poly_probe <worlds> <section> <id>...`, or `... near x y z r`: every
//! polygon with a vertex within `r` of the point.
use galapagos_formats::worlds::{Archive, Object};
fn main() {
    let mut args = std::env::args().skip(1);
    let data = std::fs::read(args.next().unwrap()).unwrap();
    let sec: usize = args.next().unwrap().parse().unwrap();
    let a = Archive::parse(&data).unwrap();
    let s = a.world(sec).unwrap();
    let rest: Vec<String> = args.collect();
    if rest.first().is_some_and(|a| a == "near") {
        let q: Vec<f64> = rest[1..5].iter().map(|x| x.parse().unwrap()).collect();
        for (k, o) in s.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let Some(m) = &w.mesh else { continue };
            for (pi, p) in m.polygons.iter().enumerate() {
                let pts: Vec<[f64; 3]> = p.iter().map(|&i| {
                    let v = m.vertices[i as usize];
                    [0, 1, 2].map(|a| (v[a] as i32 + w.position[a]) as f64)
                }).collect();
                let lo: Vec<f64> = (0..3).map(|a| pts.iter().map(|v| v[a]).fold(f64::MAX, f64::min)).collect();
                let hi: Vec<f64> = (0..3).map(|a| pts.iter().map(|v| v[a]).fold(f64::MIN, f64::max)).collect();
                let d2: f64 = (0..3).map(|a| (q[a] - q[a].clamp(lo[a], hi[a])).powi(2)).sum();
                if d2.sqrt() <= q[3] {
                    let first = w.appearance.as_ref().and_then(|a| a.first()).and_then(|a| a.polygons.get(pi)).and_then(|f| f.first());
                    println!("#{} {} poly {pi}: tex {:?} flags {:x?} n={} box {lo:?}..{hi:?}", k + 1, String::from_utf8_lossy(&w.kind), first.map(|f| f.texture), first.map(|f| f.flags), p.len());
                }
            }
        }
        return;
    }
    for id in rest.iter().map(|x| x.parse::<usize>().unwrap()) {
        let Object::World(w) = &s.objects[id - 1] else { continue };
        let Some(m) = &w.mesh else { continue };
        println!("#{id} {} at {:?} folder {} state {}", String::from_utf8_lossy(&w.kind), w.position, w.folder, w.initial_state());
        let shape = w.logic.shape.as_ref().and_then(|s| s.states.first()).and_then(|s| s.frames.first());
        let verts = shape.map_or(&m.vertices, |f| &f.vertices);
        for (pi, p) in m.polygons.iter().enumerate() {
            let first = w.appearance.as_ref().and_then(|a| a.first()).and_then(|a| a.polygons.get(pi)).and_then(|f| f.first());
            let pts: Vec<[i32; 3]> = p.iter().map(|&i| {
                let v = verts[i as usize];
                [v[0] as i32 + w.position[0], v[1] as i32 + w.position[1], v[2] as i32 + w.position[2]]
            }).collect();
            println!("  poly {pi}: tex {:?} flags {:x?} idx {p:?}\n    {pts:?}", first.map(|f| f.texture), first.map(|f| f.flags));
        }
    }
}
