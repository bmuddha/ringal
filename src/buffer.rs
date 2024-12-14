use std::{
    io::{self, Write},
    ops::{Deref, DerefMut},
};

use crate::{
    header::{Guard, Header},
    RingAl,
};

pub struct ExtBuf<'a> {
    pub(crate) header: Header,
    pub(crate) initialized: usize,
    pub(crate) ringal: &'a mut RingAl,
    pub(crate) finalized: bool,
}

impl ExtBuf<'_> {
    pub fn finilize(mut self) -> FixedBufMut {
        let capacity = self.header.capacity();
        let ptr = self.header.buffer();
        let inner = unsafe { std::slice::from_raw_parts_mut(ptr, capacity) };
        self.finalized = true;
        FixedBufMut {
            inner,
            initialized: self.initialized,
            _guard: self.header.into(),
        }
    }

    fn copy_from(&mut self, src: &[u8]) {
        let count = src.len();
        let src = src.as_ptr();
        unsafe {
            let buffer = self.header.buffer().add(self.initialized);
            buffer.copy_from_nonoverlapping(src, count)
        };
        self.initialized += count;
    }
}

impl Write for ExtBuf<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let available = self.header.capacity() - self.initialized;
        let count = buf.len();
        if available >= count {
            self.copy_from(buf);
            return Ok(count);
        }
        let required = buf.len() - available;
        let Some(header) = self.ringal.alloc(required) else {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "allocator's capacity exhausted",
            ));
        };
        if header < self.header {
            let ptr = self.header.buffer();
            let old = unsafe { std::slice::from_raw_parts(ptr, self.initialized) };
            println!("wrapped, copying from {ptr:p} to {:p}", header.inner());
            let _guard = Guard::from(self.header);
            self.header = header;
            self.header.set();
            let _ = self.write(old)?;
            return self.write(buf);
        }
        let end = header.next().inner();
        self.header.store(end);
        self.copy_from(buf);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for ExtBuf<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            // will unset the header upon drop, unlocking the memory
            let _ = Guard::from(self.header);
        }
    }
}

pub struct FixedBufMut {
    _guard: Guard,
    inner: &'static mut [u8],
    initialized: usize,
}

impl FixedBufMut {
    pub fn spare(&self) -> usize {
        self.inner.len() - self.initialized
    }
}

impl Deref for FixedBufMut {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { self.inner.get_unchecked(..self.initialized) }
    }
}

impl DerefMut for FixedBufMut {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.inner.get_unchecked_mut(..self.initialized) }
    }
}

impl Write for FixedBufMut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = buf;
        todo!()
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
