//! The named pipe `\\.\pipe\ScreenControl`. The service sends messages through it to the helper
//! in each session. Messages only go from the service to the helper.
//!
//! Two threads, so the main loop never waits for a helper:
//! - `pipe-accept` makes a pipe instance and waits for a client, then hands it to the writer.
//! - `pipe-write` keeps one client per session and writes the messages from the main loop.
//!
//! Waiting for a client blocks the accept thread. That is fine: when the service stops, the process
//! ends and Windows closes the pipe. Writes have a time limit, so a helper that does not read
//! cannot block the other helpers.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    PIPE_ACCESS_OUTBOUND, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientSessionId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{HSTRING, PCWSTR};

use crate::app::Event;
use crate::helper_protocol::{Message, PIPE_NAME};

/// Who may use the pipe:
/// - SYSTEM (the service) and Administrators: full access, so they can make more pipe instances.
/// - Interactive users (people signed in at the PC): read only. They can connect and read, but
///   cannot write and cannot make their own instances of this pipe to catch the helpers.
///
/// `P` (protected) means nothing is inherited.
const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;IU)";

/// Bytes the pipe holds for a client that has not read yet. A status line is about 120 bytes.
const BUFFER: u32 = 64 * 1024;

/// A write that takes longer means the helper does not read. It is then dropped.
const WRITE_TIMEOUT_MS: u32 = 5000;

/// Sends messages to the helpers. Cheap to use from the main loop: it never waits.
pub struct Helpers {
    tx: Sender<Cmd>,
}

impl Helpers {
    /// Sends the message to the helper in the session, if one is connected.
    pub fn send(&self, session: u32, message: Message) {
        // Fails only if the writer thread has ended. Then there are no helpers.
        let _ = self.tx.send(Cmd::Send(session, message));
    }
}

enum Cmd {
    Connected(u32, Pipe),
    Send(u32, Message),
}

/// Makes the pipe and starts both threads. Each new client is also sent to the main loop as
/// `Event::HelperConnected`, so it can get its status at once.
///
/// Fails if the pipe name is already taken by another program.
pub fn start(events: Sender<Event>) -> Result<Helpers> {
    let security = Security::new()?;
    let first = Pipe::create(&security, true).context(
        "cannot make the pipe (another program may have taken the name), helpers are off",
    )?;
    let (tx, rx) = mpsc::channel();
    let to_writer = tx.clone();
    thread::Builder::new()
        .name("pipe-write".into())
        .spawn(move || write_all(rx))
        .context("cannot start the pipe thread")?;
    thread::Builder::new()
        .name("pipe-accept".into())
        .spawn(move || accept_all(first, &security, &to_writer, &events))
        .context("cannot start the pipe thread")?;
    tracing::info!(pipe = %PIPE_NAME, "pipe ready for helpers");
    Ok(Helpers { tx })
}

fn accept_all(first: Pipe, security: &Security, writer: &Sender<Cmd>, events: &Sender<Event>) {
    let mut next = Some(first);
    loop {
        let pipe = match next
            .take()
            .map_or_else(|| Pipe::create(security, false), Ok)
        {
            Ok(pipe) => pipe,
            Err(e) => {
                tracing::error!(error = format!("{e:#}"), "cannot make a pipe instance");
                thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        if let Err(e) = pipe.wait_for_client() {
            // For example, a client that closed before it was accepted.
            tracing::warn!(error = format!("{e:#}"), "helper did not connect");
            continue;
        }
        let session = match pipe.client_session() {
            Ok(session) => session,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "cannot get the helper's session");
                continue;
            }
        };
        tracing::info!(session, "helper connected");
        if writer.send(Cmd::Connected(session, pipe)).is_err()
            || events.send(Event::HelperConnected { session }).is_err()
        {
            return; // The service is stopping.
        }
    }
}

fn write_all(rx: Receiver<Cmd>) {
    let mut clients = Clients::default();
    for cmd in rx {
        match cmd {
            Cmd::Connected(session, pipe) => {
                if clients.connect(session, pipe) {
                    tracing::info!(session, "older helper in the session replaced");
                }
            }
            Cmd::Send(session, message) => {
                if let Err(e) = clients.send(session, &message) {
                    tracing::warn!(session, error = %e, "cannot write to the helper, dropped");
                }
            }
        }
    }
}

/// One client per session. Pure, so it can be tested with any `Write`.
struct Clients<W> {
    by_session: HashMap<u32, W>,
}

impl<W> Default for Clients<W> {
    fn default() -> Self {
        Clients {
            by_session: HashMap::new(),
        }
    }
}

impl<W: Write> Clients<W> {
    /// Adds a client. Returns true if it replaced an older client in the same session.
    fn connect(&mut self, session: u32, client: W) -> bool {
        self.by_session.insert(session, client).is_some()
    }

    /// Writes the message as one line. A session without a client is not an error.
    /// A client that fails is dropped, and the error is returned.
    fn send(&mut self, session: u32, message: &Message) -> io::Result<()> {
        let Some(client) = self.by_session.get_mut(&session) else {
            return Ok(());
        };
        let result = client.write_all(message.to_line().as_bytes());
        if result.is_err() {
            self.by_session.remove(&session);
        }
        result
    }
}

