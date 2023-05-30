use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::Signal;
use crate::MVSynced;
use crate::sync::{Fence, Semaphore, SemaphoreUsage};

pub(crate) enum TaskState {
    Pending,
    Ready,
    Panicked(Box<dyn Any + Send + 'static>),
    Cancelled
}

impl PartialEq for TaskState {
    fn eq(&self, other: &Self) -> bool {
        matches!((self, other), (TaskState::Pending, TaskState::Pending) | (TaskState::Ready, TaskState::Ready) | (TaskState::Panicked(_), TaskState::Panicked(_)) | (TaskState::Cancelled, TaskState::Cancelled))
    }
}

impl TaskState {
    pub(crate) fn replace(&mut self, state: TaskState) {
        *self = state;
    }

    fn take(&mut self) -> TaskState {
        match self {
            TaskState::Pending => TaskState::Pending,
            TaskState::Ready => TaskState::Ready,
            TaskState::Panicked(_) => TaskState::Panicked(Box::new(())),
            TaskState::Cancelled => TaskState::Cancelled
        }
    }
}

unsafe impl Send for TaskState {}
unsafe impl Sync for TaskState {}

/// A wrapper around a function, which can be synchronous or asynchronous, can return a value and take
/// input parameters.
///
/// This type must be used to submit tasks to the [`Queue`].
pub struct Task {
    id: u64,
    inner: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    wait: Vec<Arc<Semaphore>>,
    signal: [Arc<Signal>; 2],
    semaphores: Vec<Arc<Semaphore>>,
    preferred_thread: Option<String>,
    self_state: Arc<RwLock<TaskState>>,
    state: Arc<RwLock<TaskState>>
}

impl Task {
    pub(crate) fn new(self_state: Arc<RwLock<TaskState>>, inner: impl Future<Output = ()> + Send + 'static, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2]) -> Self {
        Task {
            id: next_id("MVSync"),
            inner: Box::pin(inner),
            wait: Vec::new(),
            signal,
            semaphores: Vec::new(),
            preferred_thread: None,
            self_state,
            state
        }
    }

    pub(crate) fn from_function<T: MVSynced>(function: impl FnOnce() -> T + Send + 'static, buffer: Arc<RwLock<Option<T>>>, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2]) -> Self {
        Task::new(state.clone(), async move {
            let t = function();
            buffer.write().unwrap().replace(t);
            state.write().unwrap().replace(TaskState::Ready);
            drop(buffer);
        }, Arc::new(RwLock::new(TaskState::Ready)), signal)
    }

    pub(crate) fn from_continuation<T: MVSynced, R: MVSynced>(function: impl FnOnce(T) -> R + Send + 'static, buffer: Arc<RwLock<Option<R>>>, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2], predecessor: TaskHandle<T>) -> Self {
        let prev_state = predecessor.state();
        Task::new(state.clone(), async move {
            let prev_state = predecessor.state();
            let t = predecessor.wait();
            let prev_state = prev_state.write().unwrap().take();
            match prev_state {
                TaskState::Ready => {
                    if let TaskResult::Returned(t) = t {
                        let r = function(t);
                        buffer.write().unwrap().replace(r);
                        state.write().unwrap().replace(TaskState::Ready);
                    }
                }
                TaskState::Panicked(p) => {
                    state.write().unwrap().replace(TaskState::Panicked(p));
                }
                TaskState::Cancelled => {
                    state.write().unwrap().replace(TaskState::Cancelled);
                }
                TaskState::Pending => {
                    state.write().unwrap().replace(TaskState::Cancelled);
                }
            }
            drop(buffer);
        }, prev_state, signal)
    }

    pub(crate) fn from_async<T: MVSynced, F: Future<Output = T> + Send>(function: impl FnOnce() -> F + Send + 'static, buffer: Arc<RwLock<Option<T>>>, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2]) -> Self {
        Task::new(state.clone(), async move {
            let t = function().await;
            buffer.write().unwrap().replace(t);
            state.write().unwrap().replace(TaskState::Ready);
            drop(buffer);
        }, Arc::new(RwLock::new(TaskState::Ready)), signal)
    }

    pub(crate) fn from_async_continuation<T: MVSynced, R: MVSynced, F: Future<Output = R> + Send>(function: impl FnOnce(T) -> F + Send + 'static, buffer: Arc<RwLock<Option<R>>>, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2], predecessor: TaskHandle<T>) -> Self {
        let prev_state = predecessor.state();
        Task::new(state.clone(), async move {
            let prev_state = predecessor.state();
            let t = predecessor.wait();
            let prev_state = prev_state.write().unwrap().take();
            match prev_state {
                TaskState::Ready => {
                    if let TaskResult::Returned(t) = t {
                        let r = function(t).await;
                        buffer.write().unwrap().replace(r);
                        state.write().unwrap().replace(TaskState::Ready);
                    }
                }
                TaskState::Panicked(p) => {
                    state.write().unwrap().replace(TaskState::Panicked(p));
                }
                TaskState::Cancelled => {
                    state.write().unwrap().replace(TaskState::Cancelled);
                }
                TaskState::Pending => {
                    state.write().unwrap().replace(TaskState::Cancelled);
                }
            }
            drop(buffer);
        }, prev_state, signal)
    }

    pub(crate) fn from_future<T: MVSynced>(function: impl Future<Output = T> + Send + 'static, buffer: Arc<RwLock<Option<T>>>, state: Arc<RwLock<TaskState>>, signal: [Arc<Signal>; 2]) -> Self {
        Task::new(state.clone(), async move {
            let t = function.await;
            buffer.write().unwrap().replace(t);
            state.write().unwrap().replace(TaskState::Ready);
            drop(buffer);
        }, Arc::new(RwLock::new(TaskState::Ready)), signal)
    }

