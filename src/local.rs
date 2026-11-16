//! Layer 2, the LOCAL transport: the Phloem daemon on this machine, through its SDK
//! door — `ingest.sock` on Unix, a named pipe on Windows. The bytes on it are
//! the encoder's frames; the only exchange is the hello and the daemon's
//! welcome or refusal.

use std::io::Read;
use crate::wire::control::Reply;
use crate::wire::frame::FrameKind;

use crate::encode::{clock_statement, Encoder, Error};

/// The local daemon's state dir, by the daemon's own rule
/// (`control::home`): `PHLOEM_STATE_DIR`, else `<home>/.phloem` with the
/// home read from `HOME`, else `USERPROFILE`, else `HOMEDRIVE` + `HOMEPATH`
/// (Windows composes it so).
pub fn default_state_dir() -> std::path::PathBuf {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(d) = var("PHLOEM_STATE_DIR") {
        return std::path::PathBuf::from(d);
    }
    let home = var("HOME")
        .or_else(|| var("USERPROFILE"))
        .or_else(|| Some(format!("{}{}", var("HOMEDRIVE")?, var("HOMEPATH")?)))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".phloem")
}

/// The SDK door of the daemon keeping `state_dir`: `<state dir>/ingest.sock`
/// on Unix; on Windows the named pipe `\\.\pipe\phloemd-ingest-<id>`, named
/// by the one derivation the daemon uses (`crate::wire::door`). `/status
/// ingest_channel` names the same door.
pub fn door_in(state_dir: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(crate::wire::door::ingest_pipe_name(state_dir))
    }
    #[cfg(not(windows))]
    {
        state_dir.join("ingest.sock")
    }
}

/// Where the local daemon's SDK door is: [`door_in`] of
/// [`default_state_dir`] — `~/.phloem/ingest.sock`, or on Windows the
/// daemon's named pipe. The same rule the daemon derives it by.
pub fn default_socket_path() -> std::path::PathBuf {
    door_in(&default_state_dir())
}

/// The transport under a [`Client`]: a Unix socket, or on Windows a named
/// pipe opened as a file. The bytes on it are the same either way.
#[cfg(unix)]
type Door = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Door = std::fs::File;

/// Open the door at `path`: `(writer, reader)` over one connection.
#[cfg(unix)]
fn open_door(path: &std::path::Path) -> std::io::Result<(Door, Door)> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    let reader = stream.try_clone()?;
    Ok((stream, reader))
}

/// Open the named pipe at `path`. Every instance busy (`ERROR_PIPE_BUSY`, a
/// moment between the daemon's accepts) is waited out briefly, not reported.
/// The server may only IDENTIFY this client, never impersonate it.
#[cfg(windows)]
fn open_door(path: &std::path::Path) -> std::io::Result<(Door, Door)> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const ERROR_PIPE_BUSY: i32 = 231;
    const SECURITY_IDENTIFICATION: u32 = 1 << 16;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(path)
        {
            Ok(f) => {
                let reader = f.try_clone()?;
                return Ok((f, reader));
            }
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The local transport's writer: the door, buffered.
pub(crate) type LocalWriter = std::io::BufWriter<Door>;

/// Open the door at `path` (or [`default_socket_path`]), say hello and wait
/// for the daemon's verdict: the encoder over the connection and the input
/// (permit) it publishes under. An empty clock id is refused before the door
/// is opened.
pub(crate) fn connect(
    path: Option<&std::path::Path>,
    producer: &str,
    sdk: &str,
    clock_id: Option<&str>,
) -> Result<(Encoder<LocalWriter>, String), Error> {
    clock_statement(clock_id)?;
    let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_socket_path);
    let (stream, mut reader) = open_door(&path)
        .map_err(|e| Error::Unreachable(format!("no Phloem daemon at {} ({e})", path.display())))?;
    let mut enc = Encoder::new(std::io::BufWriter::new(stream));
    enc.hello_with_clock(producer, sdk, clock_id)?;
    enc.flush()?;
    let reply = read_control(&mut reader)?;
    let reply: Reply = serde_json::from_slice(&reply)
        .map_err(|e| Error::Unreachable(format!("the daemon's reply was not readable: {e}")))?;
    match reply.verdict() {
        Some(Err(refused)) => {
            Err(Error::Refused { why: refused.why, candidate: refused.candidate })
        }
        Some(Ok(welcome)) => Ok((enc, welcome.permit)),
        None => Err(Error::Unreachable("the daemon answered neither welcome nor refused".into())),
    }
}

/// Half-close the connection after the end frame. On Unix the write half is
/// shut now; a named pipe has no half-close, and its handle closes when the
/// client drops.
pub(crate) fn shut_write(_w: &LocalWriter) {
    #[cfg(unix)]
    let _ = _w.get_ref().shutdown(std::net::Shutdown::Write);
}

/// Read one Control frame's payload off the daemon's side of the socket.
fn read_control<R: Read>(r: &mut R) -> Result<Vec<u8>, Error> {
    let mut head = [0u8; crate::wire::frame::FRAME_HEADER_BYTES];
    r.read_exact(&mut head)
        .map_err(|e| Error::Unreachable(format!("the daemon closed before answering ({e})")))?;
    let h = crate::wire::frame::FrameHeader::decode(&head)
        .map_err(|e| Error::Unreachable(format!("the daemon's reply was not a frame: {e}")))?;
    if h.kind != FrameKind::Control {
        return Err(Error::Unreachable("the daemon's reply was not a control frame".into()));
    }
    let mut payload = vec![0u8; h.len as usize];
    r.read_exact(&mut payload)
        .map_err(|e| Error::Unreachable(format!("the daemon's reply was cut short ({e})")))?;
    Ok(payload)
}
