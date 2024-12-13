use std::sync::atomic::{AtomicUsize, Ordering::*};

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub(crate) struct Header(*mut AtomicUsize);
pub(crate) struct Guard(&'static AtomicUsize);

impl Header {
    pub(crate) fn set(&self) {
        unsafe { &*self.0 }.fetch_or(1, Release);
    }

    pub(crate) fn load(storage: usize) -> Self {
        let ptr = (storage >> 1) as *mut usize;
        Self::new(ptr)
    }

    pub(crate) fn store(&self, next: *mut usize) {
        let storage = (next as usize) << 1;
        unsafe { &*self.0 }.store(storage, Release);
    }

    pub(crate) fn new(ptr: *mut usize) -> Self {
        Self(ptr as *mut AtomicUsize)
    }

    pub(crate) fn next(&self) -> Self {
        let storage = unsafe { &*self.0 }.load(Acquire);
        Self::load(storage)
    }

    pub(crate) fn available(&self) -> bool {
        unsafe { &*self.0 }.load(Acquire) & 1 == 0
    }

    pub(crate) fn distance(&self, end: &Self) -> usize {
        (unsafe { end.0.offset_from(self.0) }) as usize - 1
    }

    pub(crate) fn inner(self) -> *mut usize {
        self.0 as *mut usize
    }
}

impl Guard {
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
