use serde::{Deserialize, Serialize};

/// To receive error responses, subscribe to
/// - `$aws/certificates/create-from-csr/<payloadFormat>/rejected`
/// - `$aws/certificates/create/<payloadFormat>/rejected`
/// - `$aws/provisioning-templates/<templateName>/provision/<payloadFormat>/rejected`
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
/// **<templateName>:** The provisioning template name.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ErrorResponse<'a> {
    /// The status code.
    #[serde(rename = "statusCode")]
    pub status_code: u16,

    /// The error code.
    #[serde(rename = "errorCode")]
    pub error_code: &'a str,

    /// The error message.
    #[serde(rename = "errorMessage")]
    pub error_message: &'a str,
}

/// Publish a message with the
/// `$aws/certificates/create-from-csr/<payloadFormat>` topic.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CreateCertificateFromCsrRequest<'a> {
    /// The CSR, in PEM format.
    #[serde(rename = "certificateSigningRequest")]
    pub certificate_signing_request: &'a str,
}

/// Subscribe to `$aws/certificates/create-from-csr/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CreateCertificateFromCsrResponse<'a> {
    /// The token to prove ownership of the certificate during provisioning.
    #[serde(rename = "certificateOwnershipToken")]
    pub certificate_ownership_token: &'a str,

    /// The ID of the certificate. Certificate management operations only take a
    /// certificateId.
    #[serde(rename = "certificateId")]
    pub certificate_id: &'a str,

    /// The certificate data, in PEM format.
    #[serde(rename = "certificatePem")]
    pub certificate_pem: &'a str,
}

/// Publish a message on `$aws/certificates/create/<payloadFormat>` with an empty
/// message payload.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CreateKeysAndCertificateRequest;

/// Subscribe to `$aws/certificates/create/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CreateKeysAndCertificateResponse<'a> {
    /// The certificate ID.
    #[serde(rename = "certificateId")]
    pub certificate_id: &'a str,

    /// The certificate data, in PEM format.
    #[serde(rename = "certificatePem")]
    pub certificate_pem: &'a str,

    /// The private key.
    #[serde(rename = "privateKey")]
    pub private_key: &'a str,

    /// The token to prove ownership of the certificate during provisioning.
    #[serde(rename = "certificateOwnershipToken")]
    pub certificate_ownership_token: &'a str,
}

/// Publish a message on
/// `$aws/provisioning-templates/<templateName>/provision/<payloadFormat>`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
/// **<templateName>:** The provisioning template name.
#[derive(Debug, PartialEq, Serialize)]
// #[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct RegisterThingRequest<'a, P: Serialize> {
    /// The token to prove ownership of the certificate. The token is generated
    /// by AWS IoT when you create a certificate over MQTT.
    #[serde(rename = "certificateOwnershipToken")]
    pub certificate_ownership_token: &'a str,

    /// Optional. Key-value pairs from the device that are used by the
    /// pre-provisioning hooks to evaluate the registration request.
    #[serde(rename = "parameters", skip_serializing_if = "Option::is_none")]
    pub parameters: Option<P>,
}

/// Subscribe to
/// `$aws/provisioning-templates/<templateName>/provision/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
/// **<templateName>:** The provisioning template name.
#[derive(Debug, PartialEq, Deserialize)]
// #[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct RegisterThingResponse<'a, C> {
    /// The device configuration defined in the template.
    #[serde(rename = "deviceConfiguration")]
    pub device_configuration: Option<C>,

    /// The name of the IoT thing created during provisioning.
    #[serde(rename = "thingName")]
    pub thing_name: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Parameters<'a> {
        some_key: &'a str,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct DeviceConfiguration {
        some_key: heapless::String<64>,
    }

    #[test]
    fn serialize_optional_parameters() {
        let register_request = RegisterThingRequest {
            certificate_ownership_token: "my_ownership_token",
            parameters: Some(Parameters {
                some_key: "optional_key",
            }),
        };

        let json = serde_json_core::to_string::<_, 128>(&register_request).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"certificateOwnershipToken":"my_ownership_token","parameters":{"some_key":"optional_key"}}"#
        );

        let register_request_none: RegisterThingRequest<'_, Parameters> = RegisterThingRequest {
            certificate_ownership_token: "my_ownership_token",
            parameters: None,
        };

        let json = serde_json_core::to_string::<_, 128>(&register_request_none).unwrap();
        assert_eq!(
            json.as_str(),
            r#"{"certificateOwnershipToken":"my_ownership_token"}"#
        );
    }

    #[test]
    fn deserialize_optional_device_configuration() {
        let register_response =
            r#"{"thingName":"my_thing","deviceConfiguration":{"some_key":"optional_key"}}"#;

        let (response, _) =
            serde_json_core::from_str::<RegisterThingResponse<DeviceConfiguration>>(
                register_response,
            )
            .unwrap();
        assert_eq!(
            response,
            RegisterThingResponse {
                thing_name: "my_thing",
                device_configuration: Some(DeviceConfiguration {
                    some_key: heapless::String::try_from("optional_key").unwrap()
                }),
            }
        );

        let register_response_none = r#"{"thingName":"my_thing"}"#;

        let (response, _) =
            serde_json_core::from_str::<RegisterThingResponse<()>>(register_response_none).unwrap();
        assert_eq!(
            response,
            RegisterThingResponse {
                thing_name: "my_thing",
                device_configuration: None,
            }
        );

        // // FIXME
        // let register_response_none = r#"{"thingName":"my_thing","deviceConfiguration":{}}"#;

        // let (response, _) =
        //     serde_json_core::from_str::<RegisterThingResponse<()>>(&register_response_none)
        //         .unwrap();
        // assert_eq!(
        //     response,
        //     RegisterThingResponse {
        //         thing_name: "my_thing",
        //         device_configuration: None,
        //     }
        // );
    }
}
