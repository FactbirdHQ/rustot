use embedded_hal::timer;

use super::{builder::{self, NoTimer}, control_interface::ControlInterface, data_interface::{DataInterface, NoInterface}, encoding::json::OtaJob, pal::OtaPal, state::{Error, Events, SmContext, StateMachine, States}};
use crate::jobs::StatusDetails;

// OTA Agent driving the FSM of an OTA update
pub struct OtaAgent<'a, C, DP, DS, T, ST, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    ST: timer::CountDown + timer::Cancel,
    ST::Time: From<u32>,
    PAL: OtaPal,
{
    pub(crate) state: StateMachine<SmContext<'a, C, DP, DS, T, ST, PAL, 10>>,
}

// Make sure any active OTA session is cleaned up, and the topics are
// unsubscribed on drop.
impl<'a, C, DP, DS, T, ST, PAL> Drop for OtaAgent<'a, C, DP, DS, T, ST, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    ST: timer::CountDown + timer::Cancel,
    ST::Time: From<u32>,
    PAL: OtaPal,
{
    fn drop(&mut self) {
        let sm_context = self.state.context_mut();
        sm_context.ota_close().ok();
        sm_context.control.cleanup().ok();
    }
}

impl<'a, C, DP, T, PAL> OtaAgent<'a, C, DP, NoInterface, T, NoTimer, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    pub fn builder(
        control_interface: &'a C,
        data_primary: DP,
        request_timer: T,
        pal: PAL,
    ) -> builder::OtaAgentBuilder<'a, C, DP, NoInterface, T, NoTimer, PAL> {
        builder::OtaAgentBuilder::new(control_interface, data_primary, request_timer, pal)
    }
}

/// Public interface of the OTA Agent
impl<'a, C, DP, DS, T, ST, PAL> OtaAgent<'a, C, DP, DS, T, ST, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    ST: timer::CountDown + timer::Cancel,
    ST::Time: From<u32>,
    PAL: OtaPal,
{
    pub fn init(&mut self) {
        self.state.process_event(Events::Start).ok();
    }

    pub fn job_update(
        &mut self,
        job_name: &str,
        ota_document: OtaJob,
        status_details: Option<StatusDetails>,
    ) -> Result<&States, Error> {
        self.state.process_event(Events::ReceivedJobDocument((
            heapless::String::from(job_name),
            ota_document,
            status_details,
        )))
    }

    pub fn timer_callback(&mut self) {
        if self.state.context_mut().request_timer.try_wait().is_ok() {
            self.state.process_event(Events::RequestTimer).ok();
        }
    }

    pub fn process_event(&mut self) -> Result<&States, Error> {
        if let Some(event) = self.state.context_mut().events.dequeue() {
            self.state.process_event(event)?;
        }
        Ok(self.state.state())
    }

    pub fn handle_message(&mut self, payload: &mut [u8]) -> Result<&States, Error> {
        self.state.process_event(Events::ReceivedFileBlock(payload))
    }

    pub fn check_for_update(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::RequestJobDocument)
    }

    pub fn abort(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::UserAbort)
    }

    pub fn suspend(&mut self) -> Result<&States, Error> {
        // Stop the request timer
        self.state.context_mut().request_timer.try_cancel().ok();

        // Send event to OTA agent task.
        self.state.process_event(Events::Suspend)
    }

    pub fn resume(&mut self) -> Result<&States, Error> {
        // Send event to OTA agent task
        self.state.process_event(Events::Resume)
    }
}
