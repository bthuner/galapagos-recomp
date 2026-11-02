//! Lists every glass polygon (`-4` frames) on the CD, per object and
//! appearance, with the material (shade number) each one uses.
use galapagos_formats::worlds::{Archive, Object};
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let a = Archive::parse(&data).unwrap();
    for sec in 0..a.sections.len() {
        let s = a.world(sec).unwrap();
        for (k, o) in s.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let mut seen = std::collections::BTreeMap::<(usize, i16), usize>::new();
            for (st, ap) in w.appearance.iter().flatten().enumerate() {
                for frames in &ap.polygons {
                    for f in frames { if f.texture == -4 { *seen.entry((st, f.shade)).or_default() += 1; } }
                }
            }
            if !seen.is_empty() { println!("section {sec} #{} {}: {:?}", k + 1, String::from_utf8_lossy(&w.kind), seen); }
        }
    }
}
