use crate::{
    command_dispatcher::UserHandle,
    data::{ThreadAnonimityMode, ThreadId},
    event_log::{
        Event, ThreadMessageReceivedEvent, ThreadStartedEvent, ThreadTerminatedEvent,
        UserBannedEvent, UserStartedEvent, UserStoppedEvent, UserUnbannedEvent,
    },
    util::{random_adjective, random_noun, Reader, HELP_MESSAGE, START_MESSAGE, STOP_MESSAGE},
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

use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};

////////////////////////////////////////////////////////////////////////////////

pub struct CommandRequest {
    pub command: Command,
    pub result_sender: oneshot::Sender<Result<()>>,
}

////////////////////////////////////////////////////////////////////////////////

pub struct ActionRequest {
    pub action: Action,
    pub result_sender: oneshot::Sender<Result<()>>,
}

pub enum Action {
    StartAnonymousThread(Thread),
    SendText(ThreadId, String),
    TerminateThread(ThreadId),
    Broadcast(String),
}

////////////////////////////////////////////////////////////////////////////////

pub struct Thread {
    id: ThreadId,
    anon_mode: ThreadAnonimityMode,
    other_id: ThreadId,
    other_handle: UserHandle,
}

impl Thread {
    async fn send_text(&self, text: String) -> Result<()> {
        self.other_handle
            .send_action(Action::SendText(self.other_id.clone(), text))
            .await
    }

