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
        // releases the lock from the specified memory region, thereby making it available for the allocator.
        //
        // # safety and ordering guarantees
        //
        // the operation of releasing the lock is non-atomic and migth cause race conditions in
        // multithreaded context. It is ensured that the caller of `drop` is the sole entity
        // capable of writing to the region at this point in the execution timeline. Furthermore,
        // the compiler enforces strict ordering such that this operation cannot be reordered with
        // other operations that might utilize this memory region.
        //
        // importantly, the allocator is restricted from accessing the region until it is fully released.
        // While there may be a delay in the visibility of this operation to the allocator due to CPU caching,
        // the change will eventually propagate to the allocator's thread. Consequently, potential race conditions
        // are effectively mitigated by these guarantees, ensuring correctness and eventual consistency.
        *self.0 &= usize::MAX << 1;
    }
}
