//! Dump a Smacker file to PNG frames + WAV audio, using nothing but std.
//!
//! ```sh
//! cargo run --release -p galapagos-formats --example smk_dump -- <file.smk> <outdir> [frame,frame,...]
//! ```
//!
//! Writes `frame_NNNN.png` for a handful of frames (or the ones listed) and
//! `track_N.wav` for every audio track. Both encoders are hand-written below
//! because they are tiny and instructive.

use galapagos_formats::smacker::Smacker;
use std::{env, fs, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: smk_dump <file> <outdir> [frame,frame,...]");
        return ExitCode::FAILURE;
    }
    match run(&args[1], Path::new(&args[2]), args.get(3)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(input: &str, outdir: &Path, wanted: Option<&String>) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(input)?;
    let mut smk = Smacker::from_vec(data)?;
    let info = smk.info().clone();
    fs::create_dir_all(outdir)?;

    println!(
        "{:?} {}x{} (display {}x{}, {:?}), {} frames @ {:.3} fps = {:.2} s, ring frame: {}",
        info.version,
        info.width,
        info.height,
        info.width,
        info.display_height(),
        info.y_scale,
        info.frame_count,
        info.fps(),
        info.duration_us() as f64 / 1e6,
        info.ring_frame
    );
    for t in &info.audio {
        println!(
            "  audio track {}: {} Hz, {} ch, {}-bit, {:?}, max chunk {} bytes",
            t.index, t.sample_rate, t.channels, t.bits, t.compression, t.max_chunk_bytes
        );
    }

    // Default: first, a few evenly spaced, and last.
    let n = info.frame_count as usize;
    let frames_to_dump: Vec<usize> = match wanted {
        Some(list) => list.split(',').filter_map(|s| s.parse().ok()).collect(),
        None => vec![0, n / 8, n / 4, n / 2, 3 * n / 4, n.saturating_sub(1)],
    };

    let mut tracks: Vec<Vec<i16>> = vec![Vec::new(); 7];
    let mut keyframes = 0;
    let mut palette_changes = 0;
    while smk.frames_remaining() > 0 {
        let frame = smk.decode_next_frame()?;
        keyframes += frame.keyframe as usize;
        palette_changes += frame.palette_changed as usize;
        for (all, part) in tracks.iter_mut().zip(&frame.audio) {
            all.extend_from_slice(part);
        }
        if frames_to_dump.contains(&frame.index) {
            let rgba = frame.to_rgba();
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            let path = outdir.join(format!("frame_{:04}.png", frame.index));
            fs::write(&path, png_rgb(frame.width, frame.display_height(), &rgb))?;
            println!("  wrote {}", path.display());
        }
    }
    println!("  {keyframes} keyframes, {palette_changes} palette changes");

    for t in &info.audio {
        let samples = &tracks[t.index];
        let secs = samples.len() as f64 / (t.sample_rate as f64 * t.channels as f64);
        let path = outdir.join(format!("track_{}.wav", t.index));
        fs::write(&path, wav_i16(t.sample_rate, t.channels as u16, samples))?;
        println!("  wrote {} ({:.2} s of audio)", path.display(), secs);
    }
    Ok(())
}

// --- WAV -------------------------------------------------------------------

/// A canonical 44-byte-header RIFF/WAVE file of 16-bit PCM.
fn wav_i16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes()); // bytes/s
    w.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

// --- PNG -------------------------------------------------------------------

/// A valid PNG without any compression: the zlib stream uses deflate
/// "stored" blocks (type 0), which are just length-prefixed raw bytes.
/// Big files, zero cleverness, ~40 lines.
fn png_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    // Each scanline is prefixed with filter type 0 ("None").
    let mut raw = Vec::with_capacity(rgb.len() + height as usize);
    for line in rgb.chunks_exact(width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(line);
    }

    // zlib = 2-byte header, deflate blocks, Adler-32 of the raw data.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(blocks.peek().is_none() as u8); // BFINAL bit, BTYPE = 00
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, no filter, no interlace

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &z);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

/// length, type, data, CRC-32 over type+data.
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// CRC-32 (IEEE, reflected polynomial 0xEDB88320), bit by bit.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Adler-32: two running sums modulo the largest prime below 2^16.
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
