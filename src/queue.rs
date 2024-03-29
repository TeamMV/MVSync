use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "main-thread")]
use std::sync::atomic::AtomicU8;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use crossbeam_channel::{Receiver, Sender, unbounded as channel};
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::Signal;
use crate::MVSyncSpecs;
use crate::task::{Task, TaskState};

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
    signal: Arc<Signal>,
    specs: MVSyncSpecs
}

impl Queue {
    pub(crate) fn new(specs: MVSyncSpecs, labels: Vec<String>, signal: Arc<Signal>) -> Self {
        let (sender, receiver) = channel();
        if specs.thread_count > 0 {
            let mut threads = (0..specs.thread_count).map(|_| WorkerThread::new(specs)).collect::<Vec<_>>();
            labels.iter().enumerate().for_each(|(i, label)| {
                if i < threads.len() {
                    threads[i].label(label.clone());
                }
            });
            let clone = signal.clone();
            thread::spawn(move || Self::run(receiver, threads, clone));
        }
        Queue {
            id: next_id("MVSync"),
            sender,
            signal,
            specs
        }
    }

    fn run(receiver: Receiver<Task>, threads: Vec<WorkerThread>, signal: Arc<Signal>) {
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
                        let p = task.get_panic();
                        task.state().write().unwrap().replace(TaskState::Panicked(p));
                        let (_, semaphores, signal) =  task.execute();
                        for semaphore in semaphores {
                            semaphore.signal();
                        }
                        for signal in signal {
                            signal.wake();
                        }
                        continue;
                    }
                    else if task.is_cancelled() {
                        task.state().write().unwrap().replace(TaskState::Cancelled);
                        let (_, semaphores, signal) = task.execute();
                        for semaphore in semaphores {
                            semaphore.signal();
                        }
                        for signal in signal {
                            signal.wake();
                        }
                        continue;
                    }

                    let target_thread = match task.get_preferred_thread() {
                        Some(label) => threads.iter().find(|thread| thread.get_label() == Some(label)),
                        None => threads.iter().filter(|t| {
                            if let Some(label) = t.get_label() {
                                !task.get_not_on().contains(label)
                            }
                            else {
                                true
                            }
                        }).max_by_key(|thread| thread.free_workers()),
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
                        else {
                            task.clear_not_on();
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
                signal.wait();
            }
        }
    }

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit(&self, task: Task) {
        if self.specs.thread_count == 0 {
            crate::run::run(task);
            return;
        }
        self.sender.send(task).expect("Failed to submit task!");
        self.signal.clone().wake();
    }

    /// Submit a vec of tasks to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available, the task will be popped off the queue and executed by the worker in order.
    ///
    /// # Arguments
    /// tasks - The vec of tasks to push to the back of the queue.
    pub fn submit_all(&self, tasks: Vec<Task>) {
        if self.specs.thread_count == 0 {
            crate::run::run_all(tasks);
            return;
        }
        for task in tasks {
            self.sender.send(task).expect("Failed to submit task!");
        }
        self.signal.clone().wake();
    }

    /// Submit a task to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available on the given thread, the task will be popped off the queue and executed by the worker.
    ///
    /// # Arguments
    /// task - The task to push to the back of the queue.
    pub fn submit_on(&self, mut task: Task, thread: &str) {
        if self.specs.thread_count == 0 {
            crate::run::run(task);
            return;
        }
        task.set_preferred_thread(thread.to_string());
        self.sender.send(task).expect("Failed to submit task!");
        self.signal.clone().wake();
    }

    /// Submit a vec of tasks to the queue. This will push the task to the back of the queue. When there is
    /// a free worker available on the given thread, the task will be popped off the queue and executed by the worker in order.
    ///
    /// # Arguments
    /// tasks - The vec of tasks to push to the back of the queue.
    pub fn submit_all_on(&self, tasks: Vec<Task>, thread: &str) {
        if self.specs.thread_count == 0 {
            crate::run::run_all(tasks);
            return;
        }
        for mut task in tasks {
            task.set_preferred_thread(thread.to_string());
            self.sender.send(task).expect("Failed to submit task!");
        }
        self.signal.clone().wake();
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
        if self.specs.thread_count == 0 {
            crate::run::run_all(command_buffer.tasks());
            return;
        }
        for task in command_buffer.tasks() {
            self.sender.send(task).expect("Failed to submit command buffer!");
        }
        self.signal.clone().wake();
    }

    /// Submit a vec of command buffers to the queue. This will push the tasks to the back of the queue in
    /// the same order you called them on the buffer. When there is a free worker available, each
    /// task will be popped off the queue and executed by the worker sequentially. Tasks that require
    /// other tasks to finish will not be popped until they are ready, so no tasks will clog the queue.
    ///
    /// # Panics
    /// If any of the command buffer is not baked.
    ///
    /// # Arguments
    /// command_buffers - The vec of command buffers to push to the back of the queue.
    #[cfg(feature = "command-buffers")]
    pub fn submit_command_buffers(&self, command_buffers: Vec<CommandBuffer>) {
        if self.specs.thread_count == 0 {
            crate::run::run_all(command_buffers.into_iter().flat_map(|b| b.tasks()).collect());
            return;
        }
        for command_buffer in command_buffers {
            for task in command_buffer.tasks() {
                self.sender.send(task).expect("Failed to submit command buffers!");
            }
        }
        self.signal.clone().wake();
    }
}

