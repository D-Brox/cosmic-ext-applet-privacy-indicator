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
        futures::{SinkExt, channel::mpsc::Sender, executor::block_on},
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
    camera::{get_inotify, open_cameras, proc_scan_available, procs_using_camera},
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
    cameras: HashMap<PathBuf, u32>,
    popup: Option<PopupId>,
    config: Config,
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
    CameraPrevious(HashMap<PathBuf, u32>),
    CameraReset(PathBuf),
    DisconnectNode(u32),
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
                    camera: self.cameras.values().any(|&fds| fds > 0),
                };
                // The inotify counters drift whenever an open or close event is missed, which
                // leaves the indicator stuck on. Re-check `/proc` while a camera looks busy so
                // the state converges back to what is actually open.
                if !self.cameras.is_empty() && proc_scan_available() {
                    return cosmic::task::future(async {
                        let cameras = tokio::task::spawn_blocking(open_cameras)
                            .await
                            .unwrap_or_default();
                        Message::CameraPrevious(cameras)
                    });
                }
            }
            Message::CameraPrevious(cameras) => {
                self.cameras = cameras;
            }
            Message::CameraOpen(path) => {
                *self.cameras.entry(path).or_default() += 1;
            }
            Message::CameraClose(path) => {
                if let Some(fds) = self.cameras.get_mut(&path) {
                    *fds = fds.saturating_sub(1);
                    if *fds == 0 {
                        self.cameras.remove(&path);
                    }
                }
            }
            Message::CameraReset(path) => {
                self.cameras.remove(&path);
            }
            Message::ScreenShareAdd(id, info) => {
                self.screenshares.insert(id, info);
            }
            Message::MicrophoneAdd(id, info) => {
                self.microphones.insert(id, info);
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
            Message::Config(config) => self.config = config,
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let pw_shares = Self::pipewire_subscription();
        let camera_shares = Self::inotify_subscription();
        let config = Self::config_subscription();
        let timeline = if self.should_animate() {
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
    fn should_animate(&self) -> bool {
        self.config.animated
            && (self.shared.microphone || self.shared.screenshare || self.shared.camera)
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
                std::thread::spawn(move || {
                    // Each event has to reach the applet in the order it was read, otherwise
                    // an open/close pair can be applied backwards and corrupt the counters.
                    let send = |msg| {
                        let mut output = output.clone();
                        block_on(async move {
                            let _ = output.send(msg).await;
                        });
                    };

                    // Watch first, then scan: a camera opened in between would otherwise be
                    // missed by both the scan and the watches.
                    let (mut inotify, mut wd_path) = get_inotify();
                    send(Message::CameraPrevious(open_cameras()));

                    let mut event_buffer = [0; 4096];

                    loop {
                        for event in inotify
                            .read_events_blocking(&mut event_buffer)
                            .expect("Failed to read events")
                        {
                            let mask = event.mask;
                            let device_name = || {
                                event
                                    .name
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .starts_with("video")
                            };

                            if mask.contains(EventMask::DELETE_SELF)
                                || ((mask.intersects(EventMask::CREATE | EventMask::ATTRIB))
                                    && device_name())
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
                                    send(Message::CameraReset(path.clone()));
                                }
                                // Re-watching drops whatever was still queued on the old
                                // instance, so the counters are rebuilt from `/proc` instead.
                                if proc_scan_available() {
                                    send(Message::CameraPrevious(open_cameras()));
                                }
                                break;
                            }

                            if mask.contains(EventMask::OPEN) {
                                if let Some(path) = wd_path.get_by_right(&event.wd) {
                                    send(Message::CameraOpen(path.clone()));
                                }
                            } else if mask
                                .intersects(EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE)
                                && let Some(path) = wd_path.get_by_right(&event.wd)
                            {
                                send(Message::CameraClose(path.clone()));
                            }
                        }
                    }
                });
            })
        };
        Subscription::run(inotify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_close_without_matching_open_does_not_stick() {
        let mut app = PrivacyIndicator::default();
        let device = PathBuf::from("/dev/video0");

        drop(app.update(Message::CameraOpen(device.clone())));
        drop(app.update(Message::Tick));
        assert!(app.shared.camera);

        // A close for an open that was never seen must not push the count below zero,
        // otherwise the next open would leave the indicator permanently on.
        drop(app.update(Message::CameraClose(device.clone())));
        drop(app.update(Message::CameraClose(device.clone())));
        drop(app.update(Message::Tick));
        assert!(!app.shared.camera);
        assert!(app.cameras.is_empty());

        drop(app.update(Message::CameraOpen(device.clone())));
        drop(app.update(Message::CameraClose(device)));
        drop(app.update(Message::Tick));
        assert!(!app.shared.camera);
    }

    #[test]
    fn proc_scan_overrides_stale_camera_counters() {
        let mut app = PrivacyIndicator::default();
        let device = PathBuf::from("/dev/video0");

        drop(app.update(Message::CameraOpen(device)));
        drop(app.update(Message::Tick));
        assert!(app.shared.camera);

        // A rescan reporting nothing open clears the leaked count.
        drop(app.update(Message::CameraPrevious(HashMap::new())));
        drop(app.update(Message::Tick));
        assert!(!app.shared.camera);
    }

    #[test]
    fn animation_runs_only_for_visible_indicators() {
        let mut app = PrivacyIndicator::default();

        assert!(!app.should_animate());

        drop(app.update(Message::MicrophoneAdd(1, "Microphone".to_string())));
        drop(app.update(Message::Tick));
        assert!(app.should_animate());

        app.config.animated = false;
        assert!(!app.should_animate());

        app.config.animated = true;
        drop(app.update(Message::PipeWireNodeRemove(1)));
        drop(app.update(Message::Tick));
        assert!(!app.should_animate());
    }
}
