use embedded_hal::timer;

use crate::ota::{
    config::Config,
    control_interface::ControlInterface,
    data_interface::DataInterface,
    pal::OtaPal,
    state::{SmContext, StateMachine},
};

use super::{agent::OtaAgent, data_interface::NoInterface};

pub struct OtaAgentBuilder<'a, C, DP, DS, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
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
    config: Config,
}

impl<'a, C, DP, T, PAL> OtaAgentBuilder<'a, C, DP, NoInterface, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
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
            config: Config::default(),
        }
    }
}

impl<'a, C, DP, DS, T, PAL> OtaAgentBuilder<'a, C, DP, DS, T, PAL>
where
    C: ControlInterface,
    DP: DataInterface,
    DS: DataInterface,
    T: timer::CountDown + timer::Cancel,
    T::Time: From<u32>,
    PAL: OtaPal,
{
    #[cfg(all(feature = "ota_mqtt_data", feature = "ota_http_data"))]
    pub fn data_secondary<D: DataInterface>(
        self,
        interface: D,
    ) -> OtaAgentBuilder<'a, C, DP, D, T, PAL> {
        OtaAgentBuilder {
            control: self.control,
            data_primary: self.data_primary,
            data_secondary: Some(interface),
            pal: self.pal,
            request_timer: self.request_timer,
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

    pub fn request_wait_ms(self, request_wait_ms: u32) -> Self {
        Self {
            config: Config {
                request_wait_ms,
                ..self.config
            },
            ..self
        }
    }

    pub fn max_blocks_per_request(self, max_blocks_per_request: u32) -> Self {
        assert!(max_blocks_per_request < 32);

        Self {
            config: Config {
                max_blocks_per_request,
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

    pub fn build(self) -> OtaAgent<'a, C, DP, DS, T, PAL> {
        OtaAgent {
            state: StateMachine::new(SmContext {
                events: heapless::spsc::Queue::new(),
                control: self.control,
                data_secondary: self.data_secondary,
                data_primary: self.data_primary,
                active_interface: None,
                request_momentum: 0,
                request_timer: self.request_timer,
                pal: self.pal,
                config: self.config,
            }),
        }
    }
}
