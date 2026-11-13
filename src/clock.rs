use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
    fn now(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64)
    }
}

#[cfg(test)]
pub struct FixedClock(pub i64);

#[cfg(test)]
impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}
