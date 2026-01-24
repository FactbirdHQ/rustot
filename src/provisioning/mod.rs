pub mod data_types;
mod error;
pub mod topics;

use core::future::Future;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_mqtt::{
    DeferredPayload, EncodingError, Publish, Subscribe, SubscribeTopic, Subscription,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub use error::Error;

use self::{
    data_types::{
        CreateCertificateFromCsrRequest, CreateCertificateFromCsrResponse,
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
    pub async fn provision<'a, C, M: RawMutex>(
        mqtt: &embedded_mqtt::MqttClient<'a, M>,
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

    pub async fn provision_csr<'a, C, M: RawMutex>(
        mqtt: &embedded_mqtt::MqttClient<'a, M>,
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
    pub async fn provision_cbor<'a, C, M: RawMutex>(
        mqtt: &embedded_mqtt::MqttClient<'a, M>,
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
    pub async fn provision_csr_cbor<'a, C, M: RawMutex>(
        mqtt: &embedded_mqtt::MqttClient<'a, M>,
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

    async fn provision_inner<'a, C, M: RawMutex>(
        mqtt: &embedded_mqtt::MqttClient<'a, M>,
        template_name: &str,
        parameters: Option<impl Serialize>,
        csr: Option<&str>,
        credential_handler: &mut impl CredentialHandler,
        payload_format: PayloadFormat,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned,
    {
        let mut create_subscription = Self::begin(mqtt, csr, payload_format).await?;
        let mut message = create_subscription
            .next_message()
            .await
            .ok_or(Error::InvalidState)?;

        let ownership_token = match Topic::from_str(message.topic_name()) {
            Some(Topic::CreateKeysAndCertificateAccepted(format)) => {
                let response =
                    Self::deserialize::<CreateKeysAndCertificateResponse>(format, &mut message)?;

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
                let response = Self::deserialize::<CreateCertificateFromCsrResponse>(
                    format,
                    message.payload_mut(),
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
                return Err(Self::handle_error(format, message.payload_mut()).unwrap_err());
            }

            t => {
                warn!("Got unexpected packet on topic {:?}", t);

                return Err(Error::InvalidState);
            }
        };

        let register_request = RegisterThingRequest {
            certificate_ownership_token: ownership_token,
            parameters,
        };

        let payload = DeferredPayload::new(
            |buf| {
                Ok(match payload_format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        let mut serializer = minicbor_serde::Serializer::new(
                            minicbor::encode::write::Cursor::new(buf),
                        );
                        register_request
                            .serialize(&mut serializer)
                            .map_err(|_| EncodingError::BufferSize)?;
                        serializer.into_encoder().writer().position()
                    }
                    PayloadFormat::Json => serde_json_core::to_slice(&register_request, buf)
                        .map_err(|_| EncodingError::BufferSize)?,
                })
            },
            1024,
        );

        debug!("Starting RegisterThing");

        let mut register_subscription = mqtt
            .subscribe::<2>(
                Subscribe::builder()
                    .topics(&[
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::RegisterThingAccepted(template_name, payload_format)
                                    .format::<150>()?
                                    .as_str(),
                            )
                            .build(),
                        SubscribeTopic::builder()
                            .topic_path(
                                Topic::RegisterThingRejected(template_name, payload_format)
                                    .format::<150>()?
                                    .as_str(),
                            )
                            .build(),
                    ])
                    .build(),
            )
            .await?;

        mqtt.publish(
            Publish::builder()
                .topic_name(
                    Topic::RegisterThing(template_name, payload_format)
                        .format::<69>()?
                        .as_str(),
                )
                .payload(payload)
                .build(),
        )
        .await?;

        drop(message);
        create_subscription.unsubscribe().await?;

        let mut message = register_subscription
            .next_message()
            .await
            .ok_or(Error::InvalidState)?;

        match Topic::from_str(message.topic_name()) {
            Some(Topic::RegisterThingAccepted(_, format)) => {
                let response = Self::deserialize::<RegisterThingResponse<'_, C>>(
                    format,
                    message.payload_mut(),
                )?;

                Ok(response.device_configuration)
            }

            // Error happened!
            Some(Topic::RegisterThingRejected(_, format)) => {
                Err(Self::handle_error(format, message.payload_mut()).unwrap_err())
            }

            t => {
                trace!("{:?}", t);

                Err(Error::InvalidState)
            }
        }
    }

    async fn begin<'a, 'b, M: RawMutex>(
        mqtt: &'b embedded_mqtt::MqttClient<'a, M>,
        csr: Option<&str>,
        payload_format: PayloadFormat,
    ) -> Result<Subscription<'a, 'b, M, 2>, Error> {
        if let Some(csr) = csr {
            let subscription = mqtt
                .subscribe(
                    Subscribe::builder()
                        .topics(&[
                            SubscribeTopic::builder()
                                .topic_path(
                                    Topic::CreateCertificateFromCsrRejected(payload_format)
                                        .format::<47>()?
                                        .as_str(),
                                )
                                .build(),
                            SubscribeTopic::builder()
                                .topic_path(
                                    Topic::CreateCertificateFromCsrAccepted(payload_format)
                                        .format::<47>()?
                                        .as_str(),
                                )
                                .build(),
                        ])
                        .build(),
                )
                .await?;

            let request = CreateCertificateFromCsrRequest {
                certificate_signing_request: csr,
            };

            let payload = DeferredPayload::new(
                |buf| {
                    Ok(match payload_format {
                        #[cfg(feature = "provision_cbor")]
                        PayloadFormat::Cbor => {
                            let mut serializer = minicbor_serde::Serializer::new(
                                minicbor::encode::write::Cursor::new(buf),
                            );
                            request
                                .serialize(&mut serializer)
                                .map_err(|_| EncodingError::BufferSize)?;
                            serializer.into_encoder().writer().position()
                        }
                        PayloadFormat::Json => serde_json_core::to_slice(&request, buf)
                            .map_err(|_| EncodingError::BufferSize)?,
                    })
                },
                csr.len() + 32,
            );

            mqtt.publish(
                Publish::builder()
                    .topic_name(
                        Topic::CreateCertificateFromCsr(payload_format)
                            .format::<40>()?
                            .as_str(),
                    )
                    .payload(payload)
                    .build(),
            )
            .await?;

            Ok(subscription)
        } else {
            let subscription = mqtt
                .subscribe(
                    Subscribe::builder()
                        .topics(&[
                            SubscribeTopic::builder()
                                .topic_path(
                                    Topic::CreateKeysAndCertificateAccepted(payload_format)
                                        .format::<38>()?
                                        .as_str(),
                                )
                                .build(),
                            SubscribeTopic::builder()
                                .topic_path(
                                    Topic::CreateKeysAndCertificateRejected(payload_format)
                                        .format::<38>()?
                                        .as_str(),
                                )
                                .build(),
                        ])
                        .build(),
                )
                .await?;

            mqtt.publish(
                Publish::builder()
                    .topic_name(
                        Topic::CreateKeysAndCertificate(payload_format)
                            .format::<29>()?
                            .as_str(),
                    )
                    .payload(b"")
                    .build(),
            )
            .await?;

            Ok(subscription)
        }
    }

    fn deserialize<'a, R: Deserialize<'a>>(
        payload_format: PayloadFormat,
        payload: &'a mut [u8],
    ) -> Result<R, Error> {
        Ok(match payload_format {
            #[cfg(feature = "provision_cbor")]
            PayloadFormat::Cbor => minicbor_serde::from_slice::<R>(payload)?,
            PayloadFormat::Json => serde_json_core::from_slice::<R>(payload)?.0,
        })
    }

    fn handle_error(format: PayloadFormat, payload: &mut [u8]) -> Result<(), Error> {
        let response = match format {
            #[cfg(feature = "provision_cbor")]
            PayloadFormat::Cbor => minicbor_serde::from_slice::<ErrorResponse>(payload)?,
            PayloadFormat::Json => serde_json_core::from_slice::<ErrorResponse>(payload)?.0,
        };

        error!("{:?}", response);

        Err(Error::Response(response.status_code))
    }
}
