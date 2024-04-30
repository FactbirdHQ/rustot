#![allow(dead_code)]

use core::fmt::Write;

use heapless::String;

use crate::jobs::MAX_THING_NAME_LEN;

use super::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Topic {
    // Outgoing Topics
    Get,
    Update,
    Delete,

    // Incoming Topics
    GetAccepted,
    GetRejected,
    UpdateDelta,
    UpdateAccepted,
    UpdateDocuments,
    UpdateRejected,
    DeleteAccepted,
    DeleteRejected,
    Any,
}

impl Topic {
    const PREFIX: &'static str = "$aws/things";
    const SHADOW: &'static str = "shadow";

    pub fn from_str(s: &str) -> Option<(Self, &str, Option<&str>)> {
        let tt = s.splitn(9, '/').collect::<heapless::Vec<&str, 9>>();
        match (tt.get(0), tt.get(1), tt.get(2), tt.get(3)) {
            (Some(&"$aws"), Some(&"things"), Some(thing_name), Some(&Self::SHADOW)) => {
                // This is a shadow topic, now figure out which one.
                let (shadow_name, next_index) = if let Some(&"name") = tt.get(4) {
                    (tt.get(5).map(|s| *s), 6)
                } else {
                    (None, 4)
                };

                Some(match (tt.get(next_index), tt.get(next_index + 1)) {
                    (Some(&"get"), Some(&"accepted")) => {
                        (Topic::GetAccepted, *thing_name, shadow_name)
                    }
                    (Some(&"get"), Some(&"rejected")) => {
                        (Topic::GetRejected, *thing_name, shadow_name)
                    }
                    (Some(&"update"), Some(&"delta")) => {
                        (Topic::UpdateDelta, *thing_name, shadow_name)
                    }
                    (Some(&"update"), Some(&"accepted")) => {
                        (Topic::UpdateAccepted, *thing_name, shadow_name)
                    }
                    (Some(&"update"), Some(&"documents")) => {
                        (Topic::UpdateDocuments, *thing_name, shadow_name)
                    }
                    (Some(&"update"), Some(&"rejected")) => {
                        (Topic::UpdateRejected, *thing_name, shadow_name)
                    }
                    (Some(&"delete"), Some(&"accepted")) => {
                        (Topic::DeleteAccepted, *thing_name, shadow_name)
                    }
                    (Some(&"delete"), Some(&"rejected")) => {
                        (Topic::DeleteRejected, *thing_name, shadow_name)
                    }
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    pub const fn direction(&self) -> Direction {
        if matches!(self, Topic::Get | Topic::Update | Topic::Delete) {
            Direction::Outgoing
        } else {
            Direction::Incoming
        }
    }

    pub fn format<const L: usize>(
        &self,
        thing_name: &str,
        shadow_name: Option<&'static str>,
    ) -> Result<String<L>, Error> {
        let (name_prefix, shadow_name) = shadow_name.map(|n| ("/name/", n)).unwrap_or_default();

        let mut topic_path = String::new();
        match self {
            Self::Get => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/get",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::Update => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/update",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::Delete => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/delete",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),

            Self::GetAccepted => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/get/accepted",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::GetRejected => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/get/rejected",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::UpdateDelta => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/update/delta",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::UpdateAccepted => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/update/accepted",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::UpdateDocuments => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/update/documents",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::UpdateRejected => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/update/rejected",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::DeleteAccepted => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/delete/accepted",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::DeleteRejected => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/delete/rejected",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
            )),
            Self::Any => topic_path.write_fmt(format_args!(
                "{}/{}/{}{}{}/#",
                Self::PREFIX,
                thing_name,
                Self::SHADOW,
                name_prefix,
                shadow_name
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

    pub fn topic(self, topic: Topic, qos: QoS) -> Self {
        // Ignore attempts to subscribe to outgoing topics
        if topic.direction() != Direction::Incoming {
            return self;
        }

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
        shadow_name: Option<&'static str>,
    ) -> Result<heapless::Vec<(heapless::String<128>, QoS), N>, Error> {
        assert!(thing_name.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|(topic, qos)| Ok((Topic::from(*topic).format(thing_name, shadow_name)?, *qos)))
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
        // Ignore attempts to subscribe to outgoing topics
        if topic.direction() != Direction::Incoming {
            return self;
        }

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
        shadow_name: Option<&'static str>,
    ) -> Result<heapless::Vec<heapless::String<256>, N>, Error> {
        assert!(thing_name.len() <= MAX_THING_NAME_LEN);

        self.topics
            .iter()
            .map(|topic| Topic::from(*topic).format(thing_name, shadow_name))
            .collect()
    }
}
