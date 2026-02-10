// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::{HashMap, HashSet},
    fs::{read_dir, read_link},
    path::PathBuf,
    rc::Rc,
    sync::LazyLock,
    time::{Duration, Instant},
};

use cosmic::{
    Application, Apply, Element,
    app::{Core, Task},
    cosmic_theme::palette::WithAlpha,
    iced::{Background, Border, Subscription, core::layout::Limits, stream::channel},
    theme::{Container, Svg, Theme},
    widget::{
        Column, Row, container::Style as CtnStyle, icon, layer_container, svg::Style as SvgStyle,
    },
};
use cosmic_time::{Timeline, anim, chain};

use bimap::BiHashMap;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use pipewire::{context::ContextRc, main_loop::MainLoopRc};

use crate::camera::{get_inotify, open_cameras};

static REC_ICON: LazyLock<crate::rec_icon::Id> = LazyLock::new(crate::rec_icon::Id::unique);

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
    microphones: HashSet<u32>,
    screenshares: HashSet<u32>,
    cameras: HashMap<PathBuf, (i32, i32)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    RecTick(Instant),
    ScreenShareAdd(u32),
    MicrophoneAdd(u32),
    PipeWireNodeRemove(u32),
    CameraOpen(PathBuf),
    CameraClose(PathBuf),
    CameraPrevious(HashMap<PathBuf, (i32, i32)>),
    CameraReset(PathBuf),
}

impl Application for PrivacyIndicator {
    type Executor = cosmic::executor::Default;

    type Flags = ();

    type Message = Message;

    const APP_ID: &'static str = "dev.DBrox.CosmicPrivacyIndicator";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut timeline = Timeline::new();
        timeline.set_chain(chain![REC_ICON]).start();

        let app = PrivacyIndicator {
            core,
            timeline,
            ..Default::default()
        };

