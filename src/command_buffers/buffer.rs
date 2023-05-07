use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::sync::{Arc, RwLock};
use mvutils::{id_eq, sealable};
use mvutils::utils::next_id;
use crate::MVSynced;
use crate::sync::{Fence, Semaphore, SemaphoreUsage};
use crate::task::{Task, TaskResult};

sealable!();

struct TaskChain {
    id: u64,
    tasks: Vec<Task>
}

id_eq!(TaskChain);

impl TaskChain {
    fn new() -> TaskChain {
        TaskChain {
            id: next_id("MVSync"),
            tasks: Vec::new()
        }
    }

    fn add_task(&mut self, task: Task) {
        self.tasks.push(task);
    }

    fn starting_semaphore(&mut self, semaphore: Arc<Semaphore>) {
        if self.tasks.is_empty() {
            return;
        }
        self.tasks[0].bind_semaphore(semaphore, SemaphoreUsage::Wait);
    }

    fn ending_semaphore(&mut self, semaphore: Arc<Semaphore>) {
        if self.tasks.is_empty() {
            return;
        }
        let i = self.tasks.len() - 1;
        self.tasks[i].bind_semaphore(semaphore, SemaphoreUsage::Signal);
    }

    fn ending_fence(&mut self, fence: Arc<Fence>) {
        if self.tasks.is_empty() {
            return;
        }
        let i = self.tasks.len() - 1;
        self.tasks[i].bind_fence(fence);
    }

    fn chain_semaphores(&mut self) -> Vec<Task> {
        for i in 1..self.tasks.len() {
            let semaphore = Arc::new(Semaphore::new());
            self.tasks[i - 1].bind_semaphore(semaphore.clone(), SemaphoreUsage::Signal);
            self.tasks[i].bind_semaphore(semaphore.clone(), SemaphoreUsage::Wait);
        }
        self.tasks.drain(..).collect()
    }
}

struct RawCommandBuffer {
    tasks: Vec<TaskChain>,
    chain_links: Vec<(usize, usize)>,
    fences: Vec<(usize, Arc<Fence>)>,
    group: usize,
    timeout: u32
}

impl RawCommandBuffer {
    fn add_sync_task<T: MVSynced>(&mut self, function: impl FnOnce() -> T + Send + 'static) -> TaskResult<T> {
        if !self.tasks[self.group].tasks.is_empty() {
            self.group += 1;
            self.tasks.push(TaskChain::new());
        }
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.timeout);
        let task = Task::from_function(function, buffer);
        self.tasks[self.group].add_task(task);
        result
    }

    fn add_task<T: MVSynced, F: Future<Output = T>>(&mut self, function: impl FnOnce() -> F + Send + 'static) -> TaskResult<T> {
        if !self.tasks[self.group].tasks.is_empty() {
            self.group += 1;
            self.tasks.push(TaskChain::new());
        }
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.timeout);
        let task = Task::from_async(function, buffer);
        self.tasks[self.group].add_task(task);
        result
    }

    fn add_chained_task<T: MVSynced, R: MVSynced, F: Future<Output = R>>(&mut self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskResult<T>) -> TaskResult<R> {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.timeout);
        let task = Task::from_async_continuation(function, buffer, predecessor);
        self.tasks[self.group].add_task(task);
        result
    }

    fn add_sync_chained_task<T: MVSynced, R: MVSynced>(&mut self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskResult<T>) -> TaskResult<R> {
        let buffer = Arc::new(RwLock::new(None));
        let result = TaskResult::new(buffer.clone(), self.timeout);
        let task = Task::from_continuation(function, buffer, predecessor);
        self.tasks[self.group].add_task(task);
        result
    }

    fn finish(&self) -> Vec<Task> {
        unsafe {
            let this = (self as *const RawCommandBuffer).cast_mut().as_mut().unwrap();
            for (i, fence) in this.fences.drain(..) {
                if i < this.tasks.len() {
                    this.tasks[i].ending_fence(fence);
                }
            }
            for (e, s) in this.chain_links.drain(..) {
                if e < this.tasks.len() && s < this.tasks.len() {
                    let semaphore = Arc::new(Semaphore::new());
                    this.tasks[e].ending_semaphore(semaphore.clone());
                    this.tasks[s].starting_semaphore(semaphore);
                }
            }
            this.tasks.drain(..).flat_map(|mut chain| chain.chain_semaphores()).collect()
        }
    }
}

pub struct CommandBuffer {
    id: u64,
    ptr: *mut RawCommandBuffer,
    refs: *mut usize,
    baked: Option<Vec<Task>>,
}

seal!(CommandBuffer);

pub struct CommandBufferAllocationError;

impl Debug for CommandBufferAllocationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandBufferAllocationError: \"Failed to allocate command buffer!\"")
    }
}

impl Display for CommandBufferAllocationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failed to allocate command buffer!")
    }
}

