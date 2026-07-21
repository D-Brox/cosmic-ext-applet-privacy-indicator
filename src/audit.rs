// SPDX-License-Identifier: GPL-3.0-only

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::applet::PrivacyIndicator;
use cosmic::Application;
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
            DeviceKind::Microphone => "MICROPHONE",
            DeviceKind::ScreenShare => "SCREENSHARE",
        }
    }
}

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join(PrivacyIndicator::APP_ID).join("audit.log"))
}

fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs / 60) % 60, secs % 60)
    }
}

pub fn record(kind: DeviceKind, app: &str, start: &Zoned, end: &Zoned) {
    let Some(path) = log_path() else {
        eprintln!("audit: could not resolve log path (no XDG_STATE_HOME/HOME)");
        return;
    };
    if let Some(dir) = path.parent() {
        let Ok(_) = fs::create_dir_all(dir) else {
            eprintln!("audit: could not create log dire");
            return;
        };
    }

    match OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            let duration = end.timestamp().as_second() - start.timestamp().as_second();
            let line = format!(
                "{}  {:<7} {:<14} ({})\n",
                start.strftime("%Y-%m-%d %H:%M:%S"),
                kind.label(),
                app,
                format_duration(duration),
            );
            if let Err(e) = file.write_all(line.as_bytes()) {
                eprintln!("audit: failed to write log: {e}");
            }
        }
        Err(e) => eprintln!("audit: failed to open log {}: {e}", path.display()),
    }
}
