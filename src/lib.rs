//! `AtomicSlice<T>` is a thread-safe wrapper around an array of arbitrary data which
//! is just about as fast as possible to read while still being writable. Reading from
//! an `AtomicSlice<T>` involves exactly three atomic operations (in release builds).
//! Writing may involve some locking but is also possible from multiple threads.
//!
//! `AtomicSlice<T>` is thus heavily optimized for the case of frequent reads from
//! multiple threads with occasional updates from other threads.
//!
//! The size of the internal array is arbitrary, but is fixed during construction.
//!
//! Internally, `AtomicSlice<T>` allocates a pool of twice as much memory as requested,
//! which is partioned into two halves. During typical usage, on of these is being
//! read from exclusively while the other is available for writing. After a write,
//! the two partitions switch roles and new readers being accessing the freshly-written
//! data immediately, while existing readers guard access to the stale data until they
//! are dropped.

#[cfg(test)]
mod test;

use std::{
    cell::UnsafeCell,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

// Status 64-bit layout
// Byte 0 : active slice index
// Byte 1 : unused padding
// Byte 2 : slice 1 use count, low byte
// Byte 3 : slice 1 use count, high byte
// Byte 4 : unused padding
// Byte 5 : slice 2 use count, low byte
// Byte 6 : slice 2 use count, high byte
// Byte 7 : unused padding
// This provides 1 byte for active slice, and 2 bytes for each slice's
// use count. More slices could be accommodated by trading off the
// maximum number of simultaneous reads and the amount of padding.

#[doc(hidden)]
pub mod constants {
    pub const CURRENT_SLICE_MASK: u64 = 0x1;

    pub const SLICE_1_INC: u64 = 0x00_0000_00_0001_00_00;
    pub const SLICE_2_INC: u64 = 0x00_0001_00_0000_00_00;

    pub const VALID_STATUS_MASK: u64 = 0x00_FFFF_00_FFFF_00_01;

    pub const INC_ALL_SLICES: u64 = SLICE_1_INC | SLICE_2_INC;
}

fn slice_1_use_count(status: u64) -> u16 {
    ((status >> 16) & 0xFFFF) as u16
}

fn slice_2_use_count(status: u64) -> u16 {
    ((status >> 40) & 0xFFFF) as u16
}

fn slice_use_count(slice: u8, status: u64) -> u16 {
    match slice {
        0 => slice_1_use_count(status),
        1 => slice_2_use_count(status),
        _ => panic!("Invalid slice index"),
    }
}

fn valid_status(status: u64) -> bool {
    (status & !constants::VALID_STATUS_MASK) == 0
}

/// A slice of data that can be written and read from multiple threads,
/// which is heavily optimized for multiple concurrent reads and occasional
/// writes.
///
/// Reading from the slice involves only three atomic operations in total
/// (when compiled in release mode). Writing the data involves some locking
/// and is thus slower.
///
/// Internally, `AtomicSlice` allocates twice as much space as requested
/// during construction, and readers and writers switch back and forth
/// between accessing two partitions.
///
/// Currently, the data is stored indirectly in a boxed slice. In the future,
/// it may be stored directly within the `AtomicSlice` which would then
/// become a dynamically-sized type, giving more control to the user over the
/// amount of indirection involved.
pub struct AtomicSlice<T> {
    data: UnsafeCell<Box<[T]>>,
    stride: usize,
    status: AtomicU64,
    currently_writing: AtomicBool,
}

/// A smart pointer type representing read-only access to the data in an
/// `AtomicSlice`. When this type is dropped, it will release the read
/// lock on the `AtomicSlice`. In situations of high load where write
/// throughput is also important, this lock should ideally not be held
/// for very long.
pub struct AtomicSliceReadGuard<'a, T> {
    slice: &'a [T],
    current_slice: u8,
    status: &'a AtomicU64,
}

impl<T: Default + Clone> AtomicSlice<T> {
    /// Create a new `AtomicSlice` from a vector of data. The `AtomicSlice`
    /// will have the length of this vector for its entire lifetime.
    pub fn new(mut data: Vec<T>) -> AtomicSlice<T> {
        let stride = data.len();
        data.resize(stride * 2, T::default());
        data.shrink_to_fit();
        AtomicSlice {
            data: UnsafeCell::new(data.into_boxed_slice()),
            stride,
            status: AtomicU64::new(0),
            currently_writing: AtomicBool::new(false),
        }
    }

    /// Get the number of elements
    pub fn len(&self) -> usize {
        self.stride
    }

