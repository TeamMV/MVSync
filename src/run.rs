use std::task::Wake;
use crate::block::AwaitSync;
use crate::task::{Task, TaskState};

pub(crate) fn run(task: Task) {
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
    }
    else if task.can_execute() {
        let (task, after, signal) = task.execute();
        task.await_sync();
        for semaphore in after {
            semaphore.signal();
        }
        for signal in signal {
            signal.wake();
        }
    }
}

pub(crate) fn run_all(mut tasks: Vec<Task>) {
    loop {
        let mut new_tasks = Vec::new();
        let mut ran = false;
        let mut count = 0;

        for task in tasks {
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
                ran = true;
                count = 0;
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
                ran = true;
                count = 0;
            }
            else if task.can_execute() {
                let (task, after, signal) = task.execute();
                task.await_sync();
                for semaphore in after {
                    semaphore.signal();
                }
                for signal in signal {
                    signal.wake();
                }
                ran = true;
                count = 0;
            }
            else {
                new_tasks.push(task);
            }
        }

        if !ran {
            count += 1;
            if count >= 5 {
                break;
            }
        }

        tasks = new_tasks;
    }
}