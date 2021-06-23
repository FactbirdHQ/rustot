#![cfg_attr(not(test), no_std)]

pub mod jobs;
pub mod ota;

#[cfg(test)]
pub mod test;