pub(crate) struct WorkerThread {
    pub(crate) id: u64,
    pub(crate) sender: Sender<Task>,
    pub(crate) label: Option<String>,
    pub(crate) free_workers: Arc<AtomicU32>,
    #[cfg(feature = "main-thread")]
    pub(crate) end: Arc<AtomicU8>,
    pub(crate) signal: Arc<Signal>
}

impl WorkerThread {
    fn new(specs: MVSyncSpecs) -> Self {
        let signal = Arc::new(Signal::new());
        let (sender, receiver) = channel();
        let free_workers = Arc::new(AtomicU32::new(specs.workers_per_thread));
        let access = free_workers.clone();
        let signal_clone = signal.clone();
        #[cfg(feature = "main-thread")]
        let end = Arc::new(AtomicU8::new(0));
        #[cfg(feature = "main-thread")]
        let end_clone = end.clone();
        thread::spawn(move || Self::run(receiver, access, signal_clone, #[cfg(feature = "main-thread")] end_clone, #[cfg(feature = "main-thread")] false));
        WorkerThread {
            id: next_id("MVSync"),
            sender,
            label: None,
            free_workers,
            #[cfg(feature = "main-thread")]
            end,
            signal
        }
    }

    fn label(&mut self, label: String) {
        self.label = Some(label);
    }

    fn get_label(&self) -> Option<&String> {
        self.label.as_ref()
    }

    pub(crate) fn run(receiver: Receiver<Task>, free_workers: Arc<AtomicU32>, signal: Arc<Signal>, #[cfg(feature = "main-thread")] end: Arc<AtomicU8>,  #[cfg(feature = "main-thread")] end_when_done: bool) {
        let waker = Waker::from(signal.clone());
        let mut ctx = Context::from_waker(&waker);
        let mut futures = Vec::new();

        loop {
            if futures.is_empty() {
                #[cfg(feature = "main-thread")]
                if end.load(Ordering::Relaxed) > 0 || (end_when_done && receiver.is_empty()) {
                    break;
                }
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

            futures.retain_mut(|(state, (future, semaphores, to_signal))| {
                let mut p =  unsafe { Pin::new_unchecked(future) };
                match catch_unwind(AssertUnwindSafe(|| p.as_mut().poll(&mut ctx))) {
                    Ok(Poll::Pending) => {
                        if *state.read().unwrap() == TaskState::Cancelled {
                            for s in semaphores {
                                s.signal();
                            }
                            for signal in to_signal {
                                signal.clone().wake();
                            }
                            false
                        }
                        else {
                            true
                        }
                    },
                    Err(e) => {
                        state.write().unwrap().replace(TaskState::Panicked(e));
                        for s in semaphores {
                            s.signal();
                        }
                        for signal in to_signal {
                            signal.clone().wake();
                        }
                        false
                    }
                    Ok(Poll::Ready(_)) => {
                        free_workers.fetch_add(1, Ordering::SeqCst);
                        for s in semaphores {
                            s.signal();
                        }
                        for signal in to_signal {
                            signal.clone().wake();
                        }
                        false
                    }
                }
            });

            if !futures.is_empty() && receiver.is_empty() {
                signal.wait();
            }
        }
        #[cfg(feature = "main-thread")]
        end.store(2, Ordering::SeqCst);
    }

    pub(crate) fn send(&self, task: Task) {
        self.sender.send(task).expect("Failed to send task!");
        self.free_workers.fetch_sub(1, Ordering::SeqCst);
        self.signal.clone().wake();
    }

    #[cfg(feature = "main-thread")]
    pub(crate) fn end(&self) {
        self.end.store(1, Ordering::SeqCst);
    }

    #[cfg(feature = "main-thread")]
    pub(crate) fn ended(&self) -> bool {
        self.end.load(Ordering::Relaxed) > 0
    }

    #[cfg(feature = "main-thread")]
    pub(crate) fn finished(&self) -> bool {
        self.end.load(Ordering::Relaxed) > 1
    }

    fn free_workers(&self) -> u32 {
        self.free_workers.load(Ordering::Relaxed)
    }
}

id_eq!(Queue, WorkerThread);