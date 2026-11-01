//! Sound banks: the samples a world (or Mendel, or the predators) can play.
//!
//! The archive stores every sample four times, one copy per "Fidelity"
//! preference (8-bit 11 kHz up to 16-bit 44 kHz; docs/re/worlds.md §4). A
//! 1997 PC had to choose; a browser doesn't, so we always take the best copy
//! (quality 4, falling back like the engine does when it's absent) and
//! convert it to signed 16-bit for WebAudio.

use galapagos_formats::worlds::SoundSet;

pub struct Pcm {
    pub rate: u32,
    /// MIDI base note (60 = middle C): the pitch the sample was recorded at.
    pub base_note: u8,
    pub samples: Vec<i16>,
}

/// One bank; slot `i` is `None` where the set has an empty slot.
pub struct Bank {
    pub sounds: Vec<Option<Pcm>>,
}

/// The best quality copy, like a player with "Fidelity" at maximum.
const QUALITY: usize = 4;

impl Bank {
    pub fn decode(data: &[u8], set: &SoundSet) -> Bank {
        let sounds = set
            .samples(data, QUALITY)
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let s = s?;
                // 8-bit samples are unsigned (silence = 128); 16-bit are signed LE.
                let samples = if s.bits == 8 {
                    s.pcm.iter().map(|&b| ((b as i16) - 128) << 8).collect()
                } else {
                    s.pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
                };
                Some(Pcm { rate: s.sample_rate(), base_note: s.base_note, samples })
            })
            .collect();
        Bank { sounds }
    }

    pub fn empty() -> Bank {
        Bank { sounds: Vec::new() }
    }
}

/// The three banks a running game uses, numbered for the ABI: 0 = the
/// current world section, 1 = Mendel (11 sounds), 2 = the predators (2).
pub const BANK_WORLD: u32 = 0;
