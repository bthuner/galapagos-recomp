//! Dumps one object's appearance states (texture, enabled, flags per
//! polygon frame) and its state machine's entry appearances.
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let mut args = std::env::args().skip(1);
    let data = std::fs::read(args.next().unwrap()).unwrap();
    let section: usize = args.next().unwrap().parse().unwrap();
    let id: usize = args.next().unwrap().parse().unwrap();
    let a = Archive::parse(&data).unwrap();
    let sec = a.world(section).unwrap();
    let Object::World(w) = &sec.objects[id - 1] else { return };
    println!("#{id} {} at {:?}, initial state {}", String::from_utf8_lossy(&w.kind), w.position, w.initial_state());
    if let Some(m) = &w.mesh {
        println!("{} polygons, {} vertices", m.polygons.len(), m.vertices.len());
    }
    for (s, st) in w.appearance.iter().flatten().enumerate() {
        println!("appearance {s} (looping {}):", st.looping);
        for (p, frames) in st.polygons.iter().enumerate() {
            let f: Vec<String> = frames.iter().map(|f| format!("tex {} en {} flags {:#x} uv {},{}", f.texture, f.enabled, f.flags, f.u, f.v)).collect();
            println!("  poly {p}: {}", f.join(" | "));
        }
    }
    if let (Some(sh), Some(m)) = (&w.logic.shape, &w.mesh) {
        for (k, st) in sh.states.iter().enumerate() {
            println!("shape state {k}: rate {}, {} frames", st.rate, st.frames.len());
            for (f, fr) in st.frames.iter().enumerate().step_by((st.frames.len() / 4).max(1)) {
                // z range of each polygon in this frame, vs the rest mesh
                let zs: Vec<String> = m.polygons.iter().map(|poly| {
                    let z: Vec<i16> = poly.iter().map(|&i| fr.vertices[i as usize][2]).collect();
                    format!("{}..{}", z.iter().min().unwrap(), z.iter().max().unwrap())
                }).collect();
                println!("  frame {f}: polygon z ranges {:?}; mapping[1] {:?}", zs, fr.polygon_mapping.get(1));
            }
        }
        let zs: Vec<String> = m.polygons.iter().map(|poly| {
            let z: Vec<i16> = poly.iter().map(|&i| m.vertices[i as usize][2]).collect();
            format!("{}..{}", z.iter().min().unwrap(), z.iter().max().unwrap())
        }).collect();
        println!("rest mesh polygon z ranges {:?}", zs);
    }
    if let Some(sm) = &w.logic.states {
        for (i, st) in sm.states.iter().enumerate() {
            println!("state {i}: entry {:?}, {} transitions", st.entry, st.transitions.len());
        }
    }
}
