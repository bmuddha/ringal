#![deny(missing_docs)]

//! # RingAl
//!
//! `RingAl` is a library that implements a ring allocator. It provides a highly efficient mechanism for memory allocation, particularly suited for scenarios where allocations and deallocations happen in a contiguous or circular manner.
//!
//! ## Features
//!
//! - **Dynamic Buffer Management**: The backing store of the allocator can dynamically merge and split buffers. This allows for efficient reuse of memory and can adapt to varying allocation requests.
//!
//! - **Extendable Buffers**: The library allows for buffers that can be extended (`ExtBuf`). These buffers are useful when the memory requirements can change dynamically, allowing the buffer to grow as required.
//!
//! - **Fixed Size Buffers**: For scenarios requiring predetermined memory sizes, `FixedBufMut` provides a fixed-size buffer, ensuring that memory constraints are respected.
//!
//! - **Atomic Operations**: The library uses atomic operations to ensure safe concurrent access to the allocator, making it suitable for multithreaded applications.
//!
//! - **Minimal Overhead**: Allocations are simply pointer adjustments, with some overhead introduced due to use of Atomics, so they are very cheap and fast
//!
//! ## Getting Started
//!
//! ```rust
//! use std::io::Write;
//! use ringal::RingAl;
//!
//! let mut allocator = RingAl::new(1024); // Create an allocator with initial size
//!
//! if let Some(mut buffer) = allocator.fixed(256) {
//!     let size = buffer.write(b"hello world, this message is relatively short").unwrap();
//!     println!("{size} bytes written to fixed buffer");
//! }
//!
//! if let Some(mut buffer) = allocator.extendable(64) {
//!     let msg = b"hello world, this message is longer than allocated capacity, but buffer will
//!     grow as needed during the write, provided that allocator still has necessary capacity";
//!     let size = buffer.write(msg).unwrap();
//!     println!("{size} bytes written to extendable buffer");
//!     let fixed = buffer.finalize();
//!     assert_eq!(fixed.as_ref(), msg);
//!     assert!(fixed.len() > 64);
//! }
//! drop(allocator);
//! ```
//!
//! ## Safety
//! The library is safe!, or I that's what I'd like to say, but the truth is that the use of unsafe
//! is prevalent in this library, heck parts of it doesn't even look like Rust, more like C with
//! raw pointer arithmetic all over the place. But jokes aside, a lot of effort and consideration
//! was put into providing safe abstractions around those unsafe parts. Usage of unsafe is
//! unavaidable when working with raw pointers alas.
//!
//! ## Limitations
//! The library is not suitable for use cases, when allocated memory has be to held onto for
//! prolonged durations of time
//! For more information, refer to the module-level documentation in `buffer` and `header`.

pub use buffer::{ExtBuf, FixedBuf, FixedBufMut};
use header::Header;

/// hs
pub struct RingAl {
    head: *mut usize,
}

const USIZELEN: usize = size_of::<usize>();
const MINALLOC: usize = 4;

impl RingAl {
    /// fasf
    pub fn new(mut size: usize) -> Self {
        assert!(size > USIZELEN && size < usize::MAX >> 1);
        size = size.next_power_of_two() / USIZELEN;
        let mut buffer = Vec::<usize>::with_capacity(size);
        size = buffer.capacity();
        #[allow(clippy::uninit_vec)]
        unsafe {
            buffer.set_len(size)
        };
        let inner = Box::leak(buffer.into_boxed_slice()).as_mut_ptr();
        let head = inner;
        let tail = unsafe { inner.add(size - 1) };
        unsafe { *head = (tail as usize) << 1 };
        unsafe { *tail = (head as usize) << 1 };

        Self { head }
    }

    /// saga
    pub fn fixed(&mut self, size: usize) -> Option<FixedBufMut> {
        let header = self.alloc(size)?;
        header.set();
        let ptr = header.buffer();
        let capacity = header.capacity();
        let inner = unsafe { std::slice::from_raw_parts_mut(ptr, capacity) };
        Some(FixedBufMut {
            inner,
            initialized: 0,
            _guard: header.into(),
        })
    }

    /// fafsfa
    pub fn extendable(&mut self, size: usize) -> Option<ExtBuf<'_>> {
        let header = self.alloc(size)?;
        header.set();
        Some(ExtBuf {
            header,
            initialized: 0,
            ringal: self,
            finalized: false,
        })
    }

    fn alloc(&mut self, mut size: usize) -> Option<Header> {
        size = (size / USIZELEN + (size % USIZELEN != 0) as usize).max(MINALLOC);
        let mut start = Header::new(self.head);
        let mut accumulated = 0;
        let mut next = start;
        while accumulated < size {
            next.available().then_some(())?;
            next = next.next();
            (next != self.head).then_some(())?;
            if next < start {
                accumulated = 0;
                start = next;
                continue;
            }
            accumulated = start.distance(&next);
        }
        if (accumulated - size) <= MINALLOC {
            self.head = next.inner();
        } else {
            self.head = unsafe { start.inner().add(size + 1) };
            let head = Header::new(self.head);
            head.store(next.inner());
        }
        start.store(self.head);
        Some(start)
    }
}

/// initialize thread local allocator
#[cfg(any(feature = "tls", test))]
#[macro_export]
macro_rules! ringal {
    (@init, $capacity: expr) => {
        use std::cell::RefCell;
        thread_local! {
            pub static RINGAL: RefCell<RingAl> = RefCell::new(RingAl::new($capacity));
        }
    };
    (@fixed, $capacity: expr) => {
        RINGAL.with_borrow_mut(|ringal| ringal.fixed($capacity))
    };
    (@ext, $capacity: expr, $cb: expr) => {
        RINGAL.with_borrow_mut(|r| r.extendable($capacity).map($cb))
    };
}

#[cfg(any(feature = "drop", test))]
impl Drop for RingAl {
    fn drop(&mut self) {
        use std::time::Duration;
        let start = self.head;
        let mut head = self.head;
        let mut next = Header::new(start);
        let mut capacity = 0;
        loop {
            if !next.available() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let nnext = next.next();
            if nnext > next {
                capacity += next.capacity() / USIZELEN;
            }
            capacity += 1;
            next = nnext;
            if next.inner() == start {
                break;
            }
            let inner = next.inner();
            head = head.min(inner)
        }
        let slice = std::ptr::slice_from_raw_parts_mut(head, capacity);
        // SAFETY:
        // 1. all pointers always point inside the backing store
        // 2. the original slice length has been recomputed
        // 3. the starting address is identified via wrap around detection
        // 4. it's just a reclamation of leaked boxed slice
        let _ = unsafe { Box::from_raw(slice) };
    }
}

/// Buffer types
mod buffer;
/// Essential header canaries
mod header;
#[cfg(test)]
/// test
mod tests;
