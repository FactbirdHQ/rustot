use crate::ota::{
    config::Config,
    control_interface::ControlInterface,
    data_interface::DataInterface,
    pal::OtaPal,
    state::{SmContext, StateMachine},
};

use super::{agent::OtaAgent, data_interface::NoInterface, pal::ImageState};

pub struct NoTimer;

impl<const TIMER_HZ: u32> fugit_timer::Timer<TIMER_HZ> for NoTimer {
    type Error = ();

    fn now(&mut self) -> fugit_timer::TimerInstantU32<TIMER_HZ> {
        todo!()
    }

    fn start(
        &mut self,
        _duration: fugit_timer::TimerDurationU32<TIMER_HZ>,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        todo!()
    }

    fn wait(&mut self) -> nb::Result<(), Self::Error> {
        todo!()
    }
}

pub struct OtaAgentBuilder<'a, C, DP, DS, T, ST, PAL, const TIMER_HZ: u32>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    ST: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    control: &'a C,
    data_primary: DP,
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    data_secondary: Option<DS>,
    #[cfg(not(all(feature = "ota_mqtt_data", feature = "ota_http_data")))]
    data_secondary: core::marker::PhantomData<DS>,
    pal: PAL,
    request_timer: T,
    self_test_timer: Option<ST>,
    config: Config,
}

impl<'a, C, DP, T, PAL, const TIMER_HZ: u32>
    OtaAgentBuilder<'a, C, DP, NoInterface, T, NoTimer, PAL, TIMER_HZ>
where
    C: ControlInterface,
    DP: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    pub fn new(control_interface: &'a C, data_primary: DP, request_timer: T, pal: PAL) -> Self {
        Self {
            control: control_interface,
            data_primary,
            #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
            data_secondary: None,
            #[cfg(not(all(feature = "ota_mqtt_data", feature = "ota_http_data")))]
            data_secondary: core::marker::PhantomData,
            pal,
            request_timer,
            self_test_timer: None,
            config: Config::default(),
        }
    }
}

impl<'a, C, DP, DS, T, ST, PAL, const TIMER_HZ: u32>
    OtaAgentBuilder<'a, C, DP, DS, T, ST, PAL, TIMER_HZ>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: fugit_timer::Timer<TIMER_HZ>,
    ST: fugit_timer::Timer<TIMER_HZ>,
    PAL: OtaPal,
{
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    pub fn data_secondary<D: DataInterface>(
        self,
        interface: D,
    ) -> OtaAgentBuilder<'a, C, DP, D, T, ST, PAL, TIMER_HZ> {
        OtaAgentBuilder {
            control: self.control,
            data_primary: self.data_primary,
            data_secondary: Some(interface),
            pal: self.pal,
            request_timer: self.request_timer,
            self_test_timer: self.self_test_timer,
            config: self.config,
        }
    }

    pub fn block_size(self, block_size: usize) -> Self {
        Self {
            config: Config {
                block_size,
                ..self.config
            },
            ..self
        }
    }

    pub fn max_request_momentum(self, max_request_momentum: u8) -> Self {
        Self {
            config: Config {
                max_request_momentum,
                ..self.config
            },
            ..self
        }
    }

    pub fn activate_delay(self, activate_delay: u8) -> Self {
        Self {
            config: Config {
                activate_delay,
                ..self.config
            },
            ..self
        }
    }

    pub fn request_wait_ms(self, request_wait_ms: u32) -> Self {
        Self {
            config: Config {
                request_wait_ms,
                ..self.config
            },
            ..self
        }
    }

    pub fn status_update_frequency(self, status_update_frequency: u32) -> Self {
        Self {
            config: Config {
                status_update_frequency,
                ..self.config
            },
            ..self
        }
    }

    pub fn allow_downgrade(self) -> Self {
        Self {
            config: Config {
                allow_downgrade: true,
                ..self.config
            },
            ..self
        }
    }

    pub fn with_self_test_timeout<NST>(
        self,
        timer: NST,
        timeout_ms: u32,
    ) -> OtaAgentBuilder<'a, C, DP, DS, T, NST, PAL, TIMER_HZ>
    where
        NST: fugit_timer::Timer<TIMER_HZ>,
    {
        OtaAgentBuilder {
            control: self.control,
            data_primary: self.data_primary,
            data_secondary: self.data_secondary,
            pal: self.pal,
            request_timer: self.request_timer,
            self_test_timer: Some(timer),
            config: Config {
                self_test_timeout_ms: timeout_ms,
                ..self.config
            },
        }
    }

    pub fn build(self) -> OtaAgent<'a, C, DP, DS, T, ST, PAL, TIMER_HZ> {
        OtaAgent {
            state: StateMachine::new(SmContext {
                events: heapless::spsc::Queue::new(),
                control: self.control,
                data_secondary: self.data_secondary,
                data_primary: self.data_primary,
                active_interface: None,
                request_momentum: 0,
                request_timer: self.request_timer,
                self_test_timer: self.self_test_timer,
                pal: self.pal,
                config: self.config,
                image_state: ImageState::Unknown,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        ota::test::mock::{MockPal, MockTimer},
        test::MockMqtt,
    };

    #[test]
    fn enables_allow_downgrade() {
        let mqtt = MockMqtt::new();

        let request_timer = MockTimer::new();
        let self_test_timer = MockTimer::new();
        let pal = MockPal {};

        let builder = OtaAgentBuilder::new(&mqtt, &mqtt, request_timer, pal)
            .with_self_test_timeout(self_test_timer, 32000)
            .allow_downgrade();

        assert!(builder.config.allow_downgrade);
        assert!(builder.self_test_timer.is_some());
        assert_eq!(builder.config.self_test_timeout_ms, 32000);

        let agent = builder.build();

        assert!(agent.state.context().config.allow_downgrade);
    }
}
