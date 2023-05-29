use std::future::{Future};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration};
use futures_timer::Delay;

pub fn async_sleep(duration: Duration) -> impl Future<Output = ()> {
    Delay::new(duration)
}

pub fn async_sleep_ms(ms: u64) -> impl Future<Output = ()> {
    Delay::new(Duration::from_millis(ms))
}

struct Yield {
    set: bool
}

impl Future for Yield {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.set {
          Poll::Ready(())
        } else {
            self.get_mut().set = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

pub fn async_yield() -> impl Future<Output = ()> {
    Yield { set: false }
}