pub mod data_types;
mod error;
pub mod topics;

use core::future::Future;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::{Publish, QoS, RetainHandling, Subscribe, SubscribeTopic};
use futures::StreamExt;
use serde::de::DeserializeOwned;
#[cfg(feature = "provision_cbor")]
use serde::Serialize;

use self::{
    data_types::{
        CreateCertificateFromCsrResponse, CreateKeysAndCertificateResponse, ErrorResponse,
        RegisterThingRequest, RegisterThingResponse,
    },
    error::Error,
    topics::{PayloadFormat, Topic},
};

#[derive(Debug)]
pub struct Credentials<'a> {
    pub certificate_id: &'a str,
    pub certificate_pem: &'a str,
    pub private_key: Option<&'a str>,
}

pub struct FleetProvisioner;

impl FleetProvisioner {
    /// Instantiate a new `FleetProvisioner`, using `template_name` for the provisioning
    pub async fn provision<'a, F, Fut, P, C>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, NoopRawMutex, 2>,
        template_name: &'a str,
        parameters: Option<P>,
        f: F,
    ) -> Result<Option<C>, Error>
    where
        F: FnOnce(Credentials<'_>) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
        P: Serialize,
        C: DeserializeOwned,
    {
        Self::provision_inner(mqtt, template_name, parameters, f, PayloadFormat::Json).await
    }

    #[cfg(feature = "provision_cbor")]
    pub async fn provision_cbor<'a, F, Fut, P, C>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, NoopRawMutex, 2>,
        template_name: &'a str,
        parameters: Option<P>,
        f: F,
    ) -> Result<Option<C>, Error>
    where
        F: FnOnce(Credentials<'_>) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
        P: Serialize,
        C: DeserializeOwned,
    {
        Self::provision_inner(mqtt, template_name, parameters, f, PayloadFormat::Cbor).await
    }

    async fn provision_inner<'a, F, Fut, P, C>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, NoopRawMutex, 2>,
        template_name: &'a str,
        parameters: Option<P>,
        f: F,
        payload_format: PayloadFormat,
    ) -> Result<Option<C>, Error>
    where
        F: FnOnce(Credentials<'_>) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
        P: Serialize,
        C: DeserializeOwned,
    {
        let certificate_ownership_token =
            Self::create_keys_and_certificates(mqtt, payload_format, f).await?;

        Self::register_thing(
            mqtt,
            template_name,
            payload_format,
            certificate_ownership_token.as_str(),
            parameters,
        )
        .await
    }

    pub async fn create_keys_and_certificates<F, Fut>(
        mqtt: &embedded_mqtt::MqttClient<'_, NoopRawMutex, 2>,
        payload_format: PayloadFormat,
        f: F,
    ) -> Result<heapless::String<512>, Error>
    where
        F: FnOnce(Credentials<'_>) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let topic_paths = topics::Subscribe::<2>::new()
            .topic(
                Topic::CreateKeysAndCertificateAccepted(payload_format),
                QoS::AtLeastOnce,
            )
            .topic(
                Topic::CreateKeysAndCertificateRejected(payload_format),
                QoS::AtLeastOnce,
            )
            .topics::<38>()?;

        let subscribe_topics = topic_paths
            .iter()
            .map(|(s, qos)| SubscribeTopic {
                topic_path: s.as_str(),
                maximum_qos: *qos,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime,
            })
            .collect::<heapless::Vec<_, 2>>();

        let mut subscription = mqtt
            .subscribe::<2>(Subscribe {
                pid: None,
                properties: embedded_mqtt::Properties::Slice(&[]),
                topics: subscribe_topics.as_slice(),
            })
            .await
            .map_err(|_| Error::Mqtt)?;

        mqtt.publish(Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            pid: None,
            topic_name: Topic::CreateKeysAndCertificate(payload_format)
                .format::<29>()?
                .as_str(),
            payload: b"",
            properties: embedded_mqtt::Properties::Slice(&[]),
        })
        .await
        .map_err(|_| Error::Mqtt)?;

        let mut message = subscription.next().await.ok_or(Error::InvalidState)?;

        match Topic::from_str(message.topic_name()) {
            Some(Topic::CreateKeysAndCertificateAccepted(format)) => {
                trace!(
                    "Topic::CreateKeysAndCertificateAccepted {:?}. Payload len: {:?}",
                    format,
                    message.payload().len()
                );

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => serde_cbor::de::from_mut_slice::<
                        CreateKeysAndCertificateResponse,
                    >(message.payload_mut())?,
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<CreateKeysAndCertificateResponse>(
                            message.payload(),
                        )?
                        .0
                    }
                };

                f(Credentials {
                    certificate_id: response.certificate_id,
                    certificate_pem: response.certificate_pem,
                    private_key: Some(response.private_key),
                })
                .await?;

                Ok(heapless::String::try_from(response.certificate_ownership_token).unwrap())
            }

            // Error happened!
            Some(Topic::CreateKeysAndCertificateRejected(format)) => {
                error!(">> {:?}", message.topic_name());

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        serde_cbor::de::from_mut_slice::<ErrorResponse>(message.payload_mut())?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<ErrorResponse>(message.payload())?.0
                    }
                };

                error!("{:?}", response);

                Err(Error::Response(response.status_code))
            }

            t => {
                trace!("{:?}", t);

                Err(Error::InvalidState)
            }
        }
    }

    pub async fn create_certificate_from_csr<F, Fut>(
        mqtt: &embedded_mqtt::MqttClient<'_, NoopRawMutex, 2>,
        payload_format: PayloadFormat,
        f: F,
    ) -> Result<heapless::String<512>, Error>
    where
        F: FnOnce(Credentials<'_>) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let topic_paths = topics::Subscribe::<2>::new()
            .topic(
                Topic::CreateCertificateFromCsrAccepted(payload_format),
                QoS::AtLeastOnce,
            )
            .topic(
                Topic::CreateCertificateFromCsrRejected(payload_format),
                QoS::AtLeastOnce,
            )
            .topics::<47>()?;

        let subscribe_topics = topic_paths
            .iter()
            .map(|(s, qos)| SubscribeTopic {
                topic_path: s.as_str(),
                maximum_qos: *qos,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime,
            })
            .collect::<heapless::Vec<_, 2>>();

        let mut subscription = mqtt
            .subscribe::<2>(Subscribe {
                pid: None,
                properties: embedded_mqtt::Properties::Slice(&[]),
                topics: subscribe_topics.as_slice(),
            })
            .await
            .map_err(|_| Error::Mqtt)?;

        mqtt.publish(Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            pid: None,
            topic_name: Topic::CreateCertificateFromCsr(payload_format)
                .format::<38>()?
                .as_str(),
            payload: b"",
            properties: embedded_mqtt::Properties::Slice(&[]),
        })
        .await
        .map_err(|_| Error::Mqtt)?;

        let mut message = subscription.next().await.ok_or(Error::InvalidState)?;

        match Topic::from_str(message.topic_name()) {
            Some(Topic::CreateCertificateFromCsrAccepted(format)) => {
                trace!(
                    "Topic::CreateCertificateFromCsrAccepted {:?}. Payload len: {:?}",
                    format,
                    message.payload().len()
                );

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => serde_cbor::de::from_mut_slice::<
                        CreateCertificateFromCsrResponse,
                    >(message.payload_mut())?,
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<CreateCertificateFromCsrResponse>(
                            message.payload(),
                        )?
                        .0
                    }
                };

                f(Credentials {
                    certificate_id: response.certificate_id,
                    certificate_pem: response.certificate_pem,
                    private_key: None,
                })
                .await?;

                // FIXME: It should be possible to re-arrange stuff to get rid of the need for this 512 byte stack alloc
                Ok(heapless::String::try_from(response.certificate_ownership_token).unwrap())
            }

            // Error happened!
            Some(Topic::CreateCertificateFromCsrRejected(format)) => {
                error!(">> {:?}", message.topic_name());

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        serde_cbor::de::from_mut_slice::<ErrorResponse>(message.payload_mut())?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<ErrorResponse>(message.payload())?.0
                    }
                };

                error!("{:?}", response);

                Err(Error::Response(response.status_code))
            }

            t => {
                trace!("{:?}", t);

                Err(Error::InvalidState)
            }
        }
    }

    pub async fn register_thing<P: Serialize, C: DeserializeOwned>(
        mqtt: &embedded_mqtt::MqttClient<'_, NoopRawMutex, 2>,
        template_name: &str,
        payload_format: PayloadFormat,
        certificate_ownership_token: &str,
        parameters: Option<P>,
    ) -> Result<Option<C>, Error> {
        let topic_paths = topics::Subscribe::<2>::new()
            .topic(
                Topic::RegisterThingAccepted(template_name, payload_format),
                QoS::AtLeastOnce,
            )
            .topic(
                Topic::RegisterThingRejected(template_name, payload_format),
                QoS::AtLeastOnce,
            )
            .topics::<128>()?;

        let subscribe_topics = topic_paths
            .iter()
            .map(|(s, qos)| SubscribeTopic {
                topic_path: s.as_str(),
                maximum_qos: *qos,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime,
            })
            .collect::<heapless::Vec<_, 2>>();

        let mut subscription = mqtt
            .subscribe::<2>(Subscribe {
                pid: None,
                properties: embedded_mqtt::Properties::Slice(&[]),
                topics: subscribe_topics.as_slice(),
            })
            .await
            .map_err(|_| Error::Mqtt)?;

        let register_request = RegisterThingRequest {
            certificate_ownership_token: &certificate_ownership_token,
            parameters,
        };

        // FIXME: Serialize directly into the publish payload through `DeferredPublish` API
        let payload = &mut [0u8; 1024];

        let payload_len = match payload_format {
            #[cfg(feature = "provision_cbor")]
            PayloadFormat::Cbor => {
                let mut serializer =
                    serde_cbor::ser::Serializer::new(serde_cbor::ser::SliceWrite::new(payload));
                register_request.serialize(&mut serializer)?;
                serializer.into_inner().bytes_written()
            }
            PayloadFormat::Json => serde_json_core::to_slice(&register_request, payload)?,
        };

        mqtt.publish(Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            pid: None,
            topic_name: Topic::RegisterThing(template_name, payload_format)
                .format::<69>()?
                .as_str(),
            payload: &payload[..payload_len],
            properties: embedded_mqtt::Properties::Slice(&[]),
        })
        .await
        .map_err(|_| Error::Mqtt)?;

        let mut message = subscription.next().await.ok_or(Error::InvalidState)?;

        match Topic::from_str(message.topic_name()) {
            Some(Topic::RegisterThingAccepted(_, format)) => {
                trace!("Topic::RegisterThingAccepted {:?}", format);

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        serde_cbor::de::from_mut_slice::<RegisterThingResponse<'_, C>>(payload)?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<RegisterThingResponse<'_, C>>(payload)?.0
                    }
                };

                Ok(response.device_configuration)
            }

            // Error happened!
            Some(Topic::RegisterThingRejected(_, format)) => {
                error!(">> {:?}", message.topic_name());

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        serde_cbor::de::from_mut_slice::<ErrorResponse>(message.payload_mut())?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<ErrorResponse>(message.payload())?.0
                    }
                };

                error!("{:?}", response);

                Err(Error::Response(response.status_code))
            }

            t => {
                trace!("{:?}", t);

                Err(Error::InvalidState)
            }
        }
    }
}
