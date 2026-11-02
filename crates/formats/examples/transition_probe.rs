//! Which transition effects (`0x424520`: kind, variant) the CD uses:
//! lethal polygon flags (`0xf000` kind, `(f >> 16) & 7` variant), the
//! spawn portals' materialise effect (`Strt` kind 4: `ints[5]`, `ints[6]`,
//! `Game_SpawnCreature` @ 0x41c590) and the predators' `shorts[1..3]`.
//! Usage: transition_probe <worlds file>
use galapagos_formats::worlds::{Archive, Object};
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("worlds file");
    let data = std::fs::read(path).unwrap();
    let a = Archive::parse(&data).unwrap();
    let mut uses = BTreeMap::<(u32, u32, &str), Vec<String>>::new();
    for s in 0..8 {
        let Ok(sec) = a.world(s) else { continue };
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let kind = String::from_utf8_lossy(&w.kind).to_string();
            let id = format!("s{s} {kind}#{}", i + 1);
            for st in w.appearance.iter().flatten() {
                for p in &st.polygons {
                    for f in p {
                        if f.flags & 0xf000 != 0 {
                            let e = uses.entry(((f.flags >> 12) & 15, (f.flags >> 16) & 7, "lethal floor")).or_default();
                            if !e.contains(&id) {
                                e.push(id.clone());
                            }
                        }
                    }
                }
            }
            for p in &w.logic.params {
                match kind.as_str() {
                    "Strt" | "Tele" | "STel" if p.shorts[0] == 4 => {
                        uses.entry((p.ints[5] as u32, p.ints[6] as u32, "spawn (reversed)")).or_default().push(id.clone());
                    }
                    "Pred" | "PBal" | "PFrk" | "PMan" if p.shorts[1] != 0 => {
                        let what = if p.shorts[0] == 2 { "predator appears (reversed)" } else { "predator vanishes" };
                        uses.entry((p.shorts[2] as u32, p.shorts[3] as u32, what)).or_default().push(id.clone());
                    }
                    _ => {}
                }
            }
        }
    }
    for ((k, v, what), ids) in uses {
        println!("kind {k} variant {v} {what}: {}", ids.join(", "));
    }
}
