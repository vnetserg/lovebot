use crate::{command_dispatcher::UserHandle, util::Reader, Command};

use anyhow::{bail, ensure, Context, Result};
use log::*;
use teloxide::{adaptors::AutoSend, prelude::Requester, Bot};
use tokio::{
    select,
    sync::{mpsc, oneshot},
};

use std::collections::HashMap;

////////////////////////////////////////////////////////////////////////////////

pub struct CommandRequest {
    pub command: Command,
    pub result_sender: oneshot::Sender<Result<()>>,
}

////////////////////////////////////////////////////////////////////////////////

pub struct ActionRequest {
    action: Action,
    result_sender: oneshot::Sender<Result<()>>,
}

enum Action {
    StartAnonymousThread(Thread),
    RelayMessage(ThreadId, String),
}

////////////////////////////////////////////////////////////////////////////////

type ThreadId = String;

struct Thread {
    id: ThreadId,
    other_id: ThreadId,
    other_handle: UserHandle,
}

impl Thread {
    async fn send_message(&self, message: String) -> Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();
        let relay_request = ActionRequest {
            action: Action::RelayMessage(self.other_id.clone(), message),
            result_sender,
        };
        self.other_handle
            .channel
            .send(relay_request)
            .await
            .unwrap_or_else(|err| panic!("failed to send relay request: {}", err));
        result_receiver
            .await
            .unwrap_or_else(|err| panic!("failed to get relay result: {}", err))
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Handler {
    user_handle: UserHandle,
    bot: AutoSend<Bot>,
    chat_id: i64,
    handle_registry: Reader<HashMap<String, UserHandle>>,
    command_receiver: mpsc::Receiver<CommandRequest>,
    action_receiver: mpsc::Receiver<ActionRequest>,
    threads: HashMap<ThreadId, Thread>,
}

impl Handler {
    pub fn new(
        user_handle: UserHandle,
        bot: AutoSend<Bot>,
        chat_id: i64,
        user_handles: Reader<HashMap<String, UserHandle>>,
        command_receiver: mpsc::Receiver<CommandRequest>,
        message_receiver: mpsc::Receiver<ActionRequest>,
    ) -> Self {
        Self {
            user_handle,
            bot,
            chat_id,
            handle_registry: user_handles,
            command_receiver,
            action_receiver: message_receiver,
            threads: HashMap::new(),
        }
    }

    pub async fn run(&mut self) {
        loop {
            select! {
                mb_request = self.command_receiver.recv() => {
                    let request = match mb_request {
                        Some(request) => request,
                        None => break,
                    };
                    let result = self.handle_command(request.command).await;
                    request.result_sender.send(result).ok();
                }
                mb_request = self.action_receiver.recv() => {
                    let request = match mb_request {
                        Some(request) => request,
                        None => break,
                    };
                    let result = self.handle_action(request.action).await;
                    request.result_sender.send(result).ok();
                }
            }
        }
        debug!(
            "user handler has terminated: @{}",
            self.user_handle.user.login
        );
    }

    async fn handle_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Help | Command::Start => self.send_to_self("TODO: help message.").await,
            Command::List => {
                let mut usernames = self
                    .handle_registry
                    .read()
                    .expect("handler message_channels.read() failed")
                    .values()
                    .map(|h| {
                        if let Some(last_name) = h.user.last_name.as_ref() {
                            format!("{} {} @{}", h.user.first_name, last_name, h.user.login)
                        } else {
                            format!("{} @{}", h.user.first_name, h.user.login)
                        }
                    })
                    .collect::<Vec<_>>();
                usernames.sort();
                self.send_to_self(format!(
                    "Available users:\n  * {}",
                    usernames.join("\n  * "),
                ))
                .await
            }
            Command::Send { thread_id, message } => {
                if !self.threads.contains_key(&thread_id) {
                    let thread = self.create_thread(thread_id.clone()).await?;
                    self.threads.insert(thread_id.clone(), thread);
                }
                self.threads[&thread_id].send_message(message).await?;
                self.send_to_self("OK.").await
            }
        }
    }

    async fn create_thread(&mut self, thread_id: ThreadId) -> Result<Thread> {
        ensure!(
            thread_id.starts_with("@"),
            "thread {} is not found",
            thread_id,
        );
        let login = &thread_id[1..];

        // NB: this prevents deadlock.
        ensure!(
            login != self.user_handle.user.login,
            "cannot send a message to self"
        );

        let other_handle = self
            .handle_registry
            .read()
            .expect("handler handle_registry.read() failed")
            .get(login)
            .with_context(|| format!("user @{} has not started this bot", login))?
            .clone();
        let other_thread_id = Self::random_thread_id();

        let my_thread = Thread {
            id: thread_id.clone(),
            other_id: other_thread_id.clone(),
            other_handle: other_handle.clone(),
        };
        let other_thread = Thread {
            id: other_thread_id,
            other_id: thread_id.clone(),
            other_handle: self.user_handle.clone(),
        };

        let (result_sender, result_receiver) = oneshot::channel();
        let start_request = ActionRequest {
            action: Action::StartAnonymousThread(other_thread),
            result_sender,
        };
        other_handle
            .channel
            .send(start_request)
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to send thread start request to @{} handler: {}",
                    login, err
                )
            });
        result_receiver.await.unwrap_or_else(|err| {
            panic!(
                "failed to get thread start result of @{} handler: {}",
                login, err
            )
        })?;
        Ok(my_thread)
    }

    fn random_thread_id() -> String {
        format!("#{:08x}", rand::random::<u32>())
    }

    async fn handle_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::StartAnonymousThread(thread) => {
                let thread_id = thread.id.clone();
                if self.threads.contains_key(&thread_id) {
                    bail!("thread id {} is already used", thread_id)
                }
                self.threads.insert(thread_id, thread);
                Ok(())
            }
            Action::RelayMessage(thread_id, message) => {
                self.send_to_self(format!(">>> @{}:\n{}", thread_id, message))
                    .await
            }
        }
    }

    async fn send_to_self(&mut self, message: impl AsRef<str>) -> Result<()> {
        self.bot
            .send_message(self.chat_id, message.as_ref())
            .await
            .with_context(|| {
                format!(
                    "failed to send message to user @{}",
                    self.user_handle.user.login
                )
            })?;
        Ok(())
    }
}
