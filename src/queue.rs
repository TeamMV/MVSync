use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use futures::task::SpawnExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded as channel};
use futures::{SinkExt, StreamExt};
use futures::executor::{LocalPool, LocalSpawner};
use futures_timer::Delay;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::AwaitSync;
use crate::MVSyncSpecs;
use crate::task::Task;

#[cfg(feature = "command-buffers")]
use crate::command_buffers::buffer::CommandBuffer;

/// The MVSync queue. A single queue exists per MVSync instance. It is ran on its own thread, and
/// distributes tasks efficiently between threads that are allocated to MVSync.
///
/// If a task that is submitted is waiting on a semaphore, it will not be submitted until it is ready,
/// to prevent the workers from being clogged by functions that are waiting for other functions.
pub struct Queue {
    id: u64,
    sender: Sender<Task>,
}

impl Queue {
    pub(crate) fn new(specs: MVSyncSpecs) -> Self {
        let (sender, receiver) = unbounded();
        let threads = (0..specs.thread_count).map(|_| WorkerThread::new(specs)).collect();
        let _manager = thread::spawn(move || Self::run(receiver, threads, specs));
        Queue {
            id: next_id("MVSync"),
            sender
        }
    }

    fn run(receiver: Receiver<Task>, threads: Vec<WorkerThread>, specs: MVSyncSpecs) {
        fn optimal(workers: &[WorkerThread]) -> &WorkerThread {
            workers.iter().max_by_key(|w| w.free_workers()).unwrap()
        }

        let mut tasks = Vec::new();
        loop {
            let task = receiver.try_recv();
            match task {
                Ok(task) => tasks.push(task),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break
            }
            for i in 0..tasks.len() {
                if i >= tasks.len() {
                    break;
                }
                if tasks[i].can_execute() {
                    let thread = optimal(&threads);
                    if thread.free_workers() == 0 {
                        break;
                    }
                    thread.get_sender().send(tasks.remove(i)).await_sync().expect("Failed to send task!");
                }
            }
            Delay::new(Duration::from_millis(specs.timeout_ms as u64)).await_sync();
        }
    }

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit(&self, task: Task) {
        self.sender.send(task).expect("Failed to submit task!");
    }

    /// Submit a command buffer to the queue. This will push the tasks to the back of the queue in
    /// the same order you called them on the buffer. When there is a free worker available, each
    /// task will be popped off the queue and executed by the worker sequentially. Tasks that require
    /// other tasks to finish will not be popped until they are ready, so no tasks will clog the queue.
    ///
    /// # Panics
    /// If the command buffer is not baked.
    ///
    /// # Arguments
    /// command_buffer - The command buffer to push to the back of the queue.
    #[cfg(feature = "command-buffers")]
    pub fn submit_command_buffer(&self, command_buffer: CommandBuffer) {
        for task in command_buffer.tasks() {
            self.sender.send(task).expect("Failed to submit command buffer!");
        }
    }
}

struct WorkerThread {
    id: u64,
    sender: UnboundedSender<Task>,
    free_workers: Arc<RwLock<u32>>
}

impl WorkerThread {
    fn new(specs: MVSyncSpecs) -> Self {
        let (sender, receiver) = channel();
        let free_workers = Arc::new(RwLock::new(specs.workers_per_thread));
        let access = free_workers.clone();
        let _thread = thread::spawn(move || Self::run(receiver, access));
        WorkerThread {
            id: next_id("MVSync"),
            sender,
            free_workers
        }
    }

    fn run(mut receiver: UnboundedReceiver<Task>, free_workers: Arc<RwLock<u32>>) {
        let mut pool = LocalPool::new();
        let ptr = (&pool.spawner() as *const LocalSpawner) as usize;
        let dispatcher = async move {
            while let Some(task) = receiver.next().await {
                *free_workers.write().unwrap() -= 1;
                let access = free_workers.clone();
                let spawner = unsafe { (ptr as *const LocalSpawner).as_ref().unwrap() };
                spawner.spawn(async move {
                    task.execute().await;
                    *access.write().unwrap() += 1;
                }).expect("Failed to spawn task!");
            }
        };
        pool.spawner().spawn(dispatcher).expect("Failed to spawn task dispatcher!");
        pool.run();
    }

    fn get_sender(&self) -> UnboundedSender<Task> {
        self.sender.clone()
    }

    fn free_workers(&self) -> u32 {
        *self.free_workers.read().unwrap()
    }
}

id_eq!(Queue, WorkerThread);