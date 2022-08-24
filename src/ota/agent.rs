use super::{
    builder::{self, NoTimer},
    control_interface::ControlInterface,
    data_interface::{DataInterface, NoInterface},
    encoding::json::OtaJob,
    pal::OtaPal,
    state::{Error, Events, JobEventData, SmContext, StateMachine, States},
};
use crate::jobs::StatusDetails;

// OTA Agent driving the FSM of an OTA update
pub struct OtaAgent<'a, C, DP, DS, T, ST, PAL, const TIMER_HZ: u32>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    ST: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    pub(crate) state: StateMachine<SmContext<'a, C, DP, DS, T, ST, PAL, 3, TIMER_HZ>>,
}

// Make sure any active OTA session is cleaned up, and the topics are
// unsubscribed on drop.
impl<'a, C, DP, DS, T, ST, PAL, const TIMER_HZ: u32> Drop
    for OtaAgent<'a, C, DP, DS, T, ST, PAL, TIMER_HZ>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    ST: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    fn drop(&mut self) {
        let sm_context = self.state.context_mut();
        sm_context.ota_close().ok();
        sm_context.control.cleanup().ok();
    }
}

impl<'a, C, DP, T, PAL, const TIMER_HZ: u32>
    OtaAgent<'a, C, DP, NoInterface, T, NoTimer, PAL, TIMER_HZ>
where
    C: ControlInterface,
    DP: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    pub fn builder(
        control_interface: &'a C,
        data_primary: DP,
        request_timer: T,
        pal: PAL,
    ) -> builder::OtaAgentBuilder<'a, C, DP, NoInterface, T, NoTimer, PAL, TIMER_HZ> {
        builder::OtaAgentBuilder::new(control_interface, data_primary, request_timer, pal)
    }
}

/// Public interface of the OTA Agent
impl<'a, C, DP, DS, T, ST, PAL, const TIMER_HZ: u32> OtaAgent<'a, C, DP, DS, T, ST, PAL, TIMER_HZ>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    ST: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    pub fn init(&mut self) {
        if matches!(self.state(), &States::Ready) {
            self.state.process_event(Events::Start).ok();
        } else {
            self.state.process_event(Events::Resume).ok();
        }
    }

    pub fn job_update(
        &mut self,
        job_name: &str,
        ota_document: &OtaJob,
        status_details: Option<&StatusDetails>,
    ) -> Result<&States, Error> {
        self.state
            .process_event(Events::ReceivedJobDocument(JobEventData {
                job_name,
                ota_document,
                status_details,
            }))
    }

    pub fn timer_callback(&mut self) -> Result<(), Error> {
        let ctx = self.state.context_mut();
        if ctx.request_timer.wait().is_ok() {
            return self.state.process_event(Events::RequestTimer).map(drop);
        }

        if let Some(ref mut self_test_timer) = ctx.self_test_timer {
            if self_test_timer.wait().is_ok() {
                error!(
                    "Self test failed to complete within {} ms",
                    ctx.config.self_test_timeout_ms
                );
                ctx.pal.reset_device().ok();
            }
        }
        Ok(())
    }

    pub fn process_event(&mut self) -> Result<&States, Error> {
        if let Some(event) = self.state.context_mut().events.dequeue() {
            self.state.process_event(event)
        } else {
            Ok(self.state())
        }
    }

    pub fn handle_message(&mut self, payload: &mut [u8]) -> Result<&States, Error> {
        self.state.process_event(Events::ReceivedFileBlock(payload))
    }

    pub fn check_for_update(&mut self) -> Result<&States, Error> {
        if matches!(
            self.state(),
            States::WaitingForJob | States::RequestingJob | States::WaitingForFileBlock
        ) {
            self.state.process_event(Events::RequestJobDocument)
        } else {
            Ok(self.state())
        }
    }

    pub fn abort(&mut self) -> Result<&States, Error> {
        self.state.process_event(Events::UserAbort)
    }

    pub fn suspend(&mut self) -> Result<&States, Error> {
        // Stop the request timer
        self.state.context_mut().request_timer.cancel().ok();

        // Send event to OTA agent task.
        self.state.process_event(Events::Suspend)
    }

    pub fn resume(&mut self) -> Result<&States, Error> {
        // Send event to OTA agent task
        self.state.process_event(Events::Resume)
    }

    pub fn state(&self) -> &States {
        self.state.state()
    }
}
