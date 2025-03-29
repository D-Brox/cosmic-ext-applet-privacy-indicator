// SPDX-License-Identifier: GPL-3.0-only

mod applet;
mod rec_icon;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<applet::PrivacyIndicator>(())
}
