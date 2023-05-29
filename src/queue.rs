use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::task::{Context, Poll, Waker};
use std::thread;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded as channel};
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::{AwaitSync, Signal, State};
use crate::MVSyncSpecs;
use crate::task::Task;

#[cfg(feature = "command-buffers")]
use crate::command_buffers::buffer::CommandBuffer;
use crate::utils::async_sleep_ms;

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
    pub(crate) fn new(specs: MVSyncSpecs, labels: Vec<String>) -> Self {
        let (sender, receiver) = channel();
        let mut threads = (0..specs.thread_count).map(|_| WorkerThread::new(specs)).collect::<Vec<_>>();
        labels.iter().enumerate().for_each(|(i, label)| {
            if i < threads.len() {
                threads[i].label(label.clone());
            }
        });
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

        fn labelled<'a>(workers: &'a [WorkerThread], label: &String) -> Option<&'a WorkerThread> {
            workers.iter().find(|w| w.get_label().unwrap_or(&"".to_string()) == label)
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
                    if let Some(t) = tasks[i].get_preferred_thread() {
                        let thread = labelled(&threads, t);
                        if let Some(thread) = thread {
                            if thread.free_workers() == 0 {
                                continue;
                            }
                            thread.get_sender().send(tasks.remove(i)).expect("Failed to send task");
                            continue;
                        }
                    }
                    let thread = optimal(&threads);
                    if thread.free_workers() == 0 {
                        break;
                    }
                    thread.get_sender().send(tasks.remove(i)).expect("Failed to send task!");
                }
            }
            async_sleep_ms(specs.timeout_ms as u64).await_sync();
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

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit_on(&self, mut task: Task, thread: &str) {
        task.set_preferred_thread(thread.to_string());
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
    sender: Sender<Task>,
    label: Option<String>,
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
            label: None,
            free_workers
        }
    }

    fn label(&mut self, label: String) {
        self.label = Some(label);
    }

    fn get_label(&self) -> Option<&String> {
        self.label.as_ref()
    }

    fn run(receiver: Receiver<Task>, free_workers: Arc<RwLock<u32>>) {
        let signal = Arc::new(Signal { state: Mutex::new(State::Ready), condition: Condvar::new() });
        let waker = Waker::from(signal.clone());
        let mut ctx = Context::from_waker(&waker);
        let mut futures = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(task) => {
                    *free_workers.write().unwrap() -= 1;
                    futures.push(task.execute());
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break,
            }
            let drained = futures.drain(..).collect::<Vec<_>>();
            for mut future in drained {
                let mut p =  unsafe { std::pin::Pin::new_unchecked(&mut future) };
                match catch_unwind(AssertUnwindSafe(|| p.as_mut().poll(&mut ctx))) {
                    Ok(Poll::Pending) => {
                        signal.wait();
                        futures.push(future);
                    }
                    _ => {
                        *free_workers.write().unwrap() += 1;
                    }
                }
            }
        }
    }

    fn get_sender(&self) -> Sender<Task> {
        self.sender.clone()
    }

    fn free_workers(&self) -> u32 {
        *self.free_workers.read().unwrap()
    }
}

id_eq!(Queue, WorkerThread);