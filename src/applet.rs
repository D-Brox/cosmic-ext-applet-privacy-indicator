// SPDX-License-Identifier: GPL-3.0-only

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{LazyLock, OnceLock},
    time::{Duration, Instant},
};

use cosmic::{
    Application, Apply, Element,
    app::{self, Core, Task},
    applet::padded_control,
    cosmic_config,
    cosmic_theme::palette::WithAlpha,
    iced::{
        Alignment, Background, Border, Length, Subscription,
        core::layout::Limits,
        futures::channel::mpsc::Sender,
        stream::channel,
        window::{self, Id as PopupId},
    },
    surface::action::{LiveSettings, app_popup, destroy_popup},
    theme::{Container, Svg, Theme},
    widget::{
        Column, Row, button, container::Style as CtnStyle, divider, icon, layer_container,
        mouse_area, svg::Style as SvgStyle, text,
    },
};
use cosmic_time::{Timeline, anim, chain};

use inotify::EventMask;
use pipewire::{channel::Sender as PwSender, context::ContextRc, main_loop::MainLoopRc};

use crate::{
    CONFIG_VERSION, Config,
    camera::{get_inotify, is_ancestor_or_self, open_cameras, procs_using_camera},
};

static REC_ICON: LazyLock<crate::rec_icon::Id> = LazyLock::new(crate::rec_icon::Id::unique);
static PW_SENDER: OnceLock<PwSender<u32>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct AppInfo<'s> {
    pub name: Cow<'s, str>,
    pub id: u32,
    pub pid: Option<u32>,
}

#[derive(Default)]
struct Shared {
    pub microphone: bool,
    pub screenshare: bool,
    pub camera: bool,
}

#[derive(Default)]
pub struct PrivacyIndicator {
    core: Core,
    timeline: Timeline,
    shared: Shared,
    microphones: HashMap<u32, (String, Option<u32>)>,
    screenshares: HashMap<u32, (String, Option<u32>)>,
    cameras: HashMap<PathBuf, (i32, i32)>,
    camera_procs: Vec<AppInfo<'static>>,
    popup: Option<PopupId>,
    config: Config,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    RecTick(Instant),
    ScreenShareAdd(u32, String, Option<u32>),
    MicrophoneAdd(u32, String, Option<u32>),
    PipeWireNodeRemove(u32),
    CameraOpen(PathBuf),
    CameraClose(PathBuf),
    CameraPrevious(HashMap<PathBuf, (i32, i32)>),
    CameraReset(PathBuf),
    KillStream { id: u32, name: String, pid: Option<u32> },
    TogglePopup,
    ClosePopup(PopupId),
    KillProcess(u32),
    Config(Config),
}

impl Application for PrivacyIndicator {
    type Executor = cosmic::executor::Default;

    type Flags = Config;

    type Message = Message;

    const APP_ID: &'static str = "dev.DBrox.CosmicPrivacyIndicator";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut timeline = Timeline::new();
        timeline.set_chain(chain![REC_ICON]).start();

        let app = PrivacyIndicator {
            core,
            timeline,
            config: flags,
            ..Default::default()
        };

