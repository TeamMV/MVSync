use std::fmt::{Debug, Display};
use crate::command_buffers::buffer::{BufferedCommand, Command};
use crate::MVSynced;

pub trait End<T: MVSynced>: Command<T> {
    /// Ends the command chain by consuming the last return value and doing nothing with it. This
    /// can be used to keep values within the command chain private while still being able to share
    /// the [`TaskHandle<T>`] returned by the command chain.
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn end(self) -> BufferedCommand<()> {
        self.add_sync_command(|_| {})
    }
}

impl<T: MVSynced, C: Command<T>> End<T> for C {}

pub trait Print<T: MVSynced + Display>: Command<T> {
    /// Prints the result of the previous command `println!("{}", value);`, and returns the result
    /// so that more commands can be chained after this. This is essentially a method of inspecting
    /// the value midway through a command chain without consuming it or halting the command chain.
    fn print(self) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            println!("{}", t);
            t
        })
    }
}

impl<T: MVSynced + Display, C: Command<T>> Print<T> for C {}

pub trait Dbg<T: MVSynced + Debug>: Command<T> {
    /// Prints the result of the previous command as a debug value `println!("{:?}", value);`, and
    /// returns the result so that more commands can be chained after this. This is essentially a
    /// method of inspecting the value midway through a command chain without consuming it or
    /// halting the command chain.
    fn debug(self) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            println!("{:?}", t);
            t
        })
    }

    /// Executes the debug macro on the value returned by the previous command `dbg!(value);`, and
    /// returns the result so that more commands can be chained after this. This is essentially a
    /// method of inspecting the value midway through a command chain without consuming it or
    /// halting the command chain.
    fn dbg(self) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            let value = &t;
            dbg!(value);
            t
        })
    }
}

impl<T: MVSynced + Debug, C: Command<T>> Dbg<T> for C {}

pub trait Unwrap<T: MVSynced>: Command<Option<T>> {
    /// Unwraps the [`Option<T>`] value returned by the previous command, and returns the result if it
    /// was [`Some(T)`]. If the value was [`None`], it halts the entire command chain. This will lead to the
    /// final [`TaskHandle<T>`] retuning [`None`] as well.
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn unwrap(self) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap()
        })
    }

    /// Unwraps the [`Option<T>`] value returned by the previous command, and returns the result if it
    /// was [`Some(T)`]. If the value was [`None`], the default value provided is returned instead.
    fn unwrap_or(self, default: T) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap_or(default)
        })
    }

    /// Unwraps the [`Option<T>`] value returned by the previous command, and returns the result if it
    /// was [`Some(T)`]. If the value was [`None`], the default value provided is returned instead.
    fn unwrap_or_else(self, default: impl FnOnce() -> T + Send + 'static) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap_or_else(default)
        })
    }

    /// Unwraps the [`Option<T>`] value returned by the previous command, and returns the result if it
    /// was [`Some(T)`]. If the value was [`None`], it halts the entire command chain with a custom message.
    /// This will lead to the final [`TaskHandle<T>`] retuning [`None`] as well.
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn expect(self, msg: &str) -> BufferedCommand<T> {
        let msg = msg.to_string();
        self.add_sync_command(move |t| {
            t.expect(&msg)
        })
    }
}

impl<T: MVSynced, C: Command<Option<T>>> Unwrap<T> for C {}

pub trait UnwrapOk<T: MVSynced, E: MVSynced + Debug>: Command<Result<T, E>> {
    /// Unwraps the [`Result<T, E>`] value returned by the previous command, and returns the result if it
    /// was [`Ok(T)`]. If the value was [`Err(E)`], it halts the entire command chain. This will lead to the
    /// final [`TaskHandle<T>`] retuning [`None`].
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn unwrap(self) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap()
        })
    }

    /// Unwraps the [`Result<T, E>`] value returned by the previous command, and returns the result if it
    /// was [`Ok(T)`]. If the value was [`Err(E)`], the default value provided is returned instead.
    fn unwrap_or(self, default: T) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap_or(default)
        })
    }

    /// Unwraps the [`Result<T, E>`] value returned by the previous command, and returns the result if it
    /// was [`Ok(T)`]. If the value was [`Err(E)`], the default value provided is returned instead.
    fn unwrap_or_else(self, default: impl FnOnce(E) -> T + Send + 'static) -> BufferedCommand<T> {
        self.add_sync_command(|t| {
            t.unwrap_or_else(default)
        })
    }

    /// Unwraps the [`Result<T, E>`] value returned by the previous command, and returns the result if it
    /// was [`Ok(T)`]. If the value was [`Err(E)`], it halts the entire command chain with a custom message.
    /// This will lead to the final [`TaskHandle<T>`] retuning [`None`].
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn expect(self, msg: &str) -> BufferedCommand<T> {
        let msg = msg.to_string();
        self.add_sync_command(move |t| {
            t.expect(&msg)
        })
    }
}

impl<T: MVSynced, E: MVSynced + Debug, C: Command<Result<T, E>>> UnwrapOk<T, E> for C {}

pub trait UnwrapErr<T: MVSynced + Debug, E: MVSynced>: Command<Result<T, E>> {
    /// Unwraps the [`Result<T, E>`] error value returned by the previous command, and returns the result if it
    /// was [`Err(E)`]. If the value was [`Ok(T)`], it halts the entire command chain. This will lead to the
    /// final [`TaskHandle<T>`] retuning [`None`].
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn unwrap_err(self) -> BufferedCommand<E> {
        self.add_sync_command(|t| {
            t.unwrap_err()
        })
    }

    /// Unwraps the [`Result<T, E>`] error value returned by the previous command, and returns the result if it
    /// was [`Err(E)`]. If the value was [`Ok(T)`], it halts the entire command chain with a custom message.
    /// This will lead to the final [`TaskHandle<T>`] retuning [`None`].
    ///
    /// [`TaskHandle<T>`]: crate::task::TaskHandle
    fn expect_err(self, msg: &str) -> BufferedCommand<E> {
        let msg = msg.to_string();
        self.add_sync_command(move |t| {
            t.expect_err(&msg)
        })
    }
}

impl<T: MVSynced + Debug, E: MVSynced, C: Command<Result<T, E>>> UnwrapErr<T, E> for C {}
