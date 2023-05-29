use std::future::Future;
use std::time::Duration;
use futures_timer::Delay;

pub fn async_sleep(duration: Duration) -> impl Future<Output = ()> {
    Delay::new(duration)
}

pub fn async_sleep_ms(ms: u64) -> impl Future<Output = ()> {
    Delay::new(Duration::from_millis(ms))
}

pub async fn async_yield() {//-> impl Future<Output = ()> {
    async_sleep_ms(0).await
}