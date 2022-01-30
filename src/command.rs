use anyhow::{bail, Context};
use teloxide::types::Message;

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub enum Command {
    Start,
    Help,
    Users,
    Threads,
    Send {
        thread_id: String,
        message_id: i32,
        text: String,
    },
    Reply {
        message_id: i32,
        reply_message_id: i32,
        text: String,
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
            "/send" => {
                let receiver = iter.next().context("no receiver specified")?.to_string();
                let text = iter.collect::<Vec<_>>().join(" ");
                Command::Send {
                    message_id: message.id,
                    thread_id: receiver,
                    text,
                }
            }
            _ => bail!("unknown command: {}", head),
        };
        Ok(command)
    }
}
