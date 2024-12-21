use std::sync::atomic::{AtomicUsize, Ordering::*};

use crate::USIZELEN;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
/// Helper type that wraps memory guard sequence
pub(crate) struct Header(*mut AtomicUsize);
/// Allocation guard, used through its Drop implementation, which
/// unlocks (releases) the memory region back to allocator
pub(crate) struct Guard(&'static AtomicUsize);

impl Header {
    /// lock the memory region, preventing allocator
    /// from reusing it until it's unlocked
    pub(crate) fn set(&self) {
        unsafe { &*self.0 }.fetch_or(1, Release);
    }

    /// construct new Header from pointer data
    /// (next guard) stored in some guard sequence
    pub(crate) fn load(storage: usize) -> Self {
        let ptr = (storage >> 1) as *mut usize;
        Self::new(ptr)
    }

    /// store next guard pointer data in current header
    pub(crate) fn store(&self, next: *mut usize) {
        let storage = (next as usize) << 1;
        unsafe { &*self.0 }.store(storage, Release);
    }

    /// construct new header from guard sequence pointer, this
    /// allows to perform various atomic operation on guard sequence
    pub(crate) fn new(ptr: *mut usize) -> Self {
        Self(ptr as *mut AtomicUsize)
    }

    /// jump to next guard sequence and construct new Header from it
    pub(crate) fn next(&self) -> Self {
        let storage = unsafe { &*self.0 }.load(Acquire);
        Self::load(storage)
    }

    /// checks whether or not guarded memory region is locked
    pub(crate) fn available(&self) -> bool {
        unsafe { &*self.0 }.load(Acquire) & 1 == 0
    }

    /// distance (in `size_of<usize>()`) to given guard
    pub(crate) fn distance(&self, end: &Self) -> usize {
        // SAFETY: its just a pointer arithmetic really, the caller
        // guarantees that both pointers point into the backing store
        // of allocator and `end` is always greater than `self`
        (unsafe { end.0.offset_from(self.0) }) as usize - 1
    }

    /// get the raw pointer to guard sequence
    pub(crate) fn inner(self) -> *mut usize {
        self.0 as *mut usize
    }

    /// calculate the total available capacity
    /// (in bytes) of guarded memory region
    pub(crate) fn capacity(&self) -> usize {
        let end = self.next();
        self.distance(&end) * USIZELEN
    }

    /// get the raw pointer to guarded memory region
    pub(crate) fn buffer(&self) -> *mut u8 {
        (unsafe { self.0.add(1) }) as *mut u8
    }
}

impl Guard {
    /// release the lock from memory region,
    /// making it available to allocator
    fn unset(&self) {
        self.0.fetch_and(usize::MAX << 1, Release);
    }
}

impl From<Header> for Guard {
    fn from(value: Header) -> Self {
        Self(unsafe { &*value.0 })
    }
}

impl PartialEq<*mut usize> for Header {
    fn eq(&self, other: &*mut usize) -> bool {
        self.0 as *mut usize == *other
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.unset()
    }
}
