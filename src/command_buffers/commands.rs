use std::fmt::Display;
use crate::command_buffers::buffer::{BufferedCommand, Command};
use crate::MVSynced;

pub trait Print<T: MVSynced + Display>: Command<T> {
    fn print(self) -> BufferedCommand<()>;
}

impl<T: MVSynced + Display, C: Command<T>> Print<T> for C {
    fn print(self) -> BufferedCommand<()> {
        self.add_command(|t| async move {
            println!("{}", t);
        })
    }
}