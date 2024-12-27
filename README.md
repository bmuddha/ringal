# RingAl - Fast Ring Allocator for Temporary Buffers

[![Crates.io](https://img.shields.io/crates/v/ringal.svg)](https://crates.io/crates/ringal)
[![Documentation](https://docs.rs/ringal/badge.svg)](https://docs.rs/ringal)
[![Build Status](https://github.com/bmuddha/ringal/actions/workflows/test.yml/badge.svg)](https://github.com/bmuddha/ringal/actions)

## Table of Contents
1. [Overview](#overview)
   - Key Use Cases
2. [Design Philosophy](#design-philosophy)
   - Guard Mechanics
   - Allocation Scenarios
3. [Key Features](#key-features)
4. [Optional Crate Features (Cargo)](#optional-crate-features-cargo)
5. [Usage Examples](#usage-examples)
   - Extendable Buffer
   - Fixed Buffer
   - Multi-threaded Environment
   - Thread Local Storage
6. [Benchmarks](#benchmarks)
   - Parameter Definitions
   - Fixed Preallocated Buffer Results
   - Extendable Buffer Results (Chunked Writes)
   - Key Insights
   - System Specs
7. [Dependencies](#dependencies)
8. [Future Plans](#future-plans)
9. [Safety](#safety)

## Overview

**RingAl** is a high-performance ring allocator designed for quick buffer allocations that don't live long. It uses a circular allocation method for fast memory management, which is perfect for short-lived buffers. For best performance, buffers should be freed quickly to avoid slowing things down by jamming the allocator.

### Key Use Cases:

1. **Preallocation**: Set up a memory pool with a defined size `N` bytes.
2. **Small Buffer Allocation**: Allocate small buffers from the pool, each less than `N`.
3. **Cross-thread Buffer Usage**: Share buffers across threads efficiently using lightweight cloning, similar to `Arc`.
4. **Timely Deallocation**: Release buffers before reusing the same memory, for performance.
5. **Recycling Storage**: Freed buffers automatically go back into the pool for reallocation.

# Design Philosophy

RingAl focuses on efficient memory management with a dynamic memory pool that
evolves its backing store using something called guard sequences. Guard
sequences are usually exclusively owned by allocator when they are not locked
(i.e. not guarding the allocation), and when they do become locked, the memory
region (single usize) backing the guard sequence, is only accessed using Atomic
operations. This allows for one thread (the owner of buffer) to release the
guard (write to guard sequence), when the buffer is no longer in use and the
other thread, where allocator is running, to t perform synchronized check of
whether guard sequence have been released.  


The design allows for safe multi-threaded access: one thread can write while
one other can read. If a buffer is still occupied after allocator wraps around,
the allocation attempt just returns `None`, prompting a retry.

Note: RingAl itself isn't `Sync`, so it cannot be accessed from multiple
threads concurrently. Instead, using thread-local storage is encouraged.

## Guard Mechanics:

Guards help manage memory by indicating:

1. Whether guarded memory is currently in use.
2. Address of the next guard in the memory pool.

## Allocation Scenarios:

When you request memory, RingAl checks the current guard and does one of the following:

1. **Exact Fit**: Uses the guard if it matches the request size, marking memory as used.
2. **Oversized Guard**: Splits the guard if it's larger than needed, which might fragment memory.
3. **Undersized Guard**: Combines adjacent regions until the request is satisfied, which helps defragment the memory.
4. **Insufficient Capacity**: Returns `None` if it can't satisfy the request, even after merging.

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

_Note_: `Head` and `Tail` canaries are fixed regular guards, with the exception that they always persist, and Tail guard always points to Head, forming a ring.

## Key features

1. **Dynamic Fragmentation Handling**: Manages different allocation sizes with adaptive memory handling.
2. **Expandable Buffers**: Can grow like `Vec<u8>`, unless it hits capacity limits.
3. **Fixed-Size Buffers**: Efficient, fixed sized buffer, but can be sent across threads.
4. **Read-Only Buffers**: Cloneable and shareable across threads. They're
   efficient but involve extra heap allocation for reference counting, so use
   them wisely.
5. Generic `Vec<T>` like fixed capacity buffers, these can be allocated from
   the same backing store as regular `u8` buffers
6. **Thread local storage allocator**: allocator instance can be created for
   each thread, and accessed from anywhere in the code, removeing the need to
   pass the allocator around

For more details, check the [RingAl Documentation](https://docs.rs/ringal).

## Optional Crate Features (Cargo)
1. **`tls` (Thread-Local Storage):** Adds thread-local storage, simplifying allocation calls, but might slow things slightly due to `RefCell` usage.
2. **`drop` (Allocator Deallocation):** Normally, the allocator lives for the app's duration, but this feature allows early deallocation when needed. It waits for all allocations to finish before freeing memory to prevent leaks, blocks the thread if there're active allocations.

# Usage Examples

## Extendable Buffer
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let mut buffer = allocator.extendable(64).unwrap();
let msg = b"hello world, this message is longer than the allocated capacity but will grow as needed.";
let size = buffer.write(msg).unwrap();
let fixed = buffer.finalize();
assert_eq!(fixed.as_ref(), msg);
assert_eq!(fixed.len(), size);
```

## Fixed Buffer
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let mut buffer = allocator.fixed(256).unwrap();
let size = buffer.write(b"hello world, this message is relatively short").unwrap();
assert_eq!(buffer.len(), size);
assert_eq!(buffer.spare(), 256 - size);
```

## Generic Buffer
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
struct MyType {
    field1: usize,
    field2: u128
}
let mut buffer = allocator.generic::<MyType>(16).unwrap();
buffer.push(MyType { field1: 42, field2: 43 });
assert_eq!(buffer.len(), 1);
let t = buffer.pop().unwrap();
assert_eq!(t.field1, 42);
assert_eq!(t.field2, 43);
```

## Multi-threaded Environment
```rust
let mut allocator = RingAl::new(1024); // Create an allocator with initial size
let (tx, rx) = channel();
let mut buffer = allocator.fixed(64).unwrap();
let _ = buffer.write(b"message to another thread").unwrap();
let handle = std::thread::spawn(move || {
    let buffer = rx.recv().unwrap();
    let readonly = buffer.freeze();
    let mut handles = Vec::with_capacity(16);
    for i in 0..16 {
        let (tx, rx) = channel();
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

## Thread Local Storage 
```rust
// init the thread local allocator, should be called just once, any thread
spawned afterwards, will have access to their own instance of the allocator
ringal!(@init, 1024);
// allocate fixed buffer
let mut fixed = ringal!(@fixed, 64).unwrap();
let _ = fixed.write(b"hello world!").unwrap();
// allocate extendable buffer, and pass a callback to operate on it
// this approach is necessary as ExtBuf contains reference to thread local storage, 
// and LocalKey doesn't allow any references to exist outside of access callback 
let fixed = ringal!{@ext, 64, |extendable| {
    let _ = extendable.write(b"hello world!").unwrap();
    // it's ok to return FixedBuf(Mut) from callback
    extendable.finalize()
}};
println!("bytes written: {}", fixed.len());
```

# Benchmarks

Comparisons between `RingAl` and the system allocator (`Vec<u8>`) show performance gains in various scenarios.

## Parameter Definitions
- **Iterations**: Number of operations performed
- **Buffer Size**: Maximum bytes written per buffer, the actual number is random
- **Max Buffers**: Maximum number of buffers held after allocation

## Fixed Preallocated Buffer Results

| Iterations | Buffer Size | Max Buffers | ringal (ms) | vec (ms) | Improvement |
|------------|-------------|-------------|-------------|----------|-------------|
| 10,000,000 | 64         | 64          | 696.7       | 998.0    | 1.43x      |
| 10,000,000 | 1,024      | 64          | 923.4       | 2,062.0  | 2.23x      |
| 10,000,000 | 1,024      | 1,024       | 913.7       | 1,622.0  | 1.77x      |
| 1,000,000  | 65,536     | 64          | 932.6       | 1,718.0  | 1.84x      |
| 1,000,000  | 131,072    | 1,024       | 1,709.0     | 3,960.0  | 2.32x      |

## Extendable Buffer Results (Chunked Writes)

| Iterations | Buffer Size | Max Buffers | ringal (ms) | vec (ms) | Improvement |
|------------|-------------|-------------|-------------|----------|-------------|
| 10,000,000 | 64         | 64          | 834.0       | 1,120.0  | 1.34x      |
| 10,000,000 | 1,024      | 64          | 1,164.0     | 3,228.0  | 2.77x      |
| 10,000,000 | 1,024      | 1,024       | 1,205.0     | 3,044.0  | 2.53x      |
| 1,000,000  | 65,536     | 64          | 1,958.0     | 3,646.0  | 1.86x      |
| 1,000,000  | 131,072    | 1,024       | 3,588.0     | 9,008.0  | 2.51x      |

## Key Insights

1. `RingAl` consistently beats `Vec<u8>` in all cases.
2. Gains range from 1.34x to 2.77x.
3. Bigger buffers show the most improvement.
4. Both fixed and extendable buffers show significant speedups.

## System Specs

Benchmarks done with hyperfine, running each case 10 times. Shown times are averages.

- **Device:** Apple MacBook Air
- **Processor:** Apple M2
- **Memory:** 16 GB RAM

# Dependencies
RingAl doesn't rely on any third-party crates, only the Rust standard library.

# Future Plans
- Support for generic type buffer allocations.

# Safety
The implementation uses a lot of unsafe Rust, this is an allocator after all,
so raw pointers are inevitable (some parts of code look like they were written
in C :)). A lot of effort was put into ensuring that the code is safe from UB
and data races, and the API is safe to use. Nonetheless all contributions to
making it even safer are always welcome. 
