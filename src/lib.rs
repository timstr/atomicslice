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
// Byte 2 : slice 0 use count, low byte
// Byte 3 : slice 0 use count, high byte
// Byte 4 : unused padding
// Byte 5 : slice 1 use count, low byte
// Byte 6 : slice 1 use count, high byte
// Byte 7 : unused padding
// This provides 1 byte for active slice, and 2 bytes for each slice's
// use count. More slices could be accommodated by trading off the
// maximum number of simultaneous reads and the amount of padding.

const SLICE_1_INC: u64 = 0x00_0000_00_0001_00_00;
const SLICE_2_INC: u64 = 0x00_0001_00_0000_00_00;

const VALID_STATUS_MASK: u64 = 0x00_FFFF_00_FFFF_00_01;

const INC_ALL_SLICES: u64 = SLICE_1_INC | SLICE_2_INC;

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
    (status & !VALID_STATUS_MASK) == 0
}

pub struct AtomicSlice<T> {
    data: UnsafeCell<Box<[T]>>,
    stride: usize,
    status: AtomicU64,
    currently_writing: AtomicBool,
}

pub struct AtomicSliceReadGuard<'a, T> {
    slice: &'a [T],
    current_slice: u8,
    status: &'a AtomicU64,
}

impl<T: Default + Clone> AtomicSlice<T> {
    pub fn new(mut data: Vec<T>) -> AtomicSlice<T> {
        let stride = data.len();
        data.resize(stride * 2, T::default());
        AtomicSlice {
            data: UnsafeCell::new(data.into_boxed_slice()),
            stride,
            status: AtomicU64::new(0),
            currently_writing: AtomicBool::new(false),
        }
    }

    pub fn read<'a>(&'a self) -> AtomicSliceReadGuard<'a, T> {
        // Get current slice index while also marking all slices as in use.
        let status = self.status.fetch_add(INC_ALL_SLICES, Ordering::SeqCst);

        debug_assert!(valid_status(status));
        debug_assert!(slice_1_use_count(status) < 0xFFFF);
        debug_assert!(slice_2_use_count(status) < 0xFFFF);

        let current_slice = (status & 1) as u8;

        debug_assert!(slice_use_count(current_slice, self.status.load(Ordering::SeqCst)) > 0);

        // Now that the current slice is known, mark the others as no longer in use
        let inc_other_slice = if current_slice == 0 {
            SLICE_2_INC
        } else {
            SLICE_1_INC
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
        let i = (status & 1) as u8;
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
            SLICE_1_INC
        } else {
            SLICE_2_INC
        };
        let status = self.status.fetch_sub(inc_slice, Ordering::SeqCst);
        debug_assert!(valid_status(status));
        debug_assert!(slice_use_count(self.current_slice, status) > 0);
    }
}
