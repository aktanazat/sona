use crate::input::{self, EnigoState};
use crate::modes::DeliveryPlan;
#[cfg(target_os = "linux")]
use crate::settings::TypingTool;
use crate::settings::{AutoSubmitKey, PasteMethod};
use enigo::{Direction, Enigo, Key, Keyboard};
use log::info;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[cfg(target_os = "linux")]
use crate::utils::{is_gnome_wayland, is_kde_wayland, is_wayland};

fn with_enigo<T>(
    app_handle: &AppHandle,
    f: impl FnOnce(&mut Enigo) -> Result<T, String>,
) -> Result<T, String> {
    let enigo_state = app_handle
        .try_state::<EnigoState>()
        .ok_or("Enigo state not initialized")?;
    let mut enigo = enigo_state
        .0
        .lock()
        .map_err(|e| format!("Failed to lock Enigo: {}", e))?;
    f(&mut enigo)
}

pub(crate) fn write_text_to_clipboard(app_handle: &AppHandle, text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if is_wayland() && is_wl_copy_available() {
        info!("Using wl-copy for clipboard write on Wayland");
        return write_clipboard_via_wl_copy(text);
    }

    app_handle
        .clipboard()
        .write_text(text)
        .map_err(|e| format!("Failed to write to clipboard: {}", e))
}

