//! The `statusDetails` of a transfer job execution.
//!
//! [`StatusDetails`] is the whole map as one owned type. It serializes into
//! every job update and deserializes from the job execution AWS sends back,
//! so both directions share one schema.
//!
//! Reading never borrows from the payload: each value is copied into its typed
//! field (or handed to [`StatusDetailsExt::accept_entry`]) as it is parsed.
//! This is what lets a parser that unescapes into a temporary buffer (std
//! `serde_json`, or `serde_json_core` with an unescape buffer) read values such
//! as `files`, whose JSON is escaped into a string.

use core::{
    fmt::{self, Write},
    marker::PhantomData,
    str::FromStr,
};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, DeserializeSeed, MapAccess, Visitor},
    ser::SerializeMap,
};

use super::{encoding::json::JobStatusReason, pal::ImageStateReason};
use crate::{
    jobs::{MAX_STATUS_DETAILS_KEY_LEN, MAX_STATUS_DETAILS_VALUE_LEN},
    mqtt::MaxJsonSize,
};

/// Fields a user type contributes to [`StatusDetails`], next to the base
/// transfer fields (`self_test`, `progress`, `reason`, `error_code`, `files`).
///
/// # Example
///
/// ```ignore
/// #[derive(Default)]
/// struct MyContext {
///     battery_level: u8,
/// }
///
/// impl StatusDetailsExt for MyContext {
///     const MAX_EXTRA_JSON_SIZE: usize = 24;
///
///     fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
///         map.serialize_entry("battery_level", &self.battery_level)
///     }
///
///     fn accept_entry(&mut self, key: &str, value: &str) -> bool {
///         match key {
///             "battery_level" => self.battery_level = value.parse().unwrap_or_default(),
///             _ => return false,
///         }
///         true
///     }
/// }
/// ```
pub trait StatusDetailsExt: Default {
    /// Worst-case JSON size of the fields written by
    /// [`serialize_into_map`](Self::serialize_into_map), used to size the
    /// encode buffer of [`StatusDetails`]. Pick a generous upper bound:
    /// under-estimating drops the publish.
    const MAX_EXTRA_JSON_SIZE: usize;

    /// Serialize this type's fields into the `statusDetails` map.
    fn serialize_into_map<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error>;

    /// Offered every incoming `statusDetails` entry whose key is not one of
    /// the base transfer fields.
    ///
    /// `value` is unescaped. AWS stores every value as a string, so a number
    /// written by [`serialize_into_map`](Self::serialize_into_map) comes back
    /// as its decimal string.
    ///
    /// Returns `true` if the key was recognized and consumed.
    fn accept_entry(&mut self, _key: &str, _value: &str) -> bool {
        false
    }
}

/// No extra fields.
impl StatusDetailsExt for () {
    const MAX_EXTRA_JSON_SIZE: usize = 0;

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

/// The `statusDetails` of a transfer job execution.
///
/// | Key | Field |
/// |---|---|
/// | `self_test` | [`self_test`](Self::self_test) |
/// | `progress` | [`progress`](Self::progress) |
/// | `reason`, `error_code` | the detail of a `Rejected` / `Aborted` [`self_test`](Self::self_test) |
/// | `files` | [`files`](Self::files), only for jobs of more than one file |
/// | anything else | [`extra`](Self::extra), via [`StatusDetailsExt`] |
///
/// `N` is the maximum number of files per job. With `N = 1` no `files` entry
/// is ever written, so single-file builds pay nothing for it.
///
/// Incoming values the type can't parse (e.g. a `self_test` it doesn't know)
/// are logged and left unset rather than failing the job execution: a message
/// that never parses would leave the job stuck. Every value must still be a
/// string, as AWS guarantees.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct StatusDetails<E = (), const N: usize = 1> {
    pub self_test: Option<JobStatusReason>,
    /// Blocks received, summed over every file in the job.
    pub progress: Option<Progress>,
    pub files: heapless::Vec<FileStatus, N>,
    pub extra: E,
}

