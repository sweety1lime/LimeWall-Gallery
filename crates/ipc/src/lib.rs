//! Versioned JSON protocol and bounded framing shared by LiveWall processes.
//!
//! Transport is deliberately independent from Tauri, libmpv and the platform
//! backend. Phase 2 uses these frames over `interprocess` local sockets.

use std::io::{Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod transport;

pub use transport::{LocalServer, LocalStream, TransportError, default_endpoint, send_request};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

pub type RequestId = u64;
pub type MonitorId = usize;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub id: RequestId,
    pub command: Command,
}

impl Request {
    pub fn new(id: RequestId, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            command,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                received: self.version,
                supported: PROTOCOL_VERSION,
            });
        }
        self.command.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Ping,
    ListMonitors,
    Status,
    Play {
        monitor: MonitorId,
        path: PathBuf,
        quality: Quality,
        volume: u8,
        anime4k: bool,
    },
    Stop {
        monitor: Option<MonitorId>,
    },
    Pause {
        monitor: Option<MonitorId>,
    },
    Resume {
        monitor: Option<MonitorId>,
    },
    SetVolume {
        monitor: MonitorId,
        volume: u8,
    },
    SetQuality {
        monitor: MonitorId,
        quality: Quality,
        anime4k: bool,
    },
    Shutdown,
}

impl Command {
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Play { path, volume, .. } => {
                validate_volume(*volume)?;
                if path.as_os_str().is_empty() {
                    return Err(ValidationError::EmptyMediaPath);
                }
                if !path.is_absolute() {
                    return Err(ValidationError::RelativeMediaPath(path.clone()));
                }
                Ok(())
            }
            Self::SetVolume { volume, .. } => validate_volume(*volume),
            _ => Ok(()),
        }
    }
}