    /// Bind a [`Semaphore`] to this task, the usage will specify whether to wait for the semaphore, or signal it.
    pub fn bind_semaphore(&mut self, semaphore: Arc<Semaphore>, usage: SemaphoreUsage) {
        match usage {
            SemaphoreUsage::Wait => self.wait.push(semaphore),
            SemaphoreUsage::Signal => self.semaphores.push(semaphore)
        }
    }

    /// Bind a [`Fence`] to this task, which will open when this task finishes.
    pub fn bind_fence(&mut self, fence: Arc<Fence>) {
        fence.bind(self.signal[0].clone())
    }

    pub fn set_preferred_thread(&mut self, thread: String) {
        self.preferred_thread = Some(thread);
    }

    pub fn remove_preferred_thread(&mut self) {
        self.preferred_thread = None;
    }

    pub fn get_preferred_thread(&self) -> Option<&String> {
        self.preferred_thread.as_ref()
    }

    pub(crate) fn state(&self) -> Arc<RwLock<TaskState>> {
        self.self_state.clone()
    }

    pub(crate) fn can_execute(&self) -> bool {
        (self.wait.is_empty() || self.wait.iter().all(|s| s.ready())) &&
            *self.state.read().unwrap() != TaskState::Pending
    }

    pub(crate) fn is_panicked(&self) -> bool {
        matches!(&*self.state.read().unwrap(), TaskState::Panicked(_))
    }

    pub(crate) fn get_panic(&self) -> Box<dyn Any + Send + 'static> {
        let p = self.state.write().unwrap().take();
        match p {
            TaskState::Panicked(p) => p,
            _ => panic!("Tired to get the panic value of non-panicking function!")
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.state.read().unwrap() == TaskState::Cancelled ||
            *self.self_state.read().unwrap() == TaskState::Cancelled
    }

    pub(crate) fn execute(self) -> (impl Future<Output = ()> + Send + 'static, Vec<Arc<Semaphore>>, [Arc<Signal>; 2]) {
        (self.inner, self.semaphores, self.signal)
    }
}

#[derive(Debug)]
pub enum TaskResult<T> {
    Returned(T),
    Panicked(Box<dyn Any + Send + 'static>),
    Cancelled
}

unsafe impl<T> Send for TaskResult<T> {}
unsafe impl<T> Sync for TaskResult<T> {}

/// A controller which allows you to cancel tasks and check when they are completed without having the result.
pub struct TaskController {
    id: u64,
    state: Arc<RwLock<TaskState>>,
    signal: Arc<Signal>
}

impl TaskController {
    pub(crate) fn new(state: Arc<RwLock<TaskState>>, signal: Arc<Signal>) -> Self {
        TaskController {
            id: next_id("MVSync"),
            state,
            signal
        }
    }

    /// Cancels this task.
    pub fn cancel(&self) {
        self.state.write().unwrap().replace(TaskState::Cancelled);
    }

    /// Returns whether the [`Task`] has finished executing.
    pub fn is_done(&self) -> bool {
        self.signal.ready()
    }

    /// Waits for this task to finish executing.
    pub fn wait(&self) {
        self.signal.wait();
    }
}

impl Clone for TaskController {
    fn clone(&self) -> Self {
        TaskController {
            id: next_id("MVSync"),
            state: self.state.clone(),
            signal: self.signal.clone()
        }
    }
}

/// A wrapper for getting the return value of a [`Task`] that has no successors once it has finished.
pub struct TaskHandle<T: MVSynced> {
    id: u64,
    inner: Arc<RwLock<Option<T>>>,
    state: Arc<RwLock<TaskState>>,
    signal: Arc<Signal>
}

impl<T: MVSynced> TaskHandle<T> {
    pub(crate) fn new(inner: Arc<RwLock<Option<T>>>, state: Arc<RwLock<TaskState>>, signal: Arc<Signal>) -> Self {
        TaskHandle {
            id: next_id("MVSync"),
            inner,
            state,
            signal
        }
    }

    pub(crate) fn state(&self) -> Arc<RwLock<TaskState>> {
        self.state.clone()
    }

    /// Cancels this task.
    pub fn cancel(&self) {
        self.state.write().unwrap().replace(TaskState::Cancelled);
    }

    /// Returns whether the [`Task`] has finished executing.
    pub fn is_done(&self) -> bool {
        *self.state.read().unwrap() != TaskState::Pending
    }

    /// Waits until the [`Task`] has finished executing, and return the result of the task. If the [`Task`],
    /// or any of its predecessors, have panicked, this function will return [`None`], otherwise, it will
    /// return [`Some(T)`].
    pub fn wait(self) -> TaskResult<T> {
        self.signal.wait();
        match self.state.write().unwrap().take() {
            TaskState::Ready => return TaskResult::Returned(self.inner.write().unwrap().take().unwrap()),
            TaskState::Panicked(p) => return TaskResult::Panicked(p),
            TaskState::Cancelled => return TaskResult::Cancelled,
            TaskState::Pending => panic!("Function finished but is still pending!")
        }
    }

    /// Make a new [`TaskController`] that can cancel or check the state of this [`Task`]. It will not
    /// be able to retrieve the result value of the task.
    pub fn make_controller(&self) -> TaskController {
        TaskController::new(self.state.clone(), self.signal.clone())
    }
}

id_eq!(Task, TaskHandle<T>[T: MVSynced], TaskController);