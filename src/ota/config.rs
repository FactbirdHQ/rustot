use embassy_time::Duration;

pub struct Config {
    pub(crate) block_size: usize,
    pub(crate) max_request_momentum: u8,
    pub(crate) request_wait: Duration,
    pub(crate) status_update_frequency: u32,
    pub(crate) self_test_timeout: Option<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            block_size: 256,
            max_request_momentum: 3,
            request_wait: Duration::from_secs(8),
            status_update_frequency: 24,
            self_test_timeout: None,
        }
    }
}
