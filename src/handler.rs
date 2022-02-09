use crate::{
    command_dispatcher::UserHandle,
    data::{ThreadAnonimityMode, ThreadId},
    event_log::{Event, ThreadMessageReceivedEvent, ThreadStartedEvent},
    util::{random_adjective, random_noun, Reader, HELP_MESSAGE, START_MESSAGE},
    Command, EventServiceHandle,
};

use anyhow::{bail, ensure, Context, Result};
use log::*;
use rand::prelude::IteratorRandom;
use teloxide::{adaptors::AutoSend, prelude::Requester, types::Message, Bot};
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
    SendText(ThreadId, String),
}

////////////////////////////////////////////////////////////////////////////////

struct Thread {
    id: ThreadId,
    anon_mode: ThreadAnonimityMode,
    other_id: ThreadId,
    other_handle: UserHandle,
}

impl Thread {
    async fn send_text(&self, text: String) -> Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();
        let action_request = ActionRequest {
            action: Action::SendText(self.other_id.clone(), text),
            result_sender,
        };
        self.other_handle
            .channel
            .send(action_request)
            .await
            .unwrap_or_else(|err| panic!("failed to send action request: {}", err));
        result_receiver
            .await
            .unwrap_or_else(|err| panic!("failed to get action result: {}", err))
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct HandlerBuilder {
    user_handle: UserHandle,
    chat_id: i64,
    handle_registry: Reader<HashMap<String, UserHandle>>,
    command_receiver: mpsc::Receiver<CommandRequest>,
    action_receiver: mpsc::Receiver<ActionRequest>,
    threads: HashMap<ThreadId, Thread>,
    message_id_to_thread_id: HashMap<i32, ThreadId>,
}

impl HandlerBuilder {
    pub fn new(
        user_handle: UserHandle,
        chat_id: i64,
        handle_registry: Reader<HashMap<String, UserHandle>>,
        command_receiver: mpsc::Receiver<CommandRequest>,
        action_receiver: mpsc::Receiver<ActionRequest>,
    ) -> Self {
        Self {
            user_handle,
            chat_id,
            handle_registry,
            command_receiver,
            action_receiver,
            threads: HashMap::new(),
            message_id_to_thread_id: HashMap::new(),
        }
    }

    pub fn handle_thread_started(&mut self, event: ThreadStartedEvent) -> Result<()> {
        let other_handle = self
            .handle_registry
            .read()
            .expect("HandlerBuilder handle_registry.read() failed")
            .get(&event.other_login)
            .with_context(|| format!("user not found: @{}", event.other_login))?
            .clone();
        let thread = Thread {
            id: event.my_thread_id.clone(),
            anon_mode: event.anon_mode,
            other_id: event.other_thread_id,
            other_handle,
        };
        self.threads.insert(event.my_thread_id, thread);
        Ok(())
    }

    pub fn handle_thread_message_received(&mut self, event: ThreadMessageReceivedEvent) {
        self.message_id_to_thread_id
            .insert(event.message_id, event.thread_id);
    }

