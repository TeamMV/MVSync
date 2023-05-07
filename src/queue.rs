use std::future::Future;
use std::sync::{Arc, RwLock};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use futures::task::{LocalSpawnExt, SpawnExt};
use futures::channel::mpsc::{Receiver as AsyncReceiver, Sender as AsyncSender, channel};
use futures::{SinkExt, StreamExt};
use futures::executor::{LocalPool, LocalSpawner};
use futures_timer::Delay;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::MVSyncSpecs;
use crate::task::Task;

pub struct Queue {
    id: u64,
    sender: Sender<Task>,
    manager: JoinHandle<()>,
    specs: MVSyncSpecs
}

impl Queue {
    pub(crate) fn new(specs: MVSyncSpecs) -> Self {
        let (sender, receiver) = unbounded();
        let threads = (0..specs.thread_count).map(|_| WorkerThread::new(specs)).collect();
        let manager = thread::spawn(move || Self::run(receiver, threads, specs));
        Queue {
            id: next_id("MVSync"),
            sender,
            manager,
            specs
        }
    }

    fn run(receiver: Receiver<Task>, threads: Vec<WorkerThread>, specs: MVSyncSpecs) {
        fn optimal(workers: &[WorkerThread]) -> &WorkerThread {
            workers.iter().max_by_key(|w| w.free_workers()).unwrap()
        }

        let mut pool = LocalPool::new();

        let listener = async move {
            let mut tasks = Vec::new();
            loop {
                let task = receiver.try_recv();
                match task {
                    Ok(task) => tasks.push(task),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => break
                }
                for i in 0..tasks.len() {
                    if tasks[i].can_execute() {
                        let thread = optimal(&threads);
                        if thread.free_workers() == 0 {
                            break;
                        }
                        thread.get_sender().send(tasks.remove(i)).await.expect("Failed to send task!");
                    }
                }
                Delay::new(Duration::from_millis(specs.timeout_ms as u64)).await;
            }
        };
        pool.run_until(listener);
    }

    pub fn push(&self, f: impl FnOnce() + Send + 'static) {
        self.sender.send(Task::new(async move { f() }, vec![], vec![])).expect("Failed to submit task!");
    }

    pub fn push_async(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.sender.send(Task::new(f, vec![], vec![])).expect("Failed to submit task!");
    }
}

struct WorkerThread {
    id: u64,
    sender: AsyncSender<Task>,
    free_workers: Arc<RwLock<u32>>,
    thread: JoinHandle<()>,
}

impl WorkerThread {
    fn new(specs: MVSyncSpecs) -> Self {
        let (sender, receiver) = channel(specs.workers_per_thread as usize);
        let free_workers = Arc::new(RwLock::new(specs.workers_per_thread));
        let access = free_workers.clone();
        let thread = thread::spawn(move || Self::run(receiver, access));
        WorkerThread {
            id: next_id("MVSync"),
            sender,
            free_workers,
            thread
        }
    }

    fn run(mut receiver: AsyncReceiver<Task>, free_workers: Arc<RwLock<u32>>) {
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
        pool.spawner().spawn(async move {
            dispatcher.await;
        }).expect("Failed to spawn task dispatcher!");
        pool.run();
    }

    fn get_sender(&self) -> AsyncSender<Task> {
        self.sender.clone()
    }

    fn free_workers(&self) -> u32 {
        *self.free_workers.read().unwrap()
    }
}

id_eq!(Queue, WorkerThread);