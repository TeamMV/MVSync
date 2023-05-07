use std::fmt::Display;
use crate::command_buffers::buffer::Command;
use crate::command_buffers::types::EndCommand;
use crate::MVSynced;

pub trait Print<T: MVSynced + Display>: Command<T> {
    fn print(self) -> EndCommand;
}

impl<T: MVSynced + Display, C: Command<T>> Print<T> for C {
    fn print(self) -> EndCommand {
        self.add_command(|t| async move {
            println!("{}", t);
        })
    }
}