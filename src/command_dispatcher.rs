use crate::{
    data::User,
    event_log::{Event, EventLogReader, EventTracker, UserConnectedEvent},
    handler::{ActionRequest, CommandRequest, Handler, HandlerBuilder},
    util::Writer,
    Command, EventServiceHandle,
};

use anyhow::{Context, Result};
use log::info;
use teloxide::{adaptors::AutoSend, Bot};
use tokio::sync::{mpsc, oneshot};

use std::{
    collections::HashMap,
    io::BufRead,
    sync::{atomic::AtomicBool, Arc, Mutex},
};

////////////////////////////////////////////////////////////////////////////////

#[derive(Clone)]
pub struct UserHandle {
    pub user: Arc<User>,
    pub channel: mpsc::Sender<ActionRequest>,
    pub is_stopped: Arc<AtomicBool>,
}

////////////////////////////////////////////////////////////////////////////////

pub struct CommandDispatcherBuilder {
    command_channels: HashMap<String, mpsc::Sender<CommandRequest>>,
    user_handles: Writer<HashMap<String, UserHandle>>,
    builders: HashMap<String, HandlerBuilder>,
}

impl CommandDispatcherBuilder {
    pub fn from_event_log<R: BufRead>(reader: R) -> Result<Self> {
        let mut builder = Self {
            command_channels: Default::default(),
            user_handles: Default::default(),
            builders: Default::default(),
        };

        let mut reader = EventLogReader::new(reader);
        let mut count = 0;
        for mb_event in reader.iter_events() {
            let event = mb_event.context("failed to read event")?;
            match event {
                Event::UserConnected(ev) => builder.handle_user_connected(ev),
                Event::ThreadStarted(ev) => builder
                    .builders
                    .get_mut(&ev.login)
                    .with_context(|| format!("user not found: @{}", ev.login))?
                    .handle_thread_started(ev)?,
                Event::ThreadMessageReceived(ev) => {
                    builder
                        .builders
                        .get_mut(&ev.login)
                        .with_context(|| format!("user not found: @{}", ev.login))?
                        .handle_thread_message_received(ev);
                }
                Event::ThreadTerminated(ev) => {
                    builder
                        .builders
                        .get_mut(&ev.login)
                        .with_context(|| format!("user not found: @{}", ev.login))?
                        .terminate_thread(&ev.my_thread_id)?;
                    builder
                        .builders
                        .get_mut(&ev.other_login)
                        .with_context(|| format!("user not found: @{}", ev.other_login))?
                        .terminate_thread(&ev.other_thread_id)?;
                }
                Event::UserBanned(ev) => builder
                    .builders
                    .get_mut(&ev.login)
                    .with_context(|| format!("user not found: @{}", ev.login))?
                    .handle_user_banned(ev)?,
                Event::UserUnbanned(ev) => builder
                    .builders
                    .get_mut(&ev.login)
                    .with_context(|| format!("user not found: @{}", ev.login))?
                    .handle_user_unbanned(ev)?,
                Event::UserStopped(ev) => builder
                    .builders
                    .get_mut(&ev.login)
                    .with_context(|| format!("user not found: @{}", ev.login))?
                    .handle_user_stopped(),
                Event::UserStarted(ev) => builder
                    .builders
                    .get_mut(&ev.login)
                    .with_context(|| format!("user not found: @{}", ev.login))?
                    .handle_user_started(),
            }
            count += 1;
        }

        info!("Read {} events from event log", count);

        Ok(builder)
    }

    fn handle_user_connected(&mut self, event: UserConnectedEvent) {
        let login = event.user.login.clone();

        let (command_sender, command_receiver) = mpsc::channel(100);
        self.command_channels.insert(login.clone(), command_sender);

        let (action_sender, action_receiver) = mpsc::channel(100);
        let user_handle = UserHandle {
            user: Arc::new(event.user),
            channel: action_sender,
            is_stopped: Arc::new(AtomicBool::new(false)),
        };
        self.user_handles
            .write()
            .unwrap()
            .insert(login.clone(), user_handle.clone());

        self.builders.insert(
            login,
            HandlerBuilder::new(
                user_handle,
                event.chat_id,
                self.user_handles.reader(),
                command_receiver,
                action_receiver,
            ),
        );
    }

    pub fn build(self, bot: AutoSend<Bot>, event_service: EventServiceHandle) -> CommandDispatcher {
        for builder in self.builders.into_values() {
            let mut handler = builder.build(bot.clone(), event_service.clone());
            tokio::spawn(async move {
                handler.run().await;
            });
        }

        CommandDispatcher {
            bot,
            user_handles: self.user_handles,
            command_channels: Mutex::new(self.command_channels),
            event_service,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct CommandDispatcher {
    bot: AutoSend<Bot>,
    command_channels: Mutex<HashMap<String, mpsc::Sender<CommandRequest>>>,
    user_handles: Writer<HashMap<String, UserHandle>>,
    event_service: EventServiceHandle,
}

impl CommandDispatcher {
    pub async fn handle_command(
        &self,
        user: Arc<User>,
        chat_id: i64,
        command: Command,
    ) -> Result<()> {
        let (command_sender, mb_event_tracker) = self.get_command_sender(&user, chat_id);

        let (result_sender, result_receiver) = oneshot::channel();
        let request = CommandRequest {
            command,
            result_sender,
        };

        command_sender.send(request).await.unwrap_or_else(|err| {
            panic!("failed to send request to @{} handler: {}", user.login, err)
        });
        if let Some(event_tracker) = mb_event_tracker {
            // NB: make sure that UserConnected event has been written to disk
            // before replying.
            event_tracker.wait_written().await?;
        }
        result_receiver.await.unwrap_or_else(|err| {
            panic!(
                "failed to get command request result of @{} handler: {}",
                user.login, err
            )
        })
    }

    fn get_command_sender(
        &self,
        user: &Arc<User>,
        chat_id: i64,
    ) -> (mpsc::Sender<CommandRequest>, Option<EventTracker>) {
        let mut command_channels = self
            .command_channels
            .lock()
            .expect("failed to lock command channels");

        match command_channels.get(&user.login) {
            Some(channel) => (channel.clone(), None),
            None => {
                let event_tracker =
                    self.event_service
                        .write(Event::UserConnected(UserConnectedEvent {
                            user: User::clone(&user),
                            chat_id,
                        }));
                let channel = self.spawn_handler(user, chat_id);
                command_channels.insert(user.login.clone(), channel.clone());
                (channel, Some(event_tracker))
            }
        }
    }

    fn spawn_handler(&self, user: &Arc<User>, chat_id: i64) -> mpsc::Sender<CommandRequest> {
        let (command_sender, command_receiver) = mpsc::channel(100);
        let (action_sender, action_receiver) = mpsc::channel(100);

        let user_handle = UserHandle {
            user: user.clone(),
            channel: action_sender,
            is_stopped: Arc::new(AtomicBool::new(false)),
        };
        let mut handler = Handler::new(
            self.bot.clone(),
            self.event_service.clone(),
            chat_id,
            user_handle.clone(),
            self.user_handles.reader(),
            command_receiver,
            action_receiver,
        );
        tokio::spawn(async move {
            handler.run().await;
        });

        self.user_handles
            .write()
            .expect("dispatcher user_handles.write() failed")
            .insert(user_handle.user.login.to_string(), user_handle);
        command_sender
    }
}
