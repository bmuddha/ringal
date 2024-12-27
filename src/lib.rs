#![deny(missing_docs)]

//! # RingAl - A High-Performance Ring Allocator
//!
//! RingAl is a blazing-fast ring allocator optimized for short-lived buffer allocations. It uses a circular allocation pattern, which grants low-cost allocations as long as allocations are ephemeral. Long-lived allocations can degrade performance by clogging the allocator.
//!
//! ## Key Features
//!
//! 1. **Preallocation Strategy**: Establish a fixed-size backing store of `N` bytes.
//! 2. **Efficient Small Buffer Management**: Allocate smaller buffers efficiently within the preallocated store.
//! 3. **Thread-Safe Operations**: Buffers can be safely shared across threads, offering clone optimizations similar to `Arc`.
//! 4. **Timely Buffer Deallocation**: Buffers should be dropped before the allocator wraps around to reuse the same memory, ensuring optimal recycling.
//!
//! RingAl focuses on efficient memory management with a dynamic memory pool that evolves its
//! backing store using something called guard sequences. Guard sequences are usually exclusively
//! owned by allocator when they are not locked (i.e. not guarding the allocation), and when they
//! do become locked, the memory region (single usize) backing the guard sequence, is only accessed
//! using Atomic operations. This allows for one thread (the owner of buffer) to release the guard
//! (write to guard sequence), when the buffer is no longer in use and the other thread, where
//! allocator is running, to perform synchronized check of whether guard sequence have been
//! released.  
//!
//! The design allows for safe multi-threaded access: one thread can write while one other can
//! read. If a buffer is still occupied after allocator wraps around, the allocation attempt just
//! returns `None`, prompting a retry.
//!
//! Note: RingAl itself isn't `Sync`, so it cannot be accessed from multiple threads concurrently.
//! Instead, using thread-local storage is encouraged.
//!
//! ## Allocation Strategies
//!
//! - **Exact Fit**: Perfectly matches the requested and available sizes.
//! - **Oversized Guards**: Splits larger regions to fit the requested size, potentially causing fragmentation.
//! - **Undersized Guards**: Merges smaller regions to accommodate requests, aiding in defragmentation.
//! - **Capacity Constraints**: Fails if the backing store cannot satisfy the request, signaling with `None`.
//!
//! ## Optional Features
//! 1. **`tls` (Thread-Local Storage)**: Enables allocation requests without explicitly passing the allocator. It uses `RefCell` for thread-local data, offering convenience at a minor performance cost.
//! 2. **`drop` (Early Deallocation)**: Allows the allocator to deallocate its resources upon request. If activated, it employs a `Drop` implementation that ensures no memory leaks by blocking until all buffers are released.
//!
//! ## Usage Examples
//!
//! ### Extendable Buffer
//! ```rust
//! # use std::io::Write;
//! # use ringal::RingAl;
//! let mut allocator = RingAl::new(1024);
//! let mut buffer = allocator.extendable(64).unwrap();
//! let msg = b"hello world, this message is longer than allocated capacity...";
//! let size = buffer.write(msg).unwrap();
//! let fixed = buffer.finalize();
//! assert_eq!(fixed.as_ref(), msg);
//! assert_eq!(fixed.len(), size);
//! ```
//!
//! ### Fixed Buffer
//! ```rust
//! # use std::io::Write;
//! # use ringal::RingAl;
//! let mut allocator = RingAl::new(1024);
//! let mut buffer = allocator.fixed(256).unwrap();
//! let size = buffer.write(b"hello world, short message").unwrap();
//! assert_eq!(buffer.len(), size);
//! assert!(buffer.spare() >= 256 - size);
//! ```
//!
//! ### Multi-threaded Environment
//! ```rust
//! # use std::io::Write;
//! # use std::sync::mpsc::channel;
//! # use ringal::{ RingAl, FixedBufMut, FixedBuf };
//! let mut allocator = RingAl::new(1024);
//! let (tx, rx) = channel();
//! let mut buffer = allocator.fixed(64).unwrap();
//! let _ = buffer.write(b"message to another thread").unwrap();
//! let handle = std::thread::spawn(move || {
//!     let buffer: FixedBufMut = rx.recv().unwrap();
//!     let readonly = buffer.freeze();
//!     for _ in 0..16 {
//!         let msg: FixedBuf = readonly.clone();
//!         let msg_str = std::str::from_utf8(&msg[..]).unwrap();
//!         println!("{msg_str}");
//!     }
//! });
//! tx.send(buffer);
//! handle.join().unwrap();
//! ```
//!
//! ### Thread Local Storage
//! ```rust
//! # use ringal::ringal;
//! # use std::io::Write;
//! // init the thread local allocator, should be called just once, any thread
//! // spawned afterwards, will have access to their own instance of the allocator
//! ringal!(@init, 1024);
//! // allocate fixed buffer
//! let mut fixed = ringal!(@fixed, 64).unwrap();
//! let _ = fixed.write(b"hello world!").unwrap();
//! // allocate extendable buffer, and pass a callback to operate on it
//! // this approach is necessary as ExtBuf contains reference to thread local storage,
//! // and LocalKey doesn't allow any references to exist outside of access callback
//! let fixed = ringal!{@ext, 64, |mut extendable| {
//!     let _ = extendable.write(b"hello world!").unwrap();
//!     // it's ok to return FixedBuf(Mut) from callback
//!     extendable.finalize()
//! }}.unwrap();
//! println!("bytes written: {}", fixed.len());
//! ```
//!
//! ## Safety Considerations
//!
//! While `unsafe` code is used for performance reasons, the public API is designed to be safe.
//! Significant efforts have been made to ensure no undefined behavior occurs, offering a safe
//! experience for end-users.

use std::alloc::{alloc, dealloc, Layout};

use buffer::GenericBufMut;
pub use buffer::{ExtBuf, FixedBuf, FixedBufMut};
use header::Header;

/// Ring Allocator, see crate level documentation on features and usage
pub struct RingAl {
    /// pointer to guard sequence which will be used for next allocation
    head: *mut usize,
}
/// Platform specific size of machine word
const USIZELEN: usize = size_of::<usize>();
/// We restrict granularity of allocation by rounding up the allocated
/// capacity to multiple of 8 machine words (converted to byte count)
const MINALLOC: usize = 8;
const DEFAULT_ALIGNMENT: usize = 64;
const USIZEALIGN: usize = align_of::<usize>();

impl RingAl {
    /// Initialize ring allocator with given capacity and default alignment. Allocated capacity
    /// might be different from what was requested, but will contain at least `size` bytes.
    ///
    /// NOTE:
    /// not entire capacity will be available for allocation due to guard headers being part of
    /// allocations and taking up space as well
    pub fn new(size: usize) -> Self {
        Self::new_with_align(size, DEFAULT_ALIGNMENT)
    }

    /// Initialize ring allocator with given capacity and alignment. Allocated capacity might be
    /// different from what was requested, but will contain at least `size` bytes.
    ///
    /// NOTE: not entire capacity will be available for allocation due to guard headers being part
    /// of allocations and taking up space as well
    pub fn new_with_align(mut size: usize, align: usize) -> Self {
        assert!(size > USIZELEN && size < usize::MAX >> 1 && align.is_power_of_two());
        size = size.next_power_of_two();
        let inner = unsafe { alloc(Layout::from_size_align_unchecked(size, align)) };

        let head = inner as *mut usize;
        // get a pointer to the last guard sequence
        size /= USIZELEN;
        let tail = unsafe { head.add(size - 1) };
        // as we don't have any allocations yet, put the entire capacity
        // of backing store into first guard, and let the last guard point
        // back to first one, forming ring structure.
        unsafe { *head = (tail as usize) << 1 };
        unsafe { *tail = (head as usize) << 1 };

        Self { head }
    }

    /// Try to allocate a fixed (non-extandable) buffer with at least
    /// `size` bytes of capacity, the operation will fail, returning
    /// `None`, if backing store doesn't have enough capacity available.
    #[inline(always)]
    pub fn fixed(&mut self, size: usize) -> Option<FixedBufMut> {
        let header = self.alloc(size, USIZEALIGN)?;
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

    /// Try to allocate an extandable buffer with at least `size` bytes
    /// of initial capacity, the operation will fail, returning `None`,
    /// if backing store doesn't have enough capacity available.
    ///
    /// NOTE: the allocated buffer contains an exclusive reference to
    /// allocator (used to dynamically grow the buffer), which effectively
    /// prevents any further allocations while this buffer is around
    pub fn extendable(&mut self, size: usize) -> Option<ExtBuf<'_>> {
        let header = self.alloc(size, USIZEALIGN)?;
        header.set();
        Some(ExtBuf {
            header,
            initialized: 0,
            ringal: self,
            finalized: false,
        })
    }

    /// Allocate a fixed buffer, that can accomodate `count` elements of type T
    pub fn generic<T>(&mut self, count: usize) -> Option<GenericBufMut<T>> {
        let tsize = size_of::<T>();
        let size = count * tsize;
        let header = self.alloc(size, align_of::<T>())?;
        header.set();
        let buffer = header.buffer();
        let offset = buffer.align_offset(align_of::<T>());
        let inner = unsafe { buffer.add(offset) } as *mut T;
        let inner = unsafe { std::slice::from_raw_parts_mut(inner, count) };
        Some(GenericBufMut {
            _guard: header.into(),
            inner,
            initialized: 0,
        })
    }