impl<E: StatusDetailsExt, const N: usize> Serialize for StatusDetails<E, N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(self_test) = self.self_test {
            map.serialize_entry("self_test", self_test.as_str())?;
        }
        if let Some(progress) = &self.progress {
            map.serialize_entry("progress", progress)?;
        }
        if let Some(reason) = self.self_test.and_then(|self_test| self_test.detail()) {
            serialize_failure(&mut map, reason)?;
        }
        if self.files.len() > 1 {
            map.serialize_entry("files", &FilesValue(&self.files))?;
        }
        self.extra.serialize_into_map(&mut map)?;
        map.end()
    }
}

fn serialize_failure<S: SerializeMap>(
    map: &mut S,
    reason: ImageStateReason,
) -> Result<(), S::Error> {
    map.serialize_entry("reason", reason.as_reason_str())?;
    // 0 means "not an error".
    let error_code = reason.error_code();
    if error_code != 0 {
        map.serialize_entry("error_code", &error_code)?;
    }
    Ok(())
}

impl<'de, E: StatusDetailsExt, const N: usize> Deserialize<'de> for StatusDetails<E, N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(StatusDetailsVisitor(PhantomData))
    }
}

struct StatusDetailsVisitor<E, const N: usize>(PhantomData<E>);

impl<'de, E: StatusDetailsExt, const N: usize> Visitor<'de> for StatusDetailsVisitor<E, N> {
    type Value = StatusDetails<E, N>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of status detail strings")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut details = StatusDetails::<E, N>::default();
        let mut failure = WireFailure::default();

        while let Some(Key(key)) = map.next_key()? {
            let key = key.as_str();
            map.next_value_seed(WithStr(|value: &str| match key {
                "self_test" => details.self_test = parse_value(key, value),
                "progress" => details.progress = parse_value(key, value),
                "reason" => failure.reason_is_sig_check_passed = is_sig_check_passed(value),
                "error_code" => failure.error_code = parse_value(key, value),
                "files" => {
                    details.files = parse_files(value).unwrap_or_else(|| {
                        warn!("Ignoring malformed `files` status detail");
                        heapless::Vec::new()
                    })
                }
                _ => {
                    details.extra.accept_entry(key, value);
                }
            }))?;
        }

        // `self_test` parses `rejected` / `aborted` without their detail,
        // which travels in `reason` / `error_code`.
        if let Some(JobStatusReason::Rejected(detail) | JobStatusReason::Aborted(detail)) =
            &mut details.self_test
        {
            *detail = failure.reason();
        }

        Ok(details)
    }
}

fn parse_value<T: FromStr>(key: &str, value: &str) -> Option<T> {
    let parsed = value.parse().ok();
    if parsed.is_none() {
        warn!("Ignoring malformed `{}` status detail", key);
    }
    parsed
}

/// `reason` / `error_code` as read from the wire.
#[derive(Default)]
struct WireFailure {
    error_code: Option<u16>,
    reason_is_sig_check_passed: bool,
}

impl WireFailure {
    /// The code identifies the reason. Only `sig_check_passed`, whose code is
    /// 0, travels without one.
    fn reason(&self) -> Option<ImageStateReason> {
        match self.error_code {
            Some(code) => {
                let reason = ImageStateReason::from_error_code(code);
                if reason.is_none() {
                    warn!("Ignoring unknown status detail error code {}", code);
                }
                reason
            }
            None => self
                .reason_is_sig_check_passed
                .then_some(ImageStateReason::SignatureCheckPassed),
        }
    }
}

fn is_sig_check_passed(reason: &str) -> bool {
    reason == ImageStateReason::SignatureCheckPassed.as_reason_str()
}

/// A `statusDetails` key, copied: an escaped key can only be read through a
/// temporary, and AWS caps keys at [`MAX_STATUS_DETAILS_KEY_LEN`].
struct Key(heapless::String<MAX_STATUS_DETAILS_KEY_LEN>);

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl Visitor<'_> for KeyVisitor {
            type Value = Key;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a key of at most {MAX_STATUS_DETAILS_KEY_LEN} bytes")
            }

            fn visit_str<Er: de::Error>(self, v: &str) -> Result<Key, Er> {
                heapless::String::try_from(v)
                    .map(Key)
                    .map_err(|_| Er::invalid_length(v.len(), &self))
            }
        }

        deserializer.deserialize_str(KeyVisitor)
    }
}

