use std::ops::Deref;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use mvutils::id_eq;
use mvutils::utils::next_id;

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

    pub fn ready(&self) -> bool {
        self.open
    }

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
    Wait,
    Signal
}