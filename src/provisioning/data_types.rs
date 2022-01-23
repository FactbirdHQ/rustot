use heapless::FnvIndexMap;
use serde::{Deserialize, Serialize};

/// To receive error responses, subscribe to
/// - `$aws/certificates/create-from-csr/<payloadFormat>/rejected`
/// - `$aws/certificates/create/<payloadFormat>/rejected`
/// - `$aws/provisioning-templates/<templateName>/provision/<payloadFormat>/rejected`
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
/// **<templateName>:** The provisioning template name.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
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
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub struct CreateCertificateFromCsrRequest<'a> {
    /// The CSR, in PEM format.
    #[serde(rename = "certificateSigningRequest")]
    pub certificate_signing_request: &'a str,
}

/// Subscribe to `$aws/certificates/create-from-csr/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
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
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub struct CreateKeysAndCertificateRequest;

/// Subscribe to `$aws/certificates/create/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
#[derive(Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
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
// #[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub struct RegisterThingRequest<'a, const P: usize> {
    /// The token to prove ownership of the certificate. The token is generated
    /// by AWS IoT when you create a certificate over MQTT.
    #[serde(rename = "certificateOwnershipToken")]
    pub certificate_ownership_token: &'a str,

    /// Optional. Key-value pairs from the device that are used by the
    /// pre-provisioning hooks to evaluate the registration request.
    #[serde(rename = "parameters")]
    pub parameters: Option<FnvIndexMap<&'a str, &'a str, P>>,
}

/// Subscribe to
/// `$aws/provisioning-templates/<templateName>/provision/<payloadFormat>/accepted`.
///
/// **<payloadFormat>:** The message payload format as `cbor` or `json`.
/// **<templateName>:** The provisioning template name.
#[derive(Debug, PartialEq, Deserialize)]
// #[cfg_attr(feature = "defmt-impl", derive(defmt::Format))]
pub struct RegisterThingResponse<'a, const P: usize> {
    /// The device configuration defined in the template.
    #[serde(rename = "deviceConfiguration")]
    pub device_configuration: FnvIndexMap<&'a str, &'a str, P>,

    /// The name of the IoT thing created during provisioning.
    #[serde(rename = "thingName")]
    pub thing_name: &'a str,
}