    /// Try to lock at least `size` bytes from backing store, aligning them to requested address
    fn alloc(&mut self, mut size: usize, align: usize) -> Option<Header> {
        // we need to convert the size from count of bytes to count of usizes
        size = size / USIZELEN + 1;
        // begin accumulating capacity
        let mut start = Header(self.head);
        let mut offset = start.buffer().align_offset(align) / USIZELEN;
        let mut accumulated = 0;
        let mut next = start;
        // when the current guard sequence references a memory region smaller than the requested size,
        // an attempt is made to merge subsequent regions. This process continues until the required
        // capacity is satisfied or all available capacity is exhausted.
        while accumulated < size + offset {
            next.available().then_some(())?;
            next = next.next();
            accumulated = start.distance(&next);
            // in the event that the allocation wraps around to the initial guard without acquiring
            // the required capacity, it indicates that the underlying storage is insufficient to
            // meet the requested allocation. This suggests that the storage size needs to be
            // increased to accommodate the allocation demands.
            (next.0 != self.head).then_some(())?;
            // upon wrapping around to the start of the backing store, we discard the accumulated
            // capacity. This step is crucial to ensure a contiguous memory region is available for
            // the buffer allocation.
            if next < start {
                accumulated = 0;
                start = next;
                offset = start.buffer().align_offset(align) / USIZELEN;
            }
        }
        // If the difference between the accumulated capacity and the requested size is less than
        // or equal to MINALLOC, extend the allocation to the nearest multiple of 8 machine words
        // and update the head pointer accordingly.
        if (accumulated - size) > MINALLOC {
            // Otherwise, split the accumulated memory region into the requested size and
            // the remaining portion. Update the head pointer to the start of the remainder.
            self.head = unsafe { start.0.add(size + 1) };
            Header(self.head).store(next.0);
        } else {
            self.head = next.0;
        }
        // Update the start header with the new head pointer to
        // ensure the guard sequence points to the next header.
        start.store(self.head);
        Some(start)
    }
}

/// Macro to facilitate interaction with a thread-local ring allocator. This macro provides an
/// interface for initializing, allocating fixed-size buffers, and allocating extendable buffers
/// from a thread-local ring allocator instance, `RINGAL`.
///
/// # Usage
///
/// ## Initializing the Allocator
///
/// Use `ringal!(@init, capacity)` to initialize the thread-local ring allocator with the specified
/// capacity. This sets up a `RefCell` containing a `RingAl` instance, allowing each thread to have
/// its own instance initialized with the given capacity.
///
/// ## Allocating a Fixed-Size Buffer
///
/// Use `ringal!(@fixed, capacity)` to allocate a fixed-size buffer from the thread-local ring
/// allocator. This accesses the thread-local `RINGAL` and borrows it mutably to allocate a buffer
/// of the specified capacity.
///
/// ## Allocating an Extendable Buffer
///
/// Use `ringal!(@ext, capacity, callback)` to allocate an extendable buffer from the thread-local
/// ring allocator. The allocated buffer is passed to the provided callback. This allows for custom
/// handling of the extendable buffer upon allocation. The callback is necessary as LocalKey used
/// by TLS doesn't allow to reference to inner value to be passed around freely.
#[cfg(any(feature = "tls", test))]
#[macro_export]
macro_rules! ringal {
    (@init, $capacity: expr) => {
        use std::cell::RefCell;
        use $crate::RingAl;

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
        let mut next = Header(start);
        let mut capacity = 0;

        // Ensure all allocations are released before proceeding
        loop {
            // Busy wait until the current allocation is marked as available
            if !next.available() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            // Reconstruct the original capacity used for the backing store
            let nnext = next.next();
            // Check for possible wrap-around
            if nnext > next {
                capacity += next.capacity();
            }
            // Increment capacity to account for guard size
            capacity += USIZELEN;
            next = nnext;
            // If we have looped back to the starting point, all allocations
            // are released, and the full capacity is recalculated
            if next.0 == start {
                break;
            }
            let inner = next.0;
            head = head.min(inner)
        }
        let layout = unsafe { Layout::from_size_align_unchecked(capacity, 64) };
        // SAFETY:
        // 1. All pointers are guaranteed to lie within the original backing store.
        // 2. The initial slice length has been accurately recalculated.
        // 3. The starting memory address is determined through wrap-around detection.
        // 4. This is a controlled reclamation of a previously leaked boxed slice.
        unsafe { dealloc(head as *mut u8, layout) };
    }
}

/// Buffer types
mod buffer;
/// Essential header canaries
mod header;
#[cfg(test)]
/// test
mod tests;
