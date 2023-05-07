use std::future::Future;
use crate::command_buffers::buffer::{Command, CommandBuffer};
use crate::MVSynced;
use crate::task::TaskResult;

pub struct EndCommand {
    parent: CommandBuffer,
    response: TaskResult<()>
}

impl Command<()> for EndCommand {
    fn new(parent: CommandBuffer, response: TaskResult<()>) -> Self {
        EndCommand {
            parent,
            response
        }
    }

    fn parent(&self) -> &CommandBuffer {
        &self.parent
    }

    fn response(self) -> TaskResult<()> {
        self.response
    }

    fn add_command<C: Command<R>, R: MVSynced, F: Future<Output = R>>(self, _: impl FnOnce(()) -> F + Send + 'static) -> C {
        panic!("You can't chain commands past an EndCommand!");
    }
}