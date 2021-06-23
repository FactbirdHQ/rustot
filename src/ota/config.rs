pub struct Config {
    pub(crate) block_size: usize,
    pub(crate) max_request_momentum: u8,
    pub(crate) request_wait_ms: u32,
    pub(crate) max_blocks_per_request: u32,
    pub(crate) status_update_frequency: u32,
    pub(crate) allow_downgrade: bool,
    pub(crate) unsubscribe_on_shutdown: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            block_size: 256,
            max_request_momentum: 3,
            request_wait_ms: 8000,
            max_blocks_per_request: 31,
            status_update_frequency: 24,
            allow_downgrade: false,
            unsubscribe_on_shutdown: true,
        }
    }
}
