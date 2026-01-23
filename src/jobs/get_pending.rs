use serde::Serialize;

use crate::jobs::JobTopic;
use crate::mqtt::{PayloadError, ToPayload};

use super::{JobError, MAX_CLIENT_TOKEN_LEN, MAX_THING_NAME_LEN};

/// Gets the list of all jobs for a thing that are not in a terminal state.
///
/// Topic: $aws/things/{thingName}/jobs/get
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GetPendingJobExecutionsRequest<'a> {
    /// A client token used to correlate requests and responses. Enter an
    /// arbitrary value here and it is reflected in the response.
    #[serde(rename = "clientToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_token: Option<&'a str>,
}

#[derive(Default)]
pub struct GetPending<'a> {
    client_token: Option<&'a str>,
}

impl<'a> GetPending<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn client_token(self, client_token: &'a str) -> Self {
        assert!(client_token.len() < MAX_CLIENT_TOKEN_LEN);

        Self {
            client_token: Some(client_token),
        }
    }

    pub fn topic(
        &self,
        client_id: &str,
    ) -> Result<heapless::String<{ MAX_THING_NAME_LEN + 21 }>, JobError> {
        JobTopic::GetPending.format(client_id)
    }
}

impl ToPayload for GetPending<'_> {
    fn max_size(&self) -> usize {
        256
    }

    fn encode(&self, buf: &mut [u8]) -> Result<usize, PayloadError> {
        serde_json_core::to_slice(
            &GetPendingJobExecutionsRequest {
                client_token: self.client_token,
            },
            buf,
        )
        .map_err(|_| PayloadError::BufferSize)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json_core::to_string;

    #[test]
    fn serialize_requests() {
        let req = GetPendingJobExecutionsRequest {
            client_token: Some("test_client:token_pending"),
        };
        assert_eq!(
            &to_string::<_, 512>(&req).unwrap(),
            r#"{"clientToken":"test_client:token_pending"}"#
        );
    }

    #[test]
    fn topic_and_payload() {
        let get_pending = GetPending::new().client_token("test_client:token_pending");

        let topic = get_pending.topic("test_client").unwrap();

        let mut buf = [0u8; 256];
        let len = get_pending.encode(&mut buf).unwrap();

        assert_eq!(
            &buf[..len],
            br#"{"clientToken":"test_client:token_pending"}"#
        );

        assert_eq!(topic.as_str(), "$aws/things/test_client/jobs/get");
    }
}
