use crate::shadows::Error;
use data_types::Metric;
use errors::{ErrorResponse, MetricError};
use mqttrust::{Mqtt, QoS, SubscribeTopic};
use serde::Serialize;
use topics::{Subscribe, Topic, Unsubscribe};

// pub mod aws_types;
pub mod aws_types;
pub mod data_types;
pub mod errors;
pub mod topics;

pub struct MetricHandler<'a, M: Mqtt> {
    mqtt: &'a M,
}

impl<'a, M: Mqtt> MetricHandler<'a, M> {
    pub fn new(mqtt: &'a M) -> Self {
        Self { mqtt }
    }

    pub fn publish_metric<'c, C: Serialize>(
        &self,
        metric: Metric<'c, C>,
    ) -> Result<(), MetricError> {
        let payload = DeferredPayload::new(
            |buf: &mut [u8]| {
                serde_json_core::to_slice(&metric, buf).map_err(|_| MetricError::Other)
            },
            4000, //FIXME: How big should this be?
        );

        // Get topic for publishing metric
        let topic = Topic::Publish
            .format(self.mqtt.client_id())
            .map_err(|_| MetricError::Other)?;

        //Subscribe to accepted and rejected topics
        self.subscribe().map_err(|| MetricError::Other)?;

        self.mqtt
            .publish(topic.as_str(), &payload, QoS::AtLeastOnce)?;
        Ok(())
    }

    /// Subscribe to accepted and rejected
    pub fn subscribe(&self) -> Result<(), Error> {
        let accepted = Topic::Accepted.format(self.mqtt.client_id())?;
        let rejected = Topic::Accepted.format(self.mqtt.client_id())?;

        self.mqtt.subscribe(&[
            SubscribeTopic {
                topic_path: &accepted,
                qos: QoS::AtLeastOnce,
            },
            SubscribeTopic {
                topic_path: &rejected,
                qos: QoS::AtLeastOnce,
            },
        ]);

        Ok(())
    }

    /// Unsubscribe to accepted and rejected
    pub fn unsubscribe(&self) -> Result<(), Error> {
        let accepted = Topic::Accepted.format(self.mqtt.client_id())?;
        let rejected = Topic::Accepted.format(self.mqtt.client_id())?;

        self.mqtt
            .unsubscribe(&[&accepted.as_str(), &rejected.as_str()]);

        Ok(())
    }
}

impl<'a, M: Mqtt> Drop for MetricHandler<'a, M: Mqtt> {
    fn drop(&mut self) {
        self.unsubscribe().ok();
    }
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use super::data_types::*;

    use heapless::{LinearMap, String};
    use serde::{ser::SerializeStruct, Serialize};

    #[test]
    fn custom_serialization() {
        #[derive(Debug)]
        struct WifiMetric {
            signal_strength: u8,
        }

        impl Serialize for WifiMetric {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let mut outer = serializer.serialize_struct("WifiMetricWrapper", 1)?;

                // Define the type we want to wrap our signal_strength field in
                #[derive(Serialize)]
                struct Number {
                    number: u8,
                }

                let number = Number {
                    number: self.signal_strength,
                };

                // Serialize number and wrap in array
                outer.serialize_field("MyMetricOfType_Number", &[number])?;
                outer.end()
            }
        }

        let custom_metrics: WifiMetric = WifiMetric {
            signal_strength: 23,
        };

        let metric = Metric::builder()
            .header(Default::default())
            .custom_metrics(custom_metrics)
            .build();

        let payload: String<4000> = serde_json_core::to_string(&metric).unwrap();

        println!("buffer = {}", payload);

        assert!(true)
    }

    #[test]
    fn number() {
        let mut custom_metrics: LinearMap<String<24>, [CustomMetric; 1], 16> = LinearMap::new();

        let name_of_metric = String::from_str("myMetric").unwrap();

        custom_metrics
            .insert(name_of_metric, [CustomMetric::Number(23)])
            .unwrap();

        let metric = Metric::builder()
            .header(Default::default())
            .custom_metrics(custom_metrics)
            .build();

        let payload: String<4000> = serde_json_core::to_string(&metric).unwrap();

        println!("buffer = {}", payload);

        assert!(true)
    }

    #[test]
    fn number_list() {
        let mut custom_metrics: LinearMap<String<24>, [CustomMetric; 1], 16> = LinearMap::new();

        // NUMBER LIST
        let my_number_list = String::from_str("my_number_list").unwrap();

        custom_metrics
            .insert(my_number_list, [CustomMetric::NumberList(&[123, 456, 789])])
            .unwrap();

        let metric = Metric::builder()
            .header(Default::default())
            .custom_metrics(custom_metrics)
            .build();

        let payload: String<4000> = serde_json_core::to_string(&metric).unwrap();

        println!("buffer = {}", payload);

        assert!(true)
    }

    #[test]
    fn string_list() {
        let mut custom_metrics: LinearMap<String<24>, [CustomMetric; 1], 16> = LinearMap::new();

        // STRING LIST
        let my_string_list = String::from_str("my_string_list").unwrap();

        custom_metrics
            .insert(
                my_string_list,
                [CustomMetric::StringList(&["value_1", "value_2"])],
            )
            .unwrap();

        let metric = Metric::builder()
            .header(Default::default())
            .custom_metrics(custom_metrics)
            .build();

        let payload: String<4000> = serde_json_core::to_string(&metric).unwrap();

        println!("buffer = {}", payload);

        assert!(true)
    }

    #[test]
    fn all_types() {
        let mut custom_metrics: LinearMap<String<32>, [CustomMetric; 1], 4> = LinearMap::new();

        let my_number = String::from_str("MyMetricOfType_Number").unwrap();
        custom_metrics
            .insert(my_number, [CustomMetric::Number(1)])
            .unwrap();

        let my_number_list = String::from_str("MyMetricOfType_NumberList").unwrap();
        custom_metrics
            .insert(my_number_list, [CustomMetric::NumberList(&[1, 2, 3])])
            .unwrap();

        let my_string_list = String::from_str("MyMetricOfType_StringList").unwrap();
        custom_metrics
            .insert(
                my_string_list,
                [CustomMetric::StringList(&["value_1", "value_2"])],
            )
            .unwrap();

        let my_ip_list = String::from_str("MyMetricOfType_IpList").unwrap();
        custom_metrics
            .insert(
                my_ip_list,
                [CustomMetric::IpList(&["172.0.0.0", "172.0.0.10"])],
            )
            .unwrap();

        let metric = Metric::builder()
            .header(Default::default())
            .custom_metrics(custom_metrics)
            .build();

        let payload: String<4000> = serde_json_core::to_string(&metric).unwrap();

        println!("buffer = {}", payload);

        assert!(true)
    }
}