impl Error for CommandBufferAllocationError {}

impl CommandBuffer {
    pub(crate) fn new(timeout: u32) -> Result<CommandBuffer, CommandBufferAllocationError> {
        unsafe {
            let ptr = alloc_zeroed(Layout::new::<RawCommandBuffer>()) as *mut RawCommandBuffer;
            let refs = alloc_zeroed(Layout::new::<usize>()) as *mut usize;
            if ptr.is_null() || refs.is_null() {
                return Err(CommandBufferAllocationError);
            }

            ptr.write(RawCommandBuffer {
                tasks: vec![TaskChain::new()],
                chain_links: Vec::new(),
                fences: Vec::new(),
                group: 0,
                timeout
            });
            refs.write(1);

            Ok(
                CommandBuffer {
                    id: next_id("MVSync"),
                    ptr,
                    refs,
                    baked: None
                }
            )
        }
    }

    fn chain_task<T: MVSynced, R: MVSynced, F: Future<Output=R>>(&self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskResult<T>) -> TaskResult<R> {
        unsafe {
            self.ptr.as_mut().unwrap().add_chained_task(function, predecessor)
        }
    }

    fn chain_sync_task<T: MVSynced, R: MVSynced>(&self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskResult<T>) -> TaskResult<R> {
        unsafe {
            self.ptr.as_mut().unwrap().add_sync_chained_task(function, predecessor)
        }
    }

    pub(crate) fn tasks(self) -> Vec<Task> {
        unsafe {
            let this = (&self as *const CommandBuffer).cast_mut().as_mut().unwrap();
            this.baked.take().expect("Command buffer has now been baked!")
        }
    }

    pub fn finish(&self) {
        unsafe {
            if *self.refs != 1 {
                panic!("You cannot bake a command buffer that has other living references!");
            }
            let this = (self as *const CommandBuffer).cast_mut().as_mut().unwrap();
            this.baked = Some(this.ptr.as_mut().unwrap().finish());
        }
    }

    pub fn link_chains(&self, dependency: usize, successor: usize) {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        unsafe {
            self.ptr.as_mut().unwrap().chain_links.push((dependency, successor));
        }
    }

    pub fn create_fence(&self, task_chain: usize) -> Arc<Fence> {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        unsafe {
            let raw = self.ptr.as_mut().unwrap();
            let fence = Arc::new(Fence::new(raw.timeout));
            raw.fences.push((task_chain, fence.clone()));
            fence
        }
    }
}

impl Clone for CommandBuffer {
    fn clone(&self) -> CommandBuffer {
        if self.baked.is_some() {
            panic!("You cannot clone a baked command buffer!");
        }
        unsafe {
            self.refs.write(self.refs.read() + 1);
            CommandBuffer {
                id: self.id,
                ptr: self.ptr,
                refs: self.refs,
                baked: None
            }
        }
    }
}

impl Drop for CommandBuffer {
    fn drop(&mut self) {
        unsafe {
            self.refs.write(self.refs.read() - 1);
            if self.refs.read() == 0 {
                dealloc(self.ptr as *mut u8, Layout::new::<RawCommandBuffer>());
                dealloc(self.refs as *mut u8, Layout::new::<usize>());
            }
        }
    }
}

id_eq!(CommandBuffer);

sealed!(
    pub trait CommandBufferEntry {
        fn add_sync_command<C: Command<T>, T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> C;
        fn add_command<C: Command<T>, T: MVSynced, F: Future<Output = T>>(&self, function: impl FnOnce() -> F + Send + 'static) -> C;
    }
);

impl CommandBufferEntry for CommandBuffer {
    fn add_sync_command<C: Command<T>, T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> C {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        C::new(self.clone(), unsafe {
            self.ptr.as_mut().unwrap().add_sync_task(function)
        })
    }

    fn add_command<C: Command<T>, T: MVSynced, F: Future<Output = T>>(&self, function: impl FnOnce() -> F + Send + 'static) -> C {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        C::new(self.clone(), unsafe {
            self.ptr.as_mut().unwrap().add_task(function)
        })
    }
}

pub trait Command<T: MVSynced>: Sized {
    fn new(parent: CommandBuffer, response: TaskResult<T>) -> Self;

    fn parent(&self) -> &CommandBuffer;

    fn response(self) -> TaskResult<T>;

    fn add_command<C: Command<R>, R: MVSynced, F: Future<Output = R>>(self, function: impl FnOnce(T) -> F + Send + 'static) -> C {
        let parent = self.parent().clone();
        let response = parent.chain_task(function, self.response());
        C::new(parent, response)
    }

    fn add_sync_command<C: Command<R>, R: MVSynced>(self, function: impl FnOnce(T) -> R + Send + 'static) -> C {
        let parent = self.parent().clone();
        let response = parent.chain_sync_task(function, self.response());
        C::new(parent, response)
    }
}