fn validate_volume(volume: u8) -> Result<(), ValidationError> {
    if volume <= 100 {
        Ok(())
    } else {
        Err(ValidationError::InvalidVolume(volume))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Eco,
    Balanced,
    Max,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub version: u16,
    pub id: RequestId,
    #[serde(flatten)]
    pub body: ResponseBody,
}

impl Response {
    pub fn success(id: RequestId, result: ResponseData) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            body: ResponseBody::Success { result },
        }
    }

    pub fn error(id: RequestId, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            body: ResponseBody::Error {
                error: ProtocolError {
                    code,
                    message: message.into(),
                },
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Success { result: ResponseData },
    Error { error: ProtocolError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseData {
    Pong { daemon_version: String },
    Monitors { monitors: Vec<Monitor> },
    Status { sessions: Vec<SessionStatus> },
    Acknowledged { status: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    MonitorNotFound,
    MediaNotFound,
    PlaybackFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: MonitorId,
    pub name: String,
    pub bounds: Rect,
    pub scale: f64,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub monitor: MonitorId,
    pub state: PlaybackState,
    pub path: Option<PathBuf>,
    pub quality: Quality,
    pub volume: u8,
    pub anime4k: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("unsupported protocol version {received}; supported version is {supported}")]
    UnsupportedVersion { received: u16, supported: u16 },
    #[error("volume must be 0-100, got {0}")]
    InvalidVolume(u8),
    #[error("media path must not be empty")]
    EmptyMediaPath,
    #[error("media path must be absolute: {}", .0.display())]
    RelativeMediaPath(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("IPC frame is empty")]
    Empty,
    #[error("IPC frame is {size} bytes; maximum is {maximum}")]
    TooLarge { size: usize, maximum: usize },
    #[error("failed to read or write IPC frame: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid IPC JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Serializes one value as a bounded little-endian length-prefixed JSON frame.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value)?;
    validate_frame_size(payload.len())?;
    let length = payload.len() as u32;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads one bounded little-endian length-prefixed JSON frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let mut header = [0u8; std::mem::size_of::<u32>()];
    reader.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    validate_frame_size(length)?;
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn validate_frame_size(size: usize) -> Result<(), FrameError> {
    if size == 0 {
        Err(FrameError::Empty)
    } else if size > MAX_FRAME_SIZE {
        Err(FrameError::TooLarge {
            size,
            maximum: MAX_FRAME_SIZE,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    fn absolute_media_path() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(r"C:\Wallpapers\clip.mp4")
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("/wallpapers/clip.mp4")
        }
    }

    fn commands() -> Vec<Command> {
        vec![
            Command::Ping,
            Command::ListMonitors,
            Command::Status,
            Command::Play {
                monitor: 0,
                path: absolute_media_path(),
                quality: Quality::Balanced,
                volume: 0,
                anime4k: false,
            },
            Command::Stop { monitor: Some(0) },
            Command::Stop { monitor: None },
            Command::Pause { monitor: Some(0) },
            Command::Pause { monitor: None },
            Command::Resume { monitor: Some(0) },
            Command::Resume { monitor: None },
            Command::SetVolume {
                monitor: 0,
                volume: 73,
            },
            Command::SetQuality {
                monitor: 0,
                quality: Quality::Max,
                anime4k: true,
            },
            Command::Shutdown,
        ]
    }

    #[test]
    fn every_command_round_trips_through_framing() {
        for (id, command) in commands().into_iter().enumerate() {
            let request = Request::new(id as u64, command);
            request.validate().expect("valid command");
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &request).expect("write request frame");
            let decoded: Request = read_frame(&mut Cursor::new(bytes)).expect("read request frame");
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn response_shapes_round_trip() {
        let responses = [
            Response::success(
                1,
                ResponseData::Pong {
                    daemon_version: "0.1.0".into(),
                },
            ),
            Response::error(2, ErrorCode::InvalidRequest, "bad request"),
        ];
        for response in responses {
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &response).expect("write response frame");
            let decoded: Response =
                read_frame(&mut Cursor::new(bytes)).expect("read response frame");
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn response_envelopes_use_result_or_error_fields() {
        let success = serde_json::to_value(Response::success(
            1,
            ResponseData::Acknowledged {
                status: "ok".into(),
            },
        ))
        .expect("serialize success");
        assert!(success.get("result").is_some());
        assert!(success.get("error").is_none());

        let failure =
            serde_json::to_value(Response::error(2, ErrorCode::InvalidRequest, "invalid"))
                .expect("serialize error");
        assert!(failure.get("result").is_none());
        assert!(failure.get("error").is_some());
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let mut request = Request::new(1, Command::Ping);
        request.version += 1;
        assert_eq!(
            request.validate(),
            Err(ValidationError::UnsupportedVersion {
                received: PROTOCOL_VERSION + 1,
                supported: PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn rejects_invalid_volume_and_media_paths() {
        assert_eq!(
            Command::SetVolume {
                monitor: 0,
                volume: 101,
            }
            .validate(),
            Err(ValidationError::InvalidVolume(101))
        );
        let relative = PathBuf::from("clip.mp4");
        assert_eq!(
            Command::Play {
                monitor: 0,
                path: relative.clone(),
                quality: Quality::Eco,
                volume: 0,
                anime4k: false,
            }
            .validate(),
            Err(ValidationError::RelativeMediaPath(relative))
        );
        assert!(!Path::new(&absolute_media_path()).is_relative());
    }

    #[test]
    fn rejects_oversized_frame_before_allocating_payload() {
        let size = MAX_FRAME_SIZE + 1;
        let header = (size as u32).to_le_bytes();
        let error = read_frame::<_, Request>(&mut Cursor::new(header)).expect_err("too large");
        assert!(matches!(
            error,
            FrameError::TooLarge {
                size: actual,
                maximum: MAX_FRAME_SIZE,
            } if actual == size
        ));
    }

    #[test]
    fn rejects_oversized_outgoing_frame_before_writing() {
        #[derive(Serialize)]
        struct OversizedPayload {
            data: String,
        }

        let value = OversizedPayload {
            data: "x".repeat(MAX_FRAME_SIZE),
        };
        let mut output = Vec::new();
        let error = write_frame(&mut output, &value).expect_err("too large");
        assert!(matches!(error, FrameError::TooLarge { .. }));
        assert!(output.is_empty());
    }

    #[test]
    fn rejects_empty_and_malformed_frames() {
        let empty = 0u32.to_le_bytes();
        assert!(matches!(
            read_frame::<_, Request>(&mut Cursor::new(empty)),
            Err(FrameError::Empty)
        ));

        let payload = b"not json";
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(payload);
        assert!(matches!(
            read_frame::<_, Request>(&mut Cursor::new(frame)),
            Err(FrameError::Json(_))
        ));
    }

    #[test]
    fn reads_back_to_back_frames_without_consuming_the_next_one() {
        let first = Request::new(1, Command::Ping);
        let second = Request::new(2, Command::Status);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &first).expect("write first frame");
        write_frame(&mut bytes, &second).expect("write second frame");

        let mut cursor = Cursor::new(bytes);
        assert_eq!(read_frame::<_, Request>(&mut cursor).expect("first"), first);
        assert_eq!(
            read_frame::<_, Request>(&mut cursor).expect("second"),
            second
        );
    }
}
