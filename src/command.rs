use anyhow::{bail, ensure, Context};
use teloxide::types::Message;

use crate::data::ThreadId;

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub enum Command {
    Start,
    Help,
    Users,
    Threads,
    Random {
        message_id: i32,
        text: String,
    },
    Send {
        thread_id: ThreadId,
        message_id: i32,
        text: String,
    },
    Reply {
        message_id: i32,
        reply_message_id: i32,
        text: String,
    },
    Close {
        thread_id: ThreadId,
    },
    Ban {
        thread_id: ThreadId,
    },
    Unban {
        thread_id: ThreadId,
    },
}

impl TryFrom<&Message> for Command {
    type Error = anyhow::Error;

    fn try_from(message: &Message) -> anyhow::Result<Self> {
        let text = message
            .text()
            .context("non-text messages are not supported")?;
        if let Some(reply_to) = message.reply_to_message() {
            return Ok(Command::Reply {
                message_id: message.id,
                reply_message_id: reply_to.id,
                text: text.to_string(),
            });
        }

        let mut iter = text.trim_start().split(" ");
        let head = iter.next().context("empty message")?;
        let command = match head {
            "/start" => Command::Start,
            "/help" => Command::Help,
            "/users" => Command::Users,
            "/threads" => Command::Threads,
            "/random" => {
                let text = iter.collect::<Vec<_>>().join(" ");
                ensure!(!text.is_empty(), "empty message");
                Command::Random {
                    message_id: message.id,
                    text,
                }
            }
            "/send" => {
                let receiver = iter.next().context("no receiver specified")?.to_string();
                let text = iter.collect::<Vec<_>>().join(" ");
                ensure!(!text.is_empty(), "empty message");
                Command::Send {
                    message_id: message.id,
                    thread_id: receiver,
                    text,
                }
            }
            "/close" => {
                let thread_id = iter.next().context("no thread id specified")?.to_string();
                Command::Close { thread_id }
            }
            "/ban" => {
                let thread_id = iter.next().context("no thread id specified")?.to_string();
                Command::Ban { thread_id }
            }
            "/unban" => {
                let thread_id = iter.next().context("no thread id specified")?.to_string();
                Command::Unban { thread_id }
            }
            _ => bail!("unknown command: {}", head),
        };
        Ok(command)
    }
}
