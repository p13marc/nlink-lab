//! The unix-socket framing: length-prefixed frames, each a JSON message or
//! a chunk of bytes, tagged with the call it belongs to. Pure: no I/O.
//!
//! ```text
//! frame := len:u32be | kind:u8 | call:u32be | body          len = 1 + 4 + body.len()
//! kind 0  body = a ClientMsg or ServerMsg, JSON (UTF-8)
//! kind 1  body = stream:u8 | bytes                          (stdin, stdout, stderr, pty, pcap, archive)
//! ```
//!
//! Call 0 is the connection itself (the handshake, pings).

use serde::{Deserialize, Serialize};

use super::{ApiError, Request, Response, StreamItem};

/// The largest frame, header included: 16 MiB.
pub const MAX_FRAME: usize = 16 << 20;

/// What a frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Message = 0,
    Data = 1,
}

/// The byte stream a data frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataStream {
    Stdin = 0,
    Stdout = 1,
    Stderr = 2,
    Pty = 3,
    Pcap = 4,
    Archive = 5,
}

impl DataStream {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Stdin,
            1 => Self::Stdout,
            2 => Self::Stderr,
            3 => Self::Pty,
            4 => Self::Pcap,
            5 => Self::Archive,
            _ => return None,
        })
    }
}

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Message {
        call: u32,
        json: Vec<u8>,
    },
    Data {
        call: u32,
        stream: DataStream,
        bytes: Vec<u8>,
    },
}

/// A frame that cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Longer than [`MAX_FRAME`].
    TooLarge(usize),
    /// Shorter than its header.
    Truncated,
    UnknownKind(u8),
    UnknownStream(u8),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(n) => write!(f, "frame of {n} bytes is over the {MAX_FRAME}-byte limit"),
            Self::Truncated => f.write_str("frame shorter than its header"),
            Self::UnknownKind(k) => write!(f, "unknown frame kind {k}"),
            Self::UnknownStream(s) => write!(f, "unknown data stream {s}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    /// A message frame of `msg`, JSON-encoded.
    pub fn message<T: Serialize>(call: u32, msg: &T) -> Self {
        Self::Message {
            call,
            json: serde_json::to_vec(msg).unwrap_or_default(),
        }
    }

    pub fn call(&self) -> u32 {
        match self {
            Self::Message { call, .. } | Self::Data { call, .. } => *call,
        }
    }

    /// Append the encoded frame to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let (kind, call, head, body): (u8, u32, Option<u8>, &[u8]) = match self {
            Self::Message { call, json } => (FrameKind::Message as u8, *call, None, json),
            Self::Data {
                call,
                stream,
                bytes,
            } => (FrameKind::Data as u8, *call, Some(*stream as u8), bytes),
        };
        let len = 1 + 4 + head.map_or(0, |_| 1) + body.len();
        out.extend_from_slice(&(len as u32).to_be_bytes());
        out.push(kind);
        out.extend_from_slice(&call.to_be_bytes());
        if let Some(s) = head {
            out.push(s);
        }
        out.extend_from_slice(body);
    }

    /// Decode one frame from the start of `buf`: `Ok(None)` while it is
    /// incomplete, else the frame and how many bytes it took.
    pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
        if buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len + 4 > MAX_FRAME {
            return Err(FrameError::TooLarge(len + 4));
        }
        if len < 5 {
            return Err(FrameError::Truncated);
        }
        if buf.len() < 4 + len {
            return Ok(None);
        }
        let kind = buf[4];
        let call = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
        let body = &buf[9..4 + len];
        let frame = match kind {
            0 => Frame::Message {
                call,
                json: body.to_vec(),
            },
            1 => {
                let (&s, bytes) = body.split_first().ok_or(FrameError::Truncated)?;
                Frame::Data {
                    call,
                    stream: DataStream::from_u8(s).ok_or(FrameError::UnknownStream(s))?,
                    bytes: bytes.to_vec(),
                }
            }
            k => return Err(FrameError::UnknownKind(k)),
        };
        Ok(Some((frame, 4 + len)))
    }
}

/// A client's message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The first message: the client's API version (`"1.0"`), name and
    /// features.
    Hello {
        api: String,
        client: String,
        #[serde(default)]
        features: Vec<String>,
    },
    /// Start a call (its id is the frame's).
    Call {
        #[serde(flatten)]
        request: Box<Request>,
    },
    Cancel,
    /// Leave a long operation running and stop following it.
    Detach,
    /// End of the call's stdin.
    Eof,
    Resize {
        rows: u16,
        cols: u16,
    },
    Signal {
        signal: String,
    },
    Ping,
}

/// The daemon's message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The answer to `Hello`.
    Welcome {
        api: String,
        server: String,
        host: String,
        uid: u32,
        user: String,
        role: super::Role,
        #[serde(default)]
        features: Vec<String>,
    },
    Event {
        #[serde(flatten)]
        item: StreamItem,
    },
    /// The call's end: `ok` or `err`.
    End {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Response>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<ApiError>,
    },
    Pong,
}
