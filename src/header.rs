use crate::USIZELEN;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
/// Helper type that wraps memory guard sequence
pub(crate) struct Header(pub(crate) *mut usize);
/// Allocation guard, used through its Drop implementation, which
/// unlocks (releases) the memory region back to allocator
pub(crate) struct Guard(&'static mut usize);

impl Header {
    /// lock the memory region, preventing allocator
    /// from reusing it until it's unlocked
    #[inline(always)]
    pub(crate) fn set(&self) {
        unsafe { *self.0 |= 1 }
    }

    /// store next guard pointer data in current header
    #[inline(always)]
    pub(crate) fn store(&self, next: *mut usize) {
        unsafe { *self.0 = (next as usize) << 1 };
    }

    /// jump to next guard sequence and construct new Header from it
    #[inline(always)]
    pub(crate) fn next(&self) -> Self {
        Self((unsafe { *self.0 } >> 1) as *mut usize)
    }

    /// checks whether or not guarded memory region is locked
    #[inline(always)]
    pub(crate) fn available(&self) -> bool {
        (unsafe { *(self.0) }) & 1 == 0
    }

    /// distance (in `size_of<usize>()`) to given guard
    #[inline(always)]
    pub(crate) fn distance(&self, end: &Self) -> usize {
        // SAFETY: its just a pointer arithmetic really, the caller
        // guarantees that both pointers point into the backing store
        // of allocator and `end` is always greater than `self`
        (unsafe { end.0.offset_from(self.0) }) as usize - 1
    }

    /// calculate the total available capacity
    /// (in bytes) of guarded memory region
    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.distance(&self.next()) * USIZELEN
    }

    /// get the raw pointer to guarded memory region
    #[inline(always)]
    pub(crate) fn buffer(&self) -> *mut u8 {
        (unsafe { self.0.add(1) }) as *mut u8
    }
}

impl From<Header> for Guard {
    #[inline(always)]
    fn from(value: Header) -> Self {
        Self(unsafe { &mut *value.0 })
    }
}

impl Drop for Guard {
    #[inline(always)]
    fn drop(&mut self) {
        // release the lock from memory region,
        // making it available to allocator
        *self.0 &= usize::MAX << 1;
    }
}
