use std::ops::Deref;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use mvutils::id_eq;
use mvutils::utils::next_id;

/// A sync object used to add control flow to tasks. It is an internal sync object, meaning it can
/// only be read from within a task. If a task depends on another one to finish, you should use a
/// Semaphore to force the task to wait for the dependency to finish.
///
/// # Note
/// If a semaphore is bound to multiple tasks, it will signal as soon as the first task finishes.
/// Counter semaphores are planned to be added in the future.
pub struct Semaphore {
    id: u64,
    signaled: bool
}

impl Semaphore {
    pub(crate) fn new() -> Semaphore {
        Semaphore {
            id: next_id("MVSync"),
            signaled: false
        }
    }

    pub(crate) fn signal(&mut self) {
        self.signaled = true;
    }

    pub(crate) fn ready(&self) -> bool {
        self.signaled
    }
}

/// A sync object used to wait for tasks. It is an external sync object, meaning it can
/// only be read from outside tasks. If you need to await a task whose result was passed into
/// another task, or you need to share a task "handle" without sharing the result, you should
/// use a Fence.
///
/// # Note
/// If a fence is bound to multiple tasks, it will open as soon as the first task finishes.
/// Counter fences are planned to be added in the future.
pub struct Fence {
    id: u64,
    timeout: u32,
    open: bool
}

impl Fence {
    pub(crate) fn new(timeout: u32) -> Self {
        Fence {
            id: next_id("MVSync"),
            timeout,
            open: false,
        }
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
    }

    /// Check if the fence is open.
    ///
    /// If this returns `true`, the task that signalled the fence has already finished.
    ///
    /// # Note
    /// If a fence is bound to multiple tasks, it will open as soon as the first task finishes.
    /// Counter fences are planned to be added in the future.
    pub fn ready(&self) -> bool {
        self.open
    }

    /// Block the current thread until the fence is signaled, indicating that the task
    /// this fence is bound to has finished.
    ///
    /// # Note
    /// If a fence is bound to multiple tasks, it will open as soon as the first task finishes.
    /// Counter fences are planned to be added in the future.
    pub fn wait(&self) {
        loop {
            if self.open {
                break;
            }
            thread::sleep(Duration::from_millis(self.timeout as u64));
        }
    }
}

id_eq!(Semaphore, Fence);

pub(crate) enum Signal {
    Semaphore(Arc<Semaphore>),
    Fence(Arc<Fence>)
}

impl Signal {
    pub(crate) fn signal(self) {
        match self {
            Signal::Semaphore(s) => unsafe {
                (s.deref() as *const Semaphore).cast_mut().as_mut().unwrap().signal()
            }
            Signal::Fence(s) => unsafe {
                (s.deref() as *const Fence).cast_mut().as_mut().unwrap().open()
            }
        }
    }
}

pub enum SemaphoreUsage {
    /// Wait for the semaphore to be signaled.
    Wait,
    /// Signal the semaphore upon completion.
    Signal
}