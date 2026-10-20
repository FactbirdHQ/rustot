//! Status details types for job status reporting.
//!
//! This module provides types for building status details payloads that are
//! sent to AWS IoT Jobs when updating job execution status. The design allows
//! users to provide additional context fields via the [`StatusDetailsExt`] trait.

use core::fmt::{self, Write};

use serde::{Serialize, Serializer, ser::SerializeMap};

use super::pal::ImageStateReason;
use crate::mqtt::MaxJsonSize;

/// AWS IoT Jobs rejects `statusDetails` values longer than this many
/// characters, and nested objects are not accepted at all — every value must
/// be a string.
pub const STATUS_DETAILS_VALUE_MAX_LEN: usize = 1024;

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
pub trait StatusDetailsExt: Default {
    /// Worst-case JSON size of the fields this extension contributes via
    /// [`serialize_into_map`](Self::serialize_into_map). Used to size the
    /// encode buffer when this extension is combined with the base
    /// [`StatusDetails`] in [`CombinedStatusDetails`]. Pick a generous upper
    /// bound — under-estimating drops the publish.
    const MAX_EXTRA_JSON_SIZE: usize;

    /// Serialize this type's fields into an existing map.
    fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error>;

    /// Accept a key-value pair from incoming job status details.
    ///
    /// Called during `JobContext` construction when converting the raw
    /// `StatusDetails` map into typed fields. Known OTA base keys (`self_test`,
    /// `progress`, etc.) are handled by the library; unknown keys are offered
    /// to this method.
    ///
    /// Returns `true` if the key was recognized and consumed.
    fn accept_entry(&mut self, _key: &str, _value: &str) -> bool {
        false
    }
}

/// Default implementation for `()` - no extra fields.
impl StatusDetailsExt for () {
    const MAX_EXTRA_JSON_SIZE: usize = 0;

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
pub struct StatusDetails {
    /// Current self-test state (e.g., "receiving", "ready", "active", "accepted")
    pub self_test: Option<heapless::String<12>>,
    /// Download progress as "received/total" blocks
    pub progress: Option<heapless::String<16>>,
    /// Failure reason string for rejected/aborted states
    pub reason: Option<heapless::String<24>>,
    /// Numeric error code for failures
    pub error_code: Option<u16>,
}

impl StatusDetails {
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

    /// Set failure details from a reason string and error code.
    pub fn set_failure(&mut self, reason_str: &str, error_code: u16) {
        self.reason = heapless::String::try_from(reason_str).ok();
        if error_code != 0 {
            self.error_code = Some(error_code);
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
        CombinedStatusDetails {
            base: self,
            files: &[],
            extra,
        }
    }
}

/// Standalone serialization for StatusDetails (when no extra context).
impl Serialize for StatusDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        self.serialize_into_map(&mut map)?;
        map.end()
    }
}

/// Combined status details (base OTA fields + per-file status + user context).
///
/// This type serializes the base [`StatusDetails`] fields, the `files` entry
/// for multi-file jobs, and the user-provided extra context into a single
/// flat JSON object.
///
/// `N` is the maximum number of files per job and only affects
/// [`MaxJsonSize`]: with `N = 1` no `files` entry can ever be emitted, so
/// single-file builds pay nothing for it.
pub struct CombinedStatusDetails<'a, E: StatusDetailsExt, const N: usize = 1> {
    base: &'a StatusDetails,
    files: &'a [FileStatus],
    extra: &'a E,
}

impl<'a, E: StatusDetailsExt> CombinedStatusDetails<'a, E> {
    /// Attach per-file status. Emitted under `files` only when the job has
    /// more than one file.
    pub fn with_files<const N: usize>(
        self,
        files: &'a heapless::Vec<FileStatus, N>,
    ) -> CombinedStatusDetails<'a, E, N> {
        CombinedStatusDetails {
            base: self.base,
            files,
            extra: self.extra,
        }
    }
}

impl<E: StatusDetailsExt, const N: usize> Serialize for CombinedStatusDetails<'_, E, N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        self.base.serialize_into_map(&mut map)?;
        if self.files.len() > 1 {
            map.serialize_entry("files", &FilesValue(self.files))?;
        }
        self.extra.serialize_into_map(&mut map)?;
        map.end()
    }
}

/// Download state of one file in a multi-file job.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FileState {
    /// Not started yet.
    Pending,
    /// Blocks are being received.
    InProgress,
    /// Fully received and closed (including the PAL's signature check).
    Succeeded,
    /// The file failed and the job was aborted, with the reason if known.
    Failed(Option<ImageStateReason>),
}

impl FileState {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileState::Pending => "pending",
            FileState::InProgress => "in_progress",
            FileState::Succeeded => "succeeded",
            FileState::Failed(_) => "failed",
        }
    }
}

