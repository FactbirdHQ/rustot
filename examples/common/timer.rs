use embedded_hal::timer::CountDown;
use embedded_time::{clock::Error, rate::Fraction};
use std::time::{Duration, Instant};

pub struct SysTimer {
    start: Instant,
    count: u32,
}

impl SysTimer {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            count: 0,
        }
    }
}

impl embedded_time::Clock for SysTimer {
    type T = u64;

    const SCALING_FACTOR: Fraction = Fraction::new(100, 1);

    fn try_now(&self) -> Result<embedded_time::Instant<Self>, Error> {
        Ok(embedded_time::Instant::new(
            self.start.elapsed().as_millis() as u64,
        ))
    }
}

impl CountDown for SysTimer {
    type Time = u32;
    type Error = core::convert::Infallible;

    fn try_start<T>(&mut self, count: T) -> Result<(), Self::Error>
    where
        T: Into<Self::Time>,
    {
        self.start = Instant::now();
        self.count = count.into();
        Ok(())
    }

    fn try_wait(&mut self) -> nb::Result<(), Self::Error> {
        if Instant::now() - self.start > Duration::from_millis(self.count as u64) {
            Ok(())
        } else {
            Err(nb::Error::WouldBlock)
        }
    }
}
