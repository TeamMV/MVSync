use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::sync::{Arc, RwLock};
use mvutils::{id_eq, sealable};
use mvutils::utils::next_id;
use crate::MVSynced;
use crate::sync::{Fence, Semaphore, SemaphoreUsage};
use crate::task::{Task, TaskController, TaskHandle, TaskState};

sealable!();

struct TaskChain {
    id: u64,
    tasks: Vec<Task>,
    controllers: Vec<TaskController>
}

id_eq!(TaskChain);

impl TaskChain {
    fn new() -> TaskChain {
        TaskChain {
            id: next_id("MVSync"),
            tasks: Vec::new(),
            controllers: Vec::new()
        }
    }

    fn add_task(&mut self, task: Task, controller: TaskController) {
        self.tasks.push(task);
        self.controllers.push(controller);
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

    fn get_controllers(&mut self) -> Vec<TaskController> {
        self.controllers.drain(..).collect()
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
    fn add_sync_task<T: MVSynced>(&mut self, function: impl FnOnce() -> T + Send + 'static) -> TaskHandle<T> {
        if !self.tasks[self.group].tasks.is_empty() {
            self.group += 1;
            self.tasks.push(TaskChain::new());
        }
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let result = TaskHandle::new(buffer.clone(), state.clone(), self.timeout);
        let task = Task::from_function(function, buffer, state);
        self.tasks[self.group].add_task(task, result.make_controller());
        result
    }

    fn add_task<T: MVSynced, F: Future<Output = T> + Send>(&mut self, function: impl FnOnce() -> F + Send + 'static) -> TaskHandle<T> {
        if !self.tasks[self.group].tasks.is_empty() {
            self.group += 1;
            self.tasks.push(TaskChain::new());
        }
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let result = TaskHandle::new(buffer.clone(), state.clone(), self.timeout);
        let task = Task::from_async(function, buffer, state);
        self.tasks[self.group].add_task(task, result.make_controller());
        result
    }

    fn add_chained_task<T: MVSynced, R: MVSynced, F: Future<Output = R> + Send>(&mut self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskHandle<T>) -> TaskHandle<R> {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let result = TaskHandle::new(buffer.clone(), state.clone(), self.timeout);
        let task = Task::from_async_continuation(function, buffer, state, predecessor);
        self.tasks[self.group].add_task(task, result.make_controller());
        result
    }

    fn add_sync_chained_task<T: MVSynced, R: MVSynced>(&mut self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskHandle<T>) -> TaskHandle<R> {
        let buffer = Arc::new(RwLock::new(None));
        let state = Arc::new(RwLock::new(TaskState::Pending));
        let result = TaskHandle::new(buffer.clone(), state.clone(), self.timeout);
        let task = Task::from_continuation(function, buffer, state, predecessor);
        self.tasks[self.group].add_task(task, result.make_controller());
        result
    }

    fn get_controllers(&mut self) -> Vec<TaskController> {
        self.tasks[self.group].get_controllers()
    }

    fn finish(&self) -> Vec<Task> {
        unsafe {
            let this = (self as *const RawCommandBuffer).cast_mut().as_mut().unwrap();
            for (i, fence) in this.fences.drain(..) {
                if i < this.tasks.len() {
                    this.tasks[i].ending_fence(fence);
                }
                else {
                    panic!("Fence index '{}' out of bounds", i);
                }
            }
            for (e, s) in this.chain_links.drain(..) {
                if e < this.tasks.len() && s < this.tasks.len() {
                    let semaphore = Arc::new(Semaphore::new());
                    this.tasks[e].ending_semaphore(semaphore.clone());
                    this.tasks[s].starting_semaphore(semaphore);
                }
                else {
                    panic!("Semaphore index '{}' or '{}' out of bounds", s, e);
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

    fn chain_task<T: MVSynced, R: MVSynced, F: Future<Output=R> + Send>(&self, function: impl FnOnce(T) -> F + Send + 'static, predecessor: TaskHandle<T>) -> TaskHandle<R> {
        unsafe {
            self.ptr.as_mut().unwrap().add_chained_task(function, predecessor)
        }
    }

    fn chain_sync_task<T: MVSynced, R: MVSynced>(&self, function: impl FnOnce(T) -> R + Send + 'static, predecessor: TaskHandle<T>) -> TaskHandle<R> {
        unsafe {
            self.ptr.as_mut().unwrap().add_sync_chained_task(function, predecessor)
        }
    }

    fn get_controllers(&self) -> Vec<TaskController> {
        unsafe {
            self.ptr.as_mut().unwrap().get_controllers()
        }
    }

    pub(crate) fn tasks(self) -> Vec<Task> {
        unsafe {
            let this = (&self as *const CommandBuffer).cast_mut().as_mut().unwrap();
            this.baked.take().expect("Command buffer has now been baked!")
        }
    }

    /// Bake the command buffer. This will save all the commands you have called. You must call this
    /// before submitting commands to a queue.
    ///
    /// The command buffer will become read-only after this function is called.
    ///
    /// # Panics
    /// If more than one copy of the command buffer exists.
    pub fn finish(&self) {
        if self.baked.is_some() {
            panic!("Command buffer has already been baked!");
        }
        unsafe {
            if *self.refs != 1 {
                panic!("You cannot bake a command buffer that has other living references!");
            }
            let this = (self as *const CommandBuffer).cast_mut().as_mut().unwrap();
            this.baked = Some(this.ptr.as_mut().unwrap().finish());
        }
    }

    /// Add a [`Semaphore`] to link two task chains. This will not append the semaphore right now, it will
    /// append it once the command buffer is baked. So you can add more chains, or more commands to a chain,
    /// and it will be added to the last command and first command in the chain.
    ///
    /// The index works just like an array, since when you create a task chain, it is added to an array.
    ///
    /// # Parameters
    /// - dependency: The index to the task chain which should signal the semaphore.
    /// - successor: The index to the task chain which should wait for the semaphore.
    ///
    /// # Panics
    /// This will panic if the index for the task chain is out of bounds when you bake the buffer,
    /// because if an unlinked semaphore exists, it will never execute the function.
    pub fn link_chains(&self, dependency: usize, successor: usize) {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        unsafe {
            self.ptr.as_mut().unwrap().chain_links.push((dependency, successor));
        }
    }

    /// Add a [`Fence`] to the end of a task chain. This will not append the fence right now, it will
    /// append it once the command buffer is baked. So you can add more chains, or more commands to a chain,
    /// and it will be added to the last command in the chain.
    ///
    /// # Parameters
    /// - task_chain: The index to the task chain. It works just like an array, since when you create a task
    /// chain, it is added to an array.
    ///
    /// # Panics
    /// This will panic if the index for the task chain is out of bounds when you bake the buffer,
    /// because if you try to wait for the fence it will block forever.
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
        /// Add a command and continue the command chain, returning the result of the command wrapped in
        /// a buffered command. Do not call this function directly unless you are defining you own commands.
        /// This is the same as [`add_command`], however it takes in a synchronous function instead.
        fn add_sync_command<T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> BufferedCommand<T>;

        /// Add a command and continue the command chain, returning the result of the command wrapped in
        /// a buffered command. Do not call this function directly unless you are defining you own commands.
        fn add_command<T: MVSynced, F: Future<Output = T> + Send>(&self, function: impl FnOnce() -> F + Send + 'static) -> BufferedCommand<T>;
    }
);

impl CommandBufferEntry for CommandBuffer {
    fn add_sync_command<T: MVSynced>(&self, function: impl FnOnce() -> T + Send + 'static) -> BufferedCommand<T> {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        BufferedCommand::new(self.clone(), unsafe {
            self.ptr.as_mut().unwrap().add_sync_task(function)
        })
    }

    fn add_command<T: MVSynced, F: Future<Output = T> + Send>(&self, function: impl FnOnce() -> F + Send + 'static) -> BufferedCommand<T> {
        if self.baked.is_some() {
            panic!("You cannot modify a baked command buffer!");
        }
        BufferedCommand::new(self.clone(), unsafe {
            self.ptr.as_mut().unwrap().add_task(function)
        })
    }
}

/// The trait that defines a buffered command. To add custom commands that take in a type [`T`],
/// you need to define a trait which extends this trait:
/// ```
/// use mvsync::prelude::*;
///
/// struct MyType;
///
/// struct ReturnType;
///
/// //Define our trait with our custom command.
/// pub trait CustomFunction: Command<MyType> {
///     fn custom_function(self) -> BufferedCommand<ReturnType> {
///         self.add_command(|t: MyType| async move {
///             ReturnType
///         })
///     }
/// }
///
/// //Implement it for all buffered commands.
/// impl<T: Command<MyType>> CustomFunction for T {}
/// ```
///
/// Now, if some other command returns your type [`T`], as a [`BufferedCommand<T>`], you can call
/// your custom function from the buffered command:
///
/// ```
/// use std::sync::Arc;
/// use mvsync::prelude::*;
///
/// let sync = MVSync::new(MVSyncSpecs::default());
/// let queue: Arc<Queue> = sync.get_queue();
/// let command_buffer: CommandBuffer = sync.allocate_command_buffer().unwrap();
///
/// let result: TaskHandle<ReturnType> = command_buffer
///     .some_command() //Assume this adds a function that returns `MyType`, wrapped in a `BufferedCommand<MyType>`
///     .custom_function() //We can call our custom command.
///     .result(); //We get the result of our custom command.
///
/// command_buffer.finish();
///
/// queue.submit_command_buffer(command_buffer); //We can submit this command buffer now.
///
/// let ret: ReturnType = result.wait(); //We wait for our command chain to finish.
/// ```
pub trait Command<T: MVSynced>: Sized + Sealed {
    /// Get the parent command buffer, which is where the raw tasks are stored. Do not call this
    /// function directly unless you know what you are doing. It is used by the other methods in
    /// [`Command<T>`]
    fn parent(&self) -> &CommandBuffer;

    /// Get the task result, ending this command chain.
    fn response(self) -> (TaskHandle<T>, Vec<TaskController>);

    /// Add a command and continue the command chain, returning the result of the command wrapped in
    /// a buffered command. Do not call this function directly unless you are defining you own commands.
    fn add_command<R: MVSynced, F: Future<Output = R> + Send>(self, function: impl FnOnce(T) -> F + Send + 'static) -> BufferedCommand<R>;

    /// Add a command and continue the command chain, returning the result of the command wrapped in
    /// a buffered command. Do not call this function directly unless you are defining you own commands.
    /// This is the same as [`add_command`], however it takes in a synchronous function instead.
    fn add_sync_command<R: MVSynced>(self, function: impl FnOnce(T) -> R + Send + 'static) -> BufferedCommand<R>;
}

/// A buffered command. This is returned when you add a command to a command buffer, and can be
/// used to chain more commands together based on their return types, or get the return value as a
/// [`TaskHandle<T>`].
pub struct BufferedCommand<T: MVSynced> {
    parent: CommandBuffer,
    response: TaskHandle<T>,
}

impl<T: MVSynced> Sealed for BufferedCommand<T> {}

impl<T: MVSynced> BufferedCommand<T> {
    fn new(parent: CommandBuffer, response: TaskHandle<T>) -> Self {
        BufferedCommand {
            parent,
            response
        }
    }
}

impl<T: MVSynced> Command<T> for BufferedCommand<T> {
    fn parent(&self) -> &CommandBuffer {
        &self.parent
    }

    fn response(self) -> (TaskHandle<T>, Vec<TaskController>) {
        let controllers = self.parent.get_controllers();
        (self.response, controllers)
    }

    fn add_command<R: MVSynced, F: Future<Output = R> + Send>(self, function: impl FnOnce(T) -> F + Send + 'static) -> BufferedCommand<R> {
        let parent = self.parent().clone();
        let response = parent.chain_task(function, self.response);
        BufferedCommand::new(parent, response)
    }

    fn add_sync_command<R: MVSynced>(self, function: impl FnOnce(T) -> R + Send + 'static) -> BufferedCommand<R> {
        let parent = self.parent().clone();
        let response = parent.chain_sync_task(function, self.response);
        BufferedCommand::new(parent, response)
    }
}