#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![allow(async_fn_in_trait)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![deny(clippy::float_arithmetic)]

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod defender_metrics;
pub mod jobs;
pub mod mqtt;
#[cfg(any(feature = "ota_mqtt_data", feature = "ota_http_data"))]
pub mod ota;
pub mod provisioning;
pub mod shadows;

/// Re-exports for derive macro generated code. Not part of the public API.
#[doc(hidden)]
pub mod __macro_support {
    pub use heapless;
    pub use postcard;
    pub use serde_json_core;
}
