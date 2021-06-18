pub mod data_types;

use mqttrust::Mqtt;
use serde::{Deserialize, Serialize};

use self::data_types::JobExecution;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum JobStatusReason {
    #[serde(rename = "")]
    Receiving, /* Update progress status. */
    #[serde(rename = "ready")]
    SigCheckPassed, /* Set status details to Self Test Ready. */
    #[serde(rename = "active")]
    SelfTestActive, /* Set status details to Self Test Active. */
    #[serde(rename = "accepted")]
    Accepted, /* Set job state to Succeeded. */
    #[serde(rename = "rejected")]
    Rejected, /* Set job state to Failed. */
    #[serde(rename = "aborted")]
    Aborted, /* Set job state to Failed. */
    Pal(u32),
}

pub const MAX_CLIENT_TOKEN_LEN: usize = 30;

#[allow(dead_code)]
pub struct JobAgent<'a, M: Mqtt> {
    pub(crate) mqtt: &'a M,
}

impl<'a, M: Mqtt> JobAgent<'a, M> {
    pub fn new(mqtt: &'a M) -> Self {
        Self { mqtt }
    }

    pub fn handle_message<'de, J: Deserialize<'de>, const L: usize>(
        &self,
        topic_parts: &heapless::Vec<&str, L>,
        _payload: &[u8],
    ) -> Result<Option<JobExecution<J>>, ()> {
        if let (Some(&"$aws"), Some(&"jobs"), Some(_a)) =
            (topic_parts.get(0), topic_parts.get(3), topic_parts.get(4))
        {}
        Ok(None)
    }
}
