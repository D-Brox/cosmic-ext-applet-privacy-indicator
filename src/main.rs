// SPDX-License-Identifier: GPL-3.0-only

mod applet;
mod audit;
mod camera;
mod rec_icon;

use cosmic::{
    Application,
    cosmic_config::{
        self, Config as CosmicConfig, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
    },
};
use serde::{Deserialize, Serialize};

use crate::applet::PrivacyIndicator;

pub const CONFIG_VERSION: u64 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, CosmicConfigEntry)]
pub struct Config {
    pub animated: bool,
    pub refresh: u64,
    /// When enabled, privacy events are appended to a local, owner-only audit
    /// log. Opt-in: disabled by default.
    pub audit_log: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            animated: true,
            refresh: 20,
            audit_log: false,
        }
    }
}

fn main() -> cosmic::iced::Result {
    let (config, config_handler) = match CosmicConfig::new(PrivacyIndicator::APP_ID, CONFIG_VERSION)
    {
        Ok(config_handler) => match Config::get_entry(&config_handler) {
            Ok(ok) => (ok, Some(config_handler)),
            Err((errs, config)) => {
                println!("errors loading config: {errs:?}");
                (config, Some(config_handler))
            }
        },
        Err(err) => {
            println!("failed to create config handler: {err}");
            (Config::default(), None)
        }
    };

    if let Some(config_handler) = config_handler {
        _ = config.write_entry(&config_handler);
    }

    cosmic::applet::run::<applet::PrivacyIndicator>(config)
}