/// The security descriptor made from `SDDL`. Freed on drop.
struct Security(PSECURITY_DESCRIPTOR);

// Only read after it is made, and used by one thread at a time.
unsafe impl Send for Security {}

impl Security {
    fn new() -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(SDDL),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .context("ConvertStringSecurityDescriptorToSecurityDescriptorW failed")?;
        Ok(Security(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// The server end of one pipe instance, with an event for overlapped I/O. Closed on drop.
struct Pipe {
    handle: HANDLE,
    event: HANDLE,
}

// The handles are only used by the thread that owns the `Pipe`.
unsafe impl Send for Pipe {}

impl Pipe {
    fn create(security: &Security, first: bool) -> Result<Self> {
        let mut open_mode = PIPE_ACCESS_OUTBOUND | FILE_FLAG_OVERLAPPED;
        if first {
            // Fails if the name is taken, so no other program can be the server.
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let attributes = security.attributes();
        let handle = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(PIPE_NAME),
                FILE_FLAGS_AND_ATTRIBUTES(open_mode.0),
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                0,
                0,
                Some(&attributes),
            )
        };
        if handle.is_invalid() {
            return Err(windows::core::Error::from_thread()).context("CreateNamedPipeW failed");
        }
        // Manual reset, as overlapped I/O needs.
        let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(event) => event,
            Err(e) => {
                let _ = unsafe { CloseHandle(handle) };
                return Err(e).context("CreateEventW failed");
            }
        };
        Ok(Pipe { handle, event })
    }

    /// Blocks until a client connects.
    fn wait_for_client(&self) -> Result<()> {
        let mut ov = self.overlapped();
        match unsafe { ConnectNamedPipe(self.handle, Some(&mut ov)) } {
            Ok(()) => return Ok(()),
            // The client connected between `CreateNamedPipeW` and here.
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => return Ok(()),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
            Err(e) => return Err(e).context("ConnectNamedPipe failed"),
        }
        let mut n = 0;
        unsafe { GetOverlappedResult(self.handle, &ov, &mut n, true) }
            .context("ConnectNamedPipe failed")
    }

    fn client_session(&self) -> Result<u32> {
        let mut session = 0;
        unsafe { GetNamedPipeClientSessionId(self.handle, &mut session) }
            .context("GetNamedPipeClientSessionId failed")?;
        Ok(session)
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        }
    }
}

impl Write for Pipe {
    /// Waits up to `WRITE_TIMEOUT_MS`. The pipe buffer is large, so this only waits when the
    /// client has stopped reading.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut ov = self.overlapped();
        if let Err(e) = unsafe { WriteFile(self.handle, Some(buf), None, Some(&mut ov)) }
            && e.code() != ERROR_IO_PENDING.to_hresult()
        {
            return Err(io::Error::other(e));
        }
        if unsafe { WaitForSingleObject(self.event, WRITE_TIMEOUT_MS) } != WAIT_OBJECT_0 {
            // `ov` must stay alive until the cancel is done, so wait for the result below.
            let _ = unsafe { CancelIoEx(self.handle, Some(&ov)) };
        }
        let mut written = 0;
        unsafe { GetOverlappedResult(self.handle, &ov, &mut written, true) }
            .map_err(io::Error::other)?;
        if written == 0 && !buf.is_empty() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // The client can still read what was written, then sees the end of the pipe.
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::Reason;

    /// A client that keeps what is written, or fails.
    #[derive(Default)]
    struct Fake {
        written: Vec<u8>,
        fail: bool,
    }

    impl Write for &mut Fake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn warn(minutes_left: u32) -> Message {
        Message::Warn {
            minutes_left,
            reason: Reason::Budget,
        }
    }

    fn text(fake: &Fake) -> &str {
        std::str::from_utf8(&fake.written).unwrap()
    }

    #[test]
    fn writes_one_line_to_the_right_session() {
        let (mut a, mut b) = (Fake::default(), Fake::default());
        let mut clients = Clients::default();
        assert!(!clients.connect(1, &mut a));
        assert!(!clients.connect(2, &mut b));
        clients.send(2, &warn(5)).unwrap();
        clients.send(2, &warn(1)).unwrap();
        drop(clients);
        assert_eq!(text(&a), "");
        assert_eq!(text(&b), warn(5).to_line() + &warn(1).to_line());
    }

    #[test]
    fn session_without_client_is_not_an_error() {
        let mut clients: Clients<&mut Fake> = Clients::default();
        clients.send(7, &warn(10)).unwrap();
    }

    #[test]
    fn newer_client_replaces_older() {
        let (mut old, mut new) = (Fake::default(), Fake::default());
        let mut clients = Clients::default();
        clients.connect(3, &mut old);
        assert!(clients.connect(3, &mut new));
        clients.send(3, &warn(5)).unwrap();
        drop(clients);
        assert_eq!(text(&old), "");
        assert_eq!(text(&new), warn(5).to_line());
    }

    #[test]
    fn failed_client_is_dropped() {
        let mut broken = Fake {
            fail: true,
            ..Default::default()
        };
        let mut clients = Clients::default();
        clients.connect(4, &mut broken);
        assert!(clients.send(4, &warn(5)).is_err());
        assert!(clients.by_session.is_empty());
        // Later messages to the session are not an error.
        clients.send(4, &warn(1)).unwrap();
    }
}
