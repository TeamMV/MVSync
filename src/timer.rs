use std::sync::{Arc, Mutex, Condvar, Once};
use std::time::{Duration, Instant};
use std::thread;
use std::task::{Waker, Context, Poll};
use std::pin::Pin;
use std::collections::BinaryHeap;
use std::future::Future;
use mvutils::lazy;
use mvutils::once::Lazy;
use mvutils::utils::Recover;

pub struct Sleep {
    duration: Duration,
    when: Instant,
    started: bool
}

lazy! {
    static QUEUE: Arc<Mutex<BinaryHeap<TimerEntry>>> = Arc::new(Mutex::new(BinaryHeap::new()));
    static SIGNAL: Condvar = Condvar::new();
    static INIT: Once = Once::new();
}

impl Sleep {
    pub(crate) fn new(duration: Duration) -> Self {
        INIT.call_once(start_timer_thread);
        Sleep {
            duration,
            when: Instant::now(),
            started: false
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.started {
            let this = self.get_mut();
            this.when = Instant::now() + this.duration;
            this.started = true;
            let mut queue = QUEUE.lock().recover();
            queue.push(TimerEntry {
                when: this.when,
                waker: cx.waker().clone(),
            });
            drop(queue);
            SIGNAL.notify_one();
            Poll::Pending
        }
        else if Instant::now() >= self.when {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

struct TimerEntry {
    when: Instant,
    waker: Waker,
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.when.cmp(&other.when).reverse()
    }
}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when
    }
}

impl Eq for TimerEntry {}

fn start_timer_thread() {
    thread::spawn(|| {
        loop {
            let next = QUEUE.lock().recover().peek().map(|e| e.when);
            match next {
                Some(t) => {
                    let now = Instant::now();
                    if now >= t {
                        let next = QUEUE.lock().recover().pop().unwrap();
                        next.waker.wake();
                    }
                    else {
                        let wait_duration = if now > t {
                            Duration::from_secs(0)
                        } else {
                            t.duration_since(now)
                        };
                        let (_queue, _) = SIGNAL.wait_timeout(QUEUE.lock().recover(), wait_duration).recover();
                    }
                }
                None => {
                    let _queue = SIGNAL.wait(QUEUE.lock().recover()).recover();
                }
            }
        }
    });
}