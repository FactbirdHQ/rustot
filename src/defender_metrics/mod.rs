use crate::shadows::Error;
use data_types::Metric;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{DeferredPayload, Publish, Subscribe, SubscribeTopic, ToPayload};
use errors::{ErrorResponse, MetricError};
use futures::StreamExt;
use serde::Serialize;
use topics::Topic;

// pub mod aws_types;
pub mod aws_types;
pub mod data_types;
pub mod errors;
pub mod topics;

pub struct MetricHandler<'a, 'm, M: RawMutex> {
    mqtt: &'m embedded_mqtt::MqttClient<'a, M>,
}

impl<'a, 'm, M: RawMutex> MetricHandler<'a, 'm, M> {
    pub fn new(mqtt: &'m embedded_mqtt::MqttClient<'a, M>) -> Self {
        Self { mqtt }
    }

    pub async fn publish_metric<'c, C: Serialize>(
        &self,
        metric: Metric<'c, C>,
        max_payload_size: usize,
    ) -> Result<(), MetricError> {
        //Wait for mqtt to connect
        self.mqtt.wait_connected().await;

        let payload = DeferredPayload::new(
            |buf: &mut [u8]| {
                #[cfg(feature = "metric_cbor")]
                {
                    let mut serializer = minicbor_serde::Serializer::new(
                        minicbor::encode::write::Cursor::new(&mut *buf),
                    );

                    match metric.serialize(&mut serializer) {
                        Ok(_) => {}
                        Err(_) => {
                            error!("An error happened when serializing metric with cbor");
                            return Err(embedded_mqtt::EncodingError::BufferSize);
                        }
                    };

                    Ok(serializer.into_encoder().writer().position())
                }

                #[cfg(not(feature = "metric_cbor"))]
                {
                    serde_json_core::to_slice(&metric, buf)
                        .map_err(|_| embedded_mqtt::EncodingError::BufferSize)
                }
            },
            max_payload_size,
        );

        let mut subscription = self
            .publish_and_subscribe(payload)
            .await
            .map_err(|_| MetricError::Other)?;

        loop {
            let message = subscription.next().await.ok_or(MetricError::Malformed)?;

            match Topic::from_str(message.topic_name()) {
                Some(Topic::Accepted) => return Ok(()),
                Some(Topic::Rejected) => {
                    let error_response =
                        serde_json_core::from_slice::<ErrorResponse>(message.payload())
                            .map_err(|_| MetricError::InvalidPayload)?;

                    return Err(error_response.0.status_details.error_code);
                }

                _ => (),
            };
        }
    }
    async fn publish_and_subscribe(
        &self,
        payload: impl ToPayload,
    ) -> Result<embedded_mqtt::Subscription<'a, '_, M, 2>, Error> {
        let sub = self
            .mqtt
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::Accepted
                                    .format::<64>(self.mqtt.client_id())?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::Rejected
                                    .format::<64>(self.mqtt.client_id())?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await
            .map_err(Error::MqttError)?;

        //*** PUBLISH REQUEST ***/
        let topic_name = Topic::Publish.format::<64>(self.mqtt.client_id())?;

        match self
            .mqtt
            .publish(
                Publish::builder()
                    .topic_name(topic_name.as_str())
                    .payload(payload)
                    .build(),
            )
            .await
            .map_err(Error::MqttError)
        {
            Ok(_) => {}
            Err(_) => {
                error!("ERROR PUBLISHING PAYLOAD");
                return Err(Error::MqttError(embedded_mqtt::Error::BadTopicFilter));
            }
        };

        Ok(sub)
    }
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use super::data_types::*;

    use heapless::{LinearMap, String};
    use serde::{ser::SerializeStruct, Serialize};

    #[test]
    fn serialize_version() {
        let version = Version(2, 0);

        let mut buf = [0u8; 3];

        let len = serde_json_core::to_slice(&version, &mut buf).unwrap();

        assert_eq!(len, 3);

        println!("buf = {}", buf);
    }

    #[test]
    fn custom_serialization_cbor() {
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

        let mut buf = [255u8; 1000];

        let mut cbor = minicbor_serde::Serializer::new(&mut buf[..]);

        match metric.serialize(&mut cbor) {
            Ok(_) => {
                error!("SERIALIZE WAS OK");
            }
            Err(e) => {
                error!("SERIALIZE WAS NOT OK!!!!!!!!");
            }
        };

        print!("CBOR: {:x?}", buf);

        assert!(true)
    }

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
    fn custom_serialization_string_list() {
        #[derive(Debug)]
        struct CellType<const N: usize> {
            cell_type: String<N>,
        }

        impl<const N: usize> Serialize for CellType<N> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let mut outer = serializer.serialize_struct("CellType", 1)?;

                // Define the type we want to wrap our signal_strength field in
                #[derive(Serialize)]
                struct StringList<'a> {
                    string_list: &'a [&'a str],
                }

                let list = StringList {
                    string_list: &[&self.cell_type.as_str()],
                };

                // Serialize number and wrap in array
                outer.serialize_field("cell_type", &[list])?;
                outer.end()
            }
        }

        let custom_metrics: CellType<4> = CellType {
            cell_type: String::from_str("gsm").unwrap(),
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
