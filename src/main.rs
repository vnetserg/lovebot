use std::sync::Arc;

use lovebot::{CommandDispatcher, User};

use anyhow::{Context, Result};
use log::*;
use teloxide::prelude::*;

////////////////////////////////////////////////////////////////////////////////

async fn handle_message(
    cx: &UpdateWithCx<AutoSend<Bot>, Message>,
    command_dispatcher: Arc<CommandDispatcher>,
) -> Result<()> {
    let user = User::try_from(cx.update.from().context("message has unknown sender")?)
        .context("failed to parse user")?;
    let message = cx
        .update
        .text()
        .context("non-text messages are not supported")?;
    let command = message.parse().context("failed to parse command")?;
    command_dispatcher
        .handle_command(Arc::new(user), cx.chat_id(), command)
        .await
}

#[tokio::main]
async fn main() {
    teloxide::enable_logging!();
    info!("Starting lovebot...");

    let bot = Bot::from_env().auto_send();
    let handler_registry = Arc::new(CommandDispatcher::new(bot.clone()));

    teloxide::repl(bot, move |cx| {
        let handler_registry = handler_registry.clone();
        async move {
            debug!("Incoming: {:?}", cx.update);
            if let Err(err) = handle_message(&cx, handler_registry).await {
                let message = format!("Error: {:#}.", err);
                debug!("Sending reply to chat {}: {}", cx.chat_id(), message);
                cx.answer(message).await?;
            }
            respond(())
        }
    })
    .await;
}