/// Deserializes a string value and hands it to `F`, borrowed or not.
struct WithStr<F>(F);

impl<'de, F: FnOnce(&str)> DeserializeSeed<'de> for WithStr<F> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<F: FnOnce(&str)> Visitor<'_> for WithStr<F> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<Er: de::Error>(self, v: &str) -> Result<(), Er> {
        (self.0)(v);
        Ok(())
    }
}

/// Download progress, reported as `"received/total"` blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Progress {
    pub received: usize,
    pub total: usize,
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.received, self.total)
    }
}

impl FromStr for Progress {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (received, total) = s.split_once('/').ok_or(())?;
        Ok(Progress {
            received: received.parse().map_err(|_| ())?,
            total: total.parse().map_err(|_| ())?,
        })
    }
}

impl Serialize for Progress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
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
/// without an intermediate buffer. Read back by [`parse_files`].
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
                // 0 means "not an error".
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

/// One entry of the `files` array, as written by [`FilesValue`].
#[derive(Deserialize)]
struct WireFile<'a> {
    id: u8,
    status: &'a str,
    progress: &'a str,
    reason: Option<&'a str>,
    error_code: Option<u16>,
}

impl TryFrom<WireFile<'_>> for FileStatus {
    type Error = ();

    fn try_from(file: WireFile<'_>) -> Result<Self, ()> {
        let state = match file.status {
            "pending" => FileState::Pending,
            "in_progress" => FileState::InProgress,
            "succeeded" => FileState::Succeeded,
            "failed" => FileState::Failed(
                WireFailure {
                    error_code: file.error_code,
                    reason_is_sig_check_passed: file.reason.is_some_and(is_sig_check_passed),
                }
                .reason(),
            ),
            _ => return Err(()),
        };
        let progress: Progress = file.progress.parse()?;
        Ok(FileStatus {
            fileid: file.id,
            state,
            received_blocks: progress.received,
            total_blocks: progress.total,
        })
    }
}

