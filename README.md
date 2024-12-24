# RingAl - Efficient Ring Allocator for Short-lived Buffers

[![Crates.io](https://img.shields.io/crates/v/ringal.svg)](https://crates.io/crates/ringal)
[![Documentation](https://docs.rs/ringal/badge.svg)](https://docs.rs/ringal)
[![Build Status](https://github.com/bmuddha/ringal/actions/workflows/test.yml/badge.svg)](https://github.com/bmuddha/ringal/actions)

## Overview

**RingAl** is a highly efficient ring allocator designed specifically for the
allocation of short-lived buffers. The allocator operates in a circular manner,
which allows for fast and inexpensive buffer allocations, provided they are
ephemeral in nature. It is crucial that these allocations are short-lived;
otherwise, the allocator may become clogged with long-lived allocations,
rendering it inefficient.

### Primary Use Case:

1. **Preallocation**: Establish a backing store of size `N` bytes.
2. **Small Buffer Allocation**: Allocate buffers from the backing store, where
   `M < N`.
3. **Buffer Utilization**: Use the allocated buffer across threads if
   necessary. Buffers can be cloned efficiently, akin to using an `Arc`.
4. **Timely Deallocation**: Ensure buffers are dropped before the allocator
   cycles back to the same memory region.
5. **Recycled Storage**: Upon buffer deallocation, the backing store becomes
   available for subsequent allocations.

# Design Philosophy
RingAl library focuses on a robust and versatile memory management system, through the use of dynamic and self descriptive backing store. This system is engineered to accommodate a wide range of allocation requirements through the use of guard sequences. These guard sequences are capable of adjusting dynamically to different allocation conditions by being created, modified, and removed as necessary. The system effectively manages these memory guards using a single `usize` head pointer.

The design is structured to ensure both safe and efficient multithreaded buffer operations. It grants exclusive write access to one thread while allowing another thread to read simultaneously. Although this design does inevitably lead to race conditions, particularly when the writing thread releases the buffer, without proper synchronization with reading or allocating thread, these issues are addressed through a method of optimistic availability checks. If the allocating thread finds a buffer in use, it returns `None`, indicating to the caller to retry the operation. This method avoids the need for expensive atomic synchronization by relying on eventual consistency, which is suitable for the use cases of this allocator.

It is important to note that this allocator is not marked as `Sync`, which restricts its concurrent use across multiple threads. All allocation actions require `&mut self`, inherently preventing the allocator from being enclosed within an `Arc`. Using locks around the allocator is not recommended as it can greatly reduce performance; instead, it is advisable to use thread-local storage.

### Guard Insights:

Each guard encodes:
1. A flag indicating whether the guarded memory region is in use.
2. The address of the next guard in the backing store.

### Allocation Scenarios:

When an allocation request is made, RingAl assesses the current guard, which
can result in one of four scenarios:

1. **Exact Fit**: The requested size matches the guarded region. The guard is
   marked as occupied, the pointer shifts to the next guard, and the buffer is
   returned.
2. **Oversized Guard**: The guarded region exceeds the requested size. The
   region is split, a new guard is established for the remainder, and the
   buffer of the requested size is returned. This can lead to fragmentation.
3. **Undersized Guard**: The guarded region is smaller than required. The
   allocator proceeds to merge subsequent regions until the requested size is
   met, effectively defragmenting the storage. Only the initial guard persists.
4. **Insufficient Capacity**: Even after merging, the accumulated buffer is
   insufficient. The allocation fails, returning `None`.

```plaintext
   allocator
      |
      v
-----------------------------------------------------------------------------------
| head canary | N bytes | guard1 | L bytes | guard2 | M bytes | ... | tail canary |
-----------------------------------------------------------------------------------
     |                    ^   |              ^   |              ^ |          ^ 
     |                    |   |              |   |              | |          |
     ----------------------   ----------------   ---------------- ------------
     ^                                                                       |
     |                                                                       |
     -------------------------------------------------------------------------
```

_*Note*_: `Head` and `Tail` canaries are standard guard sequences that persist,
with the `Tail` canary perpetually pointing to the `Head`, forming a circular
(ring) structure.

## Features

1. **Dynamic Fragmentation and Defragmentation**: Facilitates variable-size
   allocations through adaptive backing store management.
2. **Extendable Buffers**: Allow dynamic reallocations akin to `Vec<u8>`,
   typically inexpensive due to minimal pointer arithmetic and no data copy.
   Such reallocations may fail if capacity limits are reached.
3. **Fixed-Size Buffers**: Unexpandable but more efficient due to simpler
   design, with safe cross-thread transportation. They make storage available
   upon deallocation.
4. **Read-Only Buffers**: Fixed-size buffers that are easily cloneable and
   distributable across multiple threads. These involve an additional heap
   allocation for a reference counter and should be avoided unless necessary to
   prevent overhead.

For more details, visit the [RingAl Documentation](https://docs.rs/ringal).

## Optional Crate Features (Cargo)
1. **`tls` (Thread-Local Storage):** This feature enables advanced
   functionalities related to thread-local storage within the allocator. By
   activating `tls`, developers can initiate allocation requests from any point
   in the codebase, thereby eliminating the cumbersome need to pass the
   allocator instance explicitly. This enhancement streamlines code ergonomics,
   albeit with a slight performance trade-off due to the utilization of
   `RefCell` for managing thread-local data.
2. **`drop` (Allocator Deallocation):** Typically, the allocator is designed to
   remain active for the duration of the application's execution. However, in
   scenarios where early deallocation of the allocator and its associated
   resources is required, activating the `drop` feature is essential. This
   feature implements a tailored `Drop` mechanism that blocks (by busy wating)
   the executing thread until all associated allocations are conclusively
   released, subsequently deallocating the underlying storage. It is critical
   to ensure allocations do not extend significantly beyond the intended drop
   point. Failure to enable this feature will result in a memory leak upon
   attempting to drop the allocator.

## Usage examples
### Extendable buffer
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let mut buffer = allocator.extendable(64).unwrap();
// the slice length exceeds preallocated capacity of 64
let msg = b"hello world, this message is longer than allocated capacity, but buffer will
grow as needed during the write, provided that allocator still has necessary capacity";
// but we're still able to write the entire message, as the buffer grows dynamically
let size = buffer.write(msg).unwrap();
// until the ExtBuf is finalized or dropped no further allocations are possible
let fixed = buffer.finalize();
assert_eq!(fixed.as_ref(), msg);
assert_eq!(fixed.len(), size);
```
### Fixed buffer
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let mut buffer = allocator.fixed(256).unwrap();
let size = buffer.write(b"hello world, this message is relatively short").unwrap();
// we have written some some bytes 
assert_eq!(buffer.len(), size);
// but we still have some capacity left for more writes if necessary
assert_eq!(buffer.spare(), 256 - size);
```
### Multi-threaded environment
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let (tx, rx) = channel();
let mut buffer = allocator.fixed(64).unwrap();
let _ = buffer.write(b"this a message to other thread").unwrap();
// send the buffer to another thread
let handle = std::thread::spawn(move || {
    let buffer = rx.recv().unwrap();
    // from another thread, freeze the buffer, making it readonly
    let readonly = buffer.freeze();
    let mut handles = Vec::with_capacity(16);
    for i in 0..16 {
        let (tx, rx) = channel();
        // send the clones (cheap) of readonly buffer to more threads
        let h = std::thread::spawn(move || {
            let msg = rx.recv().unwrap();
            let msg = std::str::from_utf8(&msg[..]).unwrap();
            println!("{i}. {msg}");
        });
        tx.send(readonly.clone());
        handles.push(h);
    }
    for h in handles {
        h.join();
    }
});
tx.send(buffer);
handle.join();
```

### Thread Local Storage 
```rust
ringal!(@init, 1024);
// allocate fixed buffer
let mut fixed = ringal!(@fixed, 64).unwrap();
let _ = fixed.write(b"hello world!").unwrap();
// allocate extendable buffer and write some data to it
ringal!{@ext, 64, |extendable| {
    let _ = extendable.write(b"hello world!").unwrap();
    extendable.finalize()
}};
```

# Benchmarks

Benchmark comparisons are made between two buffer allocator implementations:
`RingAl` and `System allocator` used by `Vec<u8>`. The benchmarks measure
performance across different scenarios with varying parameters.

## Parameters Explanation
- **Iterations**: Number of operations performed
- **Buffer Size**: Maximum number of bytes that could be written to buffer, the actual number is random
- **Max Buffers**: Maximum number of buffers that are kept around after allocation

## Fixed Preallocated Buffer Results
*Scenario:*
1. Allocate the buffer with respective allocator, the buffer capacity is equal
   to or greater than the size of data to be written
2. Write random number of bytes via `Write` trait implementation, upper limit
   is capped at `Buffer Size`
3. Send the buffer to another thread via unbounded channel, to prevent immediate deallocation
4. After enough buffers are collected on other thread, drop them all in one batch
5. Perform this sequence `Iterations` times

| Iterations | Buffer Size | Max Buffers | ringal (ms) | vec (ms) | Speed Improvement |
|------------|-------------|-------------|-------------|----------|-------------------|
| 10,000,000 | 64         | 64          | 696.7       | 998.0    | 1.43x            |
| 10,000,000 | 1,024      | 64          | 923.4       | 2,062.0  | 2.23x            |
| 10,000,000 | 1,024      | 1,024       | 913.7       | 1,622.0  | 1.77x            |
| 1,000,000  | 65,536     | 64          | 932.6       | 1,718.0  | 1.84x            |
| 1,000,000  | 131,072    | 1,024       | 1,709.0     | 3,960.0  | 2.32x            |

## Extendable Buffer Results (Chunked Writes)
*Scenario:*
1. Create an empty buffer with respective allocator, the buffer capacity is 0
2. Write random number of bytes via `Write` trait implementation, upper limit
   is capped at `Buffer Size`, the writes are performed in chunks of 64 bytes,
   forcing the buffers to grow dynamically and keep reallocating.
3. Send the buffer to another thread via unbounded channel, to prevent
   immediate deallocation
4. After enough buffers are collected on other thread, drop them all in one batch
5. Perform this sequence `Iterations` times

| Iterations | Buffer Size | Max Buffers | ringal (ms) | vec (ms) | Speed Improvement |
|------------|-------------|-------------|-------------|----------|-------------------|
| 10,000,000 | 64         | 64          | 834.0       | 1,120.0  | 1.34x            |
| 10,000,000 | 1,024      | 64          | 1,164.0     | 3,228.0  | 2.77x            |
| 10,000,000 | 1,024      | 1,024       | 1,205.0     | 3,044.0  | 2.53x            |
| 1,000,000  | 65,536     | 64          | 1,958.0     | 3,646.0  | 1.86x            |
| 1,000,000  | 131,072    | 1,024       | 3,588.0     | 9,008.0  | 2.51x            |

## Key Findings

1. The `ringal` implementation consistently outperforms the `vec` implementation across all test scenarios
2. Performance improvements range from 1.34x to 2.77x faster
3. Largest performance gains are observed with larger buffer sizes
4. Both fixed preallocated and extendable buffer scenarios show significant improvements

## System Information

All benchmarks were run using hyperfine with 10 runs per test case. Times shown are mean values.

This setup was used to run all benchmarks mentioned in this document:
- **Device:** Apple MacBook Air
- **Processor:** Apple M2
- **Memory:** 16 GB RAM

## Dependencies
The crate is designed without any external dependencies, and only relies on standard library

## Planned features
- Allocation of buffers with generic types

# Safety
This library is the epitome of cautious engineering! Well, that's what we'd
love to claim, but the truth is it's peppered with `unsafe` blocks. At times,
it seems like the code is channeling its inner C spirit, with raw pointer
operations lurking around every corner. But in all seriousness, considerable
effort has been devoted to ensuring that the safe API exposed by this crate is
truly safe for users and doesn't invite any unwelcome Undefined Behaviors or
other nefarious calamities. Proceed with confidence...
