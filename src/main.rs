use std::{
    fs,
    io::{BufReader, BufWriter, Write},
    path::PathBuf,
    sync::Arc,
    thread,
};

use lovebot::{
    Command, CommandDispatcher, CommandDispatcherBuilder, EventService, EventServiceHandle, User,
};

use anyhow::{Context, Result};
use clap::Parser;
use log::*;
use teloxide::prelude::*;
use tokio::runtime;

////////////////////////////////////////////////////////////////////////////////

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    event_log: PathBuf,
}

////////////////////////////////////////////////////////////////////////////////

async fn handle_message(
    cx: &UpdateWithCx<AutoSend<Bot>, Message>,
    command_dispatcher: Arc<CommandDispatcher>,
) -> Result<()> {
    let user = User::try_from(cx.update.from().context("message has unknown sender")?)
        .context("failed to parse user")?;
    let command = Command::try_from(&cx.update).context("failed to parse command")?;
    command_dispatcher
        .handle_command(Arc::new(user), cx.chat_id(), command)
        .await
}

fn launch_event_service<W: Write + Send + 'static>(writer: W) -> EventServiceHandle {
    let (mut service, handle) = EventService::new(writer);
    thread::spawn(move || {
        let runtime = runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build tokio runtime");
        runtime.block_on(async move {
            match service.run().await {
                Ok(()) => info!("event service terminated"),
                Err(err) => error!("event service terminated with error: {}", err),
            }
        });
    });
    handle
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    teloxide::enable_logging!();
    info!("Starting lovebot...");

    let bot = Bot::from_env().auto_send();

    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&args.event_log)
        .context("failed to open event log file")?;

    let builder = CommandDispatcherBuilder::from_event_log(BufReader::new(&mut file))
        .context("failed to restore state from event log")?;
    let event_service = launch_event_service(BufWriter::new(file));
    let command_dispatcher = Arc::new(builder.build(bot.clone(), event_service));

    teloxide::repl(bot, move |cx| {
        let command_dispatcher = command_dispatcher.clone();
        async move {
            debug!("Incoming: {:?}", cx.update);
            if let Err(err) = handle_message(&cx, command_dispatcher).await {
                let message = format!("Error: {:#}.", err);
                debug!("Sending reply to chat {}: {}", cx.chat_id(), message);
                cx.answer(message).await?;
            }
            respond(())
        }
    })
    .await;

    Ok(())
}
