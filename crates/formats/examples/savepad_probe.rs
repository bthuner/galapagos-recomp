//! Lists the objects of a section whose appearance frames carry the
//! save-pad flag (`0x20`), with their state count and initial state.
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let mut args = std::env::args().skip(1);
    let data = std::fs::read(args.next().unwrap()).unwrap();
    let section: usize = args.next().unwrap().parse().unwrap();
    let a = Archive::parse(&data).unwrap();
    let sec = a.world(section).unwrap();
    let arg = args.next();
    if arg.as_deref() == Some("folders") {
        // Every folder: parent, initial state, and what its trigger entries
        // do (shorts[0] 1 = enable, 0 = disable; shorts[1] descendants).
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            if &w.kind != b"Fldr" {
                continue;
            }
            let p: Vec<(i16, i16)> = w.logic.params.iter().map(|p| (p.shorts[0], p.shorts[1])).collect();
            let trig: Vec<String> = w.logic.states.iter().flat_map(|sm| sm.states.iter().enumerate()).map(|(k, s)| format!("s{k}:{:?}", s.entry.trigger)).collect();
            println!("#{} parent {} init {} params {:?} entries {:?}", i + 1, w.folder, w.initial_state(), p, trig);
        }
        return;
    }
    if let Some(id) = arg {
        // Geometry of one object: each polygon's vertices and normal
        // (v2 - v1) x (v0 - v1), the exe's 0x4019a0.
        let id: usize = id.parse().unwrap();
        let Object::World(w) = &sec.objects[id - 1] else { return };
        let m = w.mesh.as_ref().unwrap();
        let tex = w.logic.shape.as_ref().map(|s| s.states[0].frames[0].polygon_mapping.clone());
        for (k, poly) in m.polygons.iter().enumerate() {
            let p: Vec<[f32; 3]> = poly.iter().map(|&i| m.vertices[i as usize].map(|c| c as f32)).collect();
            let a = [p[2][0] - p[1][0], p[2][1] - p[1][1], p[2][2] - p[1][2]];
            let b = [p[0][0] - p[1][0], p[0][1] - p[1][1], p[0][2] - p[1][2]];
            let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            println!("poly {k}: n {:?} verts {:?} texels {:?}", n.map(|c| (c / l * 100.).round() / 100.), p, tex.as_ref().map(|t| t[k]));
        }
        return;
    }
    for (i, o) in sec.objects.iter().enumerate() {
        let Object::World(w) = o else { continue };
        let Some(app) = &w.appearance else { continue };
        let hit = app.iter().flat_map(|s| s.polygons.iter().flatten()).any(|f| f.flags & 0x20 != 0);
        if hit {
            println!(
                "#{} {} folder {} pos {:?} init {} states {}",
                i + 1,
                String::from_utf8_lossy(&w.kind),
                w.folder,
                w.position,
                w.initial_state(),
                app.len()
            );
            // Polygons of other objects within 200 units of the pad's plane
            // whose bounding box covers the pad's centre: the visible floor.
            let [px, py, pz] = w.position;
            for (j, o2) in sec.objects.iter().enumerate() {
                let Object::World(w2) = o2 else { continue };
                let Some(m) = &w2.mesh else { continue };
                for (k, poly) in m.polygons.iter().enumerate() {
                    let pts: Vec<[i32; 3]> = poly.iter().map(|&i| {
                        let v = m.vertices[i as usize];
                        [w2.position[0] + v[0] as i32, w2.position[1] + v[1] as i32, w2.position[2] + v[2] as i32]
                    }).collect();
                    let (x0, x1) = (pts.iter().map(|p| p[0]).min().unwrap(), pts.iter().map(|p| p[0]).max().unwrap());
                    let (y0, y1) = (pts.iter().map(|p| p[1]).min().unwrap(), pts.iter().map(|p| p[1]).max().unwrap());
                    let (z0, z1) = (pts.iter().map(|p| p[2]).min().unwrap(), pts.iter().map(|p| p[2]).max().unwrap());
                    if x0 <= px && px <= x1 && y0 <= py && py <= y1 && (z0 - pz).abs() < 200 && (z1 - pz).abs() < 200 {
                        let tex: Vec<String> = w2.appearance.iter().flatten().map(|st| {
                            st.polygons.get(k).map(|f| f.iter().map(|f| format!("{}@{},{}", f.texture, f.u, f.v)).collect::<Vec<_>>().join(" ")).unwrap_or_default()
                        }).collect();
                        println!("   under/over: #{} poly {k} z {z0}..{z1} frames {:?}", j + 1, tex);
                    }
                }
            }
        }
    }
}
