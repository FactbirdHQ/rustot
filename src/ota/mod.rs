//! ## Over-the-air (OTA) flashing of firmware
//!
//! AWS IoT OTA works by using AWS IoT Jobs to manage firmware transfer and
//! status reporting of OTA.
//!
//! The OTA Jobs API makes use of the following special MQTT Topics:
//! - $aws/things/{thing_name}/jobs/$next/get/accepted
//! - $aws/things/{thing_name}/jobs/notify-next
//! - $aws/things/{thing_name}/jobs/$next/get
//! - $aws/things/{thing_name}/jobs/{job_id}/update
//! - $aws/things/{thing_name}/streams/{stream_id}/data/cbor
//! - $aws/things/{thing_name}/streams/{stream_id}/get/cbor
//!
//! Most of the data structures for the Jobs API has been copied from Rusoto:
//! https://docs.rs/rusoto_iot_jobs_data/0.43.0/rusoto_iot_jobs_data/
//!
//! ### OTA Flow:
//! 1. Device subscribes to notification topics for AWS IoT jobs and listens for
//!    update messages.
//! 2. When an update is available, the OTA agent publishes requests to AWS IoT
//!    and receives updates using the HTTP or MQTT protocol, depending on the
//!    settings you chose.
//! 3. The OTA agent checks the digital signature of the downloaded files and,
//!    if the files are valid, installs the firmware update to the appropriate
//!    flash bank.
//!
//! The OTA depends on working, and correctly setup:
//! - Bootloader
//! - MQTT Client
//! - Code sign verification
//! - CBOR deserializer

pub mod agent;
pub mod builder;
pub mod callback;
pub mod config;
pub mod control_interface;
pub mod data_interface;
pub mod encoding;
pub mod pal;
pub mod state;
#[macro_use]
pub mod logging;

#[cfg(test)]
pub mod test;
