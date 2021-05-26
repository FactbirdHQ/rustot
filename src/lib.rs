#![cfg_attr(not(test), no_std)]

pub mod jobs;
pub mod ota;

pub mod consts {
    // Jobs:

    /// https://docs.aws.amazon.com/iot/latest/apireference/API_DescribeThing.html
    pub const MAX_THING_NAME_LEN: usize = 128;
    pub const MAX_CLIENT_TOKEN_LEN: usize = 30;
    pub const MAX_JOB_ID_LEN: usize = 64;
    pub const MAX_STREAM_ID_LEN: usize = 64;
    pub const MAX_PENDING_JOBS: usize = 4;
    pub const MAX_RUNNING_JOBS: usize = 4;
}

#[cfg(test)]
mod tests {
    //! This module is required in order to satisfy the requirements of defmt, while running tests.
    //! Note that this will cause all log `defmt::` log statements to be thrown away.

    use core::ptr::NonNull;

    #[defmt::global_logger]
    struct Logger;
    impl defmt::Write for Logger {
        fn write(&mut self, _bytes: &[u8]) {}
    }

    unsafe impl defmt::Logger for Logger {
        fn acquire() -> Option<NonNull<dyn defmt::Write>> {
            Some(NonNull::from(&Logger as &dyn defmt::Write))
        }

        unsafe fn release(_: NonNull<dyn defmt::Write>) {}
    }

    defmt::timestamp!("");

    #[export_name = "_defmt_panic"]
    fn panic() -> ! {
        panic!()
    }
}
