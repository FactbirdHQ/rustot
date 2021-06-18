#[derive(Debug)]
pub enum JobEvent {
    SelfTestFailed,
    StartTest,
    Activate,
    UpdateComplete,
    Fail,
    Processed,
}