    pub fn build(self, bot: AutoSend<Bot>, event_service: EventServiceHandle) -> Handler {
        Handler {
            bot,
            event_service,
            chat_id: self.chat_id,
            user_handle: self.user_handle,
            handle_registry: self.handle_registry,
            command_receiver: self.command_receiver,
            action_receiver: self.action_receiver,
            threads: self.threads,
            message_id_to_thread_id: self.message_id_to_thread_id,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Handler {
    bot: AutoSend<Bot>,
    event_service: EventServiceHandle,
    chat_id: i64,
    user_handle: UserHandle,
    handle_registry: Reader<HashMap<String, UserHandle>>,
    command_receiver: mpsc::Receiver<CommandRequest>,
    action_receiver: mpsc::Receiver<ActionRequest>,
    threads: HashMap<ThreadId, Thread>,
    message_id_to_thread_id: HashMap<i32, ThreadId>,
}

impl Handler {
    pub fn new(
        bot: AutoSend<Bot>,
        event_service: EventServiceHandle,
        chat_id: i64,
        user_handle: UserHandle,
        user_handles: Reader<HashMap<String, UserHandle>>,
        command_receiver: mpsc::Receiver<CommandRequest>,
        action_receiver: mpsc::Receiver<ActionRequest>,
    ) -> Self {
        Self {
            bot,
            event_service,
            chat_id,
            user_handle,
            handle_registry: user_handles,
            command_receiver,
            action_receiver,
            threads: HashMap::new(),
            message_id_to_thread_id: HashMap::new(),
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
            Command::Start => {
                self.send_to_self(START_MESSAGE).await?;
            }
            Command::Help => {
                self.send_to_self(HELP_MESSAGE).await?;
            }
            Command::Users => {
                self.handle_command_users().await?;
            }
            Command::Threads => {
                self.handle_command_threads().await?;
            }
            Command::Random { message_id, text } => {
                self.handle_command_random(message_id, text).await?;
            }
            Command::Send {
                thread_id,
                message_id,
                text,
            } => {
                self.handle_command_send(thread_id, message_id, text)
                    .await?;
            }
            Command::Reply {
                reply_message_id,
                message_id,
                text,
            } => {
                self.handle_command_reply(reply_message_id, message_id, text)
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_command_users(&mut self) -> Result<()> {
        let mut usernames = self
            .handle_registry
            .read()
            .expect("handler handle_registry.read() failed")
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
            .await?;
        Ok(())
    }

    async fn handle_command_threads(&mut self) -> Result<()> {
        let mut thread_ids = self
            .threads
            .keys()
            .filter(|s| s.starts_with("#"))
            .cloned()
            .collect::<Vec<_>>();
        thread_ids.sort();
        if thread_ids.is_empty() {
            self.send_to_self("There are no active threads.").await?;
        } else {
            self.send_to_self(format!("Active threads:\n* {}", thread_ids.join("\n* ")))
                .await?;
        }
        Ok(())
    }

    async fn handle_command_random(&mut self, message_id: i32, text: String) -> Result<()> {
        let other_login = self
            .handle_registry
            .read()
            .expect("handler handle_registry.read() failed")
            .values()
            .map(|h| h.user.login.clone())
            .filter(|login| login != &self.user_handle.user.login)
            .choose(&mut rand::thread_rng())
            .context("there are currently no other users to chat with")?;

        let my_thread_id = Self::random_thread_id();
        let other_thread_id = Self::random_thread_id();
        self.create_thread(my_thread_id.clone(), other_thread_id.clone(), &other_login)
            .await?;
        self.message_id_to_thread_id
            .insert(message_id, my_thread_id.clone());

        let first_tracker = self.event_service.write_batch(vec![
            Event::ThreadStarted(ThreadStartedEvent {
                login: self.user_handle.user.login.clone(),
                other_login,
                my_thread_id: my_thread_id.clone(),
                other_thread_id,
                anon_mode: ThreadAnonimityMode::Both,
            }),
            Event::ThreadMessageReceived(ThreadMessageReceivedEvent {
                login: self.user_handle.user.login.clone(),
                message_id,
                thread_id: my_thread_id.clone(),
            }),
        ]);

        self.threads[&my_thread_id].send_text(text).await?;

        let message = self
            .send_to_self(format!("Started a new anonymous thread {}.", my_thread_id))
            .await?;
        self.message_id_to_thread_id
            .insert(message.id, my_thread_id.clone());
        let second_tracker =
            self.event_service
                .write(Event::ThreadMessageReceived(ThreadMessageReceivedEvent {
                    login: self.user_handle.user.login.clone(),
                    message_id,
                    thread_id: my_thread_id,
                }));

        first_tracker.wait_written().await?;
        second_tracker.wait_written().await?;
        Ok(())
    }

    async fn handle_command_send(
        &mut self,
        thread_id: ThreadId,
        message_id: i32,
        text: String,
    ) -> Result<()> {
        let mut events = vec![];

        if !self.threads.contains_key(&thread_id) {
            if !thread_id.starts_with("@") {
                bail!("unknown thread: {}", thread_id);
            }

            let other_login = &thread_id[1..];
            ensure!(
                other_login != self.user_handle.user.login,
                "cannot send a message to self"
            );
            let other_thread_id = Self::random_thread_id();
            self.create_thread(thread_id.clone(), other_thread_id.clone(), other_login)
                .await?;

            events.push(Event::ThreadStarted(ThreadStartedEvent {
                login: self.user_handle.user.login.clone(),
                other_login: other_login.to_string(),
                my_thread_id: thread_id.clone(),
                other_thread_id,
                anon_mode: ThreadAnonimityMode::Both,
            }));
        }

        self.message_id_to_thread_id
            .insert(message_id, thread_id.clone());
        events.push(Event::ThreadMessageReceived(ThreadMessageReceivedEvent {
            login: self.user_handle.user.login.clone(),
            message_id,
            thread_id: thread_id.clone(),
        }));
        self.event_service
            .write_batch(events)
            .wait_written()
            .await?;

        self.threads[&thread_id].send_text(text).await?;
        Ok(())
    }

    async fn handle_command_reply(
        &mut self,
        reply_message_id: i32,
        message_id: i32,
        text: String,
    ) -> Result<()> {
        let thread_id = self
            .message_id_to_thread_id
            .get(&reply_message_id)
            .context("message you are replying to does not belong to a thread")?
            .clone();
        self.threads
            .get(&thread_id)
            .context("thread does not exist anymore")?
            .send_text(text)
            .await?;

        self.message_id_to_thread_id
            .insert(message_id, thread_id.clone());
        self.event_service
            .write(Event::ThreadMessageReceived(ThreadMessageReceivedEvent {
                login: self.user_handle.user.login.clone(),
                message_id,
                thread_id,
            }))
            .wait_written()
            .await?;
        Ok(())
    }

    async fn create_thread(
        &mut self,
        my_thread_id: ThreadId,
        other_thread_id: ThreadId,
        other_login: &str,
    ) -> Result<()> {
        let other_handle = self
            .handle_registry
            .read()
            .expect("handler handle_registry.read() failed")
            .get(other_login)
            .with_context(|| format!("user @{} has not started this bot", other_login))?
            .clone();

        let my_thread = Thread {
            id: my_thread_id.clone(),
            anon_mode: ThreadAnonimityMode::Me,
            other_id: other_thread_id.clone(),
            other_handle: other_handle.clone(),
        };
        let other_thread = Thread {
            id: other_thread_id,
            anon_mode: ThreadAnonimityMode::Them,
            other_id: my_thread_id.clone(),
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
                    other_login, err
                )
            });
        result_receiver.await.unwrap_or_else(|err| {
            panic!(
                "failed to get thread start result of @{} handler: {}",
                other_login, err
            )
        })?;

        self.threads.insert(my_thread_id, my_thread);
        Ok(())
    }

    fn random_thread_id() -> ThreadId {
        format!("#{}_{}", random_adjective(), random_noun())
    }

    async fn handle_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::StartAnonymousThread(thread) => {
                let thread_id = thread.id.clone();
                if self.threads.contains_key(&thread_id) {
                    bail!("thread id {} is already used", thread_id)
                }

                self.event_service
                    .write(Event::ThreadStarted(ThreadStartedEvent {
                        login: self.user_handle.user.login.clone(),
                        other_login: thread.other_handle.user.login.clone(),
                        my_thread_id: thread.id.clone(),
                        other_thread_id: thread.other_id.clone(),
                        anon_mode: thread.anon_mode,
                    }))
                    .wait_written()
                    .await?;
                self.threads.insert(thread_id, thread);
            }
            Action::SendText(thread_id, text) => {
                let thread = &self.threads[&thread_id];
                let formatted_text = match thread.anon_mode {
                    ThreadAnonimityMode::Me => {
                        format!(">>> Message from {}:\n{}", thread_id, text)
                    }
                    ThreadAnonimityMode::Them => {
                        format!(">>> Message from anonymous {}:\n{}", thread_id, text)
                    }
                    ThreadAnonimityMode::Both => {
                        format!(">>> Message from random chat {}:\n{}", thread_id, text)
                    }
                };
                let message = self.send_to_self(formatted_text).await?;

                self.event_service
                    .write(Event::ThreadMessageReceived(ThreadMessageReceivedEvent {
                        login: self.user_handle.user.login.clone(),
                        message_id: message.id,
                        thread_id: thread_id.clone(),
                    }))
                    .wait_written()
                    .await?;
                self.message_id_to_thread_id.insert(message.id, thread_id);
            }
        }
        Ok(())
    }

    async fn send_to_self(&mut self, message: impl AsRef<str>) -> Result<Message> {
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
            })
    }
}
