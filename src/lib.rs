//! Simple asynchronous task manager abstraction.
//!
//! Provides an abstraction layer over executing asynchronous tasks over multiple threads, without
//! re-creating threads, providing a speed increase over manually creating threads.
//!
//! This crate aims to provide both a lower-level, more manual API which is highly configurable,
//! and a higher-level API via the [`command-buffers`] feature.
//!
//! # Features:
//!
//! - [`queue`]\: Manages tasks once they are submitted. Efficiently spreads them across threads.
//! Prevents waiting tasks from clogging the workers, by not executing they are ready.
//!
//! - [`task`]\: Abstraction layer over functions, which can be synchronous or asynchronous, take
//! in parameters, and return a result.
//!
//! - [`sync`]\: Synchronization objects which provide a way of implementing control flow.
//!
//! - [`block`]\: Very simple 'poll to completion' awaiter. (Adapted from [`pollster`])
//!
//! - [`utils`]\: Some simple async utility functions.
//!
//! - [`command buffers`]\: A higher-level API abstraction layer, which allows making custom tasks,
//! as well as chaining tasks.
//!
//! [`pollster`]: https://crates.io/crates/pollster
//! [`command buffers`]: command_buffers

use std::future::Future;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU32, AtomicU8};
use crossbeam_channel::unbounded as channel;
use mvutils::id_eq;
use mvutils::utils::{next_id, Recover};
use crate::block::Signal;
use crate::queue::{Queue, WorkerThread};
use crate::sync::{Fence, Semaphore};
use crate::task::{Task, TaskHandle, TaskState};

#[cfg(feature = "command-buffers")]
use crate::command_buffers::buffer::{CommandBuffer, CommandBufferAllocationError};

pub mod prelude;

pub mod queue;
pub mod task;
pub mod sync;
pub mod block;
pub mod utils;
pub mod timer;
#[cfg(feature = "command-buffers")]
pub mod command_buffers;

/// A marker trait for types that can be used with MVSync.
pub trait MVSynced: Send + Sync + 'static {}
impl<T> MVSynced for T where T: Send + Sync + 'static {}

/// Main structure for managing multithreaded asynchronous tasks.
pub struct MVSync {
    id: u64,
    queue: Arc<Queue>,
    signal: Arc<Signal>,
    #[cfg(feature = "main-thread")]
    worker: RwLock<Option<WorkerThread>>
}

impl MVSync {
    /// Create a new MVSync instance.
    pub fn new(specs: MVSyncSpecs) -> Arc<MVSync> {
        next_id("MVSync");
        let signal = Arc::new(Signal::new());
        Arc::new(MVSync {
            id: next_id("MVSync"),
            queue: Arc::new(Queue::new(specs, vec![], signal.clone())),
            signal,
            #[cfg(feature = "main-thread")]
            worker: RwLock::new(None)
        })
    }

    /// Create a new MVSync instance.
    pub fn labelled(specs: MVSyncSpecs, labels: Vec<&'static str>) -> Arc<MVSync> {
        next_id("MVSync");
        let signal = Arc::new(Signal::new());
        Arc::new(MVSync {
            id: next_id("MVSync"),
            queue: Arc::new(Queue::new(specs, labels.into_iter().map(ToString::to_string).collect(), signal.clone())),
            signal,
            #[cfg(feature = "main-thread")]
            worker: RwLock::new(None)
        })
    }

