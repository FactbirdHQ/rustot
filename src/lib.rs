#![cfg_attr(not(test), no_std)]

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod jobs;
#[cfg(any(feature = "ota_mqtt_data", feature = "ota_http_data"))]
pub mod ota;
<<<<<<< HEAD
pub mod provisioning;
=======
pub mod shadows;
>>>>>>> 2981062 (Initial commit of IoT Shadows support)

#[cfg(test)]
pub mod test;
