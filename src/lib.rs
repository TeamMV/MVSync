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

pub trait MVSynced: Send + Sync + 'static {}
impl<T> MVSynced for T where T: Send + Sync + 'static {}

pub struct MVSync {
    id: u64,
    specs: MVSyncSpecs,
    queue: Arc<Queue>
}

impl MVSync {
    pub fn new(specs: MVSyncSpecs) -> MVSync {
        next_id("MVSync");
        MVSync {
            id: next_id("MVSync"),
            specs,
            queue: Arc::new(Queue::new(specs))
        }
    }

    pub fn get_queue(&self) -> Arc<Queue> {
        self.queue.clone()
    }

    pub fn create_semaphore(&self) -> Arc<Semaphore> {
        Arc::new(Semaphore::new())
    }

    pub fn create_fence(&self) -> Arc<Fence> {
        Arc::new(Fence::new(self.specs.timeout_ms))
    }

    #[cfg(feature = "command-buffers")]
    pub fn allocate_command_buffer(&self) -> Result<CommandBuffer, CommandBufferAllocationError> {
        CommandBuffer::new(self.specs.timeout_ms)
    }

    pub fn create_task<T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_function(function, buffer);
        (task, result)
    }

    pub fn create_continuation<T: MVSynced, R: MVSynced>(&self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskResult<T>) -> (Task, TaskResult<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_continuation(function, buffer, predecessor);
        (task, result)
    }

    pub fn create_async_task<T: MVSynced, F: Future<Output = T>>(&self, function: impl FnOnce() -> F + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_async(function, buffer);
        (task, result)
    }

    pub fn create_future_task<T: MVSynced>(&self, function: impl Future<Output = T> + Send + 'static) -> (Task, TaskResult<T>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_future(function, buffer);
        (task, result)
    }

    pub fn create_async_continuation<T: MVSynced, R: MVSynced, F: Future<Output = R>>(&self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskResult<T>) -> (Task, TaskResult<R>) {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.specs.timeout_ms);
        let task = Task::from_async_continuation(function, buffer, predecessor);
        (task, result)
    }
}

id_eq!(MVSync);

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct MVSyncSpecs {
    pub thread_count: u32,
    pub workers_per_thread: u32,
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
    use crate::command_buffers::buffer::{Command, CommandBuffer, CommandBufferEntry};
    use crate::command_buffers::commands::Print;
    use crate::task::TaskResult;

    struct StringCommand {
        parent: CommandBuffer,
        response: TaskResult<String>
    }

    impl Command<String> for StringCommand {
        fn new(parent: CommandBuffer, response: TaskResult<String>) -> Self {
            StringCommand {
                parent,
                response
            }
        }

        fn parent(&self) -> &CommandBuffer {
            &self.parent
        }

        fn response(self) -> TaskResult<String> {
            self.response
        }
    }

    trait GenString: CommandBufferEntry {
        fn gen_string<F: Future<Output = String>>(&self, function: impl FnOnce() -> F + Send + 'static) -> StringCommand;
    }

    impl<T: CommandBufferEntry> GenString for T {
        fn gen_string<F: Future<Output=String>>(&self, function: impl FnOnce() -> F + Send + 'static) -> StringCommand {
            self.add_command(function)
        }
    }

    #[test]
    fn it_works() {
        let sync = MVSync::new(MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 2,
            timeout_ms: 10,
        });
        let queue = sync.get_queue();

        let command_buffer = sync.allocate_command_buffer().unwrap();

        let task = command_buffer
            .gen_string(|| async move {
                Delay::new(Duration::from_millis( 1000)).await;
                "Hello".to_string()
            })
            .print()
            .response();

        let task2 = command_buffer
            .gen_string(|| async move {
                Delay::new(Duration::from_millis( 1000)).await;
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
