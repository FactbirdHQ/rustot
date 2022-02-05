#![cfg_attr(not(test), no_std)]

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod jobs;
pub mod ota;
pub mod provisioning;
// pub mod shadows;

#[cfg(test)]
pub mod test;
