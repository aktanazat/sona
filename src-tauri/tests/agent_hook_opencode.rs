#![cfg(target_os = "macos")]

#[path = "../src/agent_hook_wire.rs"]
pub mod wire;

use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tempfile::TempDir;
use wire::{
    atomic_write_json, Agent, AppHeartbeat, AppLease, PolicyProjection, RuntimePaths, WriteMode,
    PROTOCOL_GENERATION, SCHEMA_VERSION,
};

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn records(path: &Path, output: &mut Vec<Value>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            records(&entry.path(), output)?;
        } else if entry.file_type()?.is_file() {
            output.push(serde_json::from_slice(&fs::read(entry.path())?)?);
        }
    }
    Ok(())
}

#[test]
fn an_open_input_closes_when_the_app_stops_refreshing_consent() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    let project = fs::canonicalize(project)?;
    let paths = RuntimePaths::from_root(temp.path().join("sona-agent"), true)?;
    let now_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let instance_id = "0123456789abcdef0123456789abcdef";
    let generation = 7;
    atomic_write_json(
        &paths.app_lock_path(),
        &AppLease {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: instance_id.into(),
            pid: std::process::id(),
            policy_generation: generation,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + 30_000,
        },
        WriteMode::CreateNew,
    )?;
    atomic_write_json(
        &paths.heartbeat_path(),
        &AppHeartbeat {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: instance_id.into(),
            policy_generation: generation,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + 3_000,
        },
        WriteMode::CreateNew,
    )?;
    atomic_write_json(
        &paths.policy_path(),
        &PolicyProjection {
            schema_version: SCHEMA_VERSION,
            protocol_generation: PROTOCOL_GENERATION,
            app_instance_id: instance_id.into(),
            generation,
            master_enabled: true,
            enabled_agents: vec![Agent::OpenCode],
            allowed_project_hashes: vec![wire::project_hash(&project)?],
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + 30_000,
        },
        WriteMode::CreateNew,
    )?;
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_sona-agent-hook"))
            .args(["opencode", "--interactive"])
            .env_clear()
            .env("HOME", temp.path())
            .env("XDG_RUNTIME_DIR", temp.path())
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    // SAFETY: kqueue has no arguments; a successful descriptor is uniquely owned here.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: descriptor is a new, valid descriptor not owned by any other Rust value.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut event = libc::kevent {
        ident: child.0.id().try_into()?,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: the change points to one initialized event; no output events are requested.
    if unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let input = json!({
        "schema_version": 1,
        "instance_id": "deadline-proof",
        "instance_started_at_ms": now_ms,
        "sequence": 1,
        "event": "input_request",
        "session_id": "deadline-session",
        "request_id": "deadline-input",
        "workspace_root": project,
    });
    let stdin = child.0.stdin.as_mut().ok_or("missing command stdin")?;
    serde_json::to_writer(&mut *stdin, &input)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    let deadline = libc::timespec {
        tv_sec: 6,
        tv_nsec: 0,
    };
    // SAFETY: event is writable for one result and deadline remains valid for the call.
    let count = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            std::ptr::null(),
            0,
            &mut event,
            1,
            &deadline,
        )
    };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    assert_eq!(
        count, 1,
        "the hook kept waiting after the app heartbeat expired"
    );
    let status = child.0.wait()?;
    assert!(
        status.success(),
        "the hook failed instead of closing its open input"
    );
    let mut output = Vec::new();
    child
        .0
        .stdout
        .take()
        .ok_or("missing command stdout")?
        .read_to_end(&mut output)?;
    assert!(
        output.is_empty(),
        "expired consent must not emit an agent action"
    );
    let mut documents = Vec::new();
    records(paths.root(), &mut documents)?;
    assert!(
        documents
            .iter()
            .any(|record| record["event"]["event"] == "input_request"),
        "the command must have opened the request before the heartbeat expired"
    );
    assert!(
        documents
            .iter()
            .any(|record| record["outcome"] == "request_cancelled"),
        "the closed request must retain a cancellation receipt"
    );
    Ok(())
}
