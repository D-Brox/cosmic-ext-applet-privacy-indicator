use std::{
    collections::{HashMap, HashSet},
    fs::{read_dir, read_link},
    path::{Path, PathBuf},
};

use bimap::BiHashMap;
use inotify::{Inotify, WatchDescriptor, WatchMask};

use crate::applet::AppInfo;

pub fn open_cameras() -> HashMap<PathBuf, (i32, i32)> {
    if std::path::Path::new("/.flatpak-info").exists() {
        return HashMap::new();
    }

    read_dir("/proc")
        .map(|paths| {
            paths
                .flatten()
                .filter(|pid| {
                    pid.file_name()
                        .to_string_lossy()
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                })
                .filter_map(|pid| {
                    read_dir(pid.path().join("fd"))
                        .ok()
                        .map(|fds| fds.flatten().map(|p| p.path()))
                })
                .flatten()
                .filter_map(|fd| {
                    let Ok(path) = read_link(fd) else {
                        return None;
                    };
                    if path.to_string_lossy().starts_with("/dev/video") {
                        Some(path)
                    } else {
                        None
                    }
                })
                .fold(HashMap::<PathBuf, (i32, i32)>::new(), |mut hm, p| {
                    hm.entry(p).and_modify(|fds| fds.0 += 1).or_insert((1, 0));
                    hm
                })
        })
        .unwrap_or_default()
}

/// True when `pid` is the calling process itself or one of its ancestors. The
/// applet runs as a child of the desktop panel, so signaling an ancestor would
/// take the panel (or the whole session) down with it.
pub fn is_ancestor_or_self(pid: u32) -> bool {
    let mut cur = std::process::id();
    loop {
        if cur == pid {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{cur}/stat")) else {
            return false;
        };
        // comm can contain spaces and parens; ppid follows the last ')'
        let Some(Ok(ppid)) = stat
            .rsplit(')')
            .next()
            .and_then(|s| s.split_whitespace().nth(1))
            .map(|s| s.parse::<u32>())
        else {
            return false;
        };
        cur = ppid;
    }
}

/// Scans /proc to find all processes currently holding a file descriptor open on `device`.
pub fn procs_using_camera(device: &Path) -> Vec<AppInfo<'static>> {
    if std::path::Path::new("/.flatpak-info").exists() {
        return vec![];
    }
    let mut seen_pids = HashSet::new();
    read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|pid_entry| {
            let id: u32 = pid_entry.file_name().to_string_lossy().parse().ok()?;
            let uses_device = read_dir(pid_entry.path().join("fd"))
                .ok()?
                .flatten()
                .any(|fd| read_link(fd.path()).ok().as_deref() == Some(device));
            if !uses_device {
                return None;
            }
            let name = std::fs::read_to_string(pid_entry.path().join("comm"))
                .ok()?
                .trim()
                .to_string()
                .into();
            Some(AppInfo { name, id, pid: Some(id) })
        })
        .filter(|info| seen_pids.insert(info.id))
        .filter(|info| !is_ancestor_or_self(info.id))
        .collect()
}

pub fn get_inotify() -> (Inotify, BiHashMap<PathBuf, WatchDescriptor>) {
    let inotify = Inotify::init().expect("Failed to initialize inotify");
    inotify
        .watches()
        .add("/dev", WatchMask::ATTRIB)
        .expect("Failed to watch for devices");
    let mut wd_path = BiHashMap::new();
    for entry in std::fs::read_dir("/dev").expect("Failed to read /dev") {
        if let Ok(entry) = entry
            && entry.file_name().to_string_lossy().starts_with("video")
        {
            let Ok(wd) = inotify.watches().add(
                entry.path(),
                WatchMask::OPEN | WatchMask::CLOSE | WatchMask::DELETE_SELF,
            ) else {
                continue;
            };
            wd_path.insert(entry.path(), wd);
        }
    }
    (inotify, wd_path)
}
