use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use crate::AtomicSlice;

#[test]
fn test_atomic_slice_u8() {
    let length: usize = 16;
    let num_readers = 2;
    let num_writers = 2;
    let num_iterations = 1_000_000;

    let next_value_to_write = Arc::new(AtomicU8::new(0));

    let mut data = Vec::<u8>::new();
    data.resize(length, 0);
    let atomic_slice = AtomicSlice::new(data);

    let readers: Vec<std::thread::JoinHandle<()>> = (0..num_readers)
        .map(|i_reader| {
            let atomic_slice = atomic_slice.clone();
            std::thread::spawn(move || {
                for iter in 0..num_iterations {
                    // Read the slice and assert that its length is as expected and that all values are the same
                    let guard = atomic_slice.read();
                    let slice: &[u8] = &*guard;
                    assert_eq!(slice.len(), length);
                    let first_value = slice[0];
                    for other_value in slice[1..].iter().cloned() {
                        assert_eq!(
                            first_value, other_value,
                            "Reader {} encountered a slice with mis-matched values {} != {} on iteration {}: {:?}",
                            i_reader, first_value, other_value, iter, slice
                        );
                    }
                }
            })
        })
        .collect();

    let writers: Vec<std::thread::JoinHandle<()>> = (0..num_writers)
        .map(|_| {
            let atomic_slice = atomic_slice.clone();
            let next_value_to_write = Arc::clone(&next_value_to_write);
            std::thread::spawn(move || {
                let mut data = Vec::<u8>::new();
                data.resize(length, 0);
                for _ in 0..num_iterations {
                    // Write an array of identical values to the slice.
                    // This should appear to be atomic, so no writer
                    // should ever be able to observer a slice containing
                    // unequal values
                    data.fill(next_value_to_write.fetch_add(1, Ordering::Relaxed));
                    atomic_slice.write(&data);
                }
            })
        })
        .collect();

    for t in readers {
        t.join().unwrap();
    }
    for t in writers {
        t.join().unwrap();
    }
}
