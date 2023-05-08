use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};

pub trait AwaitSync: Future + Sized {
    /// Poll a future to completion, blocking the current thread until it is done.
    fn await_sync(self) -> Self::Output { await_sync(self) }
}

impl<F: Future> AwaitSync for F {}

#[derive(Eq, PartialEq)]
enum State {
    Ready,
    Pending,
    Done,
}

struct Signal {
    state: Mutex<State>,
    condition: Condvar,
}

impl Signal {
    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Done => *state = State::Ready,
            State::Pending => panic!("Signal already pending!"),
            State::Ready => {
                *state = State::Pending;
                while *state == State::Pending {
                    state = self.condition.wait(state).unwrap();
                }
            }
        }
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Ready => *state = State::Done,
            State::Pending => {
                *state = State::Ready;
                self.condition.notify_one();
            }
            _ => {}
        }
    }
}

/// Poll a future to completion, blocking the current thread until it is done.
pub fn await_sync<R>(mut future: impl Future<Output = R>) -> R {
    let mut future = unsafe { std::pin::Pin::new_unchecked(&mut future) };
    let signal = Arc::new(Signal { state: Mutex::new(State::Ready), condition: Condvar::new() });
    let waker = Waker::from(signal.clone());
    let mut ctx = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut ctx) {
            Poll::Pending => signal.wait(),
            Poll::Ready(output) => return output,
        }
    }
}