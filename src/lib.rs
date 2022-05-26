#![cfg_attr(not(test), no_std)]

pub use static_assertions::assert_impl_all;

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod jobs;
#[cfg(any(feature = "ota_mqtt_data", feature = "ota_http_data"))]
pub mod ota;
pub mod provisioning;
pub mod shadows;

#[cfg(test)]
pub mod test;
