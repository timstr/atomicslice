use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use crate::AtomicSlice;

trait TestType:
    Default
    + Copy
    + From<u8>
    + std::fmt::Debug
    + std::fmt::Display
    + std::cmp::PartialEq
    + Send
    + 'static
{
}

struct TestConfig {
    length: usize,
    num_readers: usize,
    num_writers: usize,
    num_iterations: usize,
}

fn single_test_helper<T: TestType>(config: TestConfig) {
    let next_value_to_write = Arc::new(AtomicU8::new(0));

    let mut data = Vec::<T>::new();
    data.resize(config.length, T::default());
    let atomic_slice = Arc::new(AtomicSlice::new(data));

    let readers: Vec<std::thread::JoinHandle<()>> = (0..config.num_readers)
        .map(|i_reader| {
            let atomic_slice = Arc::clone(&atomic_slice);
            std::thread::spawn(move || {
                for iter in 0..config.num_iterations {
                    // Read the slice and assert that its length is as expected and that all values are the same
                    let guard = atomic_slice.read();
                    let slice: &[T] = &*guard;
                    assert_eq!(slice.len(), config.length);
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

    let writers: Vec<std::thread::JoinHandle<()>> = (0..config.num_writers)
        .map(|_| {
            let atomic_slice = Arc::clone(&atomic_slice);
            let next_value_to_write = Arc::clone(&next_value_to_write);
            std::thread::spawn(move || {
                let mut data = Vec::<T>::new();
                data.resize(config.length, T::default());
                for _ in 0..config.num_iterations {
                    // Write an array of identical values to the slice.
                    // This should appear to be atomic, so no writer
                    // should ever be able to observer a slice containing
                    // unequal values
                    data.fill(next_value_to_write.fetch_add(1, Ordering::Relaxed).into());
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

fn test_grid_helper<T: TestType>() {
    for length_bits in 0..=8 {
        for num_readers in 1..=4 {
            for num_writers in 1..=4 {
                single_test_helper::<T>(TestConfig {
                    length: (1 << length_bits),
                    num_readers,
                    num_writers,
                    num_iterations: 10_000,
                })
            }
        }
    }
}

impl TestType for u8 {}
impl TestType for u16 {}
impl TestType for u32 {}
impl TestType for u64 {}
impl TestType for f32 {}
impl TestType for f64 {}

#[test]
fn test_atomic_slice_u8() {
    test_grid_helper::<u8>()
}

#[test]
fn test_atomic_slice_u16() {
    test_grid_helper::<u16>()
}

#[test]
fn test_atomic_slice_u32() {
    test_grid_helper::<u32>()
}

#[test]
fn test_atomic_slice_u64() {
    test_grid_helper::<u64>()
}

#[test]
fn test_atomic_slice_f32() {
    test_grid_helper::<f32>()
}

#[test]
fn test_atomic_slice_f64() {
    test_grid_helper::<f64>()
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
struct ExampleStruct {
    x: u32,
    y: usize,
    z: u8,
}

impl From<u8> for ExampleStruct {
    fn from(value: u8) -> ExampleStruct {
        ExampleStruct {
            x: value.into(),
            y: value.into(),
            z: value.into(),
        }
    }
}

impl std::fmt::Display for ExampleStruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ExampleStruct {{ {}, {}, {} }}", self.x, self.y, self.z)
    }
}

impl TestType for ExampleStruct {}

#[test]
fn test_atomic_slice_example_struct() {
    test_grid_helper::<ExampleStruct>();
}

// TODO: add a test for multiple overlapping reads on the same thread.
// should work just fine but better to test anyway.
