# About
RingAl - ring allocator, this crate provides fast, cheap and efficient
allocations for short lived buffers. The key point here is short lived, as
allocation happen in circular fashion, and long lived allocations will simply
jam the allocator making it unusable.

Main use case:
1. Preallocate backing store of size N bytes
2. Allocate small buffers from backing store of size M < N
3. Make use of buffer, send it to other threads if necessary, cheaply clone it
   (just like Arc)
4. Drop the buffer before the allocator wraps around the ring to the same
   memory region
5. When buffer is dropped, the backing store becomes available for new
   allocations

# Design
The key feature of RingAl is a dynamic and self describing backing store, which
constantly evolves its own structure as program runs. This dynamic nature is
made possible by guard sequences which appear, change and disappear to
accomadate various allocation requests. The implentation is quite simple, it
uses a single usize pointer which points to memory guard. The guard is only
ever accessed using atomic CPU instructions, this allows for allocated buffers
to be used in a multithreaded environments, for a small access overhead.
Although it should be noted that allocator itsefl is not `Sync` and cannot be
accessed concurrently from different threads.

Each guard contains two pieces of information:
1. Indicator flag of whether or not the guarded memory region is in use
2. Address of the next guard in backing store

When allocation request is made, the RingAl checks current guard (wherever in
backing store that might be at present moment), then 4 outcomes are possible:
1. Guard is guarding memory region of requested size, ideal scenario, the guard
   is just marked as taken, pointer is shifted to next guard , and buffer
   containing the guarded memory region is returned
2. Guard is guarding memory region larger than requested size, the allocator
   splits the guarded region into that of `requested` size and the rest,
   creates a new guard at the start of `rest`, shifts the pointer to newly
   created guard and returns the buffer with `requested` region. This scenario
   leads to fragmentation of backing store.
3. Guard is guarding memory region smaller than requested size, the allocator
   jumps to next guard, adds its size to currently accumulated buffer size. If
   the accumulated buffer size is larger or equal to requested one, than we
   stop and returned merged memory buffers, otherwise we keep consuming other
   guards until we get the size we want. Only first guard survives this
   process, other guards effectively become part of usable buffer and thus are
   removed. This scenario leads to defragmentation of backing store.
4. Guard is guarding memory region smaller than requested size, but after
   merging all the available memory regions the allocator cannot accumulate
   enough buffer size to accomadate allocation request, in this scenario the
   allocation simply fails, and `None` is returned to caller.

```
   allocator
      |
      v
-----------------------------------------------------------------------------------
| head canary | N bytes | guard1 | L bytes | guard2 | M bytes | ... | tail canary |
-----------------------------------------------------------------------------------
     |                    ^   |              ^   |              ^ |          ^ 
     |                    |   |              |   |              | |          |
     ----------------------   ----------------   ---------------- ------------

```

_Notes_: `head` and `tail` canaries are regular guard sequences, but they never
disappear, in addition, tail canary always points to head canary, thus forming
a ring structure.

# Features
1. Dynamic fragmentation/defragmentation of backing store, that facilitate
   variable size allocations.
2. Extendable buffers: if the caller doesn't know in advance the exact size of
   required allocation, extendable buffers allow for dynamic reallocations,
   just like `Vec<u8>`, but unlike `Vec`, this type of reallocation is usually
   very cheap, as it involves a few pointer arithmetic operations and no data
   cloning. Due to limited capacity of backing store such reallocations might
   fail if the capacity is exhausted.
3. Fixed size buffers: as the name suggest, this type of buffer cannot be
   grown, but is more efficient than extendable one due to simpler
   implementation. This type of buffer can be safely sent to other threads. The
   Drop implementation will automatically take care of making its backing store
   available for future allocations once the buffer is not longer needed.
4. Read only buffers: fixed size buffers which can be cheaply cloned (one
   atomic addition) and sent to multiple threads. Drop implementation will make
   sure that once all references to buffer are dropped, the backing store
   becomes available again. The creation of this buffer type involves one extra
   allocation from heap (for reference counter) and thus shuld be avoided if no
   cloning is needed.
