//! Lists the predators (`Pred`, `PBal`, `PFrk`, `PMan`, `PPol`) of every
//! section with their trigger params and polygon flags, and every polygon
//! frame whose flags start an affliction (bits 25..28) or kill (0xf000).
//! Usage: hazard_probe <worlds file>
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let path = std::env::args().nth(1).expect("worlds file");
    let data = std::fs::read(path).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..8 {
        let Ok(sec) = a.world(s) else { continue };
        // Which world-bank slots 30..60 hold a sample (1 = present).
        let samples = sec.sounds.samples(&data, 4).unwrap_or_default();
        let map: String = (30..60).map(|i| if samples.get(i).is_some_and(|x| x.is_some()) { '1' } else { '.' }).collect();
        println!("s{s} sound slots 30..59 (0-based): {map} ({} sounds)", samples.len());
        let mut aff = std::collections::BTreeMap::<u32, Vec<String>>::new();
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let kind = String::from_utf8_lossy(&w.kind).to_string();
            let mut flags = std::collections::BTreeSet::new();
            for st in w.appearance.iter().flatten() {
                for p in &st.polygons {
                    for f in p {
                        flags.insert(f.flags);
                    }
                }
            }
            for &f in &flags {
                let n = (f >> 25) & 15;
                if n != 0 || f & 0xf000 != 0 {
                    aff.entry(f).or_default().push(format!("{kind}#{}", i + 1));
                }
            }
            if !["Pred", "PBal", "PFrk", "PMan", "PPol"].contains(&kind.as_str()) {
                continue;
            }
            let np = w.mesh.as_ref().map_or(0, |m| m.polygons.len());
            let nv = w.mesh.as_ref().map_or(0, |m| m.vertices.len());
            println!(
                "s{s} #{} {kind} folder {} at {:?} init state {} mesh {np}p/{nv}v motions {} sounds {} shapes {}",
                i + 1,
                w.folder,
                w.position,
                w.initial_state(),
                w.logic.motions.len(),
                w.logic.sounds.len(),
                w.logic.shape.as_ref().map_or(0, |s| s.states.len())
            );
            let fl: Vec<String> = flags.iter().map(|f| format!("{f:#x}")).collect();
            println!("   flags {}", fl.join(" "));
            for (k, p) in w.logic.params.iter().enumerate() {
                println!("   param {}: ints {:?} shorts {:?} ints2 {:?} shorts2 {:?}", k + 1, p.ints, p.shorts, p.ints2, p.shorts2);
            }
            if let Some(sm) = &w.logic.states {
                for (k, st) in sm.states.iter().enumerate() {
                    println!("   state {k}: entry {:?}", st.entry);
                    for t in &st.transitions {
                        println!("      -> {:?} if {:?}", t.actions, t.groups);
                    }
                }
            }
        }
        for (f, objs) in aff {
            let mut o = objs.clone();
            o.dedup();
            println!(
                "s{s} flags {f:#010x}: affliction {} lethal kind {} var {} after {} delayed {} pain {} -> {}",
                (f >> 25) & 15,
                (f >> 12) & 15,
                (f >> 16) & 7,
                (f >> 19) & 7,
                f & 0x80_0000 != 0,
                f & 0x100_0000 != 0,
                o.join(",")
            );
        }
    }
}
