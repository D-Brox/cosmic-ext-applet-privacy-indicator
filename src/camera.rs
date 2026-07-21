use std::{
    collections::{HashMap, HashSet},
    fs::{read_dir, read_link},
    path::{Path, PathBuf},
};

use bimap::BiHashMap;
use inotify::{Inotify, WatchDescriptor, WatchMask};

use crate::applet::{AppInfo, CameraShares};

pub fn open_cameras() -> HashMap<PathBuf, CameraShares> {
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
                .fold(HashMap::<PathBuf, CameraShares>::new(), |mut hm, p| {
                    hm.entry(p)
                        .and_modify(|fds| fds.shares += 1)
                        .or_insert(CameraShares { shares: 1, min: 0 });
                    hm
                })
        })
        .unwrap_or_default()
}

/// Scans /proc to find all processes currently holding a file descriptor open on `device`.
/// This is CPU intensive. **DO NOT** use in the applet main view.
pub fn procs_using_camera(device: &Path) -> Vec<AppInfo<'_>> {
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
            Some(AppInfo { name, id })
        })
        .filter(|info| seen_pids.insert(info.id))
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