    async fn terminate(&self) -> Result<()> {
        self.other_handle
            .send_action(Action::TerminateThread(self.other_id.clone()))
            .await
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
    banlist: HashMap<String, ThreadId>,
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
            banlist: HashMap::new(),
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

    pub fn terminate_thread(&mut self, thread_id: &str) -> Result<()> {
        ensure!(
            self.threads.remove(thread_id).is_some(),
            "thread is not found: {}",
            thread_id,
        );
        Ok(())
    }

    pub fn handle_user_banned(&mut self, event: UserBannedEvent) -> Result<()> {
        self.threads
            .remove(&event.banned_thread_id)
            .with_context(|| format!("thread is not found: {}", event.banned_thread_id))?;
        self.banlist
            .insert(event.banned_login, event.banned_thread_id);
        Ok(())
    }

    pub fn handle_user_unbanned(&mut self, event: UserUnbannedEvent) -> Result<()> {
        self.banlist
            .remove(&event.unbanned_login)
            .with_context(|| format!("user is not banned: {}", event.unbanned_login))?;
        Ok(())
    }

    pub fn handle_user_stopped(&mut self) {
        self.user_handle.is_stopped.store(true, Ordering::Relaxed);
    }

    pub fn handle_user_started(&mut self) {
        self.user_handle.is_stopped.store(false, Ordering::Relaxed);
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
            banlist: self.banlist,
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
    banlist: HashMap<String, ThreadId>,
}

impl Handler {
    pub fn new(
        bot: AutoSend<Bot>,
        event_service: EventServiceHandle,
        chat_id: i64,
        user_handle: UserHandle,
        handle_registry: Reader<HashMap<String, UserHandle>>,
        command_receiver: mpsc::Receiver<CommandRequest>,
        action_receiver: mpsc::Receiver<ActionRequest>,
    ) -> Self {
        Self {
            bot,
            event_service,
            chat_id,
            user_handle,
            handle_registry,
            command_receiver,
            action_receiver,
            threads: HashMap::new(),
            message_id_to_thread_id: HashMap::new(),
            banlist: HashMap::new(),
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
                    tokio::time::sleep(Duration::from_millis(250)).await;
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
        ensure!(
            !self.user_handle.is_stopped.load(Ordering::Relaxed) || command == Command::Start,
            "you have stopped the bot. Use `/start` to restart it"
        );
        match command {
            Command::Start => {
                self.handle_command_start().await?;
            }
            Command::Stop => {
                self.handle_command_stop().await?;
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
            Command::Close { thread_id } => {
                self.handle_command_close(thread_id).await?;
            }
            Command::Ban { thread_id } => {
                self.handle_command_ban(thread_id).await?;
            }
            Command::Unban { thread_id } => {
                self.handle_command_unban(thread_id).await?;
            }
            Command::Banlist => {
                self.handle_command_banlist().await?;
            }
            Command::Broadcast { text } => {
                self.handle_command_broadcast(text).await?;
            }
        }
        Ok(())
    }

    async fn handle_command_start(&mut self) -> Result<()> {
        self.user_handle.is_stopped.store(false, Ordering::Relaxed);
        self.event_service
            .write(Event::UserStarted(UserStartedEvent {
                login: self.user_handle.user.login.clone(),
            }))
            .wait_written()
            .await?;
        self.send_to_self(START_MESSAGE).await?;
        Ok(())
    }

    async fn handle_command_stop(&mut self) -> Result<()> {
        self.user_handle.is_stopped.store(true, Ordering::Relaxed);
        self.event_service
            .write(Event::UserStopped(UserStoppedEvent {
                login: self.user_handle.user.login.clone(),
            }))
            .wait_written()
            .await?;
        self.send_to_self(STOP_MESSAGE).await?;
        Ok(())
    }

    async fn handle_command_users(&mut self) -> Result<()> {
        let mut usernames = self
            .handle_registry
            .read()
            .expect("handler handle_registry.read() failed")
            .values()
            .filter_map(|h| {
                if h.is_stopped.load(Ordering::Relaxed) {
                    None
                } else if let Some(last_name) = h.user.last_name.as_ref() {
                    Some(format!(
                        "{} {} @{}",
                        h.user.first_name, last_name, h.user.login
                    ))
                } else {
                    Some(format!("{} @{}", h.user.first_name, h.user.login))
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
                anon_mode: ThreadAnonimityMode::Me,
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

    async fn handle_command_close(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self
            .threads
            .get(&thread_id)
            .with_context(|| format!("thread {} does not exist", thread_id))?;

        ensure!(
            matches!(
                thread.anon_mode,
                ThreadAnonimityMode::Both | ThreadAnonimityMode::Me
            ),
            "cannot close a semi-anonimous thread; use `/ban` instead"
        );

        thread
            .terminate()
            .await
            .context("failed to terminate peer thread")?;
        let thread = self.threads.remove(&thread_id).unwrap();

        self.event_service
            .write(Event::ThreadTerminated(ThreadTerminatedEvent {
                login: self.user_handle.user.login.clone(),
                other_login: thread.other_handle.user.login.clone(),
                my_thread_id: thread.id,
                other_thread_id: thread.other_id,
            }))
            .wait_written()
            .await?;

        Ok(())
    }

    async fn handle_command_ban(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self
            .threads
            .get(&thread_id)
            .with_context(|| format!("thread {} does not exist", thread_id))?;

        ensure!(
            thread.anon_mode == ThreadAnonimityMode::Them,
            "cannot ban random or non-anonimous chat; use `/close` instead",
        );

        thread
            .terminate()
            .await
            .context("failed to terminate peer thread")?;
        let thread = self.threads.remove(&thread_id).unwrap();

        self.event_service
            .write(Event::UserBanned(UserBannedEvent {
                login: self.user_handle.user.login.clone(),
                banned_login: thread.other_handle.user.login.clone(),
                banned_thread_id: thread_id.clone(),
            }))
            .wait_written()
            .await?;
        self.banlist
            .insert(thread.other_handle.user.login.clone(), thread_id);

        Ok(())
    }

    async fn handle_command_unban(&mut self, thread_id: ThreadId) -> Result<()> {
        let login = self
            .banlist
            .iter()
            .find(|&(_, th)| th == &thread_id)
            .with_context(|| format!("no {} in your ban list", thread_id))?
            .0
            .clone();

        self.event_service
            .write(Event::UserUnbanned(UserUnbannedEvent {
                login: self.user_handle.user.login.clone(),
                unbanned_login: login.clone(),
            }))
            .wait_written()
            .await?;
        self.banlist.remove(&login);

        Ok(())
    }

    async fn handle_command_banlist(&mut self) -> Result<()> {
        let mut banlist = self.banlist.values().cloned().collect::<Vec<_>>();
        banlist.sort();
        if banlist.is_empty() {
            self.send_to_self("You have not banned anybody.").await?;
        } else {
            self.send_to_self(format!("Banned threads:\n* {}", banlist.join("\n* ")))
                .await?;
        }
        Ok(())
    }

    async fn handle_command_broadcast(&mut self, text: String) -> Result<()> {
        ensure!(
            self.user_handle.user.login == "sergio_4min",
            "you are not admin"
        );
        let handles = self
            .handle_registry
            .read()
            .expect("handle_registry.read() failed")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        self.send_to_self(format!("Starting broadcast to {} users...", handles.len()))
            .await?;
        for handle in handles {
            let result = handle.send_action(Action::Broadcast(text.clone())).await;
            if let Err(err) = result {
                self.send_to_self(format!(
                    "Failed to send broadcast to user @{}: {:#}",
                    handle.user.login, err,
                ))
                .await?;
            }
        }
        self.send_to_self("Broadcast is finished.").await?;
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
            .context("user has not started this bot")?
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
        ensure!(
            !self.user_handle.is_stopped.load(Ordering::Relaxed),
            "user has stopped the bot"
        );
        match action {
            Action::StartAnonymousThread(thread) => {
                ensure!(
                    !self.threads.contains_key(&thread.id),
                    "thread id {} is already used",
                    thread.id,
                );

                ensure!(
                    !self.banlist.contains_key(&thread.other_handle.user.login)
                        || thread.anon_mode != ThreadAnonimityMode::Them,
                    "you are banned by this user",
                );

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
                self.threads.insert(thread.id.clone(), thread);
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
            Action::TerminateThread(thread_id) => {
                self.send_to_self(format!(
                    "Thread {} has been closed by the other side.",
                    thread_id
                ))
                .await?;
                self.threads
                    .remove(&thread_id)
                    .expect("thread is not found");
            }
            Action::Broadcast(text) => {
                self.send_to_self(text).await?;
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
            .context("failed to send message to user")
    }
}