/// Parse the unescaped `files` value. `None` if it is malformed or describes
/// more than `N` files.
fn parse_files<const N: usize>(value: &str) -> Option<heapless::Vec<FileStatus, N>> {
    let (wire, _) = serde_json_core::from_str::<heapless::Vec<WireFile<'_>, N>>(value).ok()?;
    let mut files = heapless::Vec::new();
    for file in wire {
        // Same capacity as `wire`.
        let _ = files.push(FileStatus::try_from(file).ok()?);
    }
    Some(files)
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
/// [`MAX_STATUS_DETAILS_VALUE_LEN`].
pub(crate) const fn assert_files_fit<const N: usize>() {
    // `core::assert!`: the crate-level `assert!` maps to `defmt::assert!`,
    // which is not const.
    ::core::assert!(
        N <= 1 || files_value_max_len(N) <= MAX_STATUS_DETAILS_VALUE_LEN,
        "too many files per job: the `files` status detail would exceed AWS's 1024 character limit"
    );
}

/// Worst-case JSON size of the base fields, without `files`.
const BASE_MAX_JSON_SIZE: usize =
    r#"{"self_test":"","progress":"/","reason":"","error_code":65535}"#.len()
        + "receiving".len()
        + 2 * USIZE_MAX_DIGITS
        + REASON_MAX_LEN;

impl<E: StatusDetailsExt, const N: usize> MaxJsonSize for StatusDetails<E, N> {
    const MAX_JSON_SIZE: usize =
        BASE_MAX_JSON_SIZE + files_json_max_size(N) + E::MAX_EXTRA_JSON_SIZE;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::pal::PalError;

    fn status<const N: usize>(
        self_test: JobStatusReason,
        progress: Option<(usize, usize)>,
    ) -> StatusDetails<(), N> {
        StatusDetails {
            self_test: Some(self_test),
            progress: progress.map(|(received, total)| Progress { received, total }),
            ..Default::default()
        }
    }

    #[test]
    fn serialize_basic_status() {
        let status = status::<1>(JobStatusReason::Receiving, Some((10, 100)));

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"receiving","progress":"10/100"}"#
        );
    }

    #[test]
    fn serialize_with_failure() {
        let reason = ImageStateReason::Pal(PalError::SignatureCheckFailed);
        let status = status::<1>(JobStatusReason::Rejected(Some(reason)), None);

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"rejected","reason":"sig_check_failed","error_code":1001}"#
        );
    }

    #[derive(Debug, Default, PartialEq)]
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

        fn accept_entry(&mut self, key: &str, value: &str) -> bool {
            match key {
                "device_temp" => self.device_temp = value.parse().unwrap(),
                _ => return false,
            }
            true
        }
    }

    #[test]
    fn serialize_with_extra_context() {
        let status = StatusDetails::<_, 1> {
            self_test: Some(JobStatusReason::SelfTestActive),
            extra: TestContext { device_temp: 42 },
            ..Default::default()
        };

        let json = serde_json_core::to_string::<_, 128>(&status).unwrap();
        assert_eq!(json.as_str(), r#"{"self_test":"active","device_temp":42}"#);
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
        let mut status = status::<4>(JobStatusReason::Receiving, Some((3, 3)));
        status
            .files
            .push(file(0, FileState::Succeeded, 3, 3))
            .unwrap();

        let json = serde_json_core::to_string::<_, 256>(&status).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"self_test":"receiving","progress":"3/3"}"#
        );
    }

    fn three_files() -> heapless::Vec<FileStatus, 3> {
        heapless::Vec::from_slice(&[
            file(0, FileState::Succeeded, 3, 3),
            file(
                4,
                FileState::Failed(Some(ImageStateReason::Pal(PalError::SignatureCheckFailed))),
                2,
                2,
            ),
            file(9, FileState::Pending, 0, 7),
        ])
        .unwrap()
    }

    #[test]
    fn files_encoded_as_string_value() {
        let status = StatusDetails::<(), 3> {
            files: three_files(),
            ..Default::default()
        };
        let json = serde_json_core::to_string::<_, 512>(&status).unwrap();

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

    /// What AWS sends back: every value a string, `error_code` included.
    fn as_received(json: &str) -> String {
        let details: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(json).unwrap();
        let details: std::collections::BTreeMap<String, String> = details
            .into_iter()
            .map(|(key, value)| match value {
                serde_json::Value::String(s) => (key, s),
                other => (key, other.to_string()),
            })
            .collect();
        serde_json::to_string(&details).unwrap()
    }

    /// Parse with both parsers that read job executions: std `serde_json`,
    /// and `serde_json_core` with the unescape buffer `parse_job_message` uses.
    fn parse_both<E: StatusDetailsExt + fmt::Debug + PartialEq, const N: usize>(
        json: &str,
    ) -> StatusDetails<E, N> {
        let std: StatusDetails<E, N> = serde_json::from_str(json).unwrap();
        let mut buf = [0; MAX_STATUS_DETAILS_VALUE_LEN];
        let (core, _) =
            serde_json_core::from_slice_escaped::<StatusDetails<E, N>>(json.as_bytes(), &mut buf)
                .unwrap();
        assert_eq!(std, core);
        std
    }

    #[test]
    fn round_trips_through_aws() {
        let reason = ImageStateReason::Pal(PalError::SignatureCheckFailed);
        let sent = StatusDetails::<_, 3> {
            self_test: Some(JobStatusReason::Aborted(Some(reason))),
            progress: Some(Progress {
                received: 5,
                total: 12,
            }),
            files: three_files(),
            extra: TestContext { device_temp: 42 },
        };
        let json = serde_json_core::to_string::<_, 1024>(&sent).unwrap();

        assert_eq!(parse_both::<TestContext, 3>(&as_received(&json)), sent);
    }

    #[test]
    fn version_check_reasons_are_told_apart_by_code() {
        for reason in [
            ImageStateReason::VersionCheck,
            ImageStateReason::Pal(PalError::VersionCheck),
            ImageStateReason::SignatureCheckPassed,
        ] {
            let sent = status::<1>(JobStatusReason::Rejected(Some(reason)), None);
            let json = serde_json_core::to_string::<_, 256>(&sent).unwrap();

            assert_eq!(parse_both::<(), 1>(&as_received(&json)), sent);
        }
    }

    #[test]
    fn every_error_code_maps_back() {
        let reasons = [
            ImageStateReason::NewerJob,
            ImageStateReason::FailedIngest,
            ImageStateReason::MomentumAbort,
            ImageStateReason::ImageStateMismatch,
            ImageStateReason::InvalidDataProtocol,
            ImageStateReason::UserAbort,
            ImageStateReason::VersionCheck,
            ImageStateReason::SelfTestTimeout,
        ];
        for reason in reasons
            .into_iter()
            .chain(all_pal_errors().map(ImageStateReason::Pal))
        {
            assert_eq!(
                ImageStateReason::from_error_code(reason.error_code()),
                Some(reason)
            );
        }
        assert_eq!(ImageStateReason::from_error_code(0), None);
    }

    #[test]
    fn unknown_keys_go_to_the_extension_and_others_are_ignored() {
        let details = parse_both::<TestContext, 1>(
            r#"{"device_temp":"7","from_the_cloud":"x","self_test":"active"}"#,
        );
        assert_eq!(details.extra, TestContext { device_temp: 7 });
        assert_eq!(details.self_test, Some(JobStatusReason::SelfTestActive));
    }

    #[test]
    fn malformed_values_are_left_unset() {
        let details = parse_both::<(), 2>(
            r#"{"self_test":"dancing","progress":"most","files":"[{\"id\":0}]","error_code":"x"}"#,
        );
        assert_eq!(details, StatusDetails::default());
    }

    #[test]
    fn more_files_than_n_are_left_unset() {
        let sent = StatusDetails::<(), 3> {
            files: three_files(),
            ..Default::default()
        };
        let json = serde_json_core::to_string::<_, 512>(&sent).unwrap();

        assert!(parse_both::<(), 2>(&json).files.is_empty());
    }

    #[test]
    fn non_string_values_are_rejected() {
        assert!(serde_json::from_str::<StatusDetails>(r#"{"self_test":1}"#).is_err());
        assert!(serde_json_core::from_str::<StatusDetails>(r#"{"self_test":1}"#).is_err());
    }

    fn all_pal_errors() -> impl Iterator<Item = PalError> {
        [
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
        ]
        .into_iter()
    }

    #[test]
    fn reason_strings_fit_bound() {
        let reasons = [
            ImageStateReason::NewerJob,
            ImageStateReason::FailedIngest,
            ImageStateReason::MomentumAbort,
            ImageStateReason::ImageStateMismatch,
            ImageStateReason::SignatureCheckPassed,
            ImageStateReason::InvalidDataProtocol,
            ImageStateReason::UserAbort,
            ImageStateReason::VersionCheck,
            ImageStateReason::SelfTestTimeout,
        ];
        for reason in reasons
            .into_iter()
            .chain(all_pal_errors().map(ImageStateReason::Pal))
        {
            assert!(reason.as_reason_str().len() <= REASON_MAX_LEN, "{reason:?}");
        }
    }

    /// The largest `N` accepted by `assert_files_fit` must produce values AWS
    /// accepts, and serialized output must stay within `MAX_JSON_SIZE`.
    #[test]
    fn worst_case_files_fit_bounds() {
        const N: usize = 10;
        const _: () = assert_files_fit::<N>();
        assert!(files_value_max_len(N + 1) > MAX_STATUS_DETAILS_VALUE_LEN);

        let mut status = StatusDetails::<(), N> {
            self_test: Some(JobStatusReason::Aborted(Some(ImageStateReason::Pal(
                PalError::FileCloseFailed,
            )))),
            progress: Some(Progress {
                received: usize::MAX,
                total: usize::MAX,
            }),
            ..Default::default()
        };
        for id in 0..N {
            status
                .files
                .push(file(
                    255 - id as u8,
                    FileState::InProgress,
                    usize::MAX,
                    usize::MAX,
                ))
                .unwrap();
        }
        status.files[0].state =
            FileState::Failed(Some(ImageStateReason::Pal(PalError::FileCloseFailed)));

        let value = std::format!("{}", FilesValue(&status.files));
        assert!(value.len() <= files_value_max_len(N));
        assert!(value.len() <= MAX_STATUS_DETAILS_VALUE_LEN);

        let json = serde_json_core::to_string::<_, 4096>(&status).unwrap();
        assert!(json.len() <= <StatusDetails<(), N> as MaxJsonSize>::MAX_JSON_SIZE);
    }
}
