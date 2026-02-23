use serde::Serialize;

use crate::jobs::JobTopic;

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

    pub fn topic_payload(
        self,
        client_id: &str,
        buf: &mut [u8],
    ) -> Result<(heapless::String<{ MAX_THING_NAME_LEN + 21 }>, usize), JobError> {
        let payload_len = serde_json_core::to_slice(
            &&GetPendingJobExecutionsRequest {
                client_token: self.client_token,
            },
            buf,
        )
        .map_err(|_| JobError::Encoding)?;

        Ok((JobTopic::GetPending.format(client_id)?, payload_len))
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
    fn topic_payload() {
        let mut buf = [0u8; 512];
        let (topic, payload_len) = GetPending::new()
            .client_token("test_client:token_pending")
            .topic_payload("test_client", &mut buf)
            .unwrap();

        assert_eq!(
            &buf[..payload_len],
            br#"{"clientToken":"test_client:token_pending"}"#
        );

        assert_eq!(topic.as_str(), "$aws/things/test_client/jobs/get");
    }
}
