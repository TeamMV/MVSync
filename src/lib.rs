use std::sync::Arc;
use mvutils::id_eq;
use mvutils::utils::next_id;
use crate::queue::Queue;

pub mod queue;
pub mod task;
pub mod sync;

pub trait MVSynced: Send + Sync + 'static {}
impl<T> MVSynced for T where T: Send + Sync + 'static {}

pub struct MVSync {
    id: u64,
    specs: MVSyncSpecs,
    queue: Arc<Queue>
}

impl MVSync {
    pub fn new(specs: MVSyncSpecs) -> MVSync {
        next_id("MVSync");
        MVSync {
            id: next_id("MVSync"),
            specs,
            queue: Arc::new(Queue::new(specs))
        }
    }

    pub fn get_queue(&self) -> Arc<Queue> {
        self.queue.clone()
    }
}

id_eq!(MVSync);

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct MVSyncSpecs {
    pub thread_count: u32,
    pub workers_per_thread: u32,
    pub timeout_ms: u32,
}

impl Default for MVSyncSpecs {
    fn default() -> Self {
        MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 1,
            timeout_ms: 10
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;
    use futures_timer::Delay;
    use crate::{MVSync, MVSyncSpecs};

    #[test]
    fn it_works() {
        let sync = MVSync::new(MVSyncSpecs {
            thread_count: 1,
            workers_per_thread: 2,
            timeout_ms: 10,
        });
        let queue = sync.get_queue();

        queue.push_async(async {
            Delay::new(Duration::from_millis(1000)).await;
            println!("a");
        });
        queue.push_async(async {
            Delay::new(Duration::from_millis(500)).await;
            println!("b");
        });

        thread::sleep(Duration::from_millis(1100));
    }
}
