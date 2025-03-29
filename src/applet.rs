// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cosmic::app::{Core, Task};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::{stream, Background, Border, Subscription};
use cosmic::theme::{Container, Svg, Theme};
use cosmic::widget::icon::Named;
use cosmic::widget::{container::Style as ContainerStyle, svg::Style as SvgStyle};
use cosmic::widget::{icon, layer_container, Column, Row};
use cosmic::{Application, Apply, Element};
use cosmic_time::{anim, chain, once_cell::sync::Lazy, Timeline};

use glob::glob;
use pipewire::context::Context;
use pipewire::main_loop::MainLoop;

static REC_ICON: Lazy<crate::rec_icon::Id> = Lazy::new(crate::rec_icon::Id::unique);

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
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    RecTick(Instant),
    ScreenShareAdd(u32),
    MicrophoneAdd(u32),
    PipeWireNodeRemove(u32),
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

    fn view(&self) -> Element<Self::Message> {
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
            return "".into();
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
            ContainerStyle {
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
                .spacing(pad)
                .apply(layer_container)
        } else {
            Column::with_children(shared)
                .spacing(pad)
                .apply(layer_container)
        }
        .padding(pad)
        .class(Container::Custom(Box::new(container_style)));

        self.core.applet.autosize_window(container).into()
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                self.shared = Shared {
                    microphone: !self.microphones.is_empty(),
                    screenshare: !self.screenshares.is_empty(),
                    camera: is_camera_shared(),
                };
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
        };
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        struct Pipewire;
        let shares = Subscription::run_with_id(
            std::any::TypeId::of::<Pipewire>(),
            stream::channel(100, move |output| async move {
                std::thread::spawn(move || {
                    pipewire::init();
                    let main_loop =
                        MainLoop::new(None).expect("Failed to create PipeWire main loop");
                    let context =
                        Context::new(&main_loop).expect("Failed to create PipeWire context");
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
                                                    eprintln!("Failed to send ScreenCast share event");
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
            }),
        );
        // Weirdly enough, self.timeline.as_subscription() is too resource heavy, even comparing at 200Hz
        let timeline = cosmic::iced::time::every(Duration::from_millis(20)).map(Message::RecTick); // 50Hz
        let tick = cosmic::iced::time::every(Duration::from_millis(2000)).map(|_| Message::Tick);

        Subscription::batch([shares, timeline, tick])
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
        Some(cosmic::applet::style())
    }
}

fn is_camera_shared() -> bool {
    glob("/proc/[0-9]*/fd/[0-9]*")
        .unwrap()
        .filter_map(Result::ok)
        .any(|path| {
            if let Ok(link) = std::fs::read_link(path) {
                if link.to_string_lossy().starts_with("/dev/video") {
                    return true;
                }
            }
            false
        })
}
