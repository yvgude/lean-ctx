pub trait Clock {
    fn now(&self) -> u64;
}

pub fn stamp(clock: &dyn Clock) -> u64 {
    clock.now()
}
