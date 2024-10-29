use embassy_time::Duration;

pub struct Config {
    pub block_size: usize,
    pub max_request_momentum: u8,
    pub request_wait: Duration,
    pub status_update_frequency: u32,
    pub self_test_timeout: Option<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            block_size: 1024,
            max_request_momentum: 3,
            request_wait: Duration::from_secs(5),
            status_update_frequency: 24,
            self_test_timeout: None,
        }
    }
}
