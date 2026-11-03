//! Lists every `Over` (2-D overlay) and `Rend` object of every section:
//! its parameter entries (the `shorts` its class trigger reads) and its
//! state machine, so we can see which state shows which picture and when.
//!
//! cargo run --release -p galapagos-formats --example overlay_probe -- <worlds>
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let path = std::env::args().nth(1).expect("path to data/worlds");
    let data = std::fs::read(path).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..a.header.section_offsets.len() {
        let Ok(sec) = a.world(s) else { continue };
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            if &w.kind != b"Over" && &w.kind != b"Rend" {
                continue;
            }
            println!("section {s} #{} {} folder {} init {}", i + 1, String::from_utf8_lossy(&w.kind), w.folder, w.initial_state());
            for (k, p) in w.logic.params.iter().enumerate() {
                println!("  param {}: shorts {:?} ints {:?}", k + 1, p.shorts, p.ints);
            }
            if let Some(sm) = &w.logic.states {
                for (k, st) in sm.states.iter().enumerate() {
                    println!("  state {k}: entry {:?}", st.entry);
                    for t in &st.transitions {
                        println!("    -> {:?} if {:?}", t.actions, t.groups);
                    }
                }
            }
        }
    }
}
