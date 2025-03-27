// SPDX-License-Identifier: GPL-3.0-only

mod applet;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<applet::PrivacyIndicator>(())
}
