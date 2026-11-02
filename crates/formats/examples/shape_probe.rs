//! Shape-morph statistics: for every object with a mesh, does it carry a
//! `ShapeAnim`, is its state-0 frame 0 the mesh, is its texel mapping the
//! default (0,127) (0,0) (127,0), and does its initial state select one.
use galapagos_formats::logic::Actions;
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..a.sections.len() {
        let sec = a.world(s).unwrap();
        let (mut meshes, mut with, mut same_v, mut default_map, mut initial, mut initial_nondefault, mut rate_is_np, mut multi) = (0, 0, 0, 0, 0, 0, 0, 0);
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let Some(m) = &w.mesh else { continue };
            meshes += 1;
            let Some(sh) = &w.logic.shape else { continue };
            with += 1;
            let f0 = &sh.states[0].frames[0];
            same_v += (f0.vertices == m.vertices) as u32;
            let def = |f: &galapagos_formats::logic::ShapeFrame| f.polygon_mapping.iter().all(|p| *p == [0.0, 127.0, 0.0, 0.0, 127.0, 0.0]);
            default_map += def(f0) as u32;
            rate_is_np += sh.states.iter().all(|st| st.polygon_count as usize == m.polygons.len()) as u32;
            multi += sh.states.iter().any(|st| st.frames.len() > 1) as u32;
            if let Some(sm) = &w.logic.states {
                let a = sm.resolve(Actions { goto_state: Some(w.initial_state() as i16), ..Default::default() });
                if let Some(n) = a.shape {
                    initial += 1;
                    let st = &sh.states[(n - 1).max(0) as usize];
                    if !def(&st.frames[0]) || st.frames[0].vertices != m.vertices {
                        initial_nondefault += 1;
                        if s == 0 {
                            println!("  s0 #{} initial shape {n}: verts same {}, default map {}", i + 1, st.frames[0].vertices == m.vertices, def(&st.frames[0]));
                        }
                    }
                }
            }
        }
        println!("section {s}: meshes {meshes}, with shape {with}, f0==mesh {same_v}, f0 default map {default_map}, rate==np {rate_is_np}, multi-frame {multi}, initial selects {initial} (non-default {initial_nondefault})");
    }
}
