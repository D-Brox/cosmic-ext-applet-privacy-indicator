// SPDX-License-Identifier: GPL-3.0-only

use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::cosmic_theme::palette::Mix;
use cosmic::theme::{Svg, Theme};
use cosmic::widget::{icon, icon::Handle, svg::Style as SvgStyle, Icon, Id as CosmicId};
use cosmic_time::{
    timeline::{Chain as TimelineChain, Frame, Interped},
    Ease, MovementType, Quadratic, Repeat, Timeline,
};

static REC_ICON_HANDLE: LazyLock<Handle> =
    LazyLock::new(|| icon::from_name("media-record-symbolic").into());

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Id(CosmicId);

impl Id {
    pub fn new(id: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self(CosmicId::new(id))
    }

    pub fn unique() -> Self {
        Self(CosmicId::unique())
    }

    pub fn into_chain(self) -> Chain {
        Chain::new(self)
    }

    pub fn as_widget(&self, timeline: &Timeline, size: u16) -> Icon {
        RecIcon::as_widget(self, timeline, size)
    }
}

impl From<Id> for CosmicId {
    fn from(value: Id) -> Self {
        value.0
    }
}

#[derive(Debug)]
pub struct Chain {
    id: Id,
    links: Vec<RecIcon>,
}

impl Chain {
    pub fn new(id: Id) -> Self {
        Chain {
            id,
            links: vec![
                RecIcon::new(Duration::ZERO).alpha(0.0),
                RecIcon::new(Duration::from_millis(1000)).alpha(1.0),
                RecIcon::new(Duration::from_millis(250)).alpha(1.0),
                RecIcon::new(Duration::from_millis(1000)).alpha(0.0),
                RecIcon::new(Duration::from_millis(250)).alpha(0.0),
            ],
        }
    }
}

impl From<Chain> for TimelineChain {
    fn from(chain: Chain) -> Self {
        TimelineChain::new(
            chain.id.0,
            Repeat::Forever,
            chain
                .links
                .into_iter()
                .map(std::convert::Into::into)
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RecIcon {
    at: MovementType,
    ease: Ease,
    alpha: f32,
}

impl RecIcon {
    pub fn new(at: impl Into<MovementType>) -> RecIcon {
        let at = at.into();
        RecIcon {
            at,
            ease: Quadratic::InOut.into(),
            alpha: 1.0,
        }
    }

    pub fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha;
        self
    }

    pub fn as_widget(id: &Id, timeline: &Timeline, size: u16) -> Icon {
        let value = if let Some(Interped { value, .. }) = timeline.get(&id.0, 0) {
            value
        } else {
            1.0
        };
        icon(REC_ICON_HANDLE.clone())
            .class(Svg::Custom(Rc::new(move |theme: &Theme| {
                let cosmic = theme.cosmic();
                SvgStyle {
                    color: Some(
                        cosmic
                            .background
                            .base
                            .mix(cosmic.destructive_text_color(), value)
                            .into(),
                    ),
                }
            })))
            .size(size)
    }
}

impl From<RecIcon> for Vec<Option<Frame>> {
    fn from(icon: RecIcon) -> Vec<Option<Frame>> {
        vec![Some(Frame::lazy(icon.at, icon.alpha, icon.ease))]
    }
}