        (app, Task::none())
    }

    fn on_close_requested(&self, id: PopupId) -> Option<Self::Message> {
        if self.popup == Some(id) {
            Some(Message::ClosePopup(id))
        } else {
            None
        }
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let horizontal = self.core.applet.is_horizontal();
        let size = self.core.applet.suggested_size(true);
        let pad = self.core.applet.suggested_padding(true);

        let Shared {
            microphone,
            screenshare,
            camera,
        } = self.shared;

        if !microphone && !screenshare && !camera {
            return self
                .core
                .applet
                .autosize_window("")
                .limits(Limits::NONE)
                .into();
        }

        let mut icons: Vec<Element<Self::Message>> =
            vec![anim![REC_ICON, &self.timeline, size.0].into()];

        let icon_style = Rc::new(|theme: &Theme| SvgStyle {
            color: Some(theme.cosmic().button_color().into()),
        });
        let indicator = |name: &str| {
            icon(icon::from_name(name).into())
                .class(Svg::Custom(icon_style.clone()))
                .size(size.0)
        };

        if camera {
            icons.push(indicator("camera-web-symbolic").into());
        }
        if microphone {
            icons.push(indicator("audio-input-microphone-symbolic").into());
        }
        if screenshare {
            icons.push(indicator("accessories-screenshot-symbolic").into());
        }

        let container_style = |theme: &Theme| {
            let cosmic = theme.cosmic();
            CtnStyle {
                background: Some(Background::Color(
                    cosmic.primary(false).base.with_alpha(0.5).into(),
                )),
                border: Border {
                    radius: cosmic.corner_radii.radius_xl.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        };

        let content = if horizontal {
            Row::with_children(icons)
                .spacing(pad.0)
                .apply(layer_container)
        } else {
            Column::with_children(icons)
                .spacing(pad.1)
                .apply(layer_container)
        }
        .padding(pad.0.min(pad.1))
        .class(Container::Custom(Box::new(container_style)));

        self.core
            .applet
            .autosize_window(mouse_area(content).on_press(Message::TogglePopup))
            .limits(Limits::NONE)
            .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Self::Message> {
        let microphones: Vec<_> = self
            .microphones
            .iter()
            .map(|(&id, (name, pid))| AppInfo {
                name: name.into(),
                id,
                pid: *pid,
            })
            .collect();
        let screenshares: Vec<_> = self
            .screenshares
            .iter()
            .map(|(&id, (name, pid))| AppInfo {
                name: name.into(),
                id,
                pid: *pid,
            })
            .collect();
        // Camera processes are cached in update() (see Tick/TogglePopup): the
        // /proc scan is far too expensive to run from the render path, which
        // executes on every frame the popup is visible.
        let cameras = self.camera_procs.clone();

        let mut rows: Vec<Element<Self::Message>> = vec![];

        macro_rules! section {
            ($label:expr, $apps:expr, $kill_msg:expr, $can_kill:expr) => {
                if !$apps.is_empty() {
                    if !rows.is_empty() {
                        rows.push(divider::horizontal::default().into());
                    }
                    rows.push(padded_control(text::heading($label)).into());
                    for app in &$apps {
                        let kill_btn = button::destructive("Kill").on_press_maybe(
                            if app.id > 0 && $can_kill(app) {
                                Some($kill_msg(app))
                            } else {
                                None
                            },
                        );
                        rows.push(
                            padded_control(
                                Row::new()
                                    .push(text::body(app.name.to_string()).width(Length::Fill))
                                    .push(kill_btn)
                                    .align_y(Alignment::Center),
                            )
                            .into(),
                        );
                    }
                }
            };
        }

        section!(
            "Camera",
            cameras,
            |app: &AppInfo| Message::KillProcess(app.id),
            |app: &AppInfo| !is_camera_daemon(&app.name)
        );
        section!(
            "Microphone",
            microphones,
            |app: &AppInfo| Message::KillStream {
                id: app.id,
                name: app.name.to_string(),
                pid: app.pid
            },
            |_| true
        );
        section!(
            "Screen Share",
            screenshares,
            |app: &AppInfo| Message::KillStream {
                id: app.id,
                name: app.name.to_string(),
                pid: app.pid
            },
            |_| true
        );

        self.core
            .applet
            .popup_container(Column::with_children(rows))
            .into()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.shared = Shared {
                    microphone: !self.microphones.is_empty(),
                    screenshare: !self.screenshares.is_empty(),
                    camera: self
                        .cameras
                        .values()
                        .fold(0, |acc, (shares, min)| acc + shares - min)
                        > 0,
                };
                self.refresh_camera_procs();
            }
            Message::CameraPrevious(cameras) => {
                self.cameras = cameras;
            }
            Message::CameraOpen(path) => {
                self.cameras
                    .entry(path.clone())
                    .and_modify(|v| v.0 += 1)
                    .or_insert((1, 0));
            }
            Message::CameraClose(path) => {
                self.cameras
                    .entry(path.clone())
                    .and_modify(|v| {
                        v.0 -= 1;
                        v.1 = v.1.min(v.0);
                    })
                    .or_insert((0, 0));
            }
            Message::CameraReset(path) => {
                self.cameras.remove(&path);
            }
            Message::ScreenShareAdd(id, name, pid) => {
                self.screenshares.insert(id, (name, pid));
            }
            Message::MicrophoneAdd(id, name, pid) => {
                self.microphones.insert(id, (name, pid));
            }
            Message::PipeWireNodeRemove(id) => {
                self.screenshares.remove(&id);
                self.microphones.remove(&id);
            }
            Message::RecTick(now) => {
                self.timeline.now(now);
            }
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return destroy_popup(id)
                        .apply(app::Action::Surface)
                        .apply(cosmic::Action::Cosmic)
                        .apply(Task::done);
                }
                self.refresh_camera_procs();
                let live_settings = |state: &mut PrivacyIndicator| {
                    let new_id = window::Id::unique();
                    state.popup = Some(new_id);
                    state.core.applet.get_popup_settings(
                        state.core.main_window_id().unwrap_or(window::Id::RESERVED),
                        new_id,
                        None,
                        None,
                        None,
                    )
                };
                return app_popup(|_| LiveSettings::default(), live_settings, None)
                    .apply(app::Action::Surface)
                    .apply(cosmic::Action::Cosmic)
                    .apply(Task::done);
            }
            Message::ClosePopup(id) => {
                self.popup.take_if(|stored_id| stored_id == &id);
            }
            Message::KillStream { id, name, pid } => {
                if let Some(sender) = PW_SENDER.get() {
                    let _ = sender.send(id);
                }
                kill_process(pid, Some(name));
                if let Some(task) = self.close_popup() {
                    return task;
                }
            }
            Message::KillProcess(pid) => {
                kill_process(Some(pid), None);
                if let Some(task) = self.close_popup() {
                    return task;
                }
            }
            Message::Config(config) => self.config = config,
        }
        // The popup must never shrink to an empty sliver while open: its corner
        // radius then exceeds the window geometry and the compositor kills the
        // connection (radius_too_large protocol error). Close it instead.
        if self.popup.is_some()
            && self.microphones.is_empty()
            && self.screenshares.is_empty()
            && self.camera_procs.is_empty()
            && let Some(task) = self.close_popup()
        {
            return task;
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let pw_shares = Self::pipewire_subscription();
        let camera_shares = Self::inotify_subscription();
        let config = Self::config_subscription();
        let timeline = if self.config.animated {
            cosmic::iced::time::every(Duration::from_millis(self.config.refresh))
                .map(Message::RecTick)
        } else {
            Subscription::none()
        };
        let tick = cosmic::iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick);

        Subscription::batch([pw_shares, camera_shares, config, timeline, tick])
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl PrivacyIndicator {
    fn close_popup(&mut self) -> Option<Task<Message>> {
        let id = self.popup.take()?;
        Some(
            destroy_popup(id)
                .apply(app::Action::Surface)
                .apply(cosmic::Action::Cosmic)
                .apply(Task::done),
        )
    }

    /// Refresh the cached list of processes using the cameras. Only runs while
    /// the popup is open: scanning /proc walks every process's fds, so it must
    /// be done from update() (bounded to once per tick) instead of the render
    /// path, which runs on every frame the popup is visible.
    fn refresh_camera_procs(&mut self) {
        if self.popup.is_none() {
            return;
        }
        self.camera_procs = self
            .cameras
            .keys()
            .flat_map(|path| procs_using_camera(path))
            .collect();
    }

    pub fn config_subscription() -> Subscription<Message> {
        struct ConfigSubscription;
        cosmic_config::config_subscription(
            std::any::TypeId::of::<ConfigSubscription>(),
            Self::APP_ID.into(),
            CONFIG_VERSION,
        )
        .map(|update| {
            if !update.errors.is_empty() {
                println!(
                    "errors loading config {:?}: {:?}",
                    update.keys, update.errors
                );
            }
            Message::Config(update.config)
        })
    }

    fn pipewire_subscription() -> Subscription<Message> {
        let pw = || {
            channel(100, |output: Sender<_>| async {
                std::thread::spawn(move || {
                    pipewire::init();
                    let main_loop =
                        MainLoopRc::new(None).expect("Failed to create PipeWire main loop");
                    let context = ContextRc::new(&main_loop, None)
                        .expect("Failed to create PipeWire context");
                    let core = context
                        .connect_rc(None)
                        .expect("Failed to connect to PipeWire");
                    let registry = core
                        .get_registry_rc()
                        .expect("Failed to get PipeWire registry");

                    let (sender, receiver) = pipewire::channel::channel::<u32>();
                    let _ = PW_SENDER.set(sender);

                    let receiver_registry = registry.clone();
                    let _attached = receiver.attach(main_loop.loop_(), move |id| {
                        receiver_registry.destroy_global(id);
                    });

                    let output_remove = output.clone();
                    let _listener = registry
                        .add_listener_local()
                        .global(move |global| {
                            if global.type_.to_str() != "PipeWire:Interface:Node" {
                                return;
                            }
                            let Some(props) = global.props else { return };
                            let name = props
                                .get("application.name")
                                .or_else(|| props.get("node.name"))
                                .unwrap_or("Unknown")
                                .to_string();
                            let pid = props
                                .get("application.process.id")
                                .and_then(|p| p.parse().ok());
                            let Some(media_class) = props.get("media.class") else {
                                return;
                            };
                            let msg = match media_class {
                                "Stream/Input/Video" => {
                                    Some(Message::ScreenShareAdd(global.id, name, pid))
                                }
                                "Stream/Input/Audio" => {
                                    Some(Message::MicrophoneAdd(global.id, name, pid))
                                }
                                _ => None,
                            };
                            if let Some(msg) = msg {
                                send_blocking(&mut output.clone(), msg);
                            }
                        })
                        .global_remove(move |id| {
                            send_blocking(
                                &mut output_remove.clone(),
                                Message::PipeWireNodeRemove(id),
                            );
                        })
                        .register();
                    main_loop.run();
                });
            })
        };
        Subscription::run(pw)
    }

    fn inotify_subscription() -> Subscription<Message> {
        let inotify = || {
            channel(100, |mut output: Sender<_>| async {
                std::thread::spawn(move || {
                    // Ordering invariant: snapshot the camera state BEFORE the
                    // watch is created, so the snapshot can't overlap the OPEN
                    // events the watch produces (double-count) and the events
                    // delivered afterwards stay strictly incremental on it.
                    send_blocking(&mut output, Message::CameraPrevious(open_cameras()));

                    let (mut inotify, mut wd_path) = get_inotify();
                    let mut event_buffer = [0; 4096];

                    loop {
                        for event in inotify
                            .read_events_blocking(&mut event_buffer)
                            .expect("Failed to read events")
                        {
                            match event.mask {
                                EventMask::CREATE | EventMask::ATTRIB | EventMask::DELETE_SELF
                                    if (event.mask == EventMask::DELETE_SELF
                                        || event
                                            .name
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .starts_with("video")) =>
                                {
                                    let old_wd_paths = wd_path;
                                    (inotify, wd_path) = get_inotify();
                                    let old_paths = old_wd_paths
                                        .left_values()
                                        .collect::<std::collections::HashSet<_>>();
                                    let new_paths = wd_path
                                        .left_values()
                                        .collect::<std::collections::HashSet<_>>();
                                    for &path in old_paths.difference(&new_paths) {
                                        send_blocking(&mut output, Message::CameraReset(path.clone()));
                                    }
                                }
                                EventMask::OPEN => {
                                    if let Some(path) = wd_path.get_by_right(&event.wd) {
                                        send_blocking(&mut output, Message::CameraOpen(path.clone()));
                                    }
                                }

                                EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE => {
                                    if let Some(path) = wd_path.get_by_right(&event.wd) {
                                        send_blocking(
                                            &mut output,
                                            Message::CameraClose(path.clone()),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                });
            })
        };
        Subscription::run(inotify)
    }
}

/// Camera access through the desktop portal is performed by the PipeWire
/// daemon itself, so /proc attributes the device to it. Killing it would take
/// down the camera and audio stack with it, so it must never be offered as a
/// kill target. Process names (comm) are truncated to 15 chars, hence prefix
/// matching.
fn is_camera_daemon(name: &str) -> bool {
    name.starts_with("pipewire")
        || name.starts_with("wireplumber")
        || name.starts_with("xdg-desktop-")
        || name.starts_with("flatpak-portal")
}

/// Send SIGTERM, then SIGKILL if the process is still alive after 2s: some
/// apps catch SIGTERM and hang during shutdown instead of exiting. The pid may
/// be absent for portal streams, in which case the app is resolved from the
/// stream name (flatpak apps export FLATPAK_ID in their environment).
fn kill_process(pid: Option<u32>, name: Option<String>) {
    std::thread::spawn(move || {
        let Some(pid) = pid.or_else(|| name.as_deref().and_then(resolve_flatpak_pid)) else {
            return;
        };
        // The applet runs as a child of the desktop panel; signaling an
        // ancestor would take the panel (or session) down with it.
        if is_ancestor_or_self(pid) {
            return;
        }
        let pid = Pid::from_raw(pid.cast_signed());
        let _ = kill(pid, Signal::SIGTERM);
        std::thread::sleep(Duration::from_secs(2));
        // kill(pid, None) succeeds only while the process exists and is
        // signalable; ESRCH means it already exited.
        if kill(pid, None).is_ok() {
            let _ = kill(pid, Signal::SIGKILL);
        }
    });
}

/// Resolve the host pid of a flatpak app by its app id (e.g. "org.gnome.Snapshot"),
/// which the portal reports as the stream name.
fn resolve_flatpak_pid(app_id: &str) -> Option<u32> {
    if !app_id.contains('.') {
        return None;
    }
    let needle = format!("FLATPAK_ID={app_id}\0");
    let mut best: Option<u32> = None;
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return None;
    };
    for entry in procs.flatten() {
        let Ok(id) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(env) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !env.windows(needle.len()).any(|w| w == needle.as_bytes()) {
            continue;
        }
        // Prefer the app process itself over its bwrap wrappers.
        match std::fs::read_to_string(entry.path().join("comm")) {
            Ok(comm) if comm.trim() == "bwrap" => best = Some(best.map_or(id, |b| b.max(id))),
            _ => return Some(id),
        }
    }
    best
}

/// Send a message to the application channel from a worker thread, preserving
/// the order in which it was produced. Retries with a small sleep instead of
/// spinning, so a full channel can't busy-loop the CPU.
fn send_blocking(output: &mut Sender<Message>, msg: Message) {
    let mut msg = msg;
    loop {
        match output.try_send(msg) {
            Ok(()) => return,
            Err(err) if err.is_full() => {
                msg = err.into_inner();
                std::thread::sleep(Duration::from_millis(10));
            }
            // Receiver dropped: the app is shutting down.
            Err(_) => return,
        }
    }
}
