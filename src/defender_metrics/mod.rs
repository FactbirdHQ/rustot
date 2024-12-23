use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{Publish, Subscribe, SubscribeTopic, ToPayload};
use topics::Topic;

use crate::shadows::Error;

pub mod topics;

pub struct MetricHandler<'a, 'm, M: RawMutex> {
    mqtt: &'m embedded_mqtt::MqttClient<'a, M>,
}

impl<'a, 'm, M: RawMutex> MetricHandler<'a, 'm, M> {
    async fn publish_and_subscribe(
        &self,
        payload: impl ToPayload,
        metric_name: &str,
    ) -> Result<embedded_mqtt::Subscription<'a, '_, M, 2>, Error> {
        let sub = self
            .mqtt
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::Accepted
                                    .format::<64>(self.mqtt.client_id(), metric_name)?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::Rejected
                                    .format::<64>(self.mqtt.client_id(), metric_name)?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await
            .map_err(Error::MqttError)?;

        //*** PUBLISH REQUEST ***/
        let topic_name = Topic::Publish.format::<64>(self.mqtt.client_id(), metric_name)?;
        self.mqtt
            .publish(
                Publish::builder()
                    .topic_name(topic_name.as_str())
                    .payload(payload)
                    .build(),
            )
            .await
            .map_err(Error::MqttError)?;

        Ok(sub)
    }
}
