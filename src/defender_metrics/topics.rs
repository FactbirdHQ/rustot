#![allow(dead_code)]
use core::fmt::Write;

use heapless::String;

use crate::shadows::Error;

pub enum PayloadFormat {
    #[cfg(feature = "metric_cbor")]
    Cbor,
    #[cfg(not(feature = "metric_cbor"))]
    Json,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Topic {
    Accepted,
    Rejected,
    Publish,
}

impl Topic {
    const PREFIX: &'static str = "$aws/things";
    const NAME: &'static str = "defender/metrics";

    #[cfg(feature = "metric_cbor")]
    const PAYLOAD_FORMAT: &'static str = "cbor";

    #[cfg(not(feature = "metric_cbor"))]
    const PAYLOAD_FORMAT: &'static str = "json";

    pub fn format<const L: usize>(&self, thing_name: &str) -> Result<String<L>, Error> {
        let mut topic_path = String::new();

        match self {
            Self::Accepted => topic_path.write_fmt(format_args!(
                "{}/{}/{}/{}/accepted",
                Self::PREFIX,
                thing_name,
                Self::NAME,
                Self::PAYLOAD_FORMAT,
            )),
            Self::Rejected => topic_path.write_fmt(format_args!(
                "{}/{}/{}/{}/rejected",
                Self::PREFIX,
                thing_name,
                Self::NAME,
                Self::PAYLOAD_FORMAT,
            )),
            Self::Publish => topic_path.write_fmt(format_args!(
                "{}/{}/{}/{}",
                Self::PREFIX,
                thing_name,
                Self::NAME,
                Self::PAYLOAD_FORMAT,
            )),
        }
        .map_err(|_| Error::Overflow)?;

        Ok(topic_path)
    }

    pub fn from_str(s: &str) -> Option<Topic> {
        let tt = s.splitn(7, '/').collect::<heapless::Vec<&str, 7>>();
        match (tt.get(0), tt.get(1), tt.get(3), tt.get(4)) {
            (Some(&"$aws"), Some(&"things"), Some(&"defender"), Some(&"metrics")) => {
                // This is a defender metric topic, now figure out which one.

                match tt.get(6) {
                    Some(&"accepted") => Some(Topic::Accepted),
                    Some(&"rejected") => Some(Topic::Rejected),
                    _ => return None,
                }
            }
            _ => None,
        }
    }
}
