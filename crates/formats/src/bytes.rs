//! A tiny little-endian cursor. The 1997 engine was built for x86, so every
//! multi-byte integer on disk is little-endian.

#[derive(Debug)]
pub struct OutOfBounds {
    pub at: usize,
    pub want: usize,
}

pub struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], OutOfBounds> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or(OutOfBounds { at: self.pos, want: n })?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, OutOfBounds> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, OutOfBounds> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, OutOfBounds> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> Result<i32, OutOfBounds> {
        Ok(self.u32()? as i32)
    }
}
