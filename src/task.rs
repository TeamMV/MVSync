use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use mvutils::utils::next_id;
use crate::sync::{Fence, Semaphore, Signal};

pub(crate) struct Task {
    id: u64,
    inner: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    wait: Vec<Arc<Semaphore>>,
    signal: Vec<Arc<Signal>>,
}

impl Task {
    pub(crate) fn new(inner: impl Future<Output = ()> + Send + 'static, wait: Vec<Arc<Semaphore>>, signal: Vec<Arc<Signal>>) -> Self {
        Task {
            id: next_id("MVSync"),
            inner: Box::pin(inner),
            wait,
            signal,
        }
    }

    pub(crate) fn can_execute(&self) -> bool {
        self.wait.is_empty() || self.wait.iter().all(|s| s.ready())
    }

    pub(crate) async fn execute(self) {
        (self.inner).await;
        self.signal.iter().for_each(|s| s.signal());
    }
}

pub struct TaskResult<T> {
    id: u64,
    timeout: u32,
    inner: Arc<RwLock<Option<T>>>
}

impl<T> TaskResult<T> {
    pub(crate) fn new(inner: Arc<RwLock<Option<T>>>, timeout: u32) -> Self {
        TaskResult {
            id: next_id("MVSync"),
            timeout,
            inner
        }
    }

    pub fn is_done(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }

    pub fn wait(self) -> Option<T> {
        loop {
            if Arc::strong_count(&self.inner) == 1 {
                return self.inner.write().unwrap().take();
            }
            thread::sleep(Duration::from_millis(self.timeout as u64));
        }
    }
}