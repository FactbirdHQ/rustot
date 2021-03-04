#![cfg_attr(not(test), no_std)]

pub mod jobs;
pub mod ota;

pub mod consts {

    use heapless::consts;
    // Jobs:

    /// https://docs.aws.amazon.com/iot/latest/apireference/API_DescribeThing.html
    pub type MaxThingNameLen = consts::U128;
    pub type MaxClientTokenLen = consts::U30;
    pub type MaxJobIdLen = consts::U64;
    pub type MaxStreamIdLen = consts::U64;
    pub type MaxPendingJobs = consts::U4;
    pub type MaxRunningJobs = consts::U4;
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