/// Attempts to send a key combination using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_send_key_combo_linux(paste_method: &PasteMethod) -> Result<bool, String> {
    if is_wayland() {
        // Wayland: prefer wtype, then dotool, then ydotool. wtype needs the
        // zwp_virtual_keyboard_manager_v1 protocol, which neither KWin (KDE)
        // nor Mutter (GNOME) implements, so on those it fails and the cascade
        // has to fall through instead of propagating the error.
        if !is_kde_wayland() && !is_gnome_wayland() && is_wtype_available() {
            info!("Using wtype for key combo");
            send_key_combo_via_wtype(paste_method)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for key combo");
            send_key_combo_via_dotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for key combo");
            send_key_combo_via_xdotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Attempts to type text directly using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_direct_typing_linux(text: &str, preferred_tool: TypingTool) -> Result<bool, String> {
    // If user specified a tool, try only that one
    if preferred_tool != TypingTool::Auto {
        return match preferred_tool {
            TypingTool::Wtype if is_wtype_available() => {
                info!("Using user-specified wtype");
                type_text_via_wtype(text)?;
                Ok(true)
            }
            TypingTool::Kwtype if is_kwtype_available() => {
                info!("Using user-specified kwtype");
                type_text_via_kwtype(text)?;
                Ok(true)
            }
            TypingTool::Dotool if is_dotool_available() => {
                info!("Using user-specified dotool");
                type_text_via_dotool(text)?;
                Ok(true)
            }
            TypingTool::Ydotool if is_ydotool_available() => {
                info!("Using user-specified ydotool");
                type_text_via_ydotool(text)?;
                Ok(true)
            }
            TypingTool::Xdotool if is_xdotool_available() => {
                info!("Using user-specified xdotool");
                type_text_via_xdotool(text)?;
                Ok(true)
            }
            _ => Err(format!(
                "Typing tool {:?} is not available on this system",
                preferred_tool
            )),
        };
    }

    // Auto mode - existing fallback chain
    if is_wayland() {
        // KDE Wayland: prefer kwtype (uses KDE Fake Input protocol, supports umlauts)
        if is_kde_wayland() && is_kwtype_available() {
            info!("Using kwtype for direct text input on KDE Wayland");
            type_text_via_kwtype(text)?;
            return Ok(true);
        }
        // Wayland: prefer wtype, then dotool, then ydotool. Skipped on KDE and
        // GNOME for the same missing virtual-keyboard protocol as above.
        if !is_kde_wayland() && !is_gnome_wayland() && is_wtype_available() {
            info!("Using wtype for direct text input");
            type_text_via_wtype(text)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for direct text input");
            type_text_via_dotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for direct text input");
            type_text_via_xdotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Returns the list of available typing tools on this system.
/// Always includes "auto" as the first entry.
#[cfg(target_os = "linux")]
pub fn get_available_typing_tools() -> Vec<String> {
    let mut tools = vec!["auto".to_string()];
    if is_wtype_available() {
        tools.push("wtype".to_string());
    }
    if is_kwtype_available() {
        tools.push("kwtype".to_string());
    }
    if is_dotool_available() {
        tools.push("dotool".to_string());
    }
    if is_ydotool_available() {
        tools.push("ydotool".to_string());
    }
    if is_xdotool_available() {
        tools.push("xdotool".to_string());
    }
    tools
}

/// Check if wtype is available (Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_wtype_available() -> bool {
    Command::new("which")
        .arg("wtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if dotool is available (another Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_dotool_available() -> bool {
    Command::new("which")
        .arg("dotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YdotoolKeySyntax {
    Symbolic,
    RawKeycodes,
}

#[cfg(target_os = "linux")]
const YDOTOOL_UNKNOWN_HELP_FALLBACK: YdotoolKeySyntax = YdotoolKeySyntax::RawKeycodes;

#[cfg(target_os = "linux")]
static YDOTOOL_KEY_SYNTAX: OnceLock<YdotoolKeySyntax> = OnceLock::new();

/// Classifies `ydotool key --help` output without relying on version or distro metadata.
#[cfg(target_os = "linux")]
fn classify_ydotool_key_syntax(help: &str) -> Option<YdotoolKeySyntax> {
    let help = help.to_ascii_lowercase();

    // Check modern markers first in case a future help message mentions the legacy syntax.
    if help.contains("syntax: <keycode>:<pressed>")
        || help.contains("[keycodes]")
        || help.contains("using raw keycodes")
    {
        Some(YdotoolKeySyntax::RawKeycodes)
    } else if help.contains("separated by plus (+)")
        || (help.contains("<key sequence>") && help.contains("alt+r"))
    {
        Some(YdotoolKeySyntax::Symbolic)
    } else {
        None
    }
}

/// Detects and caches a recognized ydotool key syntax. Unknown or failed probes are not cached,
/// allowing a transient daemon or PATH problem to recover on a later paste attempt.
#[cfg(target_os = "linux")]
fn detect_ydotool_key_syntax() -> YdotoolKeySyntax {
    if let Some(syntax) = YDOTOOL_KEY_SYNTAX.get() {
        return *syntax;
    }

    match Command::new("ydotool").args(["key", "--help"]).output() {
        Ok(output) => {
            // ydotool 0.x writes help to stderr and its exit status varies by build.
            let mut help = String::from_utf8_lossy(&output.stdout).into_owned();
            if !help.is_empty() && !output.stderr.is_empty() {
                help.push('\n');
            }
            help.push_str(&String::from_utf8_lossy(&output.stderr));

            if let Some(syntax) = classify_ydotool_key_syntax(&help) {
                *YDOTOOL_KEY_SYNTAX.get_or_init(|| {
                    info!("Detected ydotool key syntax: {:?}", syntax);
                    syntax
                })
            } else {
                // Preserve Sona's existing behavior and compatibility with current ydotool.
                log::warn!(
                    "Could not recognize ydotool key --help output (exit status {:?}); using raw-keycode syntax",
                    output.status.code()
                );
                YDOTOOL_UNKNOWN_HELP_FALLBACK
            }
        }
        Err(error) => {
            log::warn!(
                "Could not query ydotool key syntax: {}; using raw-keycode syntax",
                error
            );
            YDOTOOL_UNKNOWN_HELP_FALLBACK
        }
    }
}

/// Check if ydotool is available (uinput-based, works on both Wayland and X11)
#[cfg(target_os = "linux")]
fn is_ydotool_available() -> bool {
    Command::new("which")
        .arg("ydotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_xdotool_available() -> bool {
    Command::new("which")
        .arg("xdotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if kwtype is available (KDE Wayland virtual keyboard input tool)
#[cfg(target_os = "linux")]
fn is_kwtype_available() -> bool {
    Command::new("which")
        .arg("kwtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if wl-copy is available (Wayland clipboard tool)
#[cfg(target_os = "linux")]
fn is_wl_copy_available() -> bool {
    Command::new("which")
        .arg("wl-copy")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Type text directly via wtype on Wayland.
#[cfg(target_os = "linux")]
fn type_text_via_wtype(text: &str) -> Result<(), String> {
    let output = Command::new("wtype")
        .arg("--") // Protect against text starting with -
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via xdotool on X11.
#[cfg(target_os = "linux")]
fn type_text_via_xdotool(text: &str) -> Result<(), String> {
    let output = Command::new("xdotool")
        .arg("type")
        .arg("--clearmodifiers")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    // `--clearmodifiers` restores the modifiers that were held when xdotool
    // started. If the user releases one while xdotool is typing, that synthetic
    // restore can leave the modifier latched on the XTEST keyboard (#1817).
    // Release both sides of Sona's supported push-style modifiers to clear any
    // stale restore. Lock keys are intentionally excluded because key events
    // toggle them.
    //
    // The release is unconditional, so it can make a modifier that is still
    // physically held appear released until the next physical event. This is
    // preferable to leaving a synthetic modifier latched system-wide.
    //
    // Clean up before checking the typing status because xdotool may have
    // changed modifier state before returning an error. Cleanup remains
    // best-effort because the text may already have been partially or fully
    // typed, but failures are logged so they can be diagnosed.
    match Command::new("xdotool")
        .arg("keyup")
        .args([
            "Control_L",
            "Control_R",
            "Shift_L",
            "Shift_R",
            "Alt_L",
            "Alt_R",
            "Super_L",
            "Super_R",
        ])
        .output()
    {
        Ok(cleanup_output) if !cleanup_output.status.success() => {
            let stderr = String::from_utf8_lossy(&cleanup_output.stderr);
            log::warn!(
                "xdotool modifier cleanup failed with status {:?}: {}",
                cleanup_output.status.code(),
                stderr.trim()
            );
        }
        Err(error) => log::warn!("Failed to execute xdotool modifier cleanup: {}", error),
        _ => {}
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via dotool (works on both Wayland and X11 via uinput).
#[cfg(target_os = "linux")]
fn type_text_via_dotool(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("dotool")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn dotool: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        // dotool uses "type <text>" command
        writeln!(stdin, "type {}", text)
            .map_err(|e| format!("Failed to write to dotool stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for dotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("dotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via ydotool (uinput-based, requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn type_text_via_ydotool(text: &str) -> Result<(), String> {
    let output = Command::new("ydotool")
        .arg("type")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via kwtype (KDE Wayland virtual keyboard, uses KDE Fake Input protocol).
#[cfg(target_os = "linux")]
fn type_text_via_kwtype(text: &str) -> Result<(), String> {
    let output = Command::new("kwtype")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute kwtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("kwtype failed: {}", stderr));
    }

    Ok(())
}

/// Write text to clipboard via wl-copy (Wayland clipboard tool).
/// Uses Stdio::null() to avoid blocking on repeated calls — wl-copy forks a
/// daemon that inherits piped fds, causing read_to_end to hang indefinitely.
#[cfg(target_os = "linux")]
fn write_clipboard_via_wl_copy(text: &str) -> Result<(), String> {
    use std::process::Stdio;
    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute wl-copy: {}", e))?;

    if !status.success() {
        return Err("wl-copy failed".into());
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via wtype on Wayland.
#[cfg(target_os = "linux")]
fn send_key_combo_via_wtype(paste_method: &PasteMethod) -> Result<(), String> {
    let args: Vec<&str> = match paste_method {
        PasteMethod::CtrlV => vec!["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        PasteMethod::ShiftInsert => vec!["-M", "shift", "-k", "Insert", "-m", "shift"],
        PasteMethod::CtrlShiftV => vec![
            "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
        ],
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("wtype")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via dotool.
#[cfg(target_os = "linux")]
fn send_key_combo_via_dotool(paste_method: &PasteMethod) -> Result<(), String> {
    let command;
    match paste_method {
        PasteMethod::CtrlV => command = "echo key ctrl+v | dotool",
        PasteMethod::ShiftInsert => command = "echo key shift+insert | dotool",
        PasteMethod::CtrlShiftV => command = "echo key ctrl+shift+v | dotool",
        _ => return Err("Unsupported paste method".into()),
    }
    use std::process::Stdio;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute dotool: {}", e))?;
    if !status.success() {
        return Err("dotool failed".into());
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn ydotool_key_args(
    paste_method: &PasteMethod,
    syntax: YdotoolKeySyntax,
) -> Result<&'static [&'static str], String> {
    let args = match (paste_method, syntax) {
        (PasteMethod::CtrlV, YdotoolKeySyntax::Symbolic) => &["key", "ctrl+v"][..],
        (PasteMethod::CtrlShiftV, YdotoolKeySyntax::Symbolic) => &["key", "ctrl+shift+v"][..],
        (PasteMethod::ShiftInsert, YdotoolKeySyntax::Symbolic) => &["key", "shift+insert"][..],
        (PasteMethod::CtrlV, YdotoolKeySyntax::RawKeycodes) => {
            &["key", "29:1", "47:1", "47:0", "29:0"][..]
        }
        (PasteMethod::CtrlShiftV, YdotoolKeySyntax::RawKeycodes) => {
            &["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"][..]
        }
        (PasteMethod::ShiftInsert, YdotoolKeySyntax::RawKeycodes) => {
            &["key", "42:1", "110:1", "110:0", "42:0"][..]
        }
        _ => return Err("Unsupported paste method".into()),
    };

    Ok(args)
}

/// Send a key combination (e.g., Ctrl+V) via ydotool (requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn send_key_combo_via_ydotool(paste_method: &PasteMethod) -> Result<(), String> {
    let syntax = detect_ydotool_key_syntax();
    let args = ydotool_key_args(paste_method, syntax)?;

    let output = Command::new("ydotool")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via xdotool on X11.
#[cfg(target_os = "linux")]
fn send_key_combo_via_xdotool(paste_method: &PasteMethod) -> Result<(), String> {
    let key_combo = match paste_method {
        PasteMethod::CtrlV => "ctrl+v",
        PasteMethod::CtrlShiftV => "ctrl+shift+v",
        PasteMethod::ShiftInsert => "shift+Insert",
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("xdotool")
        .arg("key")
        .arg("--clearmodifiers")
        .arg(key_combo)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// How long an external delivery script may run before Sona gives up on it.
/// Delivery sits on the user's critical path, so a script that has not
/// finished pasting by now is wedged; waiting longer only hides the failure.
const EXTERNAL_SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the delivery thread checks whether the script has exited.
const EXTERNAL_SCRIPT_POLL: Duration = Duration::from_millis(10);

/// Why an external delivery script did not deliver.
#[derive(Debug)]
enum ExternalScriptError {
    /// Refused before the script ran, so nothing reached the target and the
    /// caller may still try another route.
    Refused(String),
    /// The script ran. Whether it pasted before failing is unknowable, so this
    /// is never retried.
    Failed(String),
}

/// Pastes text by invoking an external script.
///
/// The script receives the text on stdin. It is never passed on the command
/// line, where any local process can read it out of the process table, and
/// never through a shell, which would make the transcript executable text.
fn paste_via_external_script(text: &str, script_path: &str) -> Result<(), ExternalScriptError> {
    run_external_script(text, script_path, EXTERNAL_SCRIPT_TIMEOUT)
}

fn run_external_script(
    text: &str,
    script_path: &str,
    timeout: Duration,
) -> Result<(), ExternalScriptError> {
    verify_external_script(Path::new(script_path)).map_err(ExternalScriptError::Refused)?;
    info!("Pasting via external script: {}", script_path);

    // Do not capture the script's stdout or stderr. Wayland clipboard helpers
    // such as wl-copy fork a background selection daemon that inherits those
    // file descriptors; reading them to EOF would block until the clipboard
    // selection is replaced instead of returning when the script itself exits.
    let mut child = Command::new(script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ExternalScriptError::Refused(format!(
                "Failed to execute external script '{}': {}",
                script_path, error
            ))
        })?;

    // Write on a separate thread: a script that never reads stdin would
    // otherwise wedge delivery once the pipe buffer fills. Whatever ends the
    // child closes the pipe, so this thread always finishes.
    if let Some(mut stdin) = child.stdin.take() {
        let payload = text.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(payload.as_bytes());
        });
    }

    let status = match wait_bounded(&mut child, timeout) {
        Ok(status) => status,
        Err(error) => {
            return Err(ExternalScriptError::Failed(format!(
                "External script '{}' {}",
                script_path, error
            )))
        }
    };

    if !status.success() {
        return Err(ExternalScriptError::Failed(format!(
            "External script '{}' failed with exit code {:?}",
            script_path,
            status.code()
        )));
    }

    Ok(())
}

/// Wait for `child`, killing it once `timeout` elapses.
fn wait_bounded(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => return Err(format!("could not be waited for: {error}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("did not finish within {:?}; killed", timeout));
        }
        std::thread::sleep(EXTERNAL_SCRIPT_POLL);
    }
}

/// Refuse a delivery script that the user is not the only one able to change.
/// Sona runs it with the user's own privileges after every transcription, so a
/// script any local account can rewrite is a standing hole, not a paste method.
#[cfg(unix)]
fn verify_external_script(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "External script '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "External script '{}' is not a regular file",
            path.display()
        ));
    }

    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(format!(
            "External script '{}' is not executable",
            path.display()
        ));
    }
    if mode & 0o002 != 0 {
        return Err(format!(
            "External script '{}' is world-writable",
            path.display()
        ));
    }

    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let parent_mode = fs::metadata(parent)
        .map_err(|error| {
            format!(
                "External script directory '{}' cannot be read: {error}",
                parent.display()
            )
        })?
        .permissions()
        .mode();
    // A world-writable directory lets any local account replace the script,
    // unless the sticky bit reserves that right for the owner (as on /tmp).
    if parent_mode & 0o002 != 0 && parent_mode & 0o1000 == 0 {
        return Err(format!(
            "External script '{}' is in a world-writable directory",
            parent.display()
        ));
    }

    Ok(())
}

/// Windows has no mode bits to check, so the guard is existence and file kind.
#[cfg(not(unix))]
fn verify_external_script(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "External script '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "External script '{}' is not a regular file",
            path.display()
        ));
    }
    Ok(())
}

/// Types text directly by simulating individual key presses.
fn paste_direct(
    text: &str,
    app_handle: &AppHandle,
    #[cfg(target_os = "linux")] typing_tool: TypingTool,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if try_direct_typing_linux(text, typing_tool)? {
            return Ok(());
        }
        info!("Falling back to enigo for direct text input");
    }

    with_enigo(app_handle, |enigo| input::paste_text_direct(enigo, text))
}

pub(crate) fn send_return_key(enigo: &mut Enigo, key_type: AutoSubmitKey) -> Result<(), String> {
    match key_type {
        AutoSubmitKey::Enter => {
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
        }
        AutoSubmitKey::CtrlEnter => {
            enigo
                .key(Key::Control, Direction::Press)
                .map_err(|e| format!("Failed to press Control key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Control, Direction::Release)
                .map_err(|e| format!("Failed to release Control key: {}", e))?;
        }
        AutoSubmitKey::CmdEnter => {
            enigo
                .key(Key::Meta, Direction::Press)
                .map_err(|e| format!("Failed to press Meta/Cmd key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Meta, Direction::Release)
                .map_err(|e| format!("Failed to release Meta/Cmd key: {}", e))?;
        }
    }

    Ok(())
}

/// Sends the mode's submit key. Delivery decides whether a route earns this;
/// this function only owns the input lock.
pub(crate) fn send_auto_submit(
    app_handle: &AppHandle,
    key_type: AutoSubmitKey,
) -> Result<(), String> {
    with_enigo(app_handle, |enigo| send_return_key(enigo, key_type))
}

/// What the non-Accessibility delivery path can prove about dispatch, and what
/// the caller still owes. Keyboard and clipboard APIs do not acknowledge
/// target-app insertion, so once a chord or typing request begins the only
/// safe state is unconfirmed dispatch.
#[derive(Debug)]
pub(crate) enum ClipboardDispatch {
    /// Nothing reached an input backend. This is the only state from which a
    /// caller may attempt another route.
    DefinitelyNotDispatched(String),
    /// The insertion request left Sona without a local error, so the caller
    /// owes this mode's finishing steps.
    Dispatched,
    /// A backend was invoked and then failed, so Sona cannot tell whether the
    /// text landed. Never retried, and never followed by a submit key that
    /// could commit stale content into the target.
    DispatchedWithBackendError,
    /// The reliable clipboard transaction dispatched and already performed the
    /// mode's finishing, which it times against the target's paste receipt
    /// instead of a fixed delay.
    DispatchedAndFinished,
}

/// A configured paste chord the running platform has no key for. macOS is the
/// case that exists: it has no Insert key, and the keycode the Shift+Insert
/// sender carries for every non-Windows target is X11's Insert (0x76), which
/// macOS reads as F4 — so the chord pastes nothing and presses a Launchpad or
/// Spotlight key at the user's frontmost application instead. Refusing here,
/// before the transcript reaches the clipboard, keeps it the clean miss the
/// caller is allowed to report.
fn unsupported_paste_method(method: PasteMethod) -> Option<&'static str> {
    match method {
        #[cfg(target_os = "macos")]
        PasteMethod::ShiftInsert => Some("Shift+Insert is not a paste chord on macOS"),
        _ => None,
    }
}

/// Delivers the already-composed text with settings copied into a RunPlan,
/// never reading the mutable store. The caller may fall back only from
/// DefinitelyNotDispatched.
pub(crate) fn paste_frozen(
    text: &str,
    app_handle: &AppHandle,
    settings: &DeliveryPlan,
) -> ClipboardDispatch {
    if let Some(reason) = unsupported_paste_method(settings.paste_method) {
        return ClipboardDispatch::DefinitelyNotDispatched(reason.to_string());
    }

    match settings.paste_method {
        PasteMethod::None => ClipboardDispatch::DefinitelyNotDispatched(
            "Text delivery is disabled for this mode".to_string(),
        ),
        PasteMethod::Direct => {
            // An input backend can fail after accepting part of a string. Once
            // it has been invoked we must not retry through the clipboard. It
            // has the same unconfirmed-dispatch certainty as a successful
            // direct request, so delivery still performs this method's
            // configured finishing.
            if let Err(error) = paste_direct(
                text,
                app_handle,
                #[cfg(target_os = "linux")]
                settings.typing_tool,
            ) {
                log::warn!("Direct typing may have been dispatched before failure: {error}");
            }
            ClipboardDispatch::Dispatched
        }
        PasteMethod::ExternalScript => {
            let Some(script_path) = settings
                .external_script_path
                .as_deref()
                .filter(|path| !path.is_empty())
            else {
                return ClipboardDispatch::DefinitelyNotDispatched(
                    "External delivery script is not configured".to_string(),
                );
            };
            // A script that starts can have sent input before it reports a
            // non-zero status, so its result is intentionally not retried. A
            // script the guard refused never ran, so that stays a clean miss
            // the caller can report and route around.
            match paste_via_external_script(text, script_path) {
                Ok(()) => ClipboardDispatch::Dispatched,
                Err(ExternalScriptError::Refused(error)) => {
                    log::warn!("External delivery script was refused: {error}");
                    ClipboardDispatch::DefinitelyNotDispatched(error)
                }
                Err(ExternalScriptError::Failed(error)) => {
                    log::warn!("External delivery script reported a failure: {error}");
                    ClipboardDispatch::DispatchedWithBackendError
                }
            }
        }
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert => {
            paste_via_clipboard_frozen(text, app_handle, settings)
        }
    }
}

fn clipboard_still_ours(current_text: Option<&str>, temporary_text: &str) -> bool {
    current_text == Some(temporary_text)
}

fn paste_via_clipboard_frozen(
    text: &str,
    app_handle: &AppHandle,
    settings: &DeliveryPlan,
) -> ClipboardDispatch {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if settings.reliable_paste {
        // The reliable path's contract is precise: Err occurs before it has
        // published or injected, so the legacy path remains a valid first
        // dispatch. Ok has injected its chord and must end this delivery.
        match with_enigo(app_handle, |enigo| {
            crate::paste_tx::try_reliable_paste(
                text,
                app_handle,
                &settings.paste_method,
                enigo,
                settings.auto_submit,
                settings.auto_submit_key,
                settings.clipboard_handling,
            )
        }) {
            Ok(()) => return ClipboardDispatch::DispatchedAndFinished,
            Err(error) => log::warn!("Reliable paste unavailable ({error}); using legacy paste"),
        }
    }

    let clipboard = app_handle.clipboard();
    let saved_text = clipboard.read_text().ok().filter(|value| !value.is_empty());
    let saved_image = if saved_text.is_none() {
        clipboard.read_image().ok().map(|image| image.to_owned())
    } else {
        None
    };

    if let Err(error) = write_text_to_clipboard(app_handle, text) {
        return ClipboardDispatch::DefinitelyNotDispatched(error);
    }
    std::thread::sleep(Duration::from_millis(settings.paste_delay_ms));

    // Once the input backend is called, errors are not evidence that no event
    // reached the target. Restore the temporary clipboard only if it is still
    // exactly the text Sona wrote; a user copy wins every race.
    let injection = {
        #[cfg(target_os = "linux")]
        let native = try_send_key_combo_linux(&settings.paste_method);
        #[cfg(not(target_os = "linux"))]
        let native: Result<bool, String> = Ok(false);

        match native {
            Ok(true) => Ok(()),
            Ok(false) => with_enigo(app_handle, |enigo| match settings.paste_method {
                PasteMethod::CtrlV => input::send_paste_ctrl_v(enigo, 100),
                PasteMethod::CtrlShiftV => input::send_paste_ctrl_shift_v(enigo, 100),
                PasteMethod::ShiftInsert => input::send_paste_shift_insert(enigo, 100),
                _ => Err("Invalid clipboard paste method".to_string()),
            }),
            Err(error) => Err(error),
        }
    };

    std::thread::sleep(Duration::from_millis(settings.paste_delay_after_ms));
    // A text read is the portable ownership proof for the legacy path. If it
    // cannot prove the temporary value remains ours (including a new image), it
    // deliberately leaves the user’s newer clipboard untouched.
    if clipboard_still_ours(clipboard.read_text().ok().as_deref(), text) {
        if let Some(saved_text) = saved_text {
            let _ = write_text_to_clipboard(app_handle, &saved_text);
        } else if let Some(saved_image) = saved_image {
            let _ = clipboard.write_image(&saved_image);
        } else {
            let _ = clipboard.clear();
        }
    }

    match injection {
        Ok(()) => ClipboardDispatch::Dispatched,
        Err(error) => {
            log::warn!("Paste input may have been dispatched before failure: {error}");
            ClipboardDispatch::DispatchedWithBackendError
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every chord the settings offer has to either exist on this platform or
    /// be refused before the transcript is published. A chord that resolves to
    /// an unrelated key is the worst of the three: it takes the clipboard,
    /// presses something else, and reports a dispatch.
    #[test]
    fn only_chords_this_platform_can_send_reach_the_clipboard() {
        for method in [
            PasteMethod::CtrlV,
            PasteMethod::CtrlShiftV,
            PasteMethod::Direct,
            PasteMethod::ExternalScript,
            PasteMethod::None,
        ] {
            assert_eq!(
                unsupported_paste_method(method),
                None,
                "supported method {method:?}"
            );
        }

        // macOS has no Insert key; every other target has one.
        assert_eq!(
            unsupported_paste_method(PasteMethod::ShiftInsert).is_some(),
            cfg!(target_os = "macos")
        );
    }

    #[cfg(target_os = "linux")]
    const YDOTOOL_0_1_8_HELP: &str = r#"
Usage: key [--delay <ms>] [--key-delay <ms>] [--repeat <times>] [--repeat-delay <ms>] <key sequence> ...
Each key sequence can be any number of modifiers and keys, separated by plus (+)
For example: alt+r Alt+F4 CTRL+alt+f3 aLT+1+2+3 ctrl+Backspace
"#;

    #[cfg(target_os = "linux")]
    const YDOTOOL_1_0_4_HELP: &str = r#"
Usage: key [OPTION]... [KEYCODES]...
Since there's no way to know how many keyboard layouts are there in the world,
we're using raw keycodes now.
Syntax: <keycode>:<pressed>
e.g. 28:1 28:0 means pressing on the Enter button on a standard US keyboard.
"#;

    #[cfg(target_os = "linux")]
    #[test]
    fn classifies_ydotool_0_1_8_symbolic_help() {
        assert_eq!(
            classify_ydotool_key_syntax(YDOTOOL_0_1_8_HELP),
            Some(YdotoolKeySyntax::Symbolic)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classifies_ydotool_1_0_4_raw_keycode_help() {
        assert_eq!(
            classify_ydotool_key_syntax(YDOTOOL_1_0_4_HELP),
            Some(YdotoolKeySyntax::RawKeycodes)
        );
    }


    #[cfg(target_os = "linux")]
    #[test]
    fn generates_symbolic_ydotool_arguments_for_all_paste_methods() {
        assert_eq!(
            ydotool_key_args(&PasteMethod::CtrlV, YdotoolKeySyntax::Symbolic).unwrap(),
            ["key", "ctrl+v"]
        );
        assert_eq!(
            ydotool_key_args(&PasteMethod::CtrlShiftV, YdotoolKeySyntax::Symbolic).unwrap(),
            ["key", "ctrl+shift+v"]
        );
        assert_eq!(
            ydotool_key_args(&PasteMethod::ShiftInsert, YdotoolKeySyntax::Symbolic).unwrap(),
            ["key", "shift+insert"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn generates_raw_ydotool_arguments_for_all_paste_methods() {
        assert_eq!(
            ydotool_key_args(&PasteMethod::CtrlV, YdotoolKeySyntax::RawKeycodes).unwrap(),
            ["key", "29:1", "47:1", "47:0", "29:0"]
        );
        assert_eq!(
            ydotool_key_args(&PasteMethod::CtrlShiftV, YdotoolKeySyntax::RawKeycodes).unwrap(),
            ["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
        );
        assert_eq!(
            ydotool_key_args(&PasteMethod::ShiftInsert, YdotoolKeySyntax::RawKeycodes).unwrap(),
            ["key", "42:1", "110:1", "110:0", "42:0"]
        );
    }

    #[test]
    fn clipboard_restore_requires_proof_of_temporary_ownership() {
        assert!(clipboard_still_ours(
            Some("Sona temporary text"),
            "Sona temporary text"
        ));
        assert!(!clipboard_still_ours(
            Some("user copied something newer"),
            "Sona temporary text"
        ));
        assert!(!clipboard_still_ours(None, "Sona temporary text"));
    }

    #[cfg(unix)]
    mod external_script {
        use super::super::{run_external_script, verify_external_script, ExternalScriptError};
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        /// A directory of its own per test, so permission changes never race.
        struct Sandbox {
            root: PathBuf,
        }

        impl Sandbox {
            fn new(name: &str) -> Self {
                let unique = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after the unix epoch")
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "sona-external-script-{}-{}-{}",
                    name,
                    std::process::id(),
                    unique
                ));
                fs::create_dir_all(&root).expect("create sandbox");
                Self { root }
            }

            fn script(&self, body: &str, mode: u32) -> PathBuf {
                let path = self.root.join("deliver.sh");
                fs::write(&path, body).expect("write script");
                fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                    .expect("set script mode");
                path
            }
        }

        impl Drop for Sandbox {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700));
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        fn path_str(path: &Path) -> &str {
            path.to_str().expect("utf-8 script path")
        }

        /// The message of a failure that happened after the script started.
        fn run_failure(result: Result<(), ExternalScriptError>) -> String {
            match result {
                Err(ExternalScriptError::Failed(error)) => error,
                other => panic!("expected a run failure, got {other:?}"),
            }
        }

        #[test]
        fn transcript_reaches_the_script_on_stdin_and_never_on_argv() {
            let sandbox = Sandbox::new("stdin");
            let stdin_out = sandbox.root.join("stdin.txt");
            let argv_out = sandbox.root.join("argv.txt");
            let script = sandbox.script(
                &format!(
                    "#!/bin/sh\ncat > {}\nprintf '%s' \"$*\" > {}\n",
                    stdin_out.display(),
                    argv_out.display()
                ),
                0o700,
            );

            run_external_script("hello tail", path_str(&script), Duration::from_secs(5))
                .expect("script runs");

            assert_eq!(fs::read_to_string(&stdin_out).unwrap(), "hello tail");
            assert_eq!(fs::read_to_string(&argv_out).unwrap(), "");
        }

        #[test]
        fn script_returns_without_waiting_for_its_background_children() {
            let sandbox = Sandbox::new("nonblocking");
            let script = sandbox.script("#!/bin/sh\nsleep 5 &\nexit 0\n", 0o700);
            let script_for_thread = script.clone();

            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                let result = run_external_script(
                    "text",
                    script_for_thread.to_str().expect("utf-8 script path"),
                    Duration::from_secs(5),
                );
                let _ = sender.send(result);
            });

            let result = receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("delivery must not wait for a child that inherited the script's stdio");
            assert!(result.is_ok(), "{result:?}");
        }

        #[test]
        fn a_wedged_script_is_killed_at_the_deadline() {
            let sandbox = Sandbox::new("timeout");
            let marker = sandbox.root.join("survived.txt");
            let script = sandbox.script(
                &format!("#!/bin/sh\nsleep 1\ntouch {}\n", marker.display()),
                0o700,
            );

            let started = Instant::now();
            let error = run_failure(run_external_script(
                "text",
                path_str(&script),
                Duration::from_millis(100),
            ));
            assert!(error.contains("did not finish"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(1));

            thread::sleep(Duration::from_millis(1500));
            assert!(
                !marker.exists(),
                "the script kept running after the deadline"
            );
        }

        #[test]
        fn a_failing_script_reports_its_exit_code() {
            let sandbox = Sandbox::new("exit-code");
            let script = sandbox.script("#!/bin/sh\nexit 3\n", 0o700);

            let error = run_failure(run_external_script(
                "text",
                path_str(&script),
                Duration::from_secs(5),
            ));
            assert!(error.contains("exit code Some(3)"), "{error}");
        }

        #[test]
        fn the_guard_refuses_scripts_the_user_does_not_solely_control() {
            let sandbox = Sandbox::new("guard");

            let missing = sandbox.root.join("absent.sh");
            assert!(verify_external_script(&missing)
                .expect_err("missing")
                .contains("cannot be read"));

            assert!(verify_external_script(&sandbox.root)
                .expect_err("directory")
                .contains("not a regular file"));

            let not_executable = sandbox.script("#!/bin/sh\nexit 0\n", 0o600);
            assert!(verify_external_script(&not_executable)
                .expect_err("not executable")
                .contains("not executable"));

            let world_writable = sandbox.script("#!/bin/sh\nexit 0\n", 0o777);
            assert!(verify_external_script(&world_writable)
                .expect_err("world-writable")
                .contains("world-writable"));

            let safe = sandbox.script("#!/bin/sh\nexit 0\n", 0o700);
            assert!(verify_external_script(&safe).is_ok());
        }

        #[test]
        fn the_guard_refuses_a_script_in_a_world_writable_directory() {
            let sandbox = Sandbox::new("open-dir");
            let script = sandbox.script("#!/bin/sh\nexit 0\n", 0o700);
            fs::set_permissions(&sandbox.root, fs::Permissions::from_mode(0o777))
                .expect("open the directory up");

            let error = verify_external_script(&script).expect_err("world-writable directory");
            assert!(error.contains("world-writable directory"), "{error}");
        }
    }
}
