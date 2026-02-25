//! OTA status details types for job status reporting.
//!
//! This module provides types for building status details payloads that are
//! sent to AWS IoT Jobs when updating job execution status. The design allows
//! users to provide additional context fields via the [`StatusDetailsExt`] trait.

use core::fmt::Write;

use serde::{ser::SerializeMap, Serialize, Serializer};

use super::pal::ImageStateReason;

/// Trait for types that can contribute fields to status details.
///
/// Implement this trait to add custom fields to the job status details
/// payload. The fields are serialized at the same level as the base OTA
/// status fields (self_test, progress, reason, error_code).
///
/// # Example
///
/// ```ignore
/// struct MyContext {
///     device_temp: u8,
///     battery_level: u8,
/// }
///
/// impl StatusDetailsExt for MyContext {
///     fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
///         map.serialize_entry("device_temp", &self.device_temp)?;
///         map.serialize_entry("battery_level", &self.battery_level)?;
///         Ok(())
///     }
/// }
/// ```
pub trait StatusDetailsExt {
    /// Serialize this type's fields into an existing map.
    fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error>;
}

/// Default implementation for `()` - no extra fields.
impl StatusDetailsExt for () {
    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

/// Base OTA status details.
///
/// Contains the standard OTA fields that are always available for job status
/// reporting:
/// - `self_test`: Current test state (receiving, ready, active, accepted, rejected, aborted)
/// - `progress`: Download progress as "received/total" blocks
/// - `reason`: Failure reason string for rejected/aborted states
/// - `error_code`: Numeric error code for failures
#[derive(Debug, Clone, Default)]
pub struct OtaStatusDetails {
    /// Current self-test state (e.g., "receiving", "ready", "active", "accepted")
    pub self_test: Option<heapless::String<12>>,
    /// Download progress as "received/total" blocks
    pub progress: Option<heapless::String<16>>,
    /// Failure reason string for rejected/aborted states
    pub reason: Option<heapless::String<24>>,
    /// Numeric error code for failures
    pub error_code: Option<u16>,
}

impl OtaStatusDetails {
    /// Create a new empty status details.
    pub fn new() -> Self {
        Default::default()
    }

    /// Set the self-test status field.
    pub fn set_self_test(&mut self, status: &str) {
        self.self_test = heapless::String::try_from(status).ok();
    }

    /// Set the progress field from block counts.
    pub fn set_progress(&mut self, received: usize, total: usize) {
        let mut s = heapless::String::new();
        let _ = write!(s, "{}/{}", received, total);
        self.progress = Some(s);
    }

    /// Set failure details from an ImageStateReason.
    pub fn set_failure(&mut self, reason: ImageStateReason) {
        self.reason = heapless::String::try_from(reason.as_reason_str()).ok();
        let code = reason.error_code();
        if code != 0 {
            self.error_code = Some(code);
        }
    }

    /// Clear the failure details (reason and error_code).
    pub fn clear_failure(&mut self) {
        self.reason = None;
        self.error_code = None;
    }

    /// Serialize the base OTA fields into an existing map.
    pub fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
        if let Some(ref v) = self.self_test {
            map.serialize_entry("self_test", v)?;
        }
        if let Some(ref v) = self.progress {
            map.serialize_entry("progress", v)?;
        }
        if let Some(ref v) = self.reason {
            map.serialize_entry("reason", v)?;
        }
        if let Some(ref v) = self.error_code {
            map.serialize_entry("error_code", v)?;
        }
        Ok(())
    }

    /// Combine with user-provided extra context for serialization.
    ///
    /// Returns a type that serializes both the base OTA fields and the
    /// extra context fields into a single flat JSON object.
    pub fn with_extra<'a, E: StatusDetailsExt>(
        &'a self,
        extra: &'a E,
    ) -> CombinedStatusDetails<'a, E> {
        CombinedStatusDetails { base: self, extra }
    }
}

/// Standalone serialization for OtaStatusDetails (when no extra context).
impl Serialize for OtaStatusDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        self.serialize_into_map(&mut map)?;
        map.end()
    }
}

/// Combined status details (base OTA fields + user context).
///
/// This type serializes both the base [`OtaStatusDetails`] fields and the
/// user-provided extra context into a single flat JSON object.
pub struct CombinedStatusDetails<'a, E: StatusDetailsExt> {
    base: &'a OtaStatusDetails,
    extra: &'a E,
}

impl<E: StatusDetailsExt> Serialize for CombinedStatusDetails<'_, E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        self.base.serialize_into_map(&mut map)?;
        self.extra.serialize_into_map(&mut map)?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_basic_status() {
        let mut status = OtaStatusDetails::new();
        status.set_self_test("receiving");
        status.set_progress(10, 100);

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"receiving","progress":"10/100"}"#
        );
    }

    #[test]
    fn serialize_with_failure() {
        let mut status = OtaStatusDetails::new();
        status.set_self_test("rejected");
        status.set_failure(ImageStateReason::Pal(
            super::super::pal::OtaPalError::SignatureCheckFailed,
        ));

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"rejected","reason":"sig_check_failed","error_code":1001}"#
        );
    }

    struct TestContext {
        device_temp: u8,
    }

    impl StatusDetailsExt for TestContext {
        fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
            map.serialize_entry("device_temp", &self.device_temp)?;
            Ok(())
        }
    }

    #[test]
    fn serialize_with_extra_context() {
        let mut status = OtaStatusDetails::new();
        status.set_self_test("active");

        let extra = TestContext { device_temp: 42 };
        let combined = status.with_extra(&extra);

        let json = serde_json_core::to_string::<_, 128>(&combined).unwrap();
        assert_eq!(json.as_str(), r#"{"self_test":"active","device_temp":42}"#);
    }

    #[test]
    fn serialize_with_unit_context() {
        let mut status = OtaStatusDetails::new();
        status.set_self_test("accepted");

        let combined = status.with_extra(&());

        let json = serde_json_core::to_string::<_, 128>(&combined).unwrap();
        assert_eq!(json.as_str(), r#"{"self_test":"accepted"}"#);
    }
}
