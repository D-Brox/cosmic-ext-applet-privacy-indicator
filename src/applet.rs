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
        futures::{SinkExt, channel::mpsc::Sender},
        stream::channel,
        window::{self, Id as PopupId},
    },
    surface::action::{LiveSettings, app_popup, destroy_popup},
    theme::{Container, Svg, Theme},
    widget::{
        Column, Row, button, checkbox, container::Style as CtnStyle, divider, icon,
        layer_container, mouse_area, svg::Style as SvgStyle, text,
    },
};
use cosmic_time::{Timeline, anim, chain};

use inotify::EventMask;
use pipewire::{channel::Sender as PwSender, context::ContextRc, main_loop::MainLoopRc};

use cosmic::cosmic_config::CosmicConfigEntry;
use jiff::Zoned;

use crate::{
    CONFIG_VERSION, Config,
    audit::{self, DeviceKind},
    camera::{get_inotify, open_cameras, procs_using_camera},
};

static REC_ICON: LazyLock<crate::rec_icon::Id> = LazyLock::new(crate::rec_icon::Id::unique);
static PW_SENDER: OnceLock<PwSender<u32>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct AppInfo<'s> {
    pub name: Cow<'s, str>,
    pub id: u32,
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
    microphones: HashMap<u32, String>,
    screenshares: HashMap<u32, String>,
    cameras: HashMap<PathBuf, (i32, i32)>,
    /// Start time of each active PipeWire node (mic/screenshare), keyed by node id.
    pw_starts: HashMap<u32, Zoned>,
    /// Start time and app name of each in-use camera, keyed by device path.
    camera_active: HashMap<PathBuf, (Zoned, String)>,
    popup: Option<PopupId>,
    config: Config,
    config_handler: Option<cosmic_config::Config>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    RecTick(Instant),
    ScreenShareAdd(u32, String),
    MicrophoneAdd(u32, String),
    PipeWireNodeRemove(u32),
    CameraOpen(PathBuf),
    CameraClose(PathBuf),
    CameraPrevious(HashMap<PathBuf, (i32, i32)>),
    CameraReset(PathBuf),
    DisconnectNode(u32),
    TogglePopup,
    ClosePopup(PopupId),
    KillProcess(u32),
    ToggleAuditLog(bool),
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
            config_handler: cosmic_config::Config::new(Self::APP_ID, CONFIG_VERSION).ok(),
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
            .map(|(&id, name)| AppInfo {
                name: name.into(),
                id,
            })
            .collect();
        let screenshares: Vec<_> = self
            .screenshares
            .iter()
            .map(|(&id, name)| AppInfo {
                name: name.into(),
                id,
            })
            .collect();
        let cameras: Vec<_> = self
            .cameras
            .keys()
            .flat_map(|path| procs_using_camera(path))
            .collect();

        let mut rows: Vec<Element<Self::Message>> = vec![];

        macro_rules! section {
            ($label:expr, $apps:expr, $id:ident) => {
                if !$apps.is_empty() {
                    if !rows.is_empty() {
                        rows.push(divider::horizontal::default().into());
                    }
                    rows.push(padded_control(text::heading($label)).into());
                    for app in $apps {
                        let kill_btn = button::destructive("Kill").on_press_maybe(if app.id > 0 {
                            Some(Message::$id(app.id))
                        } else {
                            None
                        });
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

        section!("Camera", cameras, KillProcess);
        section!("Microphone", microphones, DisconnectNode);
        section!("Screen Share", screenshares, DisconnectNode);

        if !rows.is_empty() {
            rows.push(divider::horizontal::default().into());
        }
        rows.push(
            padded_control(
                checkbox("Audit log", self.config.audit_log)
                    .on_toggle(Message::ToggleAuditLog),
            )
            .into(),
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
            }
            Message::CameraPrevious(cameras) => {
                self.cameras = cameras;
            }
            Message::CameraOpen(path) => {
                let v = self
                    .cameras
                    .entry(path.clone())
                    .and_modify(|v| v.0 += 1)
                    .or_insert((1, 0));
                let in_use = v.0 - v.1 > 0;
                // On the transition to "in use", open an audit session and
                // resolve which application opened the device.
                if self.config.audit_log && in_use && !self.camera_active.contains_key(&path) {
                    let name = procs_using_camera(&path)
                        .into_iter()
                        .next()
                        .map(|a| a.name.into_owned())
                        .unwrap_or_else(|| "unknown".to_string());
                    self.camera_active.insert(path.clone(), (Zoned::now(), name));
                }
            }
            Message::CameraClose(path) => {
                let v = self
                    .cameras
                    .entry(path.clone())
                    .and_modify(|v| {
                        v.0 -= 1;
                        v.1 = v.1.min(v.0);
                    })
                    .or_insert((0, 0));
                let in_use = v.0 - v.1 > 0;
                // On the transition back to "not in use", close the session.
                if !in_use && let Some((start, name)) = self.camera_active.remove(&path) {
                    if self.config.audit_log {
                        audit::record(DeviceKind::Camera, &name, &start, &Zoned::now());
                    }
                }
            }
            Message::CameraReset(path) => {
                // Device removed while still open: close any open session.
                if let Some((start, name)) = self.camera_active.remove(&path) {
                    if self.config.audit_log {
                        audit::record(DeviceKind::Camera, &name, &start, &Zoned::now());
                    }
                }
                self.cameras.remove(&path);
            }
            Message::ScreenShareAdd(id, info) => {
                if self.config.audit_log {
                    self.pw_starts.entry(id).or_insert_with(Zoned::now);
                }
                self.screenshares.insert(id, info);
            }
            Message::MicrophoneAdd(id, info) => {
                if self.config.audit_log {
                    self.pw_starts.entry(id).or_insert_with(Zoned::now);
                }
                self.microphones.insert(id, info);
            }
            Message::PipeWireNodeRemove(id) => {
                let start = self.pw_starts.remove(&id);
                if self.config.audit_log && let Some(start) = start {
                    let now = Zoned::now();
                    if let Some(name) = self.screenshares.get(&id) {
                        audit::record(DeviceKind::ScreenShare, name, &start, &now);
                    } else if let Some(name) = self.microphones.get(&id) {
                        audit::record(DeviceKind::Microphone, name, &start, &now);
                    }
                }
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
            Message::DisconnectNode(id) => {
                if let Some(sender) = PW_SENDER.get() {
                    let _ = sender.send(id);
                }
            }
            Message::KillProcess(pid) => {
                if let Err(e) = kill(Pid::from_raw(pid.cast_signed()), Signal::SIGTERM) {
                    println!("Failed to kill process {pid}: {e}");
                }
            }
            Message::ToggleAuditLog(enabled) => {
                self.config.audit_log = enabled;
                if let Some(handler) = &self.config_handler
                    && let Err(e) = self.config.write_entry(handler)
                {
                    println!("Failed to save audit_log config: {e:?}");
                }
            }
            Message::Config(config) => self.config = config,
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
                let handle = tokio::runtime::Handle::current();
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
                    let handle_remove = handle.clone();
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
                            let Some(media_class) = props.get("media.class") else {
                                return;
                            };
                            let msg = match media_class {
                                "Stream/Input/Video" => {
                                    Some(Message::ScreenShareAdd(global.id, name))
                                }
                                "Stream/Input/Audio" => {
                                    Some(Message::MicrophoneAdd(global.id, name))
                                }
                                _ => None,
                            };
                            if let Some(msg) = msg {
                                let mut output = output.clone();
                                handle.spawn(async move {
                                    let _ = output.send(msg).await;
                                });
                            }
                        })
                        .global_remove(move |id| {
                            let mut output = output_remove.clone();
                            handle_remove.spawn(async move {
                                let _ = output.send(Message::PipeWireNodeRemove(id)).await;
                            });
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
            channel(100, |output: Sender<_>| async {
                let handle = tokio::runtime::Handle::current();
                std::thread::spawn(move || {
                    {
                        let mut output = output.clone();
                        handle.spawn(async move {
                            let _ = output.send(Message::CameraPrevious(open_cameras())).await;
                        });
                    }
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
                                        let mut output = output.clone();
                                        let path = path.clone();
                                        handle.spawn(async move {
                                            let _ = output.send(Message::CameraReset(path)).await;
                                        });
                                    }
                                }
                                EventMask::OPEN => {
                                    if let Some(path) = wd_path.get_by_right(&event.wd) {
                                        let mut output = output.clone();
                                        let path = path.clone();
                                        handle.spawn(async move {
                                            let _ = output.send(Message::CameraOpen(path)).await;
                                        });
                                    }
                                }

                                EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE => {
                                    if let Some(path) = wd_path.get_by_right(&event.wd) {
                                        let mut output = output.clone();
                                        let path = path.clone();
                                        handle.spawn(async move {
                                            let _ = output.send(Message::CameraClose(path)).await;
                                        });
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
