use std::future::Future;
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

    pub fn submit(&self, task: Task) {
        self.sender.send(task).expect("Failed to submit task!");
    }

    #[cfg(feature = "command-buffers")]
    pub fn submit_command_buffer(&self, command_buffer: CommandBuffer) {
        for task in command_buffer.tasks() {
            self.sender.send(task).expect("Failed to submit command buffer!");
        }
    }

    pub fn push(&self, f: impl FnOnce() + Send + 'static) {
        self.sender.send(Task::new(async move { f() })).expect("Failed to submit task!");
    }

    pub fn push_async(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.sender.send(Task::new(f)).expect("Failed to submit task!");
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