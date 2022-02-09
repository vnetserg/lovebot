use std::sync::{Arc, LockResult, RwLock, RwLockReadGuard, RwLockWriteGuard};

use lazy_static::lazy_static;
use rand::{prelude::SliceRandom, thread_rng};

////////////////////////////////////////////////////////////////////////////////

pub const START_MESSAGE: &str = "\
Hello! This is anonymous chatting bot. Quick start guide:

* Use command `/send @sergio_4min Hello!` to send an anonymous message to a particular user;
* Use command `/random Hello!` to send an anonymous message to a random user.
* Use command `/users` to list all available users.

For more commands, use `/help`.";

pub const HELP_MESSAGE: &str = "\
Available commands:
* `/send [receiver] [message]` - send a message. Receiver can either be a @username or a #thread.
* `/random [message]` - send a message to a random user.
* `/users` - list available users.
* `/threads` - list active anonymous threads.
* `/help` - show this message.

Hints:
* You can reply to a message instead of using `/send` command.

Should you need any further assistance, write to @sergio_4min.";

////////////////////////////////////////////////////////////////////////////////

#[derive(Default)]
pub struct Writer<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> Writer<T> {
    #[allow(dead_code)]
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(value)),
        }
    }

    pub fn reader(&self) -> Reader<T> {
        Reader {
            inner: self.inner.clone(),
        }
    }

    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        self.inner.write()
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Reader<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> Reader<T> {
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        self.inner.read()
    }
}

////////////////////////////////////////////////////////////////////////////////

pub fn random_noun() -> &'static str {
    lazy_static! {
        static ref NOUNS: Vec<String> = {
            let data = include_str!("../data/nouns.txt");
            data.split("\n").map(|s| s.to_string()).collect()
        };
    }
    NOUNS.choose(&mut thread_rng()).unwrap()
}

pub fn random_adjective() -> &'static str {
    lazy_static! {
        static ref ADJECTIVES: Vec<String> = {
            let data = include_str!("../data/adjectives.txt");
            data.split("\n").map(|s| s.to_string()).collect()
        };
    }
    ADJECTIVES.choose(&mut thread_rng()).unwrap()
}