/// Status of one file in a multi-file job, reported under the `files`
/// status detail.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FileStatus {
    pub fileid: u8,
    pub state: FileState,
    pub received_blocks: usize,
    pub total_blocks: usize,
}

/// The `files` value: a JSON array encoded into a single string, because
/// AWS IoT Jobs only accepts string values in `statusDetails`.
///
/// Streamed through `collect_str`, which escapes it into the outer JSON
/// without an intermediate buffer.
struct FilesValue<'a>(&'a [FileStatus]);

impl Serialize for FilesValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl fmt::Display for FilesValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('[')?;
        for (idx, file) in self.0.iter().enumerate() {
            if idx > 0 {
                f.write_char(',')?;
            }
            write!(
                f,
                r#"{{"id":{},"status":"{}","progress":"{}/{}""#,
                file.fileid,
                file.state.as_str(),
                file.received_blocks,
                file.total_blocks
            )?;
            if let FileState::Failed(Some(reason)) = file.state {
                write!(f, r#","reason":"{}""#, reason.as_reason_str())?;
                // Mirrors `StatusDetails::set_failure`: 0 means "not an error".
                let error_code = reason.error_code();
                if error_code != 0 {
                    write!(f, r#","error_code":{}"#, error_code)?;
                }
            }
            f.write_char('}')?;
        }
        f.write_char(']')
    }
}

/// Longest string returned by [`ImageStateReason::as_reason_str`].
const REASON_MAX_LEN: usize = 24;

const USIZE_MAX_DIGITS: usize = usize::MAX.ilog10() as usize + 1;

/// Worst-case length of the unescaped `files` value for a job of `n` files.
///
/// Only one file can carry a failure: the engine stops at the first one.
pub(crate) const fn files_value_max_len(n: usize) -> usize {
    // Longest status is "in_progress"; `+ 1` for the separating comma.
    const ENTRY: usize =
        r#"{"id":255,"status":"in_progress","progress":"/"}"#.len() + 2 * USIZE_MAX_DIGITS + 1;
    const FAILURE: usize = r#","reason":"","error_code":65535"#.len() + REASON_MAX_LEN;
    "[]".len() + n * ENTRY + FAILURE
}

/// Worst-case JSON size of the `files` entry, including its key and the
/// escaping of the inner quotes (which at most doubles the value).
const fn files_json_max_size(n: usize) -> usize {
    if n <= 1 {
        0
    } else {
        r#","files":"""#.len() + 2 * files_value_max_len(n)
    }
}

/// Compile-time check that a job of `N` files always fits AWS's
/// [`STATUS_DETAILS_VALUE_MAX_LEN`].
pub(crate) const fn assert_files_fit<const N: usize>() {
    // `core::assert!`: the crate-level `assert!` maps to `defmt::assert!`,
    // which is not const.
    ::core::assert!(
        N <= 1 || files_value_max_len(N) <= STATUS_DETAILS_VALUE_MAX_LEN,
        "too many files per job: the `files` status detail would exceed AWS's 1024 character limit"
    );
}

/// Worst-case JSON size of the base [`StatusDetails`] fields.
///
/// Each optional field contributes its key + quoted value + comma:
/// - `self_test` (heapless::String<12>): ~28 bytes
/// - `progress`  (heapless::String<16>): ~32 bytes
/// - `reason`    (heapless::String<24>): ~36 bytes
/// - `error_code` (u16, up to 5 digits): ~21 bytes
/// Plus enclosing `{}` and a few bytes of slack for commas/whitespace.
const STATUS_DETAILS_MAX_JSON_SIZE: usize = 192;

impl MaxJsonSize for StatusDetails {
    const MAX_JSON_SIZE: usize = STATUS_DETAILS_MAX_JSON_SIZE;
}