    #[cfg(feature = "main-thread")]
    /// Registers the thread this is called from as the main thread. This allows you to run async tasks
    /// using the queue and worker system on the main thread. This will block the current thread until
    /// [`MVSync::end_main_thread`] is called and all async tasks have finished.
    ///
    /// The function parameter you provide is a `distributor`, which allows you to distribute some
    /// tasks onto the main thread. This function must return before any of the tasks start executing
    /// (or use `.await` on some non-instant future, however it is recommended you return).
    ///
    /// Only a single main thread can be registered, and all the tasks on the previous main thread
    /// must have finished before a new one is registered.
    ///
    /// If `end_when_done` is set to true, the worker will automatically end itself when there are no
    /// more tasks for it to complete, unblocking the main thread and allowing another thread to register
    /// itself as a main thread.
    pub fn register_main_thread<F: Future<Output = ()> + Send>(self: &Arc<MVSync>, init: impl FnOnce(Arc<MVSync>) -> F + Send + 'static, end_when_done: bool) {
        let mut worker = self.worker.write().recover();
        if worker.is_none() || worker.as_ref().unwrap().finished() {
            let signal = Arc::new(Signal::new());
            let (sender, receiver) = channel();
            let free_workers = Arc::new(AtomicU32::new(4294967295));
            let access = free_workers.clone();
            let signal_clone = signal.clone();
            let end = Arc::new(AtomicU8::new(0));
            let end_clone = end.clone();
            let thread = WorkerThread {
                id: next_id("MVSync"),
                sender,
                label: None,
                free_workers,
                end,
                signal
            };
            worker.replace(thread);
            let this = self.clone();
            let (task, _) = self.create_async_task(|| async move {
                init(this).await;
            });
            worker.as_ref().unwrap().send(task);
            drop(worker);
            WorkerThread::run(receiver, access, signal_clone, end_clone, end_when_done);
        }
        else {
            panic!("This MVSync instance already has a main thread registered!");
        }
    }

    /// Add a task to the main thread. This function will panic if the main thread is ended
    /// or not registered.
    #[cfg(feature = "main-thread")]
    pub fn submit_to_main_thread(self: &Arc<MVSync>, task: Task) {
        let worker = self.worker.read().recover();
        if worker.is_none() || worker.as_ref().unwrap().ended() {
            panic!("This MVSync instance does not have a main thread registered!");
        }
        else {
            worker.as_ref().unwrap().send(task);
        }
    }

    /// Ends the main thread. This function won't panic, even if nothing was actually
    /// executed due to the main thread being already ended or not registered.
    #[cfg(feature = "main-thread")]
    pub fn end_main_thread(self: &Arc<MVSync>) {
        let worker = self.worker.write().recover();
        if !(worker.is_none() || worker.as_ref().unwrap().ended()) {
            worker.as_ref().unwrap().end();
        }
    }

    /// Checks if the main thread ended.
    #[cfg(feature = "main-thread")]
    pub fn main_thread_ended(self: &Arc<MVSync>) -> bool {
        let worker = self.worker.write().recover();
        worker.is_none() || worker.as_ref().unwrap().ended()
    }

    /// Checks if the main thread finished running all tasks.
    #[cfg(feature = "main-thread")]
    pub fn main_thread_finished(self: &Arc<MVSync>) -> bool {
        let worker = self.worker.write().recover();
        worker.is_none() || worker.as_ref().unwrap().finished()
    }

    /// Get the MVSync queue bound to this [`MVSync`] instance.
    pub fn get_queue(self: &Arc<MVSync>) -> Arc<Queue> {
        self.queue.clone()
    }

    /// Create a [`Semaphore`]
    pub fn create_semaphore(self: &Arc<MVSync>) -> Arc<Semaphore> {
        Arc::new(Semaphore::new())
    }

    /// Create a [`Fence`]
    pub fn create_fence(self: &Arc<MVSync>) -> Arc<Fence> {
        Arc::new(Fence::new())
    }

    #[cfg(feature = "command-buffers")]
    /// Allocate a new [`CommandBuffer`] that can be used to record commands.
    ///
    /// # Returns:
    /// - [`Ok(CommandBuffer)`] if the command buffer was successfully allocated.
    /// - [`Err(CommandBufferAllocationError)`] if the command buffer could not be allocated on the heap.
    pub fn allocate_command_buffer(self: &Arc<MVSync>) -> Result<CommandBuffer, CommandBufferAllocationError> {
        CommandBuffer::new(self.signal.clone())
    }

