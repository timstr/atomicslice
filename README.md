# atomicslice

A Rust library for thread-safe shared slices that are just about as fast as possible to read while also being writable.

## Overview

Use `AtomicSlice<T>` like you would a `RwLock<[T]>`, and know that `.read()` is wait-free. Pass it between threads as `Arc<AtomicSlice<T>>` or between scoped threads as `&AtomicSlice<T>`, and call `.read()` and `.write()` as much as you like. The slice can be of any length at construction time, but subsequence writes must pass slices of the same length.

Reading from an `AtomicSlice<T>` is optimized to be wait-free and as fast as possible. Calling `.read()` results in a total of three atomic operations, and never blocks or otherwise spins or waits. Calling `.write()` in the other hand may result in some waiting.

## Implementation Details

Internally, `AtomicSlice<T>` stores a pool of multiple redundant slices, one of which is conceptually being read from while the others are conceptually ready for writing while possibly being read from by a few `.read()` stragglers. Which of these pools to read from is indicated by a shared atomic integer `status`, which uses bit-packing to also encode locking information for all slices simultaneously.

To read, the `status` is fetched and all pool use counts are incremented, in a single atomic operation. Once the active slice index is known, the other slices which aren't in use have their use counts decremented. Then, the active slice is guarded and may be read from as desired. After the client is doing reading, the active slice's use count is similarly decremented. The `.read()` method performs the first two operations and returns a lock guard object whose `drop()` method performs the third.

The `write()` method `AtomicSlice<T>` is effectively guarded by a mutex, such that writes are serialized. Once that is acquired, the `.write()` method locates a separate slice in the pool from the currently active one, and spins until its use count goes to zero. At this point, no new or current reads will access the out-of-use slice, and so the `.write()` method copies the supplied data into it. Finally, the index of the current slice is updated to point to the newly-filled slice, where readers will begin finding the new data.

Currently, a pool size of exactly two is used, which is the bare minimum but seems to work well enough. In the future, I may do some profiling to see what the tradeoffs are.

---

## Discussion

-   Is it safe to relax some of the atomic orderings to be less than `Ordering::SeqCst`?
    -   Idk, probably? Acquire and Release are probably the way to go for most of them, I just currently struggle to understand their exact implications.
-   Why not use the [arc-swap Crate](https://github.com/vorner/arc-swap)?
    -   Because I eventually plan to expose the internals as raw pointers and atomics operations to an LLVM-based JIT engine as part of another project. That project involves realtime DSP where arrays need to be continuously read and occasionally updated. The unusual intersection of requirements for wait-free code, a focus on array data, and the need to understand the low-level sequence of atomic operations required led me to write my own. That, and it was a fun exercise.
-   Couldn't you get away with implementing `.read()` as a single load from an `AtomicPtr`?
    -   Yes, if you're okay with leaking memory every time you write. Theoretically, you could implement this correctly by allocating and leaking an array everytime you call `.write()`, and then pointing all readers to it using a single atomic pointer only. To prevent a catastrophic leak like this, a minimum of two additional operations are needed to synchronize with the begin and end of a slice's use by the `.read()` method.
