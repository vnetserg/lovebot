use crate::{
    handler::{ActionRequest, CommandRequest, Handler},
    util::Writer,
    Command,
};

use anyhow::{Context, Result};
use teloxide::{adaptors::AutoSend, Bot};
use tokio::sync::{mpsc, oneshot};

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[derive(Clone)]
pub struct User {
    pub login: String,
    pub first_name: String,
    pub last_name: Option<String>,
}

impl TryFrom<&teloxide::types::User> for User {
    type Error = anyhow::Error;

    fn try_from(tg_user: &teloxide::types::User) -> Result<Self> {
        Ok(User {
            login: tg_user
                .username
                .as_ref()
                .context("user has no username")?
                .clone(),
            first_name: tg_user.first_name.clone(),
            last_name: tg_user.last_name.clone(),
        })
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Clone)]
pub struct UserHandle {
    pub user: Arc<User>,
    pub channel: mpsc::Sender<ActionRequest>,
}

////////////////////////////////////////////////////////////////////////////////

pub struct CommandDispatcher {
    bot: AutoSend<Bot>,
    command_channels: Mutex<HashMap<String, mpsc::Sender<CommandRequest>>>,
    user_handles: Writer<HashMap<String, UserHandle>>,
}

impl CommandDispatcher {
    pub fn new(bot: AutoSend<Bot>) -> Self {
        Self {
            bot,
            command_channels: Mutex::new(HashMap::new()),
            user_handles: Writer::new(HashMap::new()),
        }
    }

    pub async fn handle_command(
        &self,
        user: Arc<User>,
        chat_id: i64,
        command: Command,
    ) -> Result<()> {
        let command_sender = self
            .command_channels
            .lock()
            .expect("failed to lock handlers")
            .entry(user.login.clone())
            .or_insert_with(|| self.spawn_handler(user.clone(), chat_id))
            .clone();

        let (result_sender, result_receiver) = oneshot::channel();
        let request = CommandRequest {
            command,
            result_sender,
        };
        command_sender.send(request).await.unwrap_or_else(|err| {
            panic!("failed to send request to @{} handler: {}", user.login, err)
        });
        result_receiver.await.unwrap_or_else(|err| {
            panic!(
                "failed to get command request result of @{} handler: {}",
                user.login, err
            )
        })
    }

    fn spawn_handler(&self, user: Arc<User>, chat_id: i64) -> mpsc::Sender<CommandRequest> {
        let (command_sender, command_receiver) = mpsc::channel(100);
        let (action_sender, action_receiver) = mpsc::channel(100);

        let user_handle = UserHandle {
            user,
            channel: action_sender,
        };
        let mut handler = Handler::new(
            user_handle.clone(),
            self.bot.clone(),
            chat_id,
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
