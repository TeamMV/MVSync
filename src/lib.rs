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
//! - [`command-buffers`]\: A higher-level API abstraction layer, which allows making custom tasks,
//! as well as chaining tasks.
//!
//! [`queue`]: queue
//! [`task`]: task
//! [`sync`]: sync
//! [`block`]: block
//! [`pollster`]: https://crates.io/crates/pollster
//! [`command-buffers`]: command_buffers

use std::future::Future;
use std::sync::{Arc, RwLock};
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::queue::Queue;
use crate::sync::{Fence, Semaphore};
use crate::task::{Task, TaskResult};

#[cfg(feature = "command-buffers")]
use crate::command_buffers::buffer::{CommandBuffer, CommandBufferAllocationError};

pub mod prelude;

pub mod queue;
pub mod task;
pub mod sync;
pub mod block;
#[cfg(feature = "command-buffers")]
pub mod command_buffers;

/// A marker trait for types that can be used with MVSync.
pub trait MVSynced: Send + Sync + 'static {}
impl<T> MVSynced for T where T: Send + Sync + 'static {}

/// Main structure for managing multithreaded asynchronous tasks.
pub struct MVSync {
    id: u64,
    specs: MVSyncSpecs,
    queue: Arc<Queue>
}

impl MVSync {
    /// Create a new MVSync instance.
    pub fn new(specs: MVSyncSpecs) -> MVSync {
        next_id("MVSync");
        MVSync {
            id: next_id("MVSync"),
            specs,
            queue: Arc::new(Queue::new(specs))
        }
    }

    /// Get the MVSync queue bound to this [`MVSync`] instance.
    pub fn get_queue(&self) -> Arc<Queue> {
        self.queue.clone()
    }

    /// Create a [`Semaphore`]
    pub fn create_semaphore(&self) -> Arc<Semaphore> {
        Arc::new(Semaphore::new())
    }

    /// Create a [`Fence`]
    pub fn create_fence(&self) -> Arc<Fence> {
        Arc::new(Fence::new(self.specs.timeout_ms))
    }

    #[cfg(feature = "command-buffers")]
    /// Allocate a new [`CommandBuffer`] that can be used to record commands.
    ///
    /// # Returns:
    /// - [`Ok(CommandBuffer)`] if the command buffer was successfully allocated.
    /// - [`Err(CommandBufferAllocationError)`] if the command buffer could not be allocated on the heap.
    pub fn allocate_command_buffer(&self) -> Result<CommandBuffer, CommandBufferAllocationError> {
        CommandBuffer::new(self.specs.timeout_ms)
    }

    /// Create a new [`Task`], wrapping a synchronous function that returns a value.
    pub fn create_task<T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_function(function, buffer);
        (task, result)
    }

    /// Create a new [`Task`], wrapping a synchronous function that takes in a parameter from a
    /// previous function,  returning a value.
    pub fn create_continuation<T: MVSynced, R: MVSynced>(&self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskResult<T>) -> (Task, TaskResult<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_continuation(function, buffer, predecessor);
        (task, result)
    }

    /// Create a new [`Task`], wrapping an asynchronous function that returns a value.
    pub fn create_async_task<T: MVSynced, F: Future<Output = T>>(&self, function: impl FnOnce() -> F + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_async(function, buffer);
        (task, result)
    }

    /// Create a new [`Task`], wrapping a future that returns a value.
    pub fn create_future_task<T: MVSynced>(&self, function: impl Future<Output = T> + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_future(function, buffer);
        (task, result)
    }

    /// Create a new [`Task`], wrapping an asynchronous function that takes in a parameter from a
    /// previous function,  returning a value.
    pub fn create_async_continuation<T: MVSynced, R: MVSynced, F: Future<Output = R>>(&self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskResult<T>) -> (Task, TaskResult<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_async_continuation(function, buffer, predecessor);
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

    /// How long to wait between loop iterations. A lower value means there will be less latency
    /// between task executions, at the cost of increased CPU usage.
    pub timeout_ms: u32,
}

impl Default for MVSyncSpecs {
    fn default() -> Self {
        MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 1,
            timeout_ms: 10
        }
    }
}


#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;
    use futures_timer::Delay;
    use crate::{MVSync, MVSyncSpecs};
    use crate::command_buffers::buffer::{BufferedCommand, Command, CommandBuffer, CommandBufferEntry};
    use crate::command_buffers::commands::Print;
    use crate::task::TaskResult;

    trait GenString: CommandBufferEntry {
        fn gen_string<F: Future<Output = String>>(&self, function: impl FnOnce() -> F + Send + 'static) -> BufferedCommand<String> {
            self.add_command(function)
        }
    }

    impl<T: CommandBufferEntry> GenString for T {}

    #[test]
    fn it_works() {
        let sync = MVSync::new(MVSyncSpecs {
            thread_count: 2,
            workers_per_thread: 2,
            timeout_ms: 10,
        });
        let queue = sync.get_queue();

        let command_buffer = sync.allocate_command_buffer().unwrap();

        let task = command_buffer
            .gen_string(|| async move {
                Delay::new(Duration::from_millis(1000)).await;
                "Hello".to_string()
            })
            .print()
            .response();

        let task2 = command_buffer
            .gen_string(|| async move {
                Delay::new(Duration::from_millis(500)).await;
                "World".to_string()
            })
            .print()
            .response();

        command_buffer.finish();

        queue.submit_command_buffer(command_buffer);

        task.wait();
        task2.wait();
    }
}
