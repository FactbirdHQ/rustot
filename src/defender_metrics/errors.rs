use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ErrorResponse<'a> {
    #[serde(rename = "thingName")]
    pub thing_name: &'a str,
    pub status: &'a str,
    #[serde(rename = "statusDetails")]
    pub status_details: StatusDetails<'a>,
    pub timestamp: i64,
}
#[derive(Debug, Deserialize)]

pub struct StatusDetails<'a> {
    #[serde(rename = "ErrorCode")]
    pub error_code: MetricError,
    #[serde(rename = "ErrorMessage")]
    pub error_message: Option<&'a str>,
}
#[derive(defmt::Format, Debug, Deserialize)]
pub enum MetricError {
    Malformed,
    InvalidPayload,
    Throttled,
    MissingHeader,
    Other,
}
