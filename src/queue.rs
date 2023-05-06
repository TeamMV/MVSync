use mvutils::utils::next_id;

pub struct Queue {
    id: u64,
    threads: Vec<WorkerThread>
}

impl Queue {
    pub fn new(thread_count: u32, workers_per_thread: u32) -> Self {
        Queue {
            id: next_id("MVSync"),
            threads: (0..thread_count).map(|_| WorkerThread::new(workers_per_thread)).collect()
        }
    }
}

struct Worker {
    id: u64,
}

struct WorkerThread {
    id: u64,
    workers: Vec<Worker>,
}

impl WorkerThread {
    pub fn new(workers_per_thread: u32) -> Self {
        WorkerThread {
            id: next_id("MVSync"),
            workers: (0..workers_per_thread).map(|_| Worker { id: next_id("MVSync") }).collect()
        }
    }
}