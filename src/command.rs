use anyhow::{bail, Context};

use std::str::FromStr;

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub enum Command {
    Start,
    Help,
    Users,
    Threads,
    Send { thread_id: String, message: String },
}

impl FromStr for Command {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let mut iter = s.trim_start().split(" ");
        let head = iter.next().with_context(|| format!("empty message"))?;
        match head {
            "/start" => Ok(Command::Start),
            "/help" => Ok(Command::Help),
            "/users" => Ok(Command::Users),
            "/threads" => Ok(Command::Threads),
            "/send" => {
                let receiver = iter
                    .next()
                    .with_context(|| format!("no receiver specified"))?
                    .to_string();
                let message = iter.collect::<Vec<_>>().join(" ");
                Ok(Command::Send {
                    thread_id: receiver,
                    message,
                })
            }
            _ => bail!("unknown command: {}", head),
        }
    }
}
