use anyhow::{Context, Result};
use log::{debug, error};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use std::{
    io::{BufRead, ErrorKind, Write},
    sync::Arc,
};

use crate::{
    data::{ThreadAnonimityMode, ThreadId},
    User,
};

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Serialize, Deserialize)]
pub enum Event {
    UserConnected(UserConnectedEvent),
    ThreadStarted(ThreadStartedEvent),
    ThreadMessageReceived(ThreadMessageReceivedEvent),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserConnectedEvent {
    pub user: User,
    pub chat_id: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadStartedEvent {
    pub login: String,
    pub other_login: String,
    pub my_thread_id: ThreadId,
    pub other_thread_id: ThreadId,
    pub anon_mode: ThreadAnonimityMode,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadMessageReceivedEvent {
    pub login: String,
    pub message_id: i32,
    pub thread_id: String,
}

////////////////////////////////////////////////////////////////////////////////

pub struct EventLogReader<R> {
    reader: R,
}

impl<R: BufRead> EventLogReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    pub fn iter_events(&mut self) -> impl Iterator<Item = Result<Event>> + '_ {
        let mut data = vec![];

        std::iter::from_fn(move || -> Option<Result<Event>> {
            data.clear();
            if let Err(err) = self.reader.read_until(b'\n', &mut data) {
                if err.kind() == ErrorKind::UnexpectedEof && data.is_empty() {
                    return None;
                }
                return Some(Err(err.into()));
            }
            if data.is_empty() {
                return None;
            }
            Some(serde_json::from_slice(&data).map_err(|err| err.into()))
        })
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub struct EventRequest {
    events: EventCollection,
    result: oneshot::Sender<Result<(), PersistenceError>>,
}

#[derive(Debug)]
enum EventCollection {
    One(Event),
    Many(Vec<Event>),
}

#[derive(Clone)]
pub struct EventServiceHandle {
    sender: mpsc::UnboundedSender<EventRequest>,
}

impl EventServiceHandle {
    pub fn write(&self, event: Event) -> EventTracker {
        self.do_write(EventCollection::One(event))
    }

    pub fn write_batch(&self, events: Vec<Event>) -> EventTracker {
        self.do_write(EventCollection::Many(events))
    }

    fn do_write(&self, events: EventCollection) -> EventTracker {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EventRequest {
                events,
                result: sender,
            })
            .expect("failed to send events to event service");
        EventTracker { receiver }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Error, Debug, Clone)]
#[error("persistence error")]
pub struct PersistenceError {
    #[source]
    inner: Arc<anyhow::Error>,
}

#[must_use]
pub struct EventTracker {
    receiver: oneshot::Receiver<Result<(), PersistenceError>>,
}

impl EventTracker {
    pub async fn wait_written(self) -> Result<(), PersistenceError> {
        self.receiver
            .await
            .expect("failed to wait for event being written")
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct EventService<W> {
    receiver: mpsc::UnboundedReceiver<EventRequest>,
    writer: W,
}

impl<W: Write> EventService<W> {
    pub fn new(writer: W) -> (Self, EventServiceHandle) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { receiver, writer }, EventServiceHandle { sender })
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            let mut events = vec![];
            let mut channels = vec![];

            let mut process_request = |request: EventRequest| {
                channels.push(request.result);
                match request.events {
                    EventCollection::One(ev) => events.push(ev),
                    EventCollection::Many(evs) => events.extend(evs),
                }
                events.len() < 1000
            };

            let request = match self.receiver.recv().await {
                Some(request) => request,
                None => return Ok(()),
            };
            process_request(request);

            while let Ok(request) = self.receiver.try_recv() {
                if !process_request(request) {
                    break;
                }
            }

            let result = self
                .write_events(&events)
                .context("failed to write events")
                .map_err(|err| PersistenceError {
                    inner: Arc::new(err),
                });
            match result.as_ref() {
                Ok(()) => debug!("wrote {} events to log", events.len()),
                Err(err) => error!("failed to write events: {}", err),
            }

            for chan in channels {
                chan.send(result.clone()).ok();
            }
        }
    }

    fn write_events(&mut self, events: &[Event]) -> Result<()> {
        for event in events {
            serde_json::to_writer(&mut self.writer, event)?;
            self.writer.write(b"\n")?;
        }
        self.writer.flush()?;
        Ok(())
    }
}
