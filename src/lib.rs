#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![allow(async_fn_in_trait)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod jobs;
#[cfg(any(feature = "ota_mqtt_data", feature = "ota_http_data"))]
pub mod ota;
pub mod provisioning;
pub mod shadows;
