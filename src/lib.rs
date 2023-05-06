use std::sync::Arc;
use mvutils::utils::next_id;
use crate::queue::Queue;

pub mod queue;

pub trait MVSynced: Send + Sync + 'static {}
impl<T> MVSynced for T where T: Send + Sync + 'static {}

pub struct MVSync {
    id: u64,
    specs: MVSyncSpecs,
    queue: Arc<Queue>
}

impl MVSync {
    pub fn new(specs: MVSyncSpecs) -> MVSync {
        MVSync {
            id: next_id("MVSync"),
            specs,
            queue: Arc::new(Queue::new(specs.thread_count, specs.workers_per_thread))
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct MVSyncSpecs {
    pub thread_count: u32,
    pub workers_per_thread: u32
}

impl Default for MVSyncSpecs {
    fn default() -> Self {
        MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 5
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn it_works() {

    }
}
