//! A call: what the daemon streams for one request, and what the caller
//! sends it while it runs (stdin, a resize, a signal, a cancel).

use nlink_lab_shared::api::{
    ApiError, ClientMsg, DataStream, Frame, Response, ServerMsg, StreamItem,
};
use tokio::sync::mpsc;

use crate::{Client, ClientError};

/// What the reader routes to a call.
#[derive(Debug)]
pub(crate) enum Incoming {
    Msg(Box<ServerMsg>),
    Data(DataStream, Vec<u8>),
}

/// One thing a call brings.
#[derive(Debug, Clone, PartialEq)]
pub enum CallEvent {
    /// Progress, a notification, a started process.
    Item(StreamItem),
    /// Bytes of the call's process: stdout, stderr, a terminal, a pcap.
    Data(DataStream, Vec<u8>),
    /// The end: the response, or the daemon's error.
    End(Result<Response, ApiError>),
}

/// A running call. Dropping it before its end detaches it: the daemon
/// cancels an interactive call (`exec`, a subscription) and leaves a long
/// operation running (`attach_op` finds it again).
pub struct Call {
    id: u32,
    rx: mpsc::UnboundedReceiver<Incoming>,
    client: Client,
    ended: bool,
}

impl Call {
    pub(crate) fn new(id: u32, rx: mpsc::UnboundedReceiver<Incoming>, client: Client) -> Self {
        Self {
            id,
            rx,
            client,
            ended: false,
        }
    }

    /// The call's id on the connection.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The next event; `None` after the end, or when the connection
    /// closed before it.
    pub async fn next(&mut self) -> Option<CallEvent> {
        if self.ended {
            return None;
        }
        loop {
            let msg = match self.rx.recv().await? {
                Incoming::Data(stream, bytes) => return Some(CallEvent::Data(stream, bytes)),
                Incoming::Msg(msg) => *msg,
            };
            let event = match msg {
                ServerMsg::Event { item } => CallEvent::Item(item),
                ServerMsg::End { ok, err } => {
                    self.ended = true;
                    self.client.forget(self.id);
                    CallEvent::End(match (ok, err) {
                        (_, Some(e)) => Err(e),
                        (Some(r), None) => Ok(r),
                        (None, None) => Ok(Response::Ok),
                    })
                }
                // not for a call
                _ => continue,
            };
            return Some(event);
        }
    }

    /// Wait for the end, dropping the events.
    pub async fn finish(mut self) -> Result<Response, ClientError> {
        while let Some(event) = self.next().await {
            if let CallEvent::End(end) = event {
                return end.map_err(|e| ClientError::Api(Box::new(e)));
            }
        }
        Err(ClientError::Closed)
    }

    fn control(&self, msg: ClientMsg) -> Result<(), ClientError> {
        self.client.send(self.id, &msg)
    }

    /// Cancel it: a deploy, apply or restore rolls back.
    pub fn cancel(&self) -> Result<(), ClientError> {
        self.control(ClientMsg::Cancel)
    }

    /// Leave a long operation running on the daemon and stop following
    /// it (`attach_op` follows it again).
    pub fn detach(&self) -> Result<(), ClientError> {
        self.control(ClientMsg::Detach)
    }

    /// Bytes for the call's process stdin.
    pub fn stdin(&self, bytes: &[u8]) -> Result<(), ClientError> {
        self.client.send_frame(Frame::Data {
            call: self.id,
            stream: DataStream::Stdin,
            bytes: bytes.to_vec(),
        })
    }

    /// The end of its stdin.
    pub fn eof(&self) -> Result<(), ClientError> {
        self.control(ClientMsg::Eof)
    }

    /// A new terminal size, for a shell.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), ClientError> {
        self.control(ClientMsg::Resize { rows, cols })
    }

    /// A signal for the call's process (`INT`, `TERM`, …).
    pub fn signal(&self, signal: &str) -> Result<(), ClientError> {
        self.control(ClientMsg::Signal {
            signal: signal.to_string(),
        })
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.ended {
            // the daemon cancels an interactive call and leaves a long
            // operation running, as the method says
            let _ = self.control(ClientMsg::Detach);
            self.client.forget(self.id);
        }
    }
}
