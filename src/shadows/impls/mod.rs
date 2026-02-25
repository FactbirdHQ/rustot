//! ShadowNode implementations for built-in types.
//!
//! - `opaque`: Primitive types via `impl_opaque!` macro
//! - `heapless`: `heapless::String<N>`, `heapless::Vec<T, N>`, `heapless::LinearMap<K, V, N>`
//! - `std`: `String`, `Vec<T>`, `HashMap<K, V>` (behind `std` feature)

mod opaque;

mod heapless_impls;

#[cfg(feature = "std")]
mod std_impls;

// Re-export wrapper types for map collections
pub use heapless_impls::{LinearMapDelta, LinearMapReported};

#[cfg(feature = "std")]
pub use std_impls::{HashMapDelta, HashMapReported};
