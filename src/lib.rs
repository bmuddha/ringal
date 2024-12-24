#![deny(missing_docs)]

//! RingAl - Efficient Ring Allocator for Short-lived Buffers
//!
//! RingAl is a highly efficient ring allocator designed specifically for the
//! allocation of short-lived buffers. The allocator operates in a circular manner,
//! which allows for fast and inexpensive buffer allocations, provided they are
//! ephemeral in nature. It is crucial that these allocations are short-lived;
//! otherwise, the allocator may become clogged with long-lived allocations,
//! rendering it inefficient.
//!
//! # Primary Use Case:
//!
//! 1. **Preallocation**: Establish a backing store of size `N` bytes.
//! 2. **Small Buffer Allocation**: Allocate buffers from the backing store, where
//!    `M < N`.
//! 3. **Buffer Utilization**: Use the allocated buffer across threads if
//!    necessary. Buffers can be cloned efficiently, akin to using an `Arc`.
//! 4. **Timely Deallocation**: Ensure buffers are dropped before the allocator
//!    cycles back to the same memory region.
//! 5. **Recycled Storage**: Upon buffer deallocation, the backing store becomes
//!    available for subsequent allocations.
//!
//! RingAl library focuses on a robust and versatile memory management system, through the use of
//! dynamic and self descriptive backing store. This system is engineered to accommodate a wide
//! range of allocation requirements through the use of guard sequences. These guard sequences are
//! capable of adjusting dynamically to different allocation conditions by being created, modified,
//! and removed as necessary. The system effectively manages these memory guards using a single
//! `usize` head pointer.
//!
//! The design is structured to ensure both safe and efficient multithreaded buffer operations. It
//! grants exclusive write access to one thread while allowing another thread to read
//! simultaneously. Although this design does inevitably lead to race conditions, particularly when
//! the writing thread releases the buffer, without proper synchronization with reading or
//! allocating thread, these issues are addressed through a method of optimistic availability
//! checks. If the allocating thread finds a buffer in use, it returns `None`, indicating to the
//! caller to retry the operation. This method avoids the need for expensive atomic synchronization
//! by relying on eventual consistency, which is suitable for the use cases of this allocator.
//!
//! It is important to note that this allocator is not marked as `Sync`, which restricts its
//! concurrent use across multiple threads. All allocation actions require `&mut self`, inherently
//! preventing the allocator from being enclosed within an `Arc`. Using locks around the allocator
//! is not recommended as it can greatly reduce performance; instead, it is advisable to use
//! thread-local storage.
//!
//! # Guard sequence insights:
//!
//! Each guard encodes:
//! 1. A flag indicating whether the guarded memory region is in use.
//! 2. The address of the next guard in the backing store.
//!
//! # Allocation Scenarios:
//!
//! When an allocation request is made, RingAl assesses the current guard, which
//! can result in one of four scenarios:
//!
//! 1. **Exact Fit**: The requested size matches the guarded region. The guard is
//!    marked as occupied, the pointer shifts to the next guard, and the buffer is
//!    returned.
//! 2. **Oversized Guard**: The guarded region exceeds the requested size. The
//!    region is split, a new guard is established for the remainder, and the
//!    buffer of the requested size is returned. This can lead to fragmentation.
//! 3. **Undersized Guard**: The guarded region is smaller than required. The
//!    allocator proceeds to merge subsequent regions until the requested size is
//!    met, effectively defragmenting the storage. Only the initial guard persists.
//! 4. **Insufficient Capacity**: Even after merging, the accumulated buffer is
//!    insufficient. The allocation fails, returning `None`.
//!
//! ```plaintext
//!    allocator
//!       |
//!       v
//! -----------------------------------------------------------------------------------
//! | head canary | N bytes | guard1 | L bytes | guard2 | M bytes | ... | tail canary |
//! -----------------------------------------------------------------------------------
//!      |                    ^   |              ^   |              ^ |          ^
//!      |                    |   |              |   |              | |          |
//!      ----------------------   ----------------   ---------------- ------------
//!      ^                                                                       |
//!      |                                                                       |
//!      -------------------------------------------------------------------------
//! ```
//!
//! _*Note*_: `Head` and `Tail` canaries are standard guard sequences that persist,
//! with the `Tail` canary perpetually pointing to the `Head`, forming a circular
//! (ring) structure.
//!
//! # Features
//!
//! 1. **Dynamic Fragmentation and Defragmentation**: Facilitates variable-size
//!    allocations through adaptive backing store management.
//! 2. **Extendable Buffers**: Allow dynamic reallocations akin to `Vec<u8>`,
//!    typically inexpensive due to minimal pointer arithmetic and no data copy.
//!    Such reallocations may fail if capacity limits are reached.
//! 3. **Fixed-Size Buffers**: Unexpandable but more efficient due to simpler
//!    design, with safe cross-thread transportation. They make storage available
//!    upon deallocation.
//! 4. **Read-Only Buffers**: Fixed-size buffers that are easily cloneable and
//!    distributable across multiple threads. These involve an additional heap
//!    allocation for a reference counter and should be avoided unless necessary to
//!    prevent overhead.
//!
//! # Optional Crate Features (Cargo)
//! 1. **`tls` (Thread-Local Storage):** This feature enables advanced
//!    functionalities related to thread-local storage within the allocator. By
//!    activating `tls`, developers can initiate allocation requests from any point
//!    in the codebase, thereby eliminating the cumbersome need to pass the
//!    allocator instance explicitly. This enhancement streamlines code ergonomics,
//!    albeit with a slight performance trade-off due to the utilization of
//!    `RefCell` for managing thread-local data.
//! 2. **`drop` (Allocator Deallocation):** Typically, the allocator is designed to
//!    remain active for the duration of the application's execution. However, in
//!    scenarios where early deallocation of the allocator and its associated
//!    resources is required, activating the `drop` feature is essential. This
//!    feature implements a tailored `Drop` mechanism that blocks (by busy wating)
//!    the executing thread until all associated allocations are conclusively
//!    released, subsequently deallocating the underlying storage. It is critical
//!    to ensure allocations do not extend significantly beyond the intended drop
//!    point. Failure to enable this feature will result in a memory leak upon
//!    attempting to drop the allocator.
//!
//! # Usage examples
//! ## Extendable buffer
//! ```rust
//! # use std::io::Write;
//! # use ringal::RingAl;
//! let mut allocator = RingAl::new(1024); // Create an allocator with initial size
//! let mut buffer = allocator.extendable(64).unwrap();
//! // the slice length exceeds preallocated capacity of 64
//! let msg = b"hello world, this message is longer than allocated capacity, but buffer will
//! grow as needed during the write, provided that allocator still has necessary capacity";
//! // but we're still able to write the entire message, as the buffer grows dynamically
//! let size = buffer.write(msg).unwrap();
//! // until the ExtBuf is finalized or dropped no further allocations are possible
//! let fixed = buffer.finalize();
//! assert_eq!(fixed.as_ref(), msg);
//! assert_eq!(fixed.len(), size);
//! ```
//! ## Fixed buffer
//! ```rust
//! # use std::io::Write;
//! # use ringal::RingAl;
//! let mut allocator = RingAl::new(1024); // Create an allocator with initial size
//! let mut buffer = allocator.fixed(256).unwrap();
//! let size = buffer.write(b"hello world, this message is relatively short").unwrap();
//! // we have written some some bytes
//! assert_eq!(buffer.len(), size);
//! // but we still have some capacity left for more writes if necessary
//! assert!(buffer.spare() >= 256 - size);
//! ```
//! ## Multi-threaded environment
//! ```rust
//! # use std::io::Write;
//! # use std::sync::mpsc::channel;
//! # use ringal::{ RingAl, FixedBufMut, FixedBuf };
//!
//! let mut allocator = RingAl::new(1024); // Create an allocator with initial size
//! let (tx, rx) = channel();
//! let mut buffer = allocator.fixed(64).unwrap();
//! let _ = buffer.write(b"this a message to other thread").unwrap();
//! // send the buffer to another thread
//! let handle = std::thread::spawn(move || {
//!     let buffer: FixedBufMut = rx.recv().unwrap();
//!     // from another thread, freeze the buffer, making it readonly
//!     let readonly = buffer.freeze();
//!     let mut handles = Vec::with_capacity(16);
//!     for i in 0..16 {
//!         let (tx, rx) = channel();
//!         // send the clones (cheap) of readonly buffer to more threads
//!         let h = std::thread::spawn(move || {
//!             let msg: FixedBuf = rx.recv().unwrap();
//!             let msg = std::str::from_utf8(&msg[..]).unwrap();
//!             println!("{i}. {msg}");
//!         });
//!         tx.send(readonly.clone());
//!         handles.push(h);
//!     }
//!     for h in handles {
//!         h.join();
//!     }
//! });
//! tx.send(buffer);
//! handle.join();
//! ```
//!
//! ## Thread Local Storage
//! ```rust
//! # use ringal::ringal;
//! # use std::io::Write;
//! ringal!(@init, 1024);
//! // allocate fixed buffer
//! let mut fixed = ringal!(@fixed, 64).unwrap();
//! let _ = fixed.write(b"hello world!").unwrap();
//! // allocate extendable buffer and write some data to it
//! ringal!{@ext, 64, |mut extendable| {
//!     let _ = extendable.write(b"hello world!").unwrap();
//!     extendable.finalize()
//! }};
//! ```
//!
//!
//! # Dependencies
//! The crate is designed without any external dependencies, and only relies on standard library
//!
//! # Planned features
//! - Allocation of buffers with generic types
//!
//! # Safety
//! This library is the epitome of cautious engineering! Well, that's what we'd
//! love to claim, but the truth is it's peppered with `unsafe` blocks. At times,
//! it seems like the code is channeling its inner C spirit, with raw pointer
//! operations lurking around every corner. But in all seriousness, considerable
//! effort has been devoted to ensuring that the safe API exposed by this crate is
//! truly safe for users and doesn't invite any unwelcome Undefined Behaviors or
//! other nefarious calamities. Proceed with confidence...

