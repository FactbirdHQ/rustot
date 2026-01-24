use crate::shadows::Error;
use data_types::Metric;
use embassy_sync::blocking_mutex::raw::RawMutex;
use errors::{ErrorResponse, MetricError};
use mqttrust::{DeferredPayload, Publish, Subscribe, SubscribeTopic, ToPayload};
#[cfg(feature = "metric_cbor")]
use serde::Deserialize;
use serde::Serialize;
use topics::Topic;

pub mod aws_types;
pub mod data_types;
pub mod errors;
pub mod topics;

pub struct MetricHandler<'a, 'm, M: RawMutex> {
    mqtt: &'m mqttrust::MqttClient<'a, M>,
}

impl<'a, 'm, M: RawMutex> MetricHandler<'a, 'm, M> {
    pub fn new(mqtt: &'m mqttrust::MqttClient<'a, M>) -> Self {
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
                            return Err(mqttrust::EncodingError::BufferSize);
                        }
                    };

                    Ok(serializer.into_encoder().writer().position())
                }

                #[cfg(not(feature = "metric_cbor"))]
                {
                    serde_json_core::to_slice(&metric, buf)
                        .map_err(|_| mqttrust::EncodingError::BufferSize)
                }
            },
            max_payload_size,
        );

        let mut subscription = self
            .publish_and_subscribe(payload)
            .await
            .map_err(|_| MetricError::PublishSubscribe)?;

        self.await_metric_response(&mut subscription).await
    }

    async fn await_metric_response(
        &self,
        subscription: &mut mqttrust::Subscription<'a, '_, M, 2>,
    ) -> Result<(), MetricError> {
        loop {
            let message = subscription
                .next_message()
                .await
                .ok_or(MetricError::Malformed)?;

            match Topic::from_str(message.topic_name()) {
                Some(Topic::Accepted) => return Ok(()),
                Some(Topic::Rejected) => {
                    #[cfg(not(feature = "metric_cbor"))]
                    {
                        let error_response =
                            serde_json_core::from_slice::<ErrorResponse>(message.payload())
                                .map_err(|_| MetricError::ErrorResponseDeserialize)?;

                        return Err(error_response.0.status_details.error_code);
                    }

                    #[cfg(feature = "metric_cbor")]
                    {
                        let mut de = minicbor_serde::Deserializer::new(message.payload());
                        let error_response = ErrorResponse::deserialize(&mut de)
                            .map_err(|_| MetricError::ErrorResponseDeserialize)?;

                        return Err(error_response.status_details.error_code);
                    }
                }

                _ => (),
            };
        }
    }
    async fn publish_and_subscribe(
        &self,
        payload: impl ToPayload,
    ) -> Result<mqttrust::Subscription<'a, '_, M, 2>, Error> {
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
                return Err(Error::MqttError(mqttrust::Error::BadTopicFilter));
            }
        };

        Ok(sub)
    }
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use super::data_types::*;

    use serde::{ser::SerializeStruct, Serialize};
    use serde_json_core::heapless::{LinearMap, String};

    #[test]
    fn serialize_version_json() {
        let test_cases = [
            (Version(2, 0), "\"2.0\""),
            (Version(0, 0), "\"0.0\""),
            (Version(0, 1), "\"0.1\""),
            (Version(255, 200), "\"255.200\""),
        ];

        for (version, expected) in test_cases.iter() {
            let string: String<100> = serde_json_core::to_string(version).unwrap();
            assert_eq!(
                string, *expected,
                "Serialization failed for Version({}, {}): expected {}, got {}",
                version.0, version.1, expected, string
            );
        }
    }
    #[test]
    fn serialize_version_cbor() {
        let test_cases: [(Version, [u8; 8]); 4] = [
            (Version(2, 0), [99, 50, 46, 48, 0, 0, 0, 0]),
            (Version(0, 0), [99, 48, 46, 48, 0, 0, 0, 0]),
            (Version(0, 1), [99, 48, 46, 49, 0, 0, 0, 0]),
            (Version(255, 200), [103, 50, 53, 53, 46, 50, 48, 48]),
        ];

        for (version, expected) in test_cases.iter() {
            let mut buf = [0u8; 200];

            let mut serializer =
                minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(&mut buf[..]));

            version.serialize(&mut serializer).unwrap();

            let len = serializer.into_encoder().writer().position();

            assert_eq!(
                &buf[..len],
                &expected[..len],
                "Serialization failed for Version({}, {}): expected {:?}, got {:?}",
                version.0,
                version.1,
                expected,
                &buf[..len],
            );
        }
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

        let mut serializer =
            minicbor_serde::Serializer::new(minicbor::encode::write::Cursor::new(&mut buf[..]));

        metric.serialize(&mut serializer).unwrap();

        let len = serializer.into_encoder().writer().position();

        assert_eq!(
            &buf[..len],
            [
                163, 99, 104, 101, 100, 162, 99, 114, 105, 100, 0, 97, 118, 99, 49, 46, 48, 99,
                109, 101, 116, 246, 100, 99, 109, 101, 116, 161, 117, 77, 121, 77, 101, 116, 114,
                105, 99, 79, 102, 84, 121, 112, 101, 95, 78, 117, 109, 98, 101, 114, 129, 161, 102,
                110, 117, 109, 98, 101, 114, 23
            ]
        )
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"MyMetricOfType_Number\":[{\"number\":23}]}}", payload.as_str())
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
                    string_list: &[self.cell_type.as_str()],
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"cell_type\":[{\"string_list\":[\"gsm\"]}]}}", payload.as_str())
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"myMetric\":[{\"number\":23}]}}", payload.as_str())
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"my_number_list\":[{\"number_list\":[123,456,789]}]}}", payload.as_str())
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"my_string_list\":[{\"string_list\":[\"value_1\",\"value_2\"]}]}}", payload.as_str())
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

        assert_eq!("{\"hed\":{\"rid\":0,\"v\":\"1.0\"},\"met\":null,\"cmet\":{\"MyMetricOfType_Number\":[{\"number\":1}],\"MyMetricOfType_NumberList\":[{\"number_list\":[1,2,3]}],\"MyMetricOfType_StringList\":[{\"string_list\":[\"value_1\",\"value_2\"]}],\"MyMetricOfType_IpList\":[{\"ip_list\":[\"172.0.0.0\",\"172.0.0.10\"]}]}}", payload.as_str())
    }
}
