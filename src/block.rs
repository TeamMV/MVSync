use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use mvutils::utils::Recover;

pub trait AwaitSync: Future + Sized {
    /// Poll a future to completion, blocking the current thread until it is done.
    fn await_sync(self) -> Self::Output { await_sync(self) }

    /// Poll a future to completion, blocking the current thread until it is done, returning Err is the thread panics.
    fn await_sync_safe(self) -> Result<Self::Output, Box<dyn Any + Send + 'static>> { await_sync_safe(self) }
}

impl<F: Future> AwaitSync for F {}

pub struct Signal {
    ready: Mutex<bool>,
    condition: Condvar,
    wakers: Mutex<Vec<Waker>>
}

impl Signal {
    pub fn new() -> Self {
        Self {
            ready: Mutex::new(false),
            condition: Condvar::new(),
            wakers: Vec::with_capacity(1).into(),
        }
    }

    pub fn ready(&self) -> bool {
        *self.ready.lock().recover()
    }

    pub fn wait(&self) {
        let mut ready = self.ready.lock().recover();
        while !*ready {
            ready = self.condition.wait(ready).recover();
        }
        *ready = false;
    }

    pub async fn wait_async(&self) {
        SignalFuture {
            signal: self,
            started: false,
        }.await
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        let mut ready = self.ready.lock().recover();
        *ready = true;
        let mut wakers = self.wakers.lock().recover();
        for waker in wakers.drain(..) {
            waker.wake();
        }
        self.condition.notify_all();
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        let mut wakers = self.wakers.lock().recover();
        for waker in wakers.drain(..) {
            waker.wake();
        }
        self.condition.notify_all();
    }
}


pub struct SignalFuture<'a> {
    signal: &'a Signal,
    started: bool
}

impl<'a> Future for SignalFuture<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ready = self.signal.ready.lock().recover();
        if *ready {
            Poll::Ready(())
        } else {
            if !self.started {
                let this = self.get_mut();
                this.started = true;
                this.signal.wakers.lock().recover().push(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}

/// Poll a future to completion, blocking the current thread until it is done.
pub fn await_sync<R>(mut future: impl Future<Output = R>) -> R {
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    let signal = Arc::new(Signal::new());
    let waker = Waker::from(signal.clone());
    let mut ctx = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut ctx) {
            Poll::Pending => signal.wait(),
            Poll::Ready(output) => return output,
        }
    }
}

/// Poll a future to completion, blocking the current thread until it is done, returning Err if the thread panics.
pub fn await_sync_safe<R>(mut future: impl Future<Output = R>) -> Result<R, Box<dyn Any + Send + 'static>> {
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    let signal = Arc::new(Signal::new());
    let waker = Waker::from(signal.clone());
    let mut ctx = Context::from_waker(&waker);
    loop {
        match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut ctx)))? {
            Poll::Pending => signal.wait(),
            Poll::Ready(output) => return Ok(output),
        }
    }
}