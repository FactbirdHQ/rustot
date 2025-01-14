use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ErrorResponse<'a> {
    #[serde(rename = "thingName")]
    thing_name: &'a str,
    status: &'a str,
    #[serde(rename = "statusDetails")]
    status_details: StatusDetails<'a>,
    timestamp: i64,
}
#[derive(Debug, Deserialize)]

pub struct StatusDetails<'a> {
    #[serde(rename = "ErrorCode")]
    error_code: MetricError,
    #[serde(rename = "ErrorMessage")]
    error_message: Option<&'a str>,
}
#[derive(Debug, Deserialize)]
pub enum MetricError {
    Malformed,
    InvalidPayload,
    Throttled,
    MissingHeader,
    Other,
}