    /// Acquire a read lock on the slice. Never waits or blocks, and performs
    /// exactly two atomic operations (in release builds). The returned
    /// lock guard will be released when it is dropped, performing an additional
    /// single atomic operation.
    pub fn read<'a>(&'a self) -> AtomicSliceReadGuard<'a, T> {
        // Get current slice index while also marking all slices as in use.
        let status = self
            .status
            .fetch_add(constants::INC_ALL_SLICES, Ordering::SeqCst);

        debug_assert!(valid_status(status));
        debug_assert!(slice_1_use_count(status) < 0xFFFF);
        debug_assert!(slice_2_use_count(status) < 0xFFFF);

        let current_slice = (status & constants::CURRENT_SLICE_MASK) as u8;

        debug_assert!(slice_use_count(current_slice, self.status.load(Ordering::SeqCst)) > 0);

        // Now that the current slice is known, mark the others as no longer in use
        let inc_other_slice = if current_slice == 0 {
            constants::SLICE_2_INC
        } else {
            constants::SLICE_1_INC
        };
        let status = self.status.fetch_sub(inc_other_slice, Ordering::SeqCst);
        debug_assert!(valid_status(status));

        let stride = self.stride;
        let offset = current_slice as usize * stride;
        let slice: &[T] = unsafe {
            let ptr_box = self.data.get();
            let ptr_data = (*ptr_box).as_ptr();
            let ptr_begin = ptr_data.add(offset);
            std::slice::from_raw_parts(ptr_begin, stride)
        };

        debug_assert!(slice_use_count(current_slice, self.status.load(Ordering::SeqCst)) > 0);

        AtomicSliceReadGuard {
            slice,
            current_slice: current_slice,
            status: &self.status,
        }
    }

    /// Write a slice of new data. The given slice must have the same length as
    /// the `AtomicSlice` itself, otherwise this method panics.
    ///
    /// This method may block if other threads are writing and if any readers
    /// are holding lock guards for extended periods of time.
    pub fn write(&self, data: &[T]) {
        let stride = self.stride;
        if data.len() != stride {
            panic!("Attempted to write slice of the wrong length to AtomicSlice");
        }

        // Wait for exclusive access to the write portion
        while !self
            .currently_writing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            std::hint::spin_loop();
        }

        // Load the current status
        let status = self.status.load(Ordering::SeqCst);
        debug_assert!(valid_status(status));
        let i = (status & constants::CURRENT_SLICE_MASK) as u8;
        let next_i = i ^ 1;

        // Wait to ensure the next slice is not being used
        loop {
            let status = self.status.load(Ordering::SeqCst);
            debug_assert!(valid_status(status));
            if slice_use_count(next_i, status) == 0 {
                break;
            }
            std::hint::spin_loop();
        }

        // Copy data to the next slice
        let offset = (next_i as usize) * stride;
        let slice: &mut [T] = unsafe {
            let ptr_box = self.data.get();
            let ptr_data = (*ptr_box).as_mut_ptr();
            let ptr_begin = ptr_data.add(offset);
            std::slice::from_raw_parts_mut(ptr_begin, stride)
        };
        for (i, v) in slice.iter_mut().enumerate() {
            *v = data[i].clone();
        }

        // Point all new readers to the other slice
        let status = self.status.fetch_xor(1, Ordering::SeqCst);
        debug_assert!(valid_status(status));

        // Release exclusive access to the write portion
        self.currently_writing
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap();
    }
}

#[doc(hidden)]
impl<T> AtomicSlice<T> {
    pub unsafe fn raw_data(&self) -> *const T {
        let ptr_box = self.data.get();
        (*ptr_box).as_ptr()
    }

    pub unsafe fn raw_status(&self) -> *const AtomicU64 {
        &self.status
    }
}

unsafe impl<T: Send> Sync for AtomicSlice<T> {}
unsafe impl<T: Send> Send for AtomicSlice<T> {}

impl<'a, T> Deref for AtomicSliceReadGuard<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<'a, T> Drop for AtomicSliceReadGuard<'a, T> {
    fn drop(&mut self) {
        let inc_slice = if self.current_slice == 0 {
            constants::SLICE_1_INC
        } else {
            constants::SLICE_2_INC
        };
        let status = self.status.fetch_sub(inc_slice, Ordering::SeqCst);
        debug_assert!(valid_status(status));
        debug_assert!(slice_use_count(self.current_slice, status) > 0);
    }
}
