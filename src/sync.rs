use std::ops::Deref;
use std::sync::Arc;
use std::task::Wake;
use std::thread;
use std::time::Duration;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::block::Signal;

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

    pub(crate) fn signal(&self) {
        unsafe {
            (self as *const Semaphore).cast_mut().as_mut().unwrap().signaled = true;
        }
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
    signalled: Option<Arc<Signal>>
}

impl Fence {
    pub(crate) fn new() -> Self {
        Fence {
            id: next_id("MVSync"),
            signalled: None
        }
    }

    pub(crate) fn bind(&self, signal: Arc<Signal>) {
        unsafe {
            (self as *const Fence).cast_mut().as_mut().unwrap().signalled.replace(signal);
        }
    }

    /// Check if the fence is open.
    ///
    /// If this returns `true`, the task that signalled the fence has already finished.
    ///
    /// # Note
    /// If a fence is bound to multiple tasks, it will open as soon as the first task finishes.
    /// Counter fences are planned to be added in the future.
    pub fn ready(&self) -> bool {
        self.signalled.as_ref().expect("Checking unbound fence!").ready()
    }

    /// Block the current thread until the fence is signaled, indicating that the task
    /// this fence is bound to has finished.
    ///
    /// # Note
    /// If a fence is bound to multiple tasks, it will open as soon as the first task finishes.
    /// Counter fences are planned to be added in the future.
    pub fn wait(&self) {
        self.signalled.clone().expect("Checking unbound fence!").wait()
    }
}

id_eq!(Semaphore, Fence);

pub enum SemaphoreUsage {
    /// Wait for the semaphore to be signaled.
    Wait,
    /// Signal the semaphore upon completion.
    Signal
}