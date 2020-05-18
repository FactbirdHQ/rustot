use heapless::{ArrayLength, Vec};

/// Temp until serde_json can be replaced with serde_json_core
pub fn vec_to_vec<T: Clone, L: ArrayLength<T>>(v: alloc::vec::Vec<T>) -> Vec<T, L> {
    Vec::from_slice(&v).unwrap()
}
