// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::{stream, Background, Border, Subscription};
use cosmic::iced_widget::svg::Style as SvgStyle;
use cosmic::theme::Theme;
use cosmic::theme::{Container, Svg};
use cosmic::widget::container::Style as ContainerStyle;
use cosmic::widget::{icon, layer_container, Column, Row};
use cosmic::{Application, Apply, Element};
use glob::glob;
use pipewire::context::Context;
use pipewire::main_loop::MainLoop;

#[derive(Default)]
struct Shared {
    pub microphone: bool,
    pub screenshare: bool,
    pub camera: bool,
}

#[derive(Default)]
pub struct PrivacyIndicator {
    core: Core,
    shared: Shared,
    microphones: HashSet<u32>,
    screenshares: HashSet<u32>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
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
        let app = PrivacyIndicator {
            core,
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
            shared.push(
                icon(icon::from_name("media-record-symbolic").into())
                    .class(Svg::Custom(Rc::new(|theme: &Theme| SvgStyle {
                        color: Some(theme.cosmic().destructive_text_color().into()),
                    })))
                    .size(size.0)
                    .into(),
            );
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
        };
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        struct Pipewire;
        let tick = cosmic::iced::time::every(Duration::from_millis(2000)).map(|_| Message::Tick);
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
                                                    eprintln!("failed to send screen");
                                                }
                                            }
                                            "Stream/Input/Audio" => {
                                                // Microphones are
                                                let mut output = output.clone();
                                                while output
                                                    .try_send(Message::MicrophoneAdd(global.id))
                                                    .is_err()
                                                {
                                                    eprintln!("failed to send mic");
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
                                eprintln!("failed to send remove");
                            }
                        })
                        .register();
                    main_loop.run();
                });
            }),
        );

        Subscription::batch([tick, shares])
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
