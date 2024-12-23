use std::{
    io::{self, Write},
    ops::{Deref, DerefMut},
    sync::Arc,
};

use crate::{
    header::{Guard, Header},
    RingAl,
};

/// Extendable buffer. It dynamically grows to accomodate any extra data beyond the current
/// capacity, given that there's still some capacity available to allocator itself. Contains a
/// reference to allocator, thus it effectively locks the allocator, and prevents any further
/// allocations while any instance of this type exist. After the necessary data is written, the
/// buffer should be finalized in order to release allocator lock and make the underlying buffer
/// `Send`
pub struct ExtBuf<'a> {
    /// header sequence, containing allocation metadata
    pub(crate) header: Header,
    /// number of bytes written to buffer so far
    pub(crate) initialized: usize,
    /// reference to ring allocator instance, used to extend the current buffer
    pub(crate) ringal: &'a mut RingAl,
    /// finalization flag, used in drop check when buffer is dropped without finalization
    pub(crate) finalized: bool,
}

impl ExtBuf<'_> {
    /// Finalize the write to extendable buffer, releases the allocator lock, and returns another
    /// buffer type which can be sent to other threads
    pub fn finalize(mut self) -> FixedBufMut {
        let capacity = self.header.capacity();
        let ptr = self.header.buffer();
        // SAFETY:
        // 1. the underlying backing store is 'static,
        // 2. we guarantee that it's a valid address as a consequence of 1
        // 3. the data is properly aligned
        // 4. we are constructing slice from the entire available capacity, but FixedBufMut
        //    guarantees that only initialized section will be exposed
        let inner = unsafe { std::slice::from_raw_parts_mut(ptr, capacity) };
        self.finalized = true;
        FixedBufMut {
            inner,
            initialized: self.initialized,
            _guard: self.header.into(),
        }
    }

    // SAFETY: obviously this is outrageously unsafe, but the callers of this method always
    // guarantee the invariants, namely:
    // 1. underlying buffer never overlaps with src,
    // 2. the length of src never exceeds remaining capacity of buffer
    unsafe fn copy_from(&mut self, src: &[u8]) {
        let count = src.len();
        let src = src.as_ptr();
        let buffer = self.header.buffer().add(self.initialized);
        buffer.copy_from_nonoverlapping(src, count);
        self.initialized += count;
    }
}

impl Write for ExtBuf<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // optimization for edge case recursion
        if buf.is_empty() {
            return Ok(0);
        }
        // make sure that we have enough spare capacity to fit the buf.len() bytes
        let available = self.header.capacity() - self.initialized;
        let count = buf.len();
        if available >= count {
            // SAFETY: safe, we exclusively own the memory region with ExtBuf, so
            // the buf cannot overlap with it, we made sure that the underlying
            // buffer still have enough capacity to accomodate the length of buf
            unsafe { self.copy_from(buf) };
            return Ok(count);
        }
        // extend the buffer with the missing capacity
        let required = buf.len() - available;
        let Some(header) = self.ringal.alloc(required) else {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "allocator's capacity exhausted",
            ));
        };
        // we've just wrapped around the ring, but we need continuous
        // memory, so we are forced to perform copy of bytes from tail
        // to head, which might potentially fail due to lack of capacity
        if header < self.header {
            // get pointer to current buffer
            let ptr = self.header.buffer();
            // SAFETY:
            // 1. the underlying backing store is 'static,
            // 2. we guarantee that it's a valid address as a consequence of 1
            // 3. the data is properly aligned
            // 4. we are only constructing slice from initialized data
            let old = unsafe { std::slice::from_raw_parts(ptr, self.initialized) };
            // keep the guard around to auto-release the memory
            // in tail, irrespective of what happens next
            let _guard = Guard::from(self.header);
            // wrap around adjustment of pointer
            self.header = header;
            // lock the new memory region
            self.header.set();
            self.initialized = 0;
            // copy existing data from tail to new memory region
            let _ = self.write(old)?;
            // copy argument buffer
            return self.write(buf);
        }
        // readjust the next guard pointer in case if fragmentation happened
        let end = header.next().0;
        self.header.store(end);
        // SAFETY: safe, we exclusively own the memory region
        // with `ExtBuf`, so the buf cannot overlap with it, we
        // extended the memory region with extra capacity so that
        // it now has enough to accomodate the length of `buf`
        unsafe { self.copy_from(buf) };
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Custom Drop implementation in order to handle edge
// case, when ExtBuf is dropped without being finalized
impl Drop for ExtBuf<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        if !self.finalized {
            // will unset the header upon drop, unlocking the memory
            let _ = Guard::from(self.header);
        }
    }
}

/// Fixed length (not growable) mutable buffer
pub struct FixedBufMut {
    /// the lock release mechanism upon drop
    pub(crate) _guard: Guard,
    /// exclusively owned slice from backing store
    pub(crate) inner: &'static mut [u8],
    /// number of initialized bytes
    pub(crate) initialized: usize,
}

impl FixedBufMut {
    /// make the buffer immutable and cheaply cloneable,
    /// this method involves heap allocation, don't use
    /// it unless you need to clone the buffer
    pub fn freeze(self) -> FixedBuf {
        FixedBuf {
            _rc: Arc::new(self._guard),
            inner: &self.inner[..self.initialized],
        }
    }

    /// available (uninitialized) capacity in bytes
    #[inline(always)]
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
        let tocopy = self.spare().min(buf.len());
        let src = buf.as_ptr();
        unsafe {
            self.inner
                .as_mut_ptr()
                .add(self.initialized)
                .copy_from_nonoverlapping(src, tocopy);
        }
        self.initialized += tocopy;
        Ok(tocopy)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Immutable and cheaply cloneable buffer
#[derive(Clone)]
pub struct FixedBuf {
    _rc: Arc<Guard>,
    inner: &'static [u8],
}

impl Deref for FixedBuf {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}
