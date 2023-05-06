use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::ptr::null;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;
use std::time::Duration;

struct WorkerResult<T> {
    inner: Arc<RwLock<Option<T>>>
}

impl<T> WorkerResult<T> {
    fn is_done(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }

    fn wait(self) -> Option<T> {
        loop {
            if Arc::strong_count(&self.inner) == 1 {
                return self.inner.write().unwrap().take();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn run(f: impl FnOnce() + Send + 'static) {
    thread::spawn(f);
}

pub trait MVSynced: Send + Sync + 'static {}

impl<T> MVSynced for T where T: Send + Sync + 'static {}

fn wrap<T: MVSynced>(f: impl FnOnce() -> T + Send + 'static) -> WorkerResult<T> {
    let result = Arc::new(RwLock::new(None));
    let result_clone = result.clone();
    let wrapped = move || {
        let t = std::panic::catch_unwind(AssertUnwindSafe(move || f()));
        *result_clone.write().unwrap() = t.ok();
        drop(result_clone);
    };
    run(wrapped);
    WorkerResult {
        inner: result
    }
}

fn continuation<T: MVSynced, R: MVSynced>(f: impl FnOnce(T) -> R + Send + 'static, previous: WorkerResult<T>) -> WorkerResult<R> {
    let result = Arc::new(RwLock::new(None));
    let result_clone = result.clone();
    let wrapped = move || {
        let t = previous.wait();
        if let Some(t) = t {
            let r = std::panic::catch_unwind(AssertUnwindSafe(move || f(t)));
            *result_clone.write().unwrap() = r.ok();
        }
        drop(result_clone);
    };
    run(wrapped);
    WorkerResult {
        inner: result
    }
}