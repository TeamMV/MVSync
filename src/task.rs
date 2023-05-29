use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::MVSynced;
use crate::sync::{Fence, Semaphore, SemaphoreUsage, Signal};

/// A wrapper around a function, which can be synchronous or asynchronous, can return a value and take
/// input parameters.
///
/// This type must be used to submit tasks to the [`Queue`].
pub struct Task {
    id: u64,
    inner: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    wait: Vec<Arc<Semaphore>>,
    signal: Vec<Signal>,
    preferred_thread: Option<String>
}

impl Task {
    pub(crate) fn new(inner: impl Future<Output = ()> + Send + 'static) -> Self {
        Task {
            id: next_id("MVSync"),
            inner: Box::pin(inner),
            wait: Vec::new(),
            signal: Vec::new(),
            preferred_thread: None
        }
    }

    pub(crate) fn from_function<T: MVSynced>(function: impl FnOnce() -> T + Send + 'static, buffer: Arc<RwLock<Option<T>>>) -> Self {
        Task::new(async move {
            let t = function();
            buffer.write().unwrap().replace(t);
            drop(buffer);
        })
    }

    pub(crate) fn from_continuation<T: MVSynced, R: MVSynced>(function: impl FnOnce(T) -> R + Send + 'static, buffer: Arc<RwLock<Option<R>>>, predecessor: TaskResult<T>) -> Self {
        Task::new(async move {
            let t = predecessor.wait();
            if let Some(t) = t {
                let r = function(t);
                buffer.write().unwrap().replace(r);
            }
            drop(buffer);
        })
    }

    pub(crate) fn from_async<T: MVSynced, F: Future<Output = T> + Send>(function: impl FnOnce() -> F + Send + 'static, buffer: Arc<RwLock<Option<T>>>) -> Self {
        Task::new(async move {
            let t = function().await;
            buffer.write().unwrap().replace(t);
            drop(buffer);
        })
    }

    pub(crate) fn from_async_continuation<T: MVSynced, R: MVSynced, F: Future<Output = R> + Send>(function: impl FnOnce(T) -> F + Send + 'static, buffer: Arc<RwLock<Option<R>>>, predecessor: TaskResult<T>) -> Self {
        Task::new(async move {
            let t = predecessor.wait();
            if let Some(t) = t {
                let r = function(t).await;
                buffer.write().unwrap().replace(r);
            }
            drop(buffer);
        })
    }

    pub(crate) fn from_future<T: MVSynced>(function: impl Future<Output = T> + Send + 'static, buffer: Arc<RwLock<Option<T>>>) -> Self {
        Task::new(async move {
            let t = function.await;
            buffer.write().unwrap().replace(t);
            drop(buffer);
        })
    }

    /// Bind a [`Semaphore`] to this task, the usage will specify whether to wait for the semaphore, or signal it.
    pub fn bind_semaphore(&mut self, semaphore: Arc<Semaphore>, usage: SemaphoreUsage) {
        match usage {
            SemaphoreUsage::Wait => self.wait.push(semaphore),
            SemaphoreUsage::Signal => self.signal.push(Signal::Semaphore(semaphore))
        }
    }

    /// Bind a [`Fence`] to this task, which will open when this task finishes.
    pub fn bind_fence(&mut self, fence: Arc<Fence>) {
        self.signal.push(Signal::Fence(fence));
    }

    pub fn set_preferred_thread(&mut self, thread: String) {
        self.preferred_thread = Some(thread);
    }

    pub fn get_preferred_thread(&self) -> Option<&String> {
        self.preferred_thread.as_ref()
    }

    pub(crate) fn can_execute(&self) -> bool {
        self.wait.is_empty() || self.wait.iter().all(|s| s.ready())
    }

    pub(crate) async fn execute(self) {
        (self.inner).await;
        for signal in self.signal {
            signal.signal();
        }
    }
}

/// A wrapper for getting the return value of a [`Task`] that has no successors once it has finished.
pub struct TaskResult<T: MVSynced> {
    id: u64,
    timeout: u32,
    inner: Arc<RwLock<Option<T>>>
}

impl<T: MVSynced> TaskResult<T> {
    pub(crate) fn new(inner: Arc<RwLock<Option<T>>>, timeout: u32) -> Self {
        TaskResult {
            id: next_id("MVSync"),
            timeout,
            inner
        }
    }

    /// Returns whether the [`Task`] has finished executing.
    pub fn is_done(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    /// Waits until the [`Task`] has finished executing, and return the result of the task. If the [`Task`],
    /// or any of its predecessors, have panicked, this function will return [`None`], otherwise, it will
    /// return [`Some(T)`].
    pub fn wait(self) -> Option<T> {
        loop {
            if Arc::strong_count(&self.inner) == 1 {
                return self.inner.write().unwrap().take();
            }
            thread::sleep(Duration::from_millis(self.timeout as u64));
        }
    }
}

id_eq!(Task, TaskResult<T>[T: MVSynced]);