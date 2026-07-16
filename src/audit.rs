// SPDX-License-Identifier: GPL-3.0-only

//! Local, append-only audit log for privacy events.
//!
//! Records only metadata (device kind, application name, start/end/duration).
//! Never captures audio/video content, never leaves the machine, and the log
//! file is created with owner-only permissions (0600).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use jiff::Zoned;

#[derive(Debug, Clone, Copy)]
pub enum DeviceKind {
    Camera,
    Microphone,
    ScreenShare,
}

impl DeviceKind {
    fn label(self) -> &'static str {
        match self {
            DeviceKind::Camera => "CAMERA",
            DeviceKind::Microphone => "MIC",
            DeviceKind::ScreenShare => "SCREEN",
        }
    }
}

/// `$XDG_STATE_HOME/cosmic-ext-applet-privacy-indicator/audit.log`,
/// falling back to `~/.local/state/...`.
fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("cosmic-ext-applet-privacy-indicator").join("audit.log"))
}

/// Human-readable duration, e.g. `45s`, `4m41s`, `1h05m`.
fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Appends one entry to the audit log. `start`/`end` are wall-clock timestamps;
/// the duration is derived from their UTC instants (DST-safe).
pub fn record(kind: DeviceKind, app: &str, start: &Zoned, end: &Zoned) {
    let Some(path) = log_path() else {
        eprintln!("audit: could not resolve log path (no XDG_STATE_HOME/HOME)");
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }

    let duration = end.timestamp().as_second() - start.timestamp().as_second();
    let line = format!(
        "{}  {:<7} {:<14} ({})\n",
        start.strftime("%Y-%m-%d %H:%M:%S"),
        kind.label(),
        app,
        format_duration(duration),
    );

    match OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(e) = file.write_all(line.as_bytes()) {
                eprintln!("audit: failed to write log: {e}");
            }
        }
        Err(e) => eprintln!("audit: failed to open log {path:?}: {e}"),
    }
}