use std::alloc::{alloc, Layout};

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

    /// Try to allocate an extandable buffer with at least `size` bytes
    /// of initial capacity, the operation will fail, returning `None`,
    /// if backing store doesn't have enough capacity available.
    ///
    /// NOTE: the allocated buffer contains an exclusive reference to
    /// allocator (used to dynamically grow the buffer), which effectively
    /// prevents any further allocations while this buffer is around
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

    /// Try to lock at least `size` bytes from backing store
    fn alloc(&mut self, mut size: usize) -> Option<Header> {
        // we need to convert the size from count of bytes to count of usizes
        size = size / USIZELEN + 1;
        // begin accumulating capacity
        let mut start = Header(self.head);
        let mut accumulated = 0;
        let mut next = start;
        // when the current guard sequence references a memory region smaller than the requested size,
        // an attempt is made to merge subsequent regions. This process continues until the required
        // capacity is satisfied or all available capacity is exhausted.
        while accumulated < size {
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
                capacity += next.capacity() / USIZELEN;
            }
            // Increment capacity to account for guard size
            capacity += 1;
            next = nnext;
            // If we have looped back to the starting point, all allocations
            // are released, and the full capacity is recalculated
            if next.0 == start {
                break;
            }
            let inner = next.0;
            head = head.min(inner)
        }
        let slice = std::ptr::slice_from_raw_parts_mut(head, capacity);
        // SAFETY:
        // 1. All pointers are guaranteed to lie within the original backing store.
        // 2. The initial slice length has been accurately recalculated.
        // 3. The starting memory address is determined through wrap-around detection.
        // 4. This is a controlled reclamation of a previously leaked boxed slice.
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
