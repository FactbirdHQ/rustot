#![no_std]

extern crate alloc;

pub mod jobs;
pub mod ota;



mod consts {

    use heapless::consts;
    // Jobs:

    /// https://docs.aws.amazon.com/iot/latest/apireference/API_DescribeThing.html
    pub type MaxThingNameLen = consts::U128;
    pub type MaxTopicLen = consts::U128;
    pub type MaxClientTokenLen = consts::U30;
    pub type MaxJobIdLen = consts::U64;
    pub type MaxStreamIdLen = consts::U64;
    pub type MaxPendingJobs = consts::U4;
    pub type MaxRunningJobs = consts::U4;
}