        (app, Task::none())
    }

    fn view(&'_ self) -> Element<'_, Self::Message> {
        let horizontal = self.core.applet.is_horizontal();
        let size = self.core.applet.suggested_size(true);
        let pad = self.core.applet.suggested_padding(true);

        let mut shared: Vec<Element<Self::Message>> = vec![];
        let Shared {
            microphone,
            screenshare,
            camera,
        } = self.shared;

        if screenshare || microphone || camera {
            shared.push(anim![REC_ICON, &self.timeline, size.0].into());
        } else {
            return self
                .core
                .applet
                .autosize_window("")
                .limits(Limits::NONE)
                .into();
        }

        let icon_style = Rc::new(|theme: &Theme| SvgStyle {
            color: Some(theme.cosmic().button_color().into()),
        });
        let indicator = |name: &str| {
            icon(icon::from_name(name).into())
                .class(Svg::Custom(icon_style.clone()))
                .size(size.0)
        };

        if camera {
            shared.push(indicator("camera-web-symbolic").into());
        }
        if microphone {
            shared.push(indicator("audio-input-microphone-symbolic").into());
        }
        if screenshare {
            shared.push(indicator("accessories-screenshot-symbolic").into());
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
        let container = if horizontal {
            Row::with_children(shared)
                .spacing(pad.0)
                .apply(layer_container)
        } else {
            Column::with_children(shared)
                .spacing(pad.1)
                .apply(layer_container)
        }
        .padding(pad.0.min(pad.1))
        .class(Container::Custom(Box::new(container_style)));

        self.core
            .applet
            .autosize_window(container)
            .limits(Limits::NONE)
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
            Message::CameraPrevious(cameras) => self.cameras = cameras,
            Message::CameraOpen(path) => {
                self.cameras
                    .entry(path)
                    .and_modify(|v| v.0 += 1)
                    .or_insert((1, 0));
            }
            Message::CameraClose(path) => {
                self.cameras
                    .entry(path)
                    .and_modify(|v| {
                        v.0 -= 1;
                        v.1 = v.1.min(v.0);
                    })
                    .or_insert((0, 0));
            }
            Message::CameraReset(path) => {
                self.cameras.remove(&path);
            }
            Message::ScreenShareAdd(id) => {
                self.screenshares.insert(id);
            }
            Message::MicrophoneAdd(id) => {
                self.microphones.insert(id);
            }
            Message::PipeWireNodeRemove(id) => {
                self.screenshares.remove(&id);
                self.microphones.remove(&id);
            }
            Message::RecTick(now) => self.timeline.now(now),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let pw_shares = Subscription::run(|| {
            channel(100, |output| async {
                std::thread::spawn(move || {
                    pipewire::init();
                    let main_loop =
                        MainLoopRc::new(None).expect("Failed to create PipeWire main loop");
                    let context = ContextRc::new(&main_loop, None)
                        .expect("Failed to create PipeWire context");
                    let core = context
                        .connect(None)
                        .expect("Failed to connect to PipeWire");
                    let registry = core
                        .get_registry()
                        .expect("Failed to get PipeWire registry");
                    let output_remove = output.clone();
                    let _listener = registry
                        .add_listener_local()
                        .global(move |global| {
                            if global.type_.to_str() == "PipeWire:Interface:Node" {
                                global.props.map(|props| {
                                    props
                                        .get("media.class")
                                        .map(|media_class| match media_class {
                                            "Stream/Input/Video" => {
                                                // Screen captures/recordings in wayland are usually done through pipewire
                                                let mut output = output.clone();
                                                while output
                                                    .try_send(Message::ScreenShareAdd(global.id))
                                                    .is_err()
                                                {
                                                    eprintln!(
                                                        "Failed to send ScreenCast share event"
                                                    );
                                                }
                                            }
                                            "Stream/Input/Audio" => {
                                                // Microphones are
                                                let mut output = output.clone();
                                                while output
                                                    .try_send(Message::MicrophoneAdd(global.id))
                                                    .is_err()
                                                {
                                                    eprintln!(
                                                        "Failed to send Microphone share event"
                                                    );
                                                }
                                            }
                                            _ => (),
                                        })
                                });
                            }
                        })
                        .global_remove(move |id| {
                            let mut output = output_remove.clone();
                            while output.try_send(Message::PipeWireNodeRemove(id)).is_err() {
                                eprintln!("Failed to send unshare event");
                            }
                        })
                        .register();
                    main_loop.run();
                });
            })
        });

        let camera_shares = Subscription::run(|| {
            channel(100, |mut output| async {
                std::thread::spawn(move || {
                    let open_cameras = open_cameras();
                    while output
                        .try_send(Message::CameraPrevious(open_cameras.clone()))
                        .is_err()
                    {
                        eprintln!("Failed to send previously open camera event");
                    }
                    let (mut inotify, mut wd_path) = get_inotify();
                    let mut event_buffer = [0; 1024];

                    loop {
                        for event in inotify
                            .read_events_blocking(&mut event_buffer)
                            .expect("Failed to read events")
                        {
                            match event.mask {
                                EventMask::CREATE | EventMask::ATTRIB | EventMask::DELETE_SELF => {
                                    if event.mask == EventMask::DELETE_SELF
                                        || event
                                            .name
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .starts_with("video")
                                    {
                                        let old_wd_paths = wd_path;
                                        (inotify, wd_path) = get_inotify();
                                        let old_paths =
                                            old_wd_paths.left_values().collect::<HashSet<_>>();
                                        let new_paths =
                                            wd_path.left_values().collect::<HashSet<_>>();
                                        for &path in old_paths.difference(&new_paths) {
                                            while output
                                                .try_send(Message::CameraReset(path.clone()))
                                                .is_err()
                                            {
                                                eprintln!("Failed to send camera reset event");
                                            }
                                        }
                                    }
                                }
                                EventMask::OPEN => {
                                    wd_path.get_by_right(&event.wd).inspect(|&path| {
                                        println!("open {path:?}");
                                        while output
                                            .try_send(Message::CameraOpen(path.clone()))
                                            .is_err()
                                        {
                                            eprintln!("Failed to send camera open event");
                                        }
                                    });
                                }
                                EventMask::CLOSE_WRITE | EventMask::CLOSE_NOWRITE => {
                                    wd_path.get_by_right(&event.wd).inspect(|&path| {
                                        println!("close {path:?}");
                                        while output
                                            .try_send(Message::CameraClose(path.clone()))
                                            .is_err()
                                        {
                                            eprintln!("Failed to send camera close event");
                                        }
                                    });
                                }
                                _ => continue,
                            };
                        }
                    }
                });
            })
        });

        // Weirdly enough, self.timeline.as_subscription() is too resource heavy, since it follows the compositors refresh rate
        let timeline = cosmic::iced::time::every(Duration::from_millis(20)).map(Message::RecTick); // 50Hz
        let tick = cosmic::iced::time::every(Duration::from_millis(2000)).map(|_| Message::Tick);

        Subscription::batch([pw_shares, camera_shares, timeline, tick])
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
        Some(cosmic::applet::style())
    }
}
