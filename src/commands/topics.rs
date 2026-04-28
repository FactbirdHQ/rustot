use core::fmt::Write;

use heapless::String;

use super::error::CommandError;
use crate::jobs::MAX_THING_NAME_LEN;

/// Maximum executionId length the topic parser/formatter accommodates.
/// AWS IoT executionIds are UUIDs (36 chars); 64 leaves headroom.
pub const MAX_EXECUTION_ID_LEN: usize = 64;

/// Maximum topic length for commands MQTT operations.
///
/// Longest topic:
/// `$aws/commands/things/{thing}/executions/{executionId}/response/accepted/cbor`
pub const MAX_COMMAND_TOPIC_LEN: usize = "$aws/commands/things/".len()
    + MAX_THING_NAME_LEN
    + "/executions/".len()
    + MAX_EXECUTION_ID_LEN
    + "/response/accepted/cbor".len();

#[cfg(feature = "commands_cbor")]
pub(crate) const PAYLOAD_FORMAT: &str = "cbor";
#[cfg(not(feature = "commands_cbor"))]
pub(crate) const PAYLOAD_FORMAT: &str = "json";

/// Outbound topic formatter — analogue of `JobTopic` in `crate::jobs`.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandTopic<'a> {
    /// Wildcard subscribe: `$aws/commands/things/{thing}/executions/+/request/{format}`.
    Subscribe,
    /// Outbound publish: `$aws/commands/things/{thing}/executions/{id}/response/{format}`.
    Response(&'a str),
    /// Optional ack subscribe: `.../response/accepted/{format}`.
    ResponseAccepted(&'a str),
    /// Optional rejection subscribe: `.../response/rejected/{format}`.
    ResponseRejected(&'a str),
}

impl<'a> CommandTopic<'a> {
    const PREFIX: &'static str = "$aws/commands/things";

    pub fn check(s: &str) -> bool {
        s.starts_with(Self::PREFIX)
    }

    fn format_inner(&self, thing: &str, w: &mut dyn Write) -> core::fmt::Result {
        match self {
            Self::Subscribe => write!(
                w,
                "{}/{}/executions/+/request/{}",
                Self::PREFIX,
                thing,
                PAYLOAD_FORMAT
            ),
            Self::Response(id) => write!(
                w,
                "{}/{}/executions/{}/response/{}",
                Self::PREFIX,
                thing,
                id,
                PAYLOAD_FORMAT
            ),
            Self::ResponseAccepted(id) => write!(
                w,
                "{}/{}/executions/{}/response/accepted/{}",
                Self::PREFIX,
                thing,
                id,
                PAYLOAD_FORMAT
            ),
            Self::ResponseRejected(id) => write!(
                w,
                "{}/{}/executions/{}/response/rejected/{}",
                Self::PREFIX,
                thing,
                id,
                PAYLOAD_FORMAT
            ),
        }
    }

    pub fn format<const L: usize>(&self, thing: &str) -> Result<String<L>, CommandError> {
        let mut s = String::new();
        self.format_inner(thing, &mut s)
            .map_err(|_| CommandError::Overflow)?;
        Ok(s)
    }
}

/// Parsed inbound topic (cloud → device). Mirrors `crate::jobs::Topic`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Topic<'a> {
    /// Cloud → device command request. The executionId is borrowed from the
    /// MQTT topic path (the wire protocol does not require it in the payload).
    Request(&'a str),
    /// Cloud → device ack of our response publish.
    ResponseAccepted(&'a str),
    /// Cloud → device rejection of our response publish.
    ResponseRejected(&'a str),
}

impl<'a> Topic<'a> {
    /// Parse a topic name, returning the variant and (where applicable) the
    /// borrowed executionId.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &'a str) -> Option<Self> {
        // Segments: [0]=$aws [1]=commands [2]=things [3]=<thing>
        //           [4]=executions [5]=<executionId>
        //           [6]=request|response [7]=<format>|accepted|rejected
        //           [8]=<format>
        let tt = s.splitn(9, '/').collect::<heapless::Vec<&str, 9>>();
        match (tt.first(), tt.get(1), tt.get(2), tt.get(4)) {
            (Some(&"$aws"), Some(&"commands"), Some(&"things"), Some(&"executions")) => {
                let id = *tt.get(5)?;
                if id.is_empty() {
                    return None;
                }
                match (tt.get(6), tt.get(7), tt.get(8)) {
                    (Some(&"request"), Some(fmt), None) if *fmt == PAYLOAD_FORMAT => {
                        Some(Topic::Request(id))
                    }
                    (Some(&"response"), Some(&"accepted"), Some(fmt)) if *fmt == PAYLOAD_FORMAT => {
                        Some(Topic::ResponseAccepted(id))
                    }
                    (Some(&"response"), Some(&"rejected"), Some(fmt)) if *fmt == PAYLOAD_FORMAT => {
                        Some(Topic::ResponseRejected(id))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn format_subscribe() {
        let topic = CommandTopic::Subscribe
            .format::<MAX_COMMAND_TOPIC_LEN>("myThing")
            .unwrap();
        let expected = if cfg!(feature = "commands_cbor") {
            "$aws/commands/things/myThing/executions/+/request/cbor"
        } else {
            "$aws/commands/things/myThing/executions/+/request/json"
        };
        assert_eq!(topic.as_str(), expected);
    }

    #[test]
    fn format_response() {
        let topic = CommandTopic::Response("abc-123")
            .format::<MAX_COMMAND_TOPIC_LEN>("myThing")
            .unwrap();
        let expected = if cfg!(feature = "commands_cbor") {
            "$aws/commands/things/myThing/executions/abc-123/response/cbor"
        } else {
            "$aws/commands/things/myThing/executions/abc-123/response/json"
        };
        assert_eq!(topic.as_str(), expected);
    }

    #[test]
    fn format_response_accepted_and_rejected() {
        let accepted = CommandTopic::ResponseAccepted("abc-123")
            .format::<MAX_COMMAND_TOPIC_LEN>("myThing")
            .unwrap();
        let rejected = CommandTopic::ResponseRejected("abc-123")
            .format::<MAX_COMMAND_TOPIC_LEN>("myThing")
            .unwrap();
        let suffix = if cfg!(feature = "commands_cbor") {
            "cbor"
        } else {
            "json"
        };
        assert_eq!(
            accepted.as_str(),
            format!("$aws/commands/things/myThing/executions/abc-123/response/accepted/{suffix}")
        );
        assert_eq!(
            rejected.as_str(),
            format!("$aws/commands/things/myThing/executions/abc-123/response/rejected/{suffix}")
        );
    }

    #[test]
    fn from_str_request() {
        let suffix = if cfg!(feature = "commands_cbor") {
            "cbor"
        } else {
            "json"
        };
        let s = format!("$aws/commands/things/myThing/executions/abc-123/request/{suffix}");
        assert_eq!(Topic::from_str(&s), Some(Topic::Request("abc-123")));
    }

    #[test]
    fn from_str_response_accepted_and_rejected() {
        let suffix = if cfg!(feature = "commands_cbor") {
            "cbor"
        } else {
            "json"
        };
        let acc =
            format!("$aws/commands/things/myThing/executions/abc-123/response/accepted/{suffix}");
        let rej =
            format!("$aws/commands/things/myThing/executions/abc-123/response/rejected/{suffix}");
        assert_eq!(
            Topic::from_str(&acc),
            Some(Topic::ResponseAccepted("abc-123"))
        );
        assert_eq!(
            Topic::from_str(&rej),
            Some(Topic::ResponseRejected("abc-123"))
        );
    }

    #[test]
    fn from_str_rejects_invalid() {
        // Wrong prefix
        assert!(Topic::from_str("$aws/things/myThing/jobs/abc/get").is_none());
        // Wrong format suffix (json when cbor enabled, or vice versa)
        let wrong_fmt = if cfg!(feature = "commands_cbor") {
            "$aws/commands/things/myThing/executions/abc/request/json"
        } else {
            "$aws/commands/things/myThing/executions/abc/request/cbor"
        };
        assert!(Topic::from_str(wrong_fmt).is_none());
        // Empty executionId
        let suffix = if cfg!(feature = "commands_cbor") {
            "cbor"
        } else {
            "json"
        };
        let empty_id = format!("$aws/commands/things/myThing/executions//request/{suffix}");
        assert!(Topic::from_str(&empty_id).is_none());
        // Garbage
        assert!(Topic::from_str("nonsense/topic").is_none());
    }
}