    /// Create a new [`Task`], wrapping a synchronous function that returns a value.
    pub fn create_task<T: MVSynced>(self: &Arc<MVSync>, function: impl FnOnce() -> T + Send + 'static) -> (Task, TaskHandle<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let signal = Arc::new(Signal::new());
        let result = TaskHandle::new(buffer.clone(), state.clone(), signal.clone());
        let task = Task::from_function(function, buffer, state, [signal, self.signal.clone()]);
        (task, result)
    }

    /// Create a new [`Task`], wrapping a synchronous function that takes in a parameter from a
    /// previous function,  returning a value.
    pub fn create_continuation<T: MVSynced, R: MVSynced>(self: &Arc<MVSync>, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskHandle<T>) -> (Task, TaskHandle<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let signal = Arc::new(Signal::new());
        let result = TaskHandle::new(buffer.clone(), state.clone(), signal.clone());
        let task = Task::from_continuation(function, buffer, state, [signal, self.signal.clone()], predecessor);
        (task, result)
    }

    /// Create a new [`Task`], wrapping an asynchronous function that returns a value.
    pub fn create_async_task<T: MVSynced, F: Future<Output = T> + Send>(self: &Arc<MVSync>, function: impl FnOnce() -> F + Send + 'static) -> (Task, TaskHandle<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let signal = Arc::new(Signal::new());
        let result = TaskHandle::new(buffer.clone(), state.clone(), signal.clone());
        let task = Task::from_async(function, buffer, state, [signal, self.signal.clone()]);
        (task, result)
    }

    /// Create a new [`Task`], wrapping a future that returns a value.
    pub fn create_future_task<T: MVSynced>(self: &Arc<MVSync>, function: impl Future<Output = T> + Send + 'static) -> (Task, TaskHandle<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let signal = Arc::new(Signal::new());
        let result = TaskHandle::new(buffer.clone(), state.clone(), signal.clone());
        let task = Task::from_future(function, buffer, state, [signal, self.signal.clone()]);
        (task, result)
    }

    /// Create a new [`Task`], wrapping an asynchronous function that takes in a parameter from a
    /// previous function,  returning a value.
    pub fn create_async_continuation<T: MVSynced, R: MVSynced, F: Future<Output = R> + Send>(self: &Arc<MVSync>, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskHandle<T>) -> (Task, TaskHandle<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let signal = Arc::new(Signal::new());
        let result = TaskHandle::new(buffer.clone(), state.clone(), signal.clone());
        let task = Task::from_async_continuation(function, buffer, state, [signal, self.signal.clone()], predecessor);
        (task, result)
    }
}

id_eq!(MVSync);

/// A struct with configuration parameters (specifications) for MVSync.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct MVSyncSpecs {
    /// How many threads to create to handle task execution. One extra thread is created to handle
    /// distributing the tasks between the other threads and storing functions that are not ready
    /// to execute yet.
    pub thread_count: u32,

    /// How many asynchronous workers to create per thread. This does not increase the speed, and is
    /// only useful if you plan to use this to wait for events, like networking requests.
    pub workers_per_thread: u32,
}

impl Default for MVSyncSpecs {
    fn default() -> Self {
        MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 1
        }
    }
}


#[cfg(test)]
mod tests {
    use crate::{MVSync, MVSyncSpecs};
    use crate::utils::async_yield;

    #[test]
    fn it_works() {
        let sync = MVSync::new(MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 2
        });

        sync.register_main_thread(|sync| async move {
            let (a, _) = sync.create_async_task(|| async move {
                run("A").await;
            });

            let (b, _) = sync.create_async_task(|| async move {
                run("B").await;
            });

            sync.submit_to_main_thread(a);
            sync.submit_to_main_thread(b);
        }, true);
    }

    async fn run(name: &str) {
        for i in 0..10 {
            println!("{}: {}", name, i);
            async_yield().await;
        }
    }
}
