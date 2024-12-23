#![allow(dead_code)]
use core::fmt::Write;

use embedded_mqtt::QoS;
use heapless::String;

use crate::{jobs::MAX_THING_NAME_LEN, shadows::Error};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Topic {
    Accepted,
    Rejected,
    Publish,
}

impl Topic {
    const PREFIX: &'static str = "$aws/things";
    const NAME: &'static str = "defender/metrics";
    //TODO: Feature gate json or cbor
    const PAYLOAD_FORMAT: &'static str = "cbor";

    pub fn format<const L: usize>(
        &self,
        thing_name: &str,
        metric_name: &str,
    ) -> Result<String<L>, Error> {
        let mut topic_path = String::new();

        match self {
            Self::Accepted => topic_path.write_fmt(format_args!(
                "{}/{}/{}/{}/{}",
                Self::PREFIX,
                thing_name,
                Self::NAME,
                Self::PAYLOAD_FORMAT,
                metric_name
            )),
            Self::Rejected => topic_path.write_fmt(format_args!(
                "{}/{}/{}/{}/{}",
                Self::PREFIX,
                thing_name,
                Self::NAME,
                Self::PAYLOAD_FORMAT,
                metric_name
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
}

#[derive(Default)]
pub struct Subscribe<const N: usize> {
    topics: heapless::Vec<(Topic, QoS), N>,
}

impl<const N: usize> Subscribe<N> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_topic(self, topic: Topic, qos: QoS) -> Self {
        if self.topics.iter().any(|(t, _)| t == &topic) {
            return self;
        }

        let mut topics = self.topics;
        topics.push((topic, qos)).ok();

        Self { topics }
    }

    pub fn topics(
        self,
        thing_name: &str,
        metric_name: &str,
    ) -> Result<heapless::Vec<(heapless::String<N>, QoS), N>, Error> {
        assert!(thing_name.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|(topic, qos)| Ok(((*topic).format(thing_name, metric_name)?, *qos)))
            .collect()
    }
}

#[derive(Default)]
pub struct Unsubscribe<const N: usize> {
    topics: heapless::Vec<Topic, N>,
}

impl<const N: usize> Unsubscribe<N> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn topic(self, topic: Topic) -> Self {
        if self.topics.iter().any(|t| t == &topic) {
            return self;
        }

        let mut topics = self.topics;
        topics.push(topic).ok();
        Self { topics }
    }

    pub fn topics(
        self,
        thing_name: &str,
        metric_name: &str,
    ) -> Result<heapless::Vec<heapless::String<256>, N>, Error> {
        assert!(thing_name.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|topic| (*topic).format(thing_name, metric_name))
            .collect()
    }
}
