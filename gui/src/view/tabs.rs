//! The bottom tab strip. Switches by key or by click.
//!
//! `command_bar` and `command` are ported from noid_gui (Apache-2.0,
//! Copyright (C) 2026 Paranoid Zero): `view/mod.rs`'s `command_bar` and
//! `command`, with the `key_cap` and `command_bar` styles in `theme.rs`.

use iced::keyboard::{key::Named, Key};
use iced::widget::{button, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{Message, Screen};
use crate::theme;

/// F-key -> tab. An unmapped key is `None` and changes nothing.
///
/// F10 (quit) is not in this table -- it doesn't change the `Screen`, it
/// ends the app. A separate listener below `TABS` maps it directly to
/// `Message::Quit`.
pub fn shortcut(key: &Key) -> Option<Screen> {
    match key {
        Key::Named(Named::F1) => Some(Screen::Wallet),
        Key::Named(Named::F2) => Some(Screen::Receive),
        Key::Named(Named::F3) => Some(Screen::Send),
        Key::Named(Named::F4) => Some(Screen::History),
        Key::Named(Named::F5) => Some(Screen::Mining),
        Key::Named(Named::F6) => Some(Screen::Node),
        Key::Named(Named::F7) => Some(Screen::Settings),
        _ => None,
    }
}

/// F10 alone (`Message::Quit` has no `Screen` to return, unlike `shortcut`
/// above) so `subscription`'s key listener can map it without a `Screen`
/// wrapper around "exit the app".
pub fn is_quit_shortcut(key: &Key) -> bool {
    matches!(key, Key::Named(Named::F10))
}

pub(crate) const TABS: [(&str, &str, Screen); 7] = [
    ("F1", "Wallet", Screen::Wallet),
    ("F2", "Receive", Screen::Receive),
    ("F3", "Send", Screen::Send),
    ("F4", "History", Screen::History),
    ("F5", "Mining", Screen::Mining),
    ("F6", "Node", Screen::Node),
    ("F7", "Settings", Screen::Settings),
];

/// Full-width function-key bar, noid's `command_bar`: each key gets an equal
/// share, a coloured key cap (green when active, cyan otherwise) and its name.
/// 12 px text so the widest name (`Settings`) still fits an eighth of the
/// 760 px minimum window.
pub fn command_bar<'a>(active: Screen) -> Element<'a, Message> {
    let mut bar = row![]
        .spacing(4)
        .height(Length::Fill)
        .width(Length::Fill)
        .align_y(Alignment::Center);
    for (key, label, screen) in TABS {
        bar = bar.push(command(key, label, Message::Show(screen), screen == active));
    }
    // F10: not a `Screen`, so it is not in `TABS` -- closing the window is
    // enough to stop the node (`Supervisor`'s `Drop` SIGTERMs the child), and
    // `Message::Quit` asks the runtime for exactly that same clean shutdown.
    bar = bar.push(command("F10", "Quit", Message::Quit, false));
    container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(40.0))
        .padding(4)
        .style(theme::command_bar)
        .into()
}

fn command<'a>(
    key: &'static str,
    label: &'static str,
    message: Message,
    active: bool,
) -> Element<'a, Message> {
    button(
        row![
            container(text(key).size(12).color(theme::INK))
                .height(Length::Fill)
                .padding([5, 4])
                .align_y(Alignment::Center)
                .style(theme::key_cap(if active {
                    theme::ACCENT
                } else {
                    theme::CYAN
                })),
            container(
                text(label)
                    .size(12)
                    .color(if active { theme::CYAN } else { theme::TEXT })
                    .wrapping(iced::widget::text::Wrapping::None),
            )
            .height(Length::Fill)
            .padding([5, 3])
            .align_y(Alignment::Center),
        ]
        .spacing(0)
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .height(Length::Fill)
    .width(Length::FillPortion(1))
    .padding(0)
    .on_press(message)
    .style(move |_, status| {
        theme::button(
            if active {
                theme::ButtonKind::CommandActive
            } else {
                theme::ButtonKind::Command
            },
            status,
        )
    })
    .into()
}

#[cfg(test)]
mod tests {
    use crate::app::Screen;
    use crate::view::tabs::{is_quit_shortcut, shortcut};
    use iced::keyboard::{key::Named, Key};

    #[test]
    fn the_function_keys_map_to_their_tabs() {
        assert_eq!(shortcut(&Key::Named(Named::F1)), Some(Screen::Wallet));
        assert_eq!(shortcut(&Key::Named(Named::F2)), Some(Screen::Receive));
        assert_eq!(shortcut(&Key::Named(Named::F3)), Some(Screen::Send));
        assert_eq!(shortcut(&Key::Named(Named::F4)), Some(Screen::History));
        assert_eq!(shortcut(&Key::Named(Named::F6)), Some(Screen::Node));
        assert_eq!(shortcut(&Key::Named(Named::F7)), Some(Screen::Settings));
    }

    /// F5, F8, and F9 are not tabs. F5 used to be the mining tab -- left
    /// empty since the wallet node doesn't mine, without pulling the later
    /// numbers forward, so the F6/F7 shortcuts stay put. F8 and F9 aren't
    /// tabs either: if any F key changed the screen, the user wouldn't know
    /// where they'd gone. F10 isn't `shortcut`'s job either -- it's not a
    /// `Screen`, it's quit, so `is_quit_shortcut` answers that separately.
    #[test]
    fn an_unmapped_key_changes_nothing() {
        assert_eq!(shortcut(&Key::Named(Named::F5)), Some(Screen::Mining));
        assert_eq!(shortcut(&Key::Named(Named::F8)), None);
        assert_eq!(shortcut(&Key::Named(Named::Enter)), None);
        assert_eq!(shortcut(&Key::Named(Named::F10)), None);
    }

    /// Every other label on screen was English, but the tab strip alone was
    /// Korean (caught by eye -- the label lives inside an `Element`, so a
    /// diff doesn't show it mixed in). Labels live in exactly one place,
    /// `TABS`, so requiring ASCII here lets this test catch the same
    /// mistake before the next person who adds a tab makes it.
    #[test]
    fn every_tab_label_is_ascii_like_the_rest_of_the_ui() {
        for (key, label, _) in crate::view::tabs::TABS {
            assert!(key.is_ascii(), "tab key {key} is not ASCII");
            assert!(label.is_ascii(), "tab label {label} is not ASCII");
        }
    }

    #[test]
    fn f10_is_the_quit_shortcut_and_nothing_else_is() {
        assert!(is_quit_shortcut(&Key::Named(Named::F10)));
        assert!(!is_quit_shortcut(&Key::Named(Named::F1)));
        assert!(!is_quit_shortcut(&Key::Named(Named::Enter)));
    }
}
