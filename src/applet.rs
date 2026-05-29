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
    app::{Core, Task},
    applet::padded_control,
    cosmic_config,
    cosmic_theme::palette::WithAlpha,
    iced::{
        Alignment, Background, Border, Length, Subscription,
        core::layout::Limits,
        stream::channel,
        window::{self, Id as PopupId},
    },
    iced_winit::commands::popup::{destroy_popup, get_popup},
    theme::{Container, Svg, Theme},
    widget::{
        Column, Row, button, container::Style as CtnStyle, divider, icon, layer_container,
        mouse_area, svg::Style as SvgStyle, text,
    },
};
use cosmic_time::{Timeline, anim, chain};

use inotify::EventMask;
use pipewire::{channel::Sender, context::ContextRc, main_loop::MainLoopRc};
use libpulse_binding as pulse;
use pulse::mainloop::threaded::Mainloop as PulseMainloop;
use pulse::context::Context as PulseContext;
use pulse::context::State as PulseContextState;

use crate::{
    CONFIG_VERSION, Config,
    camera::{get_inotify, open_cameras, procs_using_camera},
};

static REC_ICON: LazyLock<crate::rec_icon::Id> = LazyLock::new(crate::rec_icon::Id::unique);
static PW_SENDER: OnceLock<Sender<u32>> = OnceLock::new();

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
    default_source_muted: Option<bool>,
    microphones: HashMap<u32, String>,
    screenshares: HashMap<u32, String>,
    cameras: HashMap<PathBuf, (i32, i32)>,
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
    CameraPrevious(HashMap<PathBuf, (i32, i32)>),
    CameraReset(PathBuf),
    DisconnectNode(u32),
    TogglePopup,
    ClosePopup(PopupId),
    KillProcess(u32),
    DefaultSourceMute(bool),
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

        let icon_style: Rc<dyn Fn(&Theme) -> SvgStyle> = Rc::new(|theme: &Theme| SvgStyle {
            color: Some(theme.cosmic().button_color().into()),
        });
        let muted_icon_style: Rc<dyn Fn(&Theme) -> SvgStyle> = Rc::new(|theme: &Theme| SvgStyle {
            color: Some(theme.cosmic().destructive_text_color().into()),
        });
        let indicator = |name: &str, style: Rc<dyn Fn(&Theme) -> SvgStyle>| {
            icon(icon::from_name(name).into())
                .class(Svg::Custom(style))
                .size(size.0)
        };

        if camera {
            icons.push(indicator("camera-web-symbolic", icon_style.clone()).into());
        }
        if microphone {
            let mic_icon = if self.default_source_muted.unwrap_or(false) {
                "microphone-sensitivity-muted-symbolic"
            } else {
                "audio-input-microphone-symbolic"
            };
            let mic_style = if self.default_source_muted.unwrap_or(false) {
                muted_icon_style.clone()
            } else {
                icon_style.clone()
            };
            icons.push(indicator(mic_icon, mic_style).into());
        }
        if screenshare {
            icons.push(indicator("accessories-screenshot-symbolic", icon_style.clone()).into());
        }

        let container_style = |theme: &Theme| {
            let cosmic = theme.cosmic();
            CtnStyle {
                background: Some(Background::Color(
                    cosmic.primary.base.with_alpha(0.5).into(),
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
                    return destroy_popup(id);
                }
                let new_id = window::Id::unique();
                self.popup = Some(new_id);
                let settings = self.core.applet.get_popup_settings(
                    self.core.main_window_id().unwrap_or(window::Id::RESERVED),
                    new_id,
                    None,
                    None,
                    None,
                );

                return get_popup(settings);
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
            Message::DefaultSourceMute(muted) => {
                self.default_source_muted = Some(muted);
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

        let pulse_sub = Self::pulse_subscription();

        Subscription::batch([pw_shares, camera_shares, config, timeline, tick, pulse_sub])
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
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
            channel(100, |output| async {
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
                                let mut out = output.clone();
                                loop {
                                    match out.try_send(msg.clone()) {
                                        Ok(()) => break,
                                        Err(_) => {
                                            eprintln!("Failed to send PipeWire event");
                                        }
                                    }
                                }
                            }
                        })
                        .global_remove(move |id| {
                            let mut out = output_remove.clone();
                            loop {
                                match out.try_send(Message::PipeWireNodeRemove(id)) {
                                    Ok(()) => break,
                                    Err(_) => eprintln!("Failed to send unshare event"),
                                }
                            }
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
            channel(100, |mut output| async {
                std::thread::spawn(move || {
                    let open_cameras = open_cameras();
                    loop {
                        match output.try_send(Message::CameraPrevious(open_cameras.clone())) {
                            Ok(()) => break,
                            Err(_) => eprintln!("Failed to send previously open camera event"),
                        }
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
                                        loop {
                                            match output
                                                .try_send(Message::CameraReset(path.clone()))
                                            {
                                                Ok(()) => break,
                                                Err(_) => {
                                                    eprintln!("Failed to send camera reset event");
                                                }
                                            }
                                        }
                                    }
                                }
                                EventMask::OPEN => {
                                    wd_path.get_by_right(&event.wd).inspect(|&path| {
                                        let msg = Message::CameraOpen(path.clone());
                                        loop {
                                            match output.try_send(msg.clone()) {
                                                Ok(()) => break,
                                                Err(_) => {
                                                    eprintln!("Failed to send camera open event");
                                                }
                                            }
                                        }
                                    });
                                }
                                EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE => {
                                    wd_path.get_by_right(&event.wd).inspect(|&path| {
                                        let msg = Message::CameraClose(path.clone());
                                        loop {
                                            match output.try_send(msg.clone()) {
                                                Ok(()) => break,
                                                Err(_) => {
                                                    eprintln!("Failed to send camera close event");
                                                }
                                            }
                                        }
                                    });
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

    fn pulse_subscription() -> Subscription<Message> {
        let pulse = || {
            channel(10, |mut output| async move {
                std::thread::spawn(move || {
                    use std::sync::mpsc;

                    let mut mainloop = match PulseMainloop::new() {
                        Some(mainloop) => mainloop,
                        None => {
                            eprintln!("Failed to create Pulse main loop");
                            return;
                        }
                    };

                    let mut context = match PulseContext::new(&mainloop, "cosmic-privacy-indicator") {
                        Some(context) => context,
                        None => {
                            eprintln!("Failed to create Pulse context");
                            return;
                        }
                    };

                    if let Err(e) = context.connect(None, pulse::context::FlagSet::NOFLAGS, None) {
                        eprintln!("Failed to connect Pulse context: {:?}", e);
                        return;
                    }

                    if let Err(e) = mainloop.start() {
                        eprintln!("Failed to start Pulse main loop: {:?}", e);
                        return;
                    }

                    loop {
                        match context.get_state() {
                            PulseContextState::Ready => break,
                            PulseContextState::Failed | PulseContextState::Terminated => {
                                eprintln!("Pulse context failed to connect");
                                return;
                            }
                            _ => std::thread::sleep(Duration::from_millis(100)),
                        }
                    }

                    let mut last_muted: Option<bool> = None;

                    loop {
                        let (server_tx, server_rx) = mpsc::channel::<Option<String>>();
                        let introspector = context.introspect();
                        introspector.get_server_info(move |server_info| {
                            let _ = server_tx.send(
                                server_info
                                    .default_source_name
                                    .as_ref()
                                    .map(|name| name.to_string()),
                            );
                        });

                        let default_source = match server_rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(Some(name)) => name,
                            Ok(None) => {
                                eprintln!("Pulse server did not report a default source");
                                std::thread::sleep(Duration::from_secs(2));
                                continue;
                            }
                            Err(_) => {
                                eprintln!("Timed out waiting for Pulse server info");
                                std::thread::sleep(Duration::from_secs(2));
                                continue;
                            }
                        };

                        let (mute_tx, mute_rx) = mpsc::channel::<Option<bool>>();
                        let introspector = context.introspect();
                        introspector.get_source_info_by_name(&default_source, move |result| {
                            use pulse::callbacks::ListResult;

                            match result {
                                ListResult::Item(info) => {
                                    let _ = mute_tx.send(Some(info.mute));
                                }
                                ListResult::End | ListResult::Error => {
                                    let _ = mute_tx.send(None);
                                }
                            }
                        });

                        let muted = match mute_rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(Some(muted)) => muted,
                            Ok(None) => {
                                eprintln!("Pulse did not return a source mute state");
                                std::thread::sleep(Duration::from_secs(2));
                                continue;
                            }
                            Err(_) => {
                                eprintln!("Timed out waiting for Pulse source info");
                                std::thread::sleep(Duration::from_secs(2));
                                continue;
                            }
                        };

                        if last_muted != Some(muted) {
                            last_muted = Some(muted);
                            loop {
                                match output.try_send(Message::DefaultSourceMute(muted)) {
                                    Ok(()) => break,
                                    Err(_) => eprintln!("Failed to send default source mute event"),
                                }
                            }
                        }

                        std::thread::sleep(Duration::from_secs(2));
                    }
                });
            })
        };
        Subscription::run(pulse)
    }
}
