use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use super::{decode_event, response, Agent, HookAck, HookRuntime, MAX_INPUT_BYTES};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    accepted: bool,
}

enum Signal {
    Input,
    Files,
    Deadline,
}

struct Signals {
    queue: OwnedFd,
    _directories: Option<[File; 2]>,
}

impl Signals {
    fn new(input: RawFd) -> io::Result<Self> {
        // SAFETY: kqueue takes no pointers and returns a new descriptor.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this fresh, nonnegative descriptor has no other owner.
        let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let signals = Self {
            queue,
            _directories: None,
        };
        signals.register(&[Self::event(input, libc::EVFILT_READ, 0)?])?;
        Ok(signals)
    }

    fn watch(&mut self, responses: &Path, control: &Path) -> io::Result<()> {
        let directories = [File::open(responses)?, File::open(control)?];
        let [response_event, control_event] = directories.each_ref().map(|directory| {
            Self::event(
                directory.as_raw_fd(),
                libc::EVFILT_VNODE,
                libc::NOTE_WRITE | libc::NOTE_RENAME | libc::NOTE_DELETE,
            )
        });
        self.register(&[response_event?, control_event?])?;
        self._directories = Some(directories);
        Ok(())
    }

    fn event(descriptor: RawFd, filter: i16, flags: u32) -> io::Result<libc::kevent> {
        Ok(libc::kevent {
            ident: descriptor.try_into().map_err(|_| invalid_frame())?,
            filter,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: flags,
            data: 0,
            udata: std::ptr::null_mut(),
        })
    }

    fn register(&self, changes: &[libc::kevent]) -> io::Result<()> {
        let count = changes.len().try_into().map_err(|_| invalid_frame())?;
        // SAFETY: the initialized change array remains live for this synchronous call.
        let result = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                changes.as_ptr(),
                count,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn next(&self, remaining_ms: u64) -> io::Result<Signal> {
        if remaining_ms == 0 {
            return Ok(Signal::Deadline);
        }
        let timeout = libc::timespec {
            tv_sec: (remaining_ms / 1_000)
                .try_into()
                .map_err(|_| invalid_frame())?,
            tv_nsec: ((remaining_ms % 1_000) * 1_000_000)
                .try_into()
                .map_err(|_| invalid_frame())?,
        };
        let mut event = Self::event(0, 0, 0)?;
        // SAFETY: both event and timeout remain live; one event is writable.
        let result = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Ok(Signal::Deadline);
        }
        if event.flags & libc::EV_ERROR != 0 {
            return Err(io::Error::from_raw_os_error(
                event.data.try_into().map_err(|_| invalid_frame())?,
            ));
        }
        Ok(if event.filter == libc::EVFILT_READ {
            Signal::Input
        } else {
            Signal::Files
        })
    }
}

enum Frame {
    Complete(Vec<u8>),
    Closed,
    Pending,
}

struct Frames {
    descriptor: RawFd,
    buffered: Vec<u8>,
}

impl Frames {
    fn new(descriptor: RawFd) -> io::Result<Self> {
        // SAFETY: fcntl reads flags from the hook's live pipe descriptor.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the same live descriptor receives its flags with nonblocking reads.
        let result = unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            buffered: Vec::new(),
        })
    }

    fn read(&mut self) -> io::Result<Frame> {
        let mut chunk = [0_u8; 4_096];
        loop {
            // SAFETY: chunk is writable for its full length; read retains no pointer.
            let count =
                unsafe { libc::read(self.descriptor, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::WouldBlock {
                    Ok(Frame::Pending)
                } else {
                    Err(error)
                };
            }
            if count == 0 {
                return Ok(Frame::Closed);
            }
            let count = usize::try_from(count).map_err(|_| invalid_frame())?;
            let bytes = &chunk[..count];
            if self.buffered.len() + bytes.len() > MAX_INPUT_BYTES + 1 {
                return Err(invalid_frame());
            }
            self.buffered.extend_from_slice(bytes);
            if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
                if newline + 1 != bytes.len() {
                    return Err(invalid_frame());
                }
                self.buffered.pop();
                return Ok(Frame::Complete(std::mem::take(&mut self.buffered)));
            }
        }
    }
}

pub(super) fn run(hook: &HookRuntime) -> io::Result<()> {
    let stdin = io::stdin();
    let mut frames = Frames::new(stdin.as_raw_fd())?;
    let mut signals = Signals::new(stdin.as_raw_fd())?;
    let frame_deadline = hook
        .clock
        .now_ms()
        .saturating_add(super::wire::REQUEST_TTL_MS);
    let raw = loop {
        match frames.read()? {
            Frame::Complete(raw) => break raw,
            Frame::Closed => return Ok(()),
            Frame::Pending => {}
        }
        if matches!(
            signals.next(frame_deadline.saturating_sub(hook.clock.now_ms()))?,
            Signal::Deadline
        ) {
            return Ok(());
        }
    };
    let event = decode_event(Agent::OpenCode, &raw).map_err(|_| invalid_frame())?;
    if !event.awaits_response() {
        return Err(invalid_frame());
    }
    let Some((session, request)) = hook.prepare(&event, &raw) else {
        return Ok(());
    };
    signals.watch(session.responses_directory(), hook.paths.root())?;
    session.persist_request(&request)?;
    let mut claimed = None;
    let mut stdout = io::stdout().lock();
    let accepted = loop {
        let Some(authorized_until) = hook.request_authorized_until(&request) else {
            break false;
        };
        if claimed.is_none() {
            if let Some(path) = response::try_claim(&session, &request.invocation_id)
                .map_err(|_| invalid_frame())?
            {
                let encoded = response::encode_claimed(&path, &hook.clock, &request, &event)
                    .map_err(|_| invalid_frame())?;
                claimed = Some(path);
                let Some(encoded) = encoded else {
                    break false;
                };
                stdout.write_all(&encoded)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
            }
        }
        match frames.read()? {
            Frame::Complete(frame) => {
                let receipt: Receipt =
                    serde_json::from_slice(&frame).map_err(|_| invalid_frame())?;
                break claimed.is_some() && receipt.accepted;
            }
            Frame::Closed => break false,
            Frame::Pending => {}
        }
        match signals.next(authorized_until.saturating_sub(hook.clock.now_ms()))? {
            Signal::Deadline => break false,
            Signal::Input | Signal::Files => {}
        }
    };
    let acknowledgement = if accepted {
        HookAck::response_emitted(&request, hook.clock.now_ms())
    } else {
        HookAck::cancelled(&request, hook.clock.now_ms())
    };
    session.persist_ack(&acknowledgement)?;
    if let Some(path) = claimed {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn invalid_frame() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid OpenCode bridge frame")
}