impl<E: StatusDetailsExt, const N: usize> MaxJsonSize for CombinedStatusDetails<'_, E, N> {
    const MAX_JSON_SIZE: usize =
        STATUS_DETAILS_MAX_JSON_SIZE + files_json_max_size(N) + E::MAX_EXTRA_JSON_SIZE;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_basic_status() {
        let mut status = StatusDetails::new();
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
        let mut status = StatusDetails::new();
        status.set_self_test("rejected");
        status.set_failure("sig_check_failed", 1001);

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"rejected","reason":"sig_check_failed","error_code":1001}"#
        );
    }

    #[derive(Default)]
    struct TestContext {
        device_temp: u8,
    }

    impl StatusDetailsExt for TestContext {
        // `"device_temp":<3 digits>` plus a comma — generous bound.
        const MAX_EXTRA_JSON_SIZE: usize = 32;

        fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
            map.serialize_entry("device_temp", &self.device_temp)?;
            Ok(())
        }
    }

    #[test]
    fn serialize_with_extra_context() {
        let mut status = StatusDetails::new();
        status.set_self_test("active");

        let extra = TestContext { device_temp: 42 };
        let combined = status.with_extra(&extra);

        let json = serde_json_core::to_string::<_, 128>(&combined).unwrap();
        assert_eq!(json.as_str(), r#"{"self_test":"active","device_temp":42}"#);
    }

    #[test]
    fn serialize_with_unit_context() {
        let mut status = StatusDetails::new();
        status.set_self_test("accepted");

        let combined = status.with_extra(&());

        let json = serde_json_core::to_string::<_, 128>(&combined).unwrap();
        assert_eq!(json.as_str(), r#"{"self_test":"accepted"}"#);
    }

    fn file(fileid: u8, state: FileState, received: usize, total: usize) -> FileStatus {
        FileStatus {
            fileid,
            state,
            received_blocks: received,
            total_blocks: total,
        }
    }

    #[test]
    fn files_omitted_for_single_file_jobs() {
        let mut status = StatusDetails::new();
        status.set_progress(3, 3);
        let files: heapless::Vec<FileStatus, 4> =
            heapless::Vec::from_slice(&[file(0, FileState::Succeeded, 3, 3)]).unwrap();

        let combined = status.with_extra(&()).with_files(&files);
        let json = serde_json_core::to_string::<_, 256>(&combined).unwrap();
        assert_eq!(json.as_str(), r#"{"progress":"3/3"}"#);
    }

    #[test]
    fn files_encoded_as_string_value() {
        use crate::transfer::pal::PalError;

        let status = StatusDetails::new();
        let files: heapless::Vec<FileStatus, 3> = heapless::Vec::from_slice(&[
            file(0, FileState::Succeeded, 3, 3),
            file(
                4,
                FileState::Failed(Some(ImageStateReason::Pal(PalError::SignatureCheckFailed))),
                2,
                2,
            ),
            file(9, FileState::Pending, 0, 7),
        ])
        .unwrap();

        let combined = status.with_extra(&()).with_files(&files);
        let json = serde_json_core::to_string::<_, 512>(&combined).unwrap();

        // Every statusDetails value must be a string, so the array is escaped.
        let details: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&json).unwrap();
        let files: serde_json::Value = serde_json::from_str(&details["files"]).unwrap();
        assert_eq!(
            files,
            serde_json::json!([
                {"id": 0, "status": "succeeded", "progress": "3/3"},
                {"id": 4, "status": "failed", "progress": "2/2",
                 "reason": "sig_check_failed", "error_code": 1001},
                {"id": 9, "status": "pending", "progress": "0/7"},
            ])
        );
    }

    #[test]
    fn reason_strings_fit_bound() {
        use crate::transfer::pal::PalError;

        let reasons = [
            ImageStateReason::NewerJob,
            ImageStateReason::FailedIngest,
            ImageStateReason::MomentumAbort,
            ImageStateReason::ImageStateMismatch,
            ImageStateReason::SignatureCheckPassed,
            ImageStateReason::InvalidDataProtocol,
            ImageStateReason::UserAbort,
            ImageStateReason::VersionCheck,
        ];
        let pal = [
            PalError::SignatureCheckFailed,
            PalError::FileWriteFailed,
            PalError::FileTooLarge,
            PalError::FileCloseFailed,
            PalError::BadFileHandle,
            PalError::Unsupported,
            PalError::BadImageState,
            PalError::CommitFailed,
            PalError::VersionCheck,
            PalError::Other,
        ];
        for reason in reasons
            .into_iter()
            .chain(pal.into_iter().map(ImageStateReason::Pal))
        {
            assert!(reason.as_reason_str().len() <= REASON_MAX_LEN, "{reason:?}");
        }
    }

    /// The largest `N` accepted by `assert_files_fit` must produce values AWS
    /// accepts, and serialized output must stay within `MAX_JSON_SIZE`.
    #[test]
    fn worst_case_files_fit_bounds() {
        use crate::transfer::pal::PalError;

        const N: usize = 10;
        const _: () = assert_files_fit::<N>();
        assert!(files_value_max_len(N + 1) > STATUS_DETAILS_VALUE_MAX_LEN);

        let mut files: heapless::Vec<FileStatus, N> = heapless::Vec::new();
        for id in 0..N {
            files
                .push(file(
                    255 - id as u8,
                    FileState::InProgress,
                    usize::MAX,
                    usize::MAX,
                ))
                .unwrap();
        }
        files[0].state = FileState::Failed(Some(ImageStateReason::Pal(PalError::FileCloseFailed)));

        let value = std::format!("{}", FilesValue(&files));
        assert!(value.len() <= files_value_max_len(N));
        assert!(value.len() <= STATUS_DETAILS_VALUE_MAX_LEN);

        let status = StatusDetails::new();
        let combined = status.with_extra(&()).with_files(&files);
        let json = serde_json_core::to_string::<_, 4096>(&combined).unwrap();
        assert!(json.len() <= <CombinedStatusDetails<'_, (), N> as MaxJsonSize>::MAX_JSON_SIZE);
    }
}
