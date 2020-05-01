use heapless::{Vec, ArrayLength};

/// Temp until ufmt is released with https://github.com/japaric/ufmt/pull/21
pub struct WriteAdapter<W>(pub W)
where
    W: core::fmt::Write;

impl<W> ufmt::uWrite for WriteAdapter<W>
where
    W: core::fmt::Write,
{
    type Error = core::fmt::Error;

    fn write_char(&mut self, c: char) -> Result<(), Self::Error> {
        self.0.write_char(c)
    }

    fn write_str(&mut self, s: &str) -> Result<(), Self::Error> {
        self.0.write_str(s)
    }
}

/// Temp until https://github.com/japaric/heapless/pull/151, or serde_json can
/// be replaced with serde_json_core
pub fn vec_to_vec<T: Clone, L: ArrayLength<T>>(v: alloc::vec::Vec<T>) -> Vec<T, L> {
    let mut v2 = Vec::new();
    v2.extend_from_slice(&v).ok();
    v2
}
