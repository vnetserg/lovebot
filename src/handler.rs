use crate::{
    command_dispatcher::UserHandle,
    util::{Reader, HELP_MESSAGE, START_MESSAGE},
    Command,
};

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

#[allow(dead_code)]
enum AnonimityMode {
    Me,
    Them,
    Both,
}

struct Thread {
    id: ThreadId,
    anon_mode: AnonimityMode,
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
            Command::Start => self.send_to_self(START_MESSAGE).await,
            Command::Help => self.send_to_self(HELP_MESSAGE).await,
            Command::Users => {
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
                self.send_to_self(format!("Available users:\n* {}", usernames.join("\n* "),))
                    .await
            }
            Command::Threads => {
                let thread_ids = self
                    .threads
                    .keys()
                    .filter(|s| s.starts_with("#"))
                    .cloned()
                    .collect::<Vec<_>>();
                if thread_ids.is_empty() {
                    self.send_to_self("There are no active threads.").await
                } else {
                    self.send_to_self(format!("Active threads:\n* {}", thread_ids.join("\n* "),))
                        .await
                }
            }
            Command::Send { thread_id, message } => {
                if !self.threads.contains_key(&thread_id) {
                    let thread = self.create_thread(thread_id.clone()).await?;
                    self.threads.insert(thread_id.clone(), thread);
                }
                self.threads[&thread_id].send_message(message).await
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
            anon_mode: AnonimityMode::Me,
            other_id: other_thread_id.clone(),
            other_handle: other_handle.clone(),
        };
        let other_thread = Thread {
            id: other_thread_id,
            anon_mode: AnonimityMode::Them,
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
                let thread = &self.threads[&thread_id];
                let message = match thread.anon_mode {
                    AnonimityMode::Me => {
                        format!(">>> Message from {}:\n{}", thread_id, message)
                    }
                    AnonimityMode::Them => {
                        format!(">>> Message from anonymous {}:\n{}", thread_id, message)
                    }
                    AnonimityMode::Both => {
                        format!(">>> Message from random chat {}:\n{}", thread_id, message)
                    }
                };
                self.send_to_self(message).await
            }
        }
    }

    async fn send_to_self(&mut self, message: impl AsRef<str>) -> Result<()> {
        debug!(
            "sending message to @{}: {}",
            self.user_handle.user.login,
            message.as_ref()
        );
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
