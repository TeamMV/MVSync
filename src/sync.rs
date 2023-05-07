use std::thread;
use std::time::Duration;
use mvutils::utils::next_id;

pub struct Semaphore {
    id: u64,
    timeout: u32,
    signaled: bool
}

impl Semaphore {
    pub(crate) fn new(timeout: u32) -> Semaphore {
        Semaphore {
            id: next_id("MVSync"),
            timeout,
            signaled: false
        }
    }

    pub(crate) fn signal(&mut self) {
        self.signaled = true;
    }

    pub(crate) fn ready(&self) -> bool {
        self.signaled
    }

    pub(crate) fn wait(&self) {
        loop {
            if self.signaled {
                break;
            }
            thread::sleep(Duration::from_millis(self.timeout as u64));
        }
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

pub(crate) enum Signal {
    Semaphore(Semaphore),
    Fence(Fence)
}

impl Signal {
    pub(crate) fn signal(&self) {
        unsafe {
            (self as *const Signal).cast_mut().as_mut().unwrap().inner_signal();
        }
    }

    fn inner_signal(&mut self) {
        match self {
            Signal::Semaphore(s) => s.signal(),
            Signal::Fence(s) => s.open()
        }
    }
}