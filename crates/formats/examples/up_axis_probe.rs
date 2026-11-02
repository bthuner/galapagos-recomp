//! Which axis is "up" in Galapagos? Prints the dominant normal axis of the
//! portal pads (Strt/Tele, which Mendel stands on) and an area-weighted
//! histogram of polygon normals per axis for a section.
use galapagos_formats::worlds::{Archive, Object};

fn normal(p: &[[f32; 3]]) -> ([f32; 3], f32) {
    let (a, b, c) = (p[0], p[1], p[2]);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if l == 0.0 { return ([0.0; 3], 0.0) }
    ([n[0] / l, n[1] / l, n[2] / l], l)
}

fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in [0usize, 2] {
        let sec = a.world(s).unwrap();
        let mut area = [0f32; 3];
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let Some(m) = &w.mesh else { continue };
            let mut pad_axes = [0usize; 3];
            for poly in &m.polygons {
                let pts: Vec<[f32; 3]> = poly.iter().map(|&k| m.vertices[k as usize].map(|c| c as f32)).collect();
                if pts.len() < 3 { continue }
                let (n, l) = normal(&pts);
                let ax = (0..3).max_by(|&x, &y| n[x].abs().total_cmp(&n[y].abs())).unwrap();
                area[ax] += l;
                pad_axes[ax] += 1;
            }
            if &w.kind == b"Strt" {
                for poly in &m.polygons {
                    let pts: Vec<[f32; 3]> = poly.iter().map(|&k| m.vertices[k as usize].map(|c| c as f32)).collect();
                    if pts.len() >= 3 { println!("  Strt #{} polygon normal {:?}", i + 1, normal(&pts).0.map(|v| (v * 100.0).round() / 100.0)); }
                }
            }
            if &w.kind == b"Strt" || &w.kind == b"Tele" {
                println!("section {s} #{} {}: polygons facing x/y/z = {:?}", i + 1, String::from_utf8_lossy(&w.kind), pad_axes);
            }
        }
        let t: f32 = area.iter().sum();
        println!("section {s}: area-weighted normals x {:.0}% y {:.0}% z {:.0}%", area[0] / t * 100.0, area[1] / t * 100.0, area[2] / t * 100.0);
    }
}
