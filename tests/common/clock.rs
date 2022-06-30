use std::time::{SystemTime, UNIX_EPOCH};

use embedded_hal::timer::nb::{Cancel, CountDown};
use fugit_timer::Rate;

pub struct SysClock {
    start_time: u32,
    end_time: Option<fugit_timer::TimerInstantU32<1000>>,
}

impl SysClock {
    pub fn new() -> Self {
        Self {
            start_time: Self::epoch(),
            end_time: None,
        }
    }

    pub fn epoch() -> u32 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u32
    }

    pub fn now(&self) -> u32 {
        Self::epoch() - self.start_time
    }
}

impl CountDown for SysClock {
    type Error = std::convert::Infallible;

    type Time = fugit_timer::Duration<u32, 1, 1000>;

    fn start<T>(&mut self, count: T) -> Result<(), Self::Error>
    where
        T: Into<Self::Time>,
    {
        fugit_timer::Timer::start(self, count.into())
    }

    fn wait(&mut self) -> nb::Result<(), Self::Error> {
        fugit_timer::Timer::wait(self)
    }
}

impl Cancel for SysClock {
    fn cancel(&mut self) -> Result<(), Self::Error> {
        fugit_timer::Timer::cancel(self)
    }
}

impl fugit_timer::Timer<1000> for SysClock {
    type Error = std::convert::Infallible;

    fn now(&mut self) -> fugit_timer::TimerInstantU32<1000> {
        fugit_timer::TimerInstantU32::from_ticks(SysClock::now(self))
    }

    fn start(&mut self, duration: fugit_timer::TimerDurationU32<1000>) -> Result<(), Self::Error> {
        let now = self.now();
        self.end_time.replace(now + duration);
        Ok(())
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        self.end_time.take();
        Ok(())
    }

    fn wait(&mut self) -> nb::Result<(), Self::Error> {
        match self.end_time.map(|end| end <= self.now()) {
            Some(true) => {
                self.end_time.take();
                Ok(())
            }
            _ => Err(nb::Error::WouldBlock),
        }
    }
}
