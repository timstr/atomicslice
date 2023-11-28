#[cfg(test)]
mod test;

use std::{
    cell::UnsafeCell,
    ops::{BitXor, Deref},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const CURRENT_SLICE_MASK: usize = 0x1;
const CURRENT_SLICE_SHIFT: usize = 0;
const NUM_SLICES: usize = 2; // bare minimum is 2. Consider making this larger and profiling

const SLICE_1_MASK: usize = 0xFFFF_00;
const SLICE_1_SHIFT: usize = 8;

const SLICE_2_MASK: usize = 0xFFFF_0000_00;
const SLICE_2_SHIFT: usize = 24;

const SLICE_1_INC: usize = 1 << SLICE_1_SHIFT;
const SLICE_2_INC: usize = 1 << SLICE_2_SHIFT;

const INC_ALL_SLICES: usize = SLICE_1_INC | SLICE_2_INC;

fn slice_1_use_count(status: usize) -> usize {
    (status & SLICE_1_MASK) >> SLICE_1_SHIFT
}

fn slice_2_use_count(status: usize) -> usize {
    (status & SLICE_2_MASK) >> SLICE_2_SHIFT
}

fn slice_use_count(slice: usize, status: usize) -> usize {
    match slice {
        0 => slice_1_use_count(status),
        1 => slice_2_use_count(status),
        _ => panic!("Invalid slice index"),
    }
}

fn inc_slice(slice: usize) -> usize {
    match slice {
        0 => SLICE_1_INC,
        1 => SLICE_2_INC,
        _ => panic!("Invalid slice index"),
    }
}

fn inc_all_slices_except(slice: usize) -> usize {
    match slice {
        0 => SLICE_2_INC,
        1 => SLICE_1_INC,
        _ => panic!("Invalid slice index"),
    }
}

fn current_slice(status: usize) -> usize {
    (status & CURRENT_SLICE_MASK) >> CURRENT_SLICE_SHIFT
}

fn valid_status(status: usize) -> bool {
    let combined_mask = CURRENT_SLICE_MASK | SLICE_1_MASK | SLICE_2_MASK;
    let leftover_bits = status & !combined_mask;
    leftover_bits == 0
}

pub struct AtomicSlice<T> {
    data: UnsafeCell<Vec<T>>,
    stride: usize,
    status: AtomicUsize,
    currently_writing: AtomicBool,
}

pub struct AtomicSliceReadGuard<'a, T> {
    slice: &'a [T],
    current_slice: usize,
    status: &'a AtomicUsize,
}

impl<T: Default + Clone> AtomicSlice<T> {
    pub fn new(mut data: Vec<T>) -> AtomicSlice<T> {
        let stride = data.len();
        let pool_size = NUM_SLICES;
        data.resize(stride * pool_size, T::default());
        AtomicSlice {
            data: UnsafeCell::new(data),
            stride,
            status: AtomicUsize::new(0),
            currently_writing: AtomicBool::new(false),
        }
    }

    pub fn read<'a>(&'a self) -> AtomicSliceReadGuard<'a, T> {
        // Get current slice index while also marking all slices as in use.
        let status = self.status.fetch_add(INC_ALL_SLICES, Ordering::SeqCst);

        debug_assert!(valid_status(status));
        debug_assert!(slice_1_use_count(status) < 0xFFFF);
        debug_assert!(slice_2_use_count(status) < 0xFFFF);

        let current_slice = current_slice(status);

        debug_assert!(slice_use_count(current_slice, self.status.load(Ordering::SeqCst)) > 0);

        // Now that the current slice is known, mark the others as no longer in use
        let status = self
            .status
            .fetch_sub(inc_all_slices_except(current_slice), Ordering::SeqCst);
        debug_assert!(valid_status(status));

        let stride = self.stride;
        let offset = current_slice * stride;
        let slice: &[T] = unsafe {
            let ptr_vec = self.data.get();
            let ptr_data = (*ptr_vec).as_ptr();
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
        let i = current_slice(status);
        debug_assert!(i < NUM_SLICES);
        let next_i = (i + 1) % NUM_SLICES;

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
        let offset = next_i * stride;
        let slice: &mut [T] = unsafe {
            let ptr_vec = self.data.get();
            let ptr_data = (*ptr_vec).as_mut_ptr();
            let ptr_begin = ptr_data.add(offset);
            std::slice::from_raw_parts_mut(ptr_begin, stride)
        };
        for (i, v) in slice.iter_mut().enumerate() {
            *v = data[i].clone();
        }

        // Point all new readers to the next slice
        let status = self.status.fetch_xor(i.bitxor(next_i), Ordering::SeqCst);
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
        let status = self
            .status
            .fetch_sub(inc_slice(self.current_slice), Ordering::SeqCst);
        debug_assert!(valid_status(status));
        debug_assert!(slice_use_count(self.current_slice, status) > 0);
    }
}
