use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll, Waker};
use std::thread;
use crossbeam_channel::{Receiver, Sender, unbounded as channel};
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::{AwaitSync, Signal};
use crate::MVSyncSpecs;
use crate::task::{Task, TaskState};

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
    has_tasks: Arc<(Mutex<bool>, Condvar)>
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
        let has_tasks = Arc::new((Mutex::new(false), Condvar::new()));
        let pair = has_tasks.clone();
        let _manager = thread::spawn(move || Self::run(receiver, threads, pair));
        Queue {
            id: next_id("MVSync"),
            sender,
            has_tasks
        }
    }

    fn run(receiver: Receiver<Task>, threads: Vec<WorkerThread>, has_tasks: Arc<(Mutex<bool>, Condvar)>) {
        let mut tasks = Vec::new();
        loop {
            if tasks.is_empty() {
                match receiver.recv() {
                    Ok(task) => tasks.push(task),
                    Err(_) => break
                }
            }
            else {
                while let Ok(task) = receiver.try_recv() {
                    tasks.push(task);
                }
            }

            let mut remaining_tasks = Vec::with_capacity(tasks.len());
            for mut task in tasks.drain(..) {
                if task.can_execute() {
                    if task.is_panicked() {
                        let (future, signal) =  task.execute();
                        future.await_sync();
                        for signal in signal {
                            signal.signal();
                        }
                        continue;
                    }
                    else if task.is_cancelled() {
                        task.state().write().unwrap().replace(TaskState::Cancelled);
                        let (_, signal) = task.execute();
                        for signal in signal {
                            signal.signal();
                        }
                        continue;
                    }

                    let target_thread = match task.get_preferred_thread() {
                        Some(label) => threads.iter().find(|thread| thread.get_label().as_deref() == Some(label)),
                        None => threads.iter().max_by_key(|thread| thread.free_workers()),
                    };

                    if let Some(thread) = target_thread {
                        if thread.free_workers() > 0 {
                            thread.send(task);
                        } else {
                            remaining_tasks.push(task);
                        }
                    } else {
                        if task.get_preferred_thread().is_some() {
                            // Thread with the label doesn't exist
                            task.remove_preferred_thread();
                        }
                        remaining_tasks.push(task);
                    }
                }
                else {
                    remaining_tasks.push(task);
                }
            }
            tasks = remaining_tasks;

            if !tasks.is_empty() {
                let (lock, cvar) = &*has_tasks;
                let mut has_tasks = lock.lock().unwrap_or_else(|e| e.into_inner());
                while !*has_tasks {
                    has_tasks = cvar.wait(has_tasks).unwrap_or_else(|e| e.into_inner());
                }
                *has_tasks = false;
            }
        }
    }

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit(&self, task: Task) {
        self.sender.send(task).expect("Failed to submit task!");

        let (lock, cvar) = &*self.has_tasks;
        let mut has_tasks = lock.lock().unwrap_or_else(|e| e.into_inner());
        *has_tasks = true;
        cvar.notify_one();
    }

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit_on(&self, mut task: Task, thread: &str) {
        task.set_preferred_thread(thread.to_string());
        self.sender.send(task).expect("Failed to submit task!");

        let (lock, cvar) = &*self.has_tasks;
        let mut has_tasks = lock.lock().unwrap_or_else(|e| e.into_inner());
        *has_tasks = true;
        cvar.notify_one();
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

        let (lock, cvar) = &*self.has_tasks;
        let mut has_tasks = lock.lock().unwrap_or_else(|e| e.into_inner());
        *has_tasks = true;
        cvar.notify_one();
    }
}

struct WorkerThread {
    id: u64,
    sender: Sender<Task>,
    label: Option<String>,
    free_workers: Arc<AtomicU32>
}

impl WorkerThread {
    fn new(specs: MVSyncSpecs) -> Self {
        let (sender, receiver) = channel();
        let free_workers = Arc::new(AtomicU32::new(specs.workers_per_thread));
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

    fn run(receiver: Receiver<Task>, free_workers: Arc<AtomicU32>) {
        let signal = Arc::new(Signal::new());
        let waker = Waker::from(signal.clone());
        let mut ctx = Context::from_waker(&waker);
        let mut futures = Vec::new();

        loop {
            if futures.is_empty() {
                match receiver.recv() {
                    Ok(task) => futures.push((task.state(), task.execute())),
                    Err(_) => break
                }
            }
            else {
                while let Ok(task) = receiver.try_recv() {
                    futures.push((task.state(), task.execute()));
                }
            }

            futures.retain_mut(|(state, (future, to_signal))| {
                let mut p =  unsafe { Pin::new_unchecked(future) };
                match catch_unwind(AssertUnwindSafe(|| p.as_mut().poll(&mut ctx))) {
                    Ok(Poll::Pending) => {
                        if *state.read().unwrap() == TaskState::Cancelled {
                            for s in to_signal {
                                s.signal();
                            }
                            false
                        }
                        else {
                            true
                        }
                    },
                    Err(e) => {
                        state.write().unwrap().replace(TaskState::Panicked(e));
                        for s in to_signal {
                            s.signal();
                        }
                        false
                    }
                    Ok(Poll::Ready(_)) => {
                        state.write().unwrap().replace(TaskState::Ready);
                        free_workers.fetch_add(1, Ordering::SeqCst);
                        for s in to_signal {
                            s.signal();
                        }
                        false
                    }
                }
            });

            if futures.is_empty() {
                if receiver.is_empty() {
                    break;
                }
                signal.wait();
            }
        }
    }

    fn send(&self, task: Task) {
        self.sender.send(task).expect("Failed to send task!");
        self.free_workers.fetch_sub(1, Ordering::SeqCst);
    }

    fn free_workers(&self) -> u32 {
        self.free_workers.load(Ordering::Relaxed)
    }
}

id_eq!(Queue, WorkerThread);