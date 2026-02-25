use super::JobTopic;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Topic<'a> {
    Notify,
    NotifyNext,
    GetAccepted,
    GetRejected,
    StartNextAccepted,
    StartNextRejected,
    DescribeAccepted(&'a str),
    DescribeRejected(&'a str),
    UpdateAccepted(&'a str),
    UpdateRejected(&'a str),
}

impl<'a> Topic<'a> {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &'a str) -> Option<Self> {
        let tt = s.splitn(8, '/').collect::<heapless::Vec<&str, 8>>();
        Some(match (tt.first(), tt.get(1), tt.get(2), tt.get(3)) {
            (Some(&"$aws"), Some(&"things"), _, Some(&"jobs")) => {
                // This is a job topic! Figure out which
                match (tt.get(4), tt.get(5), tt.get(6), tt.get(7)) {
                    (Some(&"notify-next"), None, None, None) => Topic::NotifyNext,
                    (Some(&"notify"), None, None, None) => Topic::Notify,
                    (Some(&"get"), Some(&"accepted"), None, None) => Topic::GetAccepted,
                    (Some(&"get"), Some(&"rejected"), None, None) => Topic::GetRejected,
                    (Some(&"start-next"), Some(&"accepted"), None, None) => {
                        Topic::StartNextAccepted
                    }
                    (Some(&"start-next"), Some(&"rejected"), None, None) => {
                        Topic::StartNextRejected
                    }
                    (Some(job_id), Some(&"update"), Some(&"accepted"), None) => {
                        Topic::UpdateAccepted(job_id)
                    }
                    (Some(job_id), Some(&"update"), Some(&"rejected"), None) => {
                        Topic::UpdateRejected(job_id)
                    }
                    (Some(job_id), Some(&"get"), Some(&"accepted"), None) => {
                        Topic::DescribeAccepted(job_id)
                    }
                    (Some(job_id), Some(&"get"), Some(&"rejected"), None) => {
                        Topic::DescribeRejected(job_id)
                    }
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

impl<'a> From<&Topic<'a>> for JobTopic<'a> {
    fn from(t: &Topic<'a>) -> Self {
        match t {
            Topic::Notify => Self::Notify,
            Topic::NotifyNext => Self::NotifyNext,
            Topic::GetAccepted => Self::GetAccepted,
            Topic::GetRejected => Self::GetRejected,
            Topic::StartNextAccepted => Self::StartNextAccepted,
            Topic::StartNextRejected => Self::StartNextRejected,
            Topic::DescribeAccepted(job_id) => Self::DescribeAccepted(job_id),
            Topic::DescribeRejected(job_id) => Self::DescribeRejected(job_id),
            Topic::UpdateAccepted(job_id) => Self::UpdateAccepted(job_id),
            Topic::UpdateRejected(job_id) => Self::UpdateRejected(job_id),
        }
    }
}
