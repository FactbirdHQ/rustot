pub mod data_types;
mod error;
pub mod topics;

use heapless::LinearMap;
use mqttrust::Mqtt;
#[cfg(feature = "provision_cbor")]
use serde::Serialize;

use self::{
    data_types::{
        CreateCertificateFromCsrResponse, CreateKeysAndCertificateResponse, ErrorResponse,
        RegisterThingRequest, RegisterThingResponse,
    },
    error::Error,
    topics::{PayloadFormat, Subscribe, Topic, Unsubscribe},
};

#[derive(Debug)]
pub struct Credentials<'a> {
    pub certificate_id: &'a str,
    pub certificate_pem: &'a str,
    pub private_key: Option<&'a str>,
}

#[derive(Debug)]
pub enum Response<'a, const P: usize> {
    Credentials(Credentials<'a>),
    DeviceConfiguration(LinearMap<&'a str, &'a str, P>),
}

pub struct FleetProvisioner<'a, M>
where
    M: Mqtt,
{
    mqtt: &'a M,
    template_name: &'a str,
    ownership_token: Option<heapless::String<512>>,
    payload_format: PayloadFormat,
}

impl<'a, M> FleetProvisioner<'a, M>
where
    M: Mqtt,
{
    /// Instantiate a new `FleetProvisioner`, using `template_name` for the provisioning
    pub fn new(mqtt: &'a M, template_name: &'a str) -> Self {
        Self {
            mqtt,
            template_name,
            ownership_token: None,
            payload_format: PayloadFormat::Json,
        }
    }

    #[cfg(feature = "provision_cbor")]
    pub fn new_cbor(mqtt: &'a M, template_name: &'a str) -> Self {
        Self {
            mqtt,
            template_name,
            ownership_token: None,
            payload_format: PayloadFormat::Cbor,
        }
    }

    pub fn initialize(&self) -> Result<(), Error> {
        Subscribe::<4>::new()
            .topic(
                Topic::CreateKeysAndCertificateAccepted(self.payload_format),
                mqttrust::QoS::AtLeastOnce,
            )
            .topic(
                Topic::CreateKeysAndCertificateRejected(self.payload_format),
                mqttrust::QoS::AtLeastOnce,
            )
            .topic(
                Topic::RegisterThingAccepted(self.template_name, self.payload_format),
                mqttrust::QoS::AtLeastOnce,
            )
            .topic(
                Topic::RegisterThingRejected(self.template_name, self.payload_format),
                mqttrust::QoS::AtLeastOnce,
            )
            .send(self.mqtt)?;

        Ok(())
    }

    // TODO: Can we handle this better? If sent from `initialize` it causes a
    // race condition with the subscription ack.
    pub fn begin(&mut self) -> Result<(), Error> {
        self.mqtt.publish(
            Topic::CreateKeysAndCertificate(self.payload_format)
                .format::<29>()?
                .as_str(),
            b"",
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    pub fn register_thing<'b, const P: usize>(
        &mut self,
        parameters: Option<LinearMap<&'b str, &'b str, P>>,
    ) -> Result<(), Error> {
        let certificate_ownership_token = self.ownership_token.take().ok_or(Error::InvalidState)?;

        let register_request = RegisterThingRequest {
            certificate_ownership_token: &certificate_ownership_token,
            parameters,
        };

        let mut payload = [0u8; 1024];

        let payload_len = match self.payload_format {
            #[cfg(feature = "provision_cbor")]
            PayloadFormat::Cbor => {
                let mut serializer = minicbor_serde::Serializer::new(
                    minicbor::encode::write::Cursor::new(&mut payload[..]),
                );
                register_request.serialize(&mut serializer)?;
                serializer.into_encoder().writer().position()
            }
            PayloadFormat::Json => serde_json_core::to_slice(&register_request, &mut payload)?,
        };

        self.mqtt.publish(
            Topic::RegisterThing(self.template_name, self.payload_format)
                .format::<69>()?
                .as_str(),
            &payload[..payload_len],
            mqttrust::QoS::AtLeastOnce,
        )?;

        Ok(())
    }

    pub fn handle_message<'b, const P: usize>(
        &mut self,
        topic_name: &'b str,
        payload: &'b mut [u8],
    ) -> Result<Option<Response<'b, P>>, Error> {
        match Topic::from_str(topic_name) {
            Some(Topic::CreateKeysAndCertificateAccepted(format)) => {
                trace!(
                    "Topic::CreateKeysAndCertificateAccepted {:?}. Payload len: {:?}",
                    format,
                    payload.len()
                );

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        minicbor_serde::from_slice::<CreateKeysAndCertificateResponse>(payload)?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<CreateKeysAndCertificateResponse>(payload)?.0
                    }
                };

                self.ownership_token
                    .replace(heapless::String::from(response.certificate_ownership_token));

                Ok(Some(Response::Credentials(Credentials {
                    certificate_id: response.certificate_id,
                    certificate_pem: response.certificate_pem,
                    private_key: Some(response.private_key),
                })))
            }
            Some(Topic::CreateCertificateFromCsrAccepted(format)) => {
                trace!("Topic::CreateCertificateFromCsrAccepted {:?}", format);

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        minicbor_serde::from_slice::<CreateCertificateFromCsrResponse>(payload)?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<CreateCertificateFromCsrResponse>(payload)?.0
                    }
                };

                self.ownership_token
                    .replace(heapless::String::from(response.certificate_ownership_token));

                Ok(Some(Response::Credentials(Credentials {
                    certificate_id: response.certificate_id,
                    certificate_pem: response.certificate_pem,
                    private_key: None,
                })))
            }
            Some(Topic::RegisterThingAccepted(_, format)) => {
                trace!("Topic::RegisterThingAccepted {:?}", format);

                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => {
                        minicbor_serde::from_slice::<RegisterThingResponse<'_, P>>(payload)?
                    }
                    PayloadFormat::Json => {
                        serde_json_core::from_slice::<RegisterThingResponse<'_, P>>(payload)?.0
                    }
                };

                assert_eq!(response.thing_name, self.mqtt.client_id());

                Ok(Some(Response::DeviceConfiguration(
                    response.device_configuration,
                )))
            }

            // Error happened!
            Some(
                Topic::CreateKeysAndCertificateRejected(format)
                | Topic::CreateCertificateFromCsrRejected(format)
                | Topic::RegisterThingRejected(_, format),
            ) => {
                let response = match format {
                    #[cfg(feature = "provision_cbor")]
                    PayloadFormat::Cbor => minicbor_serde::from_slice::<ErrorResponse>(payload)?,
                    PayloadFormat::Json => serde_json_core::from_slice::<ErrorResponse>(payload)?.0,
                };

                error!("{:?}: {:?}", topic_name, response);

                Err(Error::Response(response.status_code))
            }

            t => {
                trace!("{:?}", t);
                Ok(None)
            }
        }
    }
}

impl<'a, M> Drop for FleetProvisioner<'a, M>
where
    M: Mqtt,
{
    fn drop(&mut self) {
        Unsubscribe::<4>::new()
            .topic(Topic::CreateKeysAndCertificateAccepted(self.payload_format))
            .topic(Topic::CreateKeysAndCertificateRejected(self.payload_format))
            .topic(Topic::RegisterThingAccepted(
                self.template_name,
                self.payload_format,
            ))
            .topic(Topic::RegisterThingRejected(
                self.template_name,
                self.payload_format,
            ))
            .send(self.mqtt)
            .ok();
    }
}
