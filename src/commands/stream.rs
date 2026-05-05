use serde::{Deserialize, Serialize};

use crate::commands::data_types::CommandRequest;
use crate::commands::{
    Commands,
    data_types::CommandStatus,
    error::CommandError,
    topics::{CommandTopic, MAX_COMMAND_TOPIC_LEN, Topic},
    update::Update,
};
use crate::mqtt::{Mqtt, MqttClient, MqttMessage, MqttSubscription, PublishOptions, QoS};

/// Helper for the AWS IoT Commands subscribe / response lifecycle.
///
/// Mirrors `crate::jobs::stream::JobAgent`. Caller drives the message loop on
/// the returned subscription via [`parse_command_request`]; this struct
/// provides convenience helpers for publishing status updates.
///
/// # Example
///
/// ```ignore
/// let agent = CommandAgent::new(&mqtt);
/// let mut sub = agent.subscribe().await?;
/// while let Some(mut msg) = sub.next_message().await {
///     let Some(req) = parse_command_request::<MyCommand>(&mut msg) else { continue };
///     match req.payload {
///         MyCommand::Reboot => {
///             agent.report_in_progress(req.execution_id.as_str(), None).await.ok();
///             // ... do work ...
///             agent.succeed::<()>(req.execution_id.as_str(), None).await?;
///         }
///         MyCommand::Unknown => {
///             agent.reject(req.execution_id.as_str(), "UNKNOWN", None).await?;
///         }
///     }
/// }
/// ```
pub struct CommandAgent<'a, C: MqttClient> {
    mqtt: &'a Mqtt<&'a C>,
}

impl<'a, C: MqttClient> CommandAgent<'a, C> {
    pub fn new(mqtt: &'a Mqtt<&'a C>) -> Self {
        Self { mqtt }
    }

    /// Subscribe to `executions/+/request/{format}` (single topic, QoS 0).
    pub async fn subscribe(&self) -> Result<C::Subscription<'a, 1>, CommandError> {
        let thing = self.mqtt.0.client_id();
        let topic = CommandTopic::Subscribe.format::<MAX_COMMAND_TOPIC_LEN>(thing)?;
        self.mqtt
            .0
            .subscribe(&[(topic.as_str(), QoS::AtMostOnce)])
            .await
            .map_err(|_| CommandError::Mqtt)
    }

    /// Fire-and-forget IN_PROGRESS report (QoS 0).
    pub async fn report_in_progress(
        &self,
        execution_id: &str,
        reason: Option<(&str, Option<&str>)>,
    ) -> Result<(), CommandError> {
        let mut upd: Update<'_, ()> =
            Update::new(CommandStatus::InProgress).execution_id(execution_id);
        if let Some((code, desc)) = reason {
            upd = upd.status_reason(code, desc);
        }
        let topic = CommandTopic::Response(execution_id)
            .format::<MAX_COMMAND_TOPIC_LEN>(self.mqtt.0.client_id())?;
        self.mqtt
            .0
            .publish(&topic, upd)
            .await
            .map_err(|_| CommandError::Mqtt)
    }

    /// Publish `SUCCEEDED` and wait for cloud `accepted`/`rejected`.
    pub async fn succeed<R: Serialize>(
        &self,
        execution_id: &str,
        result: Option<&R>,
    ) -> Result<(), CommandError> {
        match result {
            Some(r) => {
                let upd = Commands::update(CommandStatus::Succeeded)
                    .execution_id(execution_id)
                    .result(r);
                self.publish_and_wait(execution_id, upd).await
            }
            None => {
                let upd: Update<'_, ()> =
                    Commands::update(CommandStatus::Succeeded).execution_id(execution_id);
                self.publish_and_wait(execution_id, upd).await
            }
        }
    }

    /// Publish `FAILED` with a `statusReason` and wait for ack.
    pub async fn fail(
        &self,
        execution_id: &str,
        reason_code: &str,
        description: Option<&str>,
    ) -> Result<(), CommandError> {
        let upd: Update<'_, ()> = Commands::update(CommandStatus::Failed)
            .execution_id(execution_id)
            .status_reason(reason_code, description);
        self.publish_and_wait(execution_id, upd).await
    }

    /// Publish `REJECTED` with a `statusReason` and wait for ack.
    pub async fn reject(
        &self,
        execution_id: &str,
        reason_code: &str,
        description: Option<&str>,
    ) -> Result<(), CommandError> {
        let upd: Update<'_, ()> = Commands::update(CommandStatus::Rejected)
            .execution_id(execution_id)
            .status_reason(reason_code, description);
        self.publish_and_wait(execution_id, upd).await
    }

    async fn publish_and_wait<R: Serialize>(
        &self,
        execution_id: &str,
        payload: Update<'_, R>,
    ) -> Result<(), CommandError> {
        let thing = self.mqtt.0.client_id();
        let accepted_topic =
            CommandTopic::ResponseAccepted(execution_id).format::<MAX_COMMAND_TOPIC_LEN>(thing)?;
        let rejected_topic =
            CommandTopic::ResponseRejected(execution_id).format::<MAX_COMMAND_TOPIC_LEN>(thing)?;

        let mut sub = self
            .mqtt
            .0
            .subscribe(&[
                (accepted_topic.as_str(), QoS::AtLeastOnce),
                (rejected_topic.as_str(), QoS::AtLeastOnce),
            ])
            .await
            .map_err(|_| CommandError::Mqtt)?;

        let topic = CommandTopic::Response(execution_id).format::<MAX_COMMAND_TOPIC_LEN>(thing)?;
        self.mqtt
            .0
            .publish_with_options(&topic, payload, PublishOptions::new().qos(QoS::AtLeastOnce))
            .await
            .map_err(|_| CommandError::Mqtt)?;

        let result = loop {
            let message = match embassy_time::with_timeout(
                embassy_time::Duration::from_secs(5),
                sub.next_message(),
            )
            .await
            {
                Ok(Some(m)) => m,
                Ok(None) => break Err(CommandError::Mqtt),
                Err(_) => break Err(CommandError::Timeout),
            };

            match Topic::from_str(message.topic_name()) {
                Some(Topic::ResponseAccepted(_)) => break Ok(()),
                Some(Topic::ResponseRejected(_)) => break Err(CommandError::Rejected(0)),
                _ => continue,
            }
        };

        let _ = sub.unsubscribe().await;
        result
    }
}

