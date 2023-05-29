use std::sync::{Arc, Mutex, Condvar, Once};
use std::time::{Duration, Instant};
use std::thread;
use std::task::{Waker, Context, Poll};
use std::pin::Pin;
use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::future::Future;
use mvutils::once::Lazy;

pub struct Sleep {
    when: Instant,
}

static TIMER_QUEUE: Lazy<Arc<Mutex<BinaryHeap<Reverse<TimerEntry>>>>> = Lazy::new(|| Arc::new(Mutex::new(BinaryHeap::new())));
static TIMER_CVAR: Lazy<Condvar> = Lazy::new(Condvar::new);
static TIMER_THREAD: Lazy<Once> = Lazy::new(Once::new);

impl Sleep {
    pub(crate) fn new(when: Duration) -> Self {
        TIMER_THREAD.call_once(start_timer_thread);
        Sleep {
            when: Instant::now() + when,
        }
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if Instant::now() >= self.when {
            Poll::Ready(())
        } else {
            let mut queue = TIMER_QUEUE.lock().unwrap();
            queue.push(Reverse(TimerEntry {
                when: self.when,
                waker: cx.waker().clone(),
            }));
            TIMER_CVAR.notify_one();
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
    let queue_clone = TIMER_QUEUE.clone();
    thread::spawn(move || {
        loop {
            let mut queue = queue_clone.lock().unwrap();
            while let Some(Reverse(ref next)) = queue.peek() {
                let now = Instant::now();
                if now >= next.when {
                    let Reverse(timed_out) = queue.pop().unwrap();
                    timed_out.waker.wake();
                } else {
                    let wait_duration = if now > next.when {
                        Duration::from_secs(0)
                    } else {
                        next.when.duration_since(now)
                    };
                    let (lock, timeout_result) = TIMER_CVAR.wait_timeout(queue, wait_duration).unwrap();
                    queue = lock;
                    if timeout_result.timed_out() {
                        continue;
                    }
                }
            }
        }
    });
}
