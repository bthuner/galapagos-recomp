//! The `worlds` archive (`install/data/worlds`, 126 MB).
//!
//! Top level, as observed on the retail EN CD:
//!
//! ```text
//! u32         section_count        (8)
//! u32[count]  section_offset       absolute file offsets, ascending
//! ...         global block         0x24 .. section_offset[0]
//! ...         section i            section_offset[i] .. section_offset[i+1]
//! ```
//!
//! The inner layout of each section is documented in `docs/re/worlds.md`
//! as it gets reverse engineered.

use crate::bytes::{Cursor, OutOfBounds};

pub struct Archive<'a> {
    pub data: &'a [u8],
    pub sections: Vec<core::ops::Range<usize>>,
    pub global: core::ops::Range<usize>,
}

impl<'a> Archive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, OutOfBounds> {
        let mut c = Cursor::new(data);
        let count = c.u32()? as usize;
        let mut offsets = Vec::with_capacity(count + 1);
        for _ in 0..count {
            offsets.push(c.u32()? as usize);
        }
        let header_end = c.pos;
        offsets.push(data.len());
        let sections = offsets.windows(2).map(|w| w[0]..w[1]).collect();
        Ok(Self { data, sections, global: header_end..offsets[0] })
    }

    pub fn section(&self, i: usize) -> &'a [u8] {
        &self.data[self.sections[i].clone()]
    }
}
