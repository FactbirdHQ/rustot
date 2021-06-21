use embedded_hal::timer;

use super::{
    builder,
    control_interface::ControlInterface,
    data_interface::{DataInterface, NoInterface},
    encoding::json::OtaJob,
    pal::OtaPal,
    state::{Error, Events, SmContext, StateMachine, States},
};
use crate::jobs::StatusDetails;

// OTA Agent driving the FSM of an OTA update
pub struct OtaAgent<'a, C, DP, DS, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    pub(crate) state: StateMachine<SmContext<'a, C, DP, DS, T, PAL, 2>>,
}

// Make sure any active OTA session is cleaned up, and the topics are
// unsubscribed on drop.
impl<'a, C, DP, DS, T, PAL> Drop for OtaAgent<'a, C, DP, DS, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    fn drop(&mut self) {
        let sm_context = self.state.context_mut();
        sm_context.ota_close().ok();
        sm_context.control.cleanup().ok();
    }
}

impl<'a, C, DP, T, PAL> OtaAgent<'a, C, DP, NoInterface, T, PAL>
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
    ) -> builder::OtaAgentBuilder<'a, C, DP, NoInterface, T, PAL> {
        builder::OtaAgentBuilder::new(control_interface, data_primary, request_timer, pal)
    }
}

/// Public interface of the OTA Agent
impl<'a, C, DP, DS, T, PAL> OtaAgent<'a, C, DP, DS, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    pub fn job_update(
        &mut self,
        ota_document: OtaJob,
        status_details: Option<StatusDetails>,
    ) -> Result<&States, Error> {
        self.state
            .process_event(Events::ReceivedJobDocument((ota_document, status_details)))
    }

    pub fn timer_callback(&mut self) {
        self.state.process_event(Events::RequestTimer).ok();
    }

    pub fn process_event(&mut self) -> Result<&States, Error> {
        if let Some(event) = self.state.context_mut().events.dequeue() {
            self.state.process_event(event)?;
        }
        Ok(self.state.state())
    }

    pub fn handle_message(&mut self, topic: &str, payload: &mut [u8]) -> Result<&States, Error> {
        // TODO: Handle topic parts / make sure this function is suitable for
        // http data as well?
        self.state.process_event(Events::ReceivedFileBlock(payload))
    }

    pub fn check_for_update(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::RequestJobDocument)
    }

    pub fn abort(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::UserAbort)
    }

    pub fn suspend(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::Suspend)
    }

    pub fn resume(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::Resume)
    }
}
