//! Clipboard copy backend for the TUI.
//!
//! Copies text to the system clipboard using a fallback strategy:
//!
//! 1. **SSH session**: use tmux clipboard or OSC 52 (reaches the *local* terminal).
//! 2. **Local session**: try `arboard` (native clipboard) first, fall back to OSC 52.
//!
//! On Linux/X11 and some Wayland compositors, the clipboard-owning process must stay
//! alive for the content to remain pasteable. `ClipboardLease` keeps the
//! `arboard::Clipboard` handle alive; store it on `TuiApp` for the TUI's lifetime.

use base64::Engine;
use std::io::Write;

/// Maximum raw bytes we will base64-encode into an OSC 52 sequence.
const OSC52_MAX_RAW_BYTES: usize = 100_000;

/// Keeps a platform clipboard owner alive on Linux where required.
///
/// On X11 and some Wayland compositors, clipboard contents are served by the
/// process that wrote them. Dropping `arboard::Clipboard` too early makes the
/// copied text vanish. On macOS/Windows this is a no-op wrapper.
pub struct ClipboardLease {
    #[cfg(target_os = "linux")]
    _clipboard: Option<arboard::Clipboard>,
}

impl ClipboardLease {
    #[cfg(target_os = "linux")]
    fn from_arboard(cb: arboard::Clipboard) -> Self {
        Self {
            _clipboard: Some(cb),
        }
    }

    fn empty() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            _clipboard: None,
        }
    }
}

/// Copy `text` to the system clipboard.
///
/// Returns a [`ClipboardLease`] that must be kept alive for the TUI's lifetime
/// (relevant on Linux/X11 only — on other platforms the lease is empty).
pub fn copy_to_clipboard(text: &str) -> Result<ClipboardLease, String> {
    if is_ssh_session() {
        // Over SSH the native clipboard writes to the remote machine (useless).
        // Use terminal-mediated copy so the text reaches the local terminal.
        return terminal_copy(text).map(|()| ClipboardLease::empty());
    }

    match arboard_copy(text) {
        Ok(lease) => Ok(lease),
        Err(native_err) => {
            tracing::warn!("native clipboard copy failed: {native_err}, falling back to OSC 52");
            terminal_copy(text).map(|()| ClipboardLease::empty())
        }
    }
}

// ── Backends ──────────────────────────────────────────────────────────────

fn arboard_copy(text: &str) -> Result<ClipboardLease, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("failed to set clipboard text: {e}"))?;

    #[cfg(target_os = "linux")]
    {
        Ok(ClipboardLease::from_arboard(clipboard))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = clipboard;
        Ok(ClipboardLease::empty())
    }
}

/// Copy through the terminal: prefer tmux when available, otherwise OSC 52.
fn terminal_copy(text: &str) -> Result<(), String> {
    if is_tmux_session() {
        match tmux_copy(text) {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!("tmux clipboard copy failed: {err}, falling back to OSC 52");
                return osc52_copy(text);
            }
        }
    }
    osc52_copy(text)
}

fn tmux_copy(text: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn tmux: {e}"))?;

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Err("failed to open tmux stdin".into());
    };

    if let Err(e) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        return Err(format!("failed to write to tmux: {e}"));
    }
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for tmux: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("tmux exited with status {}", output.status)
        } else {
            format!("tmux failed: {stderr}")
        })
    }
}

/// Write text to the clipboard via the OSC 52 terminal escape sequence.
///
/// Supported by kitty, WezTerm, iTerm2, Ghostty, and many others.
fn osc52_copy(text: &str) -> Result<(), String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "text too large for OSC 52 ({} bytes; max {OSC52_MAX_RAW_BYTES})",
            text.len()
        ));
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let in_tmux = std::env::var_os("TMUX").is_some();
    let sequence = if in_tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    };

    // Prefer /dev/tty on unix to bypass any stdout redirection.
    #[cfg(unix)]
    {
        if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let mut w = std::io::BufWriter::new(tty);
            if w.write_all(sequence.as_bytes()).is_ok() && w.flush().is_ok() {
                return Ok(());
            }
        }
    }

    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(sequence.as_bytes())
        .map_err(|e| format!("failed to write OSC 52: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("failed to flush OSC 52: {e}"))
}

// ── Environment detection ─────────────────────────────────────────────────

fn is_ssh_session() -> bool {
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
}

fn is_tmux_session() -> bool {
    std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
}
