use std::{
    io::{self, Write},
    ops::{Deref, DerefMut},
    sync::Arc,
};

use crate::{
    header::{Guard, Header},
    RingAl, USIZEALIGN,
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
        let Some(header) = self.ringal.alloc(count - available, USIZEALIGN) else {
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
            self.header.lock();
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

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.inner.get_unchecked(..self.initialized) }
    }
}

impl DerefMut for FixedBufMut {
    #[inline(always)]
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

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

/// Mutable fixed buffer, which is generic over its elements T, with `Vec<T>` like API
#[cfg(feature = "generic")]
pub struct GenericBufMut<T: Sized + 'static> {
    /// the lock release mechanism upon drop
    pub(crate) _guard: Guard,
    /// exclusively owned slice from backing store
    /// properly aligned for T
    pub(crate) inner: &'static mut [T],
    /// current item count in buffer
    pub(crate) initialized: usize,
}

#[cfg(feature = "generic")]
mod generic {
    use super::{Deref, DerefMut, GenericBufMut};

    impl<T> Deref for GenericBufMut<T> {
        type Target = [T];

        #[inline(always)]
        fn deref(&self) -> &Self::Target {
            unsafe { self.inner.get_unchecked(..self.initialized) }
        }
    }

    impl<T> DerefMut for GenericBufMut<T> {
        #[inline(always)]
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { self.inner.get_unchecked_mut(..self.initialized) }
        }
    }

    impl<T> GenericBufMut<T> {
        /// Returns the total capacity of the buffer, i.e., the number of elements it can hold.
        pub fn capacity(&self) -> usize {
            self.inner.len()
        }

        /// Pushes an element to the end of the buffer.
        ///
        /// If the buffer is full it returns the element as `Some(value)`. Otherwise, it returns `None`
        /// after successfully adding the element to end. Operation is O(1)
        pub fn push(&mut self, value: T) -> Option<T> {
            if self.inner.len() == self.initialized {
                return Some(value);
            }
            unsafe {
                let cell = self.inner.get_unchecked_mut(self.initialized) as *mut T;
                std::ptr::write(cell, value);
            }
            self.initialized += 1;
            None
        }

        /// Inserts an element at the specified index in the buffer.
        ///
        /// If the index is greater than the number of initialized elements, the operation fails and
        /// `value` is returned. Otherwise, it shifts elements to the right and inserts the new
        /// element, returning `None`. This operation is O(N).
        pub fn insert(&mut self, mut value: T, mut index: usize) -> Option<T> {
            if self.initialized < index {
                return Some(value);
            }
            self.initialized += 1;
            unsafe {
                while index < self.initialized {
                    let cell = self.inner.get_unchecked_mut(index) as *mut T;
                    let temp = std::ptr::read(cell as *const T);
                    std::ptr::write(cell, value);
                    value = temp;
                    index += 1;
                }
            }
            std::mem::forget(value);
            None
        }

        /// Removes and returns the last element from the buffer.
        ///
        /// Returns `None` if the buffer is empty. The operation is O(1)
        pub fn pop(&mut self) -> Option<T> {
            (self.initialized != 0).then_some(())?;
            self.initialized -= 1;
            let value = unsafe { self.inner.get_unchecked_mut(self.initialized) };
            let owned = unsafe { std::ptr::read(value as *const T) };
            Some(owned)
        }

        /// Removes and returns the element at the specified index from the buffer.
        ///
        /// Shifts all elements following the index to the left to fill the gap.
        /// Returns `None` if the index is out of bounds. The operation is O(N)
        pub fn remove(&mut self, mut index: usize) -> Option<T> {
            (self.initialized > index).then_some(())?;
            if self.initialized - 1 == index {
                return self.pop();
            }
            let mut value = unsafe { self.inner.get_unchecked_mut(index) } as *mut T;
            let element = unsafe { std::ptr::read(value) };
            self.initialized -= 1;
            unsafe {
                while index < self.initialized {
                    index += 1;
                    let next = self.inner.get_unchecked_mut(index) as *mut T;
                    {
                        let next = std::ptr::read(next);
                        std::ptr::write(value, next);
                    }
                    value = next;
                }
            }
            Some(element)
        }

        /// Tries to remove and return the element at the specified index, replacing
        /// it with the last element. The operation is O(1)
        pub fn swap_remove(&mut self, index: usize) -> Option<T> {
            (index < self.initialized).then_some(())?;
            if self.initialized - 1 == index {
                return self.pop();
            }

            // Swap the element at the given index with the last element
            self.initialized -= 1;
            let element = unsafe {
                let last = self.inner.get_unchecked_mut(self.initialized) as *mut T;
                let value = self.inner.get_unchecked_mut(index) as *mut T;
                let element = std::ptr::read(value);
                let temp = std::ptr::read(last);
                std::ptr::write(value, temp);
                element
            };

            Some(element)
        }
    }
}
