//! 맨 아래 탭 띠. 키로도, 눌러서도 넘어간다.
//!
//! `command_bar` and `command` are ported from noid_gui (Apache-2.0,
//! Copyright (C) 2026 Paranoid Zero): `view/mod.rs`'s `command_bar` and
//! `command`, with the `key_cap` and `command_bar` styles in `theme.rs`.

use iced::keyboard::{key::Named, Key};
use iced::widget::{button, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{Message, Screen};
use crate::theme;

/// F 키 → 탭. 매핑되지 않은 키는 `None` 이고 화면을 바꾸지 않는다.
///
/// F10 (종료) 은 이 표에 없다 -- `Screen` 을 바꾸는 게 아니라 앱을 끝낸다.
/// `TABS` 아래에 `Message::Quit` 을 직접 매핑하는 별도의 리스너가 있다.
pub fn shortcut(key: &Key) -> Option<Screen> {
    match key {
        Key::Named(Named::F1) => Some(Screen::Wallet),
        Key::Named(Named::F2) => Some(Screen::Receive),
        Key::Named(Named::F3) => Some(Screen::Send),
        Key::Named(Named::F4) => Some(Screen::History),
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

pub(crate) const TABS: [(&str, &str, Screen); 6] = [
    ("F1", "Wallet", Screen::Wallet),
    ("F2", "Receive", Screen::Receive),
    ("F3", "Send", Screen::Send),
    ("F4", "History", Screen::History),
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

    /// F5·F8·F9 는 탭이 아니다. F5 는 채굴 탭이었다 -- 지갑 노드는 채굴하지 않아
    /// 비워 두었고, 뒤의 번호를 당기지 않아 F6·F7 단축키가 그대로다.
    /// F8·F9 도 탭이 아니다. 아무 F 키나 화면을 바꾸면 사용자가 어디로
    /// 갔는지 모른다. F10 도 `shortcut` 의 몫이 아니다 -- `Screen` 이 아니라
    /// 종료라서 `is_quit_shortcut` 이 따로 답한다.
    #[test]
    fn an_unmapped_key_changes_nothing() {
        assert_eq!(shortcut(&Key::Named(Named::F5)), None);
        assert_eq!(shortcut(&Key::Named(Named::F8)), None);
        assert_eq!(shortcut(&Key::Named(Named::Enter)), None);
        assert_eq!(shortcut(&Key::Named(Named::F10)), None);
    }

    /// 화면의 다른 모든 글자가 영어인데 탭 띠만 한국어였다 (눈으로 보고
    /// 잡았다 -- 라벨은 `Element` 안에 있어 diff 로는 섞인 게 안 보인다).
    /// 라벨은 `TABS` 한 곳에만 있으므로, 여기서 ASCII 를 요구해 두면 다음에
    /// 탭을 더하는 사람이 같은 실수를 하기 전에 테스트가 먼저 막는다.
    #[test]
    fn every_tab_label_is_ascii_like_the_rest_of_the_ui() {
        for (key, label, _) in crate::view::tabs::TABS {
            assert!(key.is_ascii(), "탭 키 {key} 가 ASCII 가 아니다");
            assert!(label.is_ascii(), "탭 라벨 {label} 가 ASCII 가 아니다");
        }
    }

    #[test]
    fn f10_is_the_quit_shortcut_and_nothing_else_is() {
        assert!(is_quit_shortcut(&Key::Named(Named::F10)));
        assert!(!is_quit_shortcut(&Key::Named(Named::F1)));
        assert!(!is_quit_shortcut(&Key::Named(Named::Enter)));
    }
}
