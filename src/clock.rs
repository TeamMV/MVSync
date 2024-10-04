use std::time::{Duration, Instant};
use crate::block::AwaitSync;
use crate::utils::async_sleep;

pub struct Clock {
    last_tick: Instant,
    tick: Duration,
    enabled: bool,
}

impl Clock {
    pub fn new(tps: u16) -> Self {
        Clock {
            last_tick: Instant::now(),
            tick: Duration::from_micros(1_000_000 / tps as u64),
            enabled: true,
        }
    }

    pub fn new_disabled(tps: u16) -> Self {
        Clock {
            last_tick: Instant::now(),
            tick: Duration::from_micros(1_000_000 / tps as u64),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn ready(&self) -> bool {
        if !self.enabled { return false; }
        self.last_tick.elapsed() >= self.tick
    }

    pub fn tick(&mut self) {
        if !self.enabled { return; }
        self.last_tick = Instant::now();
    }

    pub async fn wait(&mut self) {
        if !self.enabled { return; }
        let elapsed = self.last_tick.elapsed();
        if elapsed >= self.tick {
            self.last_tick = Instant::now();
            return;
        }
        let wait_time = self.tick - elapsed;
        async_sleep(wait_time).await;
        self.last_tick = Instant::now();
    }

    pub fn wait_sync(&mut self) {
        self.wait().await_sync();
    }
}