/// Parse an inbound command request from an MQTT message.
///
/// Returns `None` if the topic is not a command request topic for the active
/// payload format, the executionId exceeds [`MAX_EXECUTION_ID_LEN`], or the
/// payload fails to deserialize.
///
/// [`MAX_EXECUTION_ID_LEN`]: crate::commands::MAX_EXECUTION_ID_LEN
pub fn parse_command_request<'m, P>(message: &'m mut impl MqttMessage) -> Option<CommandRequest<P>>
where
    P: Deserialize<'m>,
{
    // Step 1: extract the executionId by copy so the immutable borrow of
    // `topic_name()` ends before we acquire `payload_mut()` (JSON path).
    let execution_id: heapless::String<{ crate::commands::MAX_EXECUTION_ID_LEN }> = {
        let topic = message.topic_name();
        match Topic::from_str(topic)? {
            Topic::Request(id) => heapless::String::try_from(id).ok()?,
            _ => return None,
        }
    };

    // Step 2: deserialize the payload.
    #[cfg(feature = "commands_cbor")]
    let payload = {
        let mut de = minicbor_serde::Deserializer::new(message.payload());
        P::deserialize(&mut de).ok()?
    };
    #[cfg(not(feature = "commands_cbor"))]
    let payload = serde_json_core::from_slice::<P>(message.payload_mut())
        .ok()?
        .0;

    Some(CommandRequest {
        execution_id,
        payload,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mqtt::QoS;

    /// Hand-rolled `MqttMessage` for unit tests; `MockSubscription` does not
    /// deliver messages.
    struct TestMessage {
        topic: heapless::String<256>,
        payload: heapless::Vec<u8, 512>,
    }
    impl MqttMessage for TestMessage {
        fn topic_name(&self) -> &str {
            self.topic.as_str()
        }
        fn payload(&self) -> &[u8] {
            self.payload.as_slice()
        }
        fn payload_mut(&mut self) -> &mut [u8] {
            self.payload.as_mut_slice()
        }
        fn qos(&self) -> QoS {
            QoS::AtMostOnce
        }
        fn dup(&self) -> bool {
            false
        }
        fn retain(&self) -> bool {
            false
        }
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[cfg_attr(feature = "commands_cbor", derive(serde::Serialize))]
    struct MyCmd<'a> {
        action: &'a str,
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn parse_request_json_happy_path() {
        let mut msg = TestMessage {
            topic: heapless::String::try_from(
                "$aws/commands/things/myThing/executions/abc-123/request/json",
            )
            .unwrap(),
            payload: heapless::Vec::from_slice(br#"{"action":"reboot"}"#).unwrap(),
        };

        let req = parse_command_request::<MyCmd<'_>>(&mut msg).expect("parse");
        assert_eq!(req.execution_id.as_str(), "abc-123");
        assert_eq!(req.payload, MyCmd { action: "reboot" });
    }

    #[cfg(not(feature = "commands_cbor"))]
    #[test]
    fn parse_request_returns_none_for_response_topic() {
        let mut msg = TestMessage {
            topic: heapless::String::try_from(
                "$aws/commands/things/myThing/executions/abc/response/accepted/json",
            )
            .unwrap(),
            payload: heapless::Vec::from_slice(b"{}").unwrap(),
        };
        // Use a unit type to avoid the borrow checker complaining about an
        // unused borrow if parse returned Some.
        assert!(parse_command_request::<()>(&mut msg).is_none());
    }

    #[cfg(feature = "commands_cbor")]
    #[test]
    fn parse_request_cbor_happy_path() {
        // Build a CBOR map: { "action": "reboot" }
        let mut buf = [0u8; 64];
        let mut ser =
            minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(&mut buf[..]));
        serde::Serialize::serialize(&MyCmd { action: "reboot" }, &mut ser).unwrap();
        let len = ser.into_encoder().writer().position();

        let mut msg = TestMessage {
            topic: heapless::String::try_from(
                "$aws/commands/things/myThing/executions/abc-123/request/cbor",
            )
            .unwrap(),
            payload: heapless::Vec::from_slice(&buf[..len]).unwrap(),
        };

        let req = parse_command_request::<MyCmd<'_>>(&mut msg).expect("parse");
        assert_eq!(req.execution_id.as_str(), "abc-123");
        assert_eq!(req.payload.action, "reboot");
    }
}
