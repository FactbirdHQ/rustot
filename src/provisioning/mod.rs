pub mod data_types;
mod error;
pub mod topics;

use core::future::Future;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{
    DeferredPayload, EncodingError, Message, Publish, QoS, RetainHandling, Subscribe,
    SubscribeTopic, Subscription,
};
use futures::StreamExt;
use serde::Serialize;
use serde::{de::DeserializeOwned, Deserialize};

pub use error::Error;

use self::data_types::CreateCertificateFromCsrRequest;
use self::{
    data_types::{
        CreateKeysAndCertificateResponse, ErrorResponse, RegisterThingRequest,
        RegisterThingResponse,
    },
    topics::{PayloadFormat, Topic},
};

pub trait CredentialHandler {
    fn store_credentials(
        &mut self,
        credentials: Credentials<'_>,
    ) -> impl Future<Output = Result<(), Error>>;
}

#[derive(Debug)]
pub struct Credentials<'a> {
    pub certificate_id: &'a str,
    pub certificate_pem: &'a str,
    pub private_key: Option<&'a str>,
}

pub struct FleetProvisioner;

impl FleetProvisioner {
    pub async fn provision<'a, C, M: RawMutex, const SUBS: usize>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, M, SUBS>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        credential_handler: &mut impl CredentialHandler,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        Self::provision_inner(
            mqtt,
            template_name,
            parameters,
            None,
            credential_handler,
            PayloadFormat::Json,
        )
        .await
    }

    pub async fn provision_csr<'a, C, M: RawMutex, const SUBS: usize>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, M, SUBS>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        csr: &str,
        credential_handler: &mut impl CredentialHandler,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        Self::provision_inner(
            mqtt,
            template_name,
            parameters,
            Some(csr),
            credential_handler,
            PayloadFormat::Json,
        )
        .await
    }

    #[cfg(feature = "provision_cbor")]
    pub async fn provision_cbor<'a, C, M: RawMutex, const SUBS: usize>(
        mqtt: &'a embedded_mqtt::MqttClient<'a, M, SUBS>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        credential_handler: &mut impl CredentialHandler,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        Self::provision_inner(
            mqtt,
            template_name,
            parameters,
            None,
            credential_handler,
            PayloadFormat::Cbor,
        )
        .await
    }

    #[cfg(feature = "provision_cbor")]
    pub async fn provision_csr_cbor<'a, C, M: RawMutex, const SUBS: usize>(
        mqtt: &embedded_mqtt::MqttClient<'a, M, SUBS>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        csr: &str,
        credential_handler: &mut impl CredentialHandler,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        Self::provision_inner(
            mqtt,
            template_name,
            parameters,
            Some(csr),
            credential_handler,
            PayloadFormat::Cbor,
        )
        .await
    }

    #[cfg(feature = "provision_cbor")]
    async fn provision_inner<'a, C, M: RawMutex, const SUBS: usize>(
        mqtt: &embedded_mqtt::MqttClient<'a, M, SUBS>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        csr: Option<&str>,
        credential_handler: &mut impl CredentialHandler,
        payload_format: PayloadFormat,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        use crate::provisioning::data_types::CreateCertificateFromCsrResponse;

        let mut create_subscription = Self::begin(mqtt, csr, payload_format).await?;

        let mut message = create_subscription
            .next()
            .await
            .ok_or(Error::InvalidState)?;

        let ownership_token = match Topic::from_str(message.topic_name()) {
            Some(Topic::CreateKeysAndCertificateAccepted(format)) => {
                let response = Self::deserialize::<CreateKeysAndCertificateResponse, SUBS>(
                    format,
                    &mut message,
                )?;

                credential_handler
                    .store_credentials(Credentials {
                        certificate_id: response.certificate_id,
                        certificate_pem: response.certificate_pem,
                        private_key: Some(response.private_key),
                    })
                    .await?;

                response.certificate_ownership_token
            }

            Some(Topic::CreateCertificateFromCsrAccepted(format)) => {
                let response = Self::deserialize::<CreateCertificateFromCsrResponse, SUBS>(
                    format,
                    &mut message,
                )?;

                credential_handler
                    .store_credentials(Credentials {
                        certificate_id: response.certificate_id,
                        certificate_pem: response.certificate_pem,
                        private_key: None,
                    })
                    .await?;

                response.certificate_ownership_token
            }

            // Error happened!
            Some(
                Topic::CreateKeysAndCertificateRejected(format)
                | Topic::CreateCertificateFromCsrRejected(format),
            ) => {
                return Err(Self::handle_error(format, message).unwrap_err());
            }

            t => {
                warn!("Got unexpected packet on topic {:?}", t);

                return Err(Error::InvalidState);
            }
        };

        let register_request = RegisterThingRequest {
            certificate_ownership_token: &ownership_token,
            parameters,
        };

        let payload = DeferredPayload::new(
            |buf| {
                Ok(match payload_format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        let mut serializer =
                            serde_cbor::ser::Serializer::new(serde_cbor::ser::SliceWrite::new(buf));
                        register_request
                            .serialize(&mut serializer)
                            .map_err(|_| EncodingError::BufferSize)?;
                        serializer.into_inner().bytes_written()
                    }
                    PayloadFormat::Json => serde_json_core::to_slice(&register_request, buf)
                        .map_err(|_| EncodingError::BufferSize)?,
                })
            },
            1024,
        );

        debug!("Starting RegisterThing");

        let mut register_subscription = mqtt
            .subscribe::<1>(Subscribe::new(&[SubscribeTopic {
                topic_path: Topic::RegisterThingAny(template_name, payload_format)
                    .format::<128>()?
                    .as_str(),
                maximum_qos: QoS::AtLeastOnce,
                no_local: false,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeTime,
            }]))
            .await
            .map_err(|e| {
                error!("Failed subscription to RegisterThingAny! {}", e);
                Error::Mqtt
            })?;

        mqtt.publish(Publish {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            pid: None,
            topic_name: Topic::RegisterThing(template_name, payload_format)
                .format::<69>()?
                .as_str(),
            payload,
            properties: embedded_mqtt::Properties::Slice(&[]),
        })
        .await
        .map_err(|e| {
            error!("Failed publish to RegisterThing! {}", e);
            Error::Mqtt
        })?;

        drop(message);
        drop(create_subscription);

        let mut message = register_subscription
            .next()
            .await
            .ok_or(Error::InvalidState)?;

        match Topic::from_str(message.topic_name()) {
            Some(Topic::RegisterThingAccepted(_, format)) => {
                let response =
                    Self::deserialize::<RegisterThingResponse<'_, C>, SUBS>(format, &mut message)?;

                Ok(response.device_configuration)
            }

            // Error happened!
            Some(Topic::RegisterThingRejected(_, format)) => {
                Err(Self::handle_error(format, message).unwrap_err())
            }

            t => {
                trace!("{:?}", t);

                Err(Error::InvalidState)
            }
        }
    }

    async fn begin<'a, 'b, M: RawMutex, const SUBS: usize>(
        mqtt: &'b embedded_mqtt::MqttClient<'a, M, SUBS>,
        csr: Option<&str>,
        payload_format: PayloadFormat,
    ) -> Result<Subscription<'a, 'b, M, SUBS, 1>, Error> {
        if let Some(csr) = csr {
            let request = CreateCertificateFromCsrRequest {
                certificate_signing_request: csr,
            };

            // FIXME: Serialize directly into the publish payload through `DeferredPublish` API
            let payload = DeferredPayload::new(
                |buf| {
                    Ok(match payload_format {
                        #[cfg(feature = "provision_cbor")]
                        PayloadFormat::Cbor => {
                            let mut serializer = serde_cbor::ser::Serializer::new(
                                serde_cbor::ser::SliceWrite::new(buf),
                            );
                            request
                                .serialize(&mut serializer)
                                .map_err(|_| EncodingError::BufferSize)?;
                            serializer.into_inner().bytes_written()
                        }
                        PayloadFormat::Json => serde_json_core::to_slice(&request, buf)
                            .map_err(|_| EncodingError::BufferSize)?,
                    })
                },
                1024,
            );

            let subscription = mqtt
                .subscribe::<1>(Subscribe::new(&[SubscribeTopic {
                    topic_path: Topic::CreateCertificateFromCsrAny(payload_format)
                        .format::<40>()?
                        .as_str(),
                    maximum_qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: RetainHandling::SendAtSubscribeTime,
                }]))
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
                payload,
                properties: embedded_mqtt::Properties::Slice(&[]),
            })
            .await
            .map_err(|_| Error::Mqtt)?;

            Ok(subscription)
        } else {
            let subscription = mqtt
                .subscribe::<1>(Subscribe::new(&[SubscribeTopic {
                    topic_path: Topic::CreateKeysAndCertificateAny(payload_format)
                        .format::<31>()?
                        .as_str(),
                    maximum_qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: RetainHandling::SendAtSubscribeTime,
                }]))
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

            Ok(subscription)
        }
    }

    fn deserialize<'a, R: Deserialize<'a>, const SUBS: usize>(
        payload_format: PayloadFormat,
        message: &'a mut Message<'_, SUBS>,
    ) -> Result<R, Error> {
        trace!(
            "Accepted Topic {:?}. Payload len: {:?}",
            payload_format,
            message.payload().len()
        );

        Ok(match payload_format {
            #[cfg(feature = "provision_cbor")]
            PayloadFormat::Cbor => serde_cbor::de::from_mut_slice::<R>(message.payload_mut())?,
            PayloadFormat::Json => serde_json_core::from_slice::<R>(message.payload())?.0,
        })
    }

    fn handle_error<const SUBS: usize>(
        format: PayloadFormat,
        mut message: Message<'_, SUBS>,
    ) -> Result<(), Error> {
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
}
