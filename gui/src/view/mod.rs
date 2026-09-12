pub mod console;
pub mod history;
pub mod kit;
pub mod node;
pub mod receive;
pub mod send;
pub mod settings;
pub mod setup;
pub mod startup;
pub mod tabs;
pub mod wallet;

use iced::widget::{button, column, container, text};
use iced::{Element, Length};

use crate::app::Message;
use crate::theme;

/// One label-over-value pair: label in `MUTED` caption size, value in
/// `VALUE` body size on the technical font. For a value that can be long --
/// F6's DIRECTORY -- where `kit::kv_row`'s value column would be too narrow.
///
/// `Wrapping::WordOrGlyph`, not `Wrapping::Word` or a `text_input`: a
/// 40-character address or a long data-directory path must never
/// scroll or clip its tail out of sight while looking fine (commit
/// `2c28d1e`) -- a token too long for the line still breaks by glyph, so it
/// stays wholly visible. Not plain `Glyph` either: that split prose
/// mid-word ("single ad/dress").
pub fn field<'a>(label: &str, value: String) -> Element<'a, Message> {
    column![
        text(label.to_string())
            .size(f32::from(theme::CAPTION))
            .color(theme::MUTED),
        text(value)
            .size(f32::from(theme::BODY))
            .font(theme::TECH_FONT)
            .color(theme::VALUE)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
    ]
    .spacing(2.0)
    .into()
}

/// The shared "no node reachable" guidance block (spec 4.1).
///
/// "No node reachable" is a first-class state, not an error toast: the
/// explorer API is opt-in behind an environment variable, and this is where
/// nearly everyone gets stuck on first run if nothing explains it. Used by
/// both the setup screen (a restore blocked before it has a wallet to show)
/// and the wallet screen (a wallet that exists but cannot reach the node it
/// needs), so the two never drift into different presentations of the same
/// state.
pub fn no_node_guidance<'a>(message: &str, command: &str, retry: Message) -> Element<'a, Message> {
    container(
        container(
            column![
                text("No node reachable").size(f32::from(theme::HEADING)),
                text(message.to_string()).size(f32::from(theme::BODY)),
                container(text(command.to_string()).font(theme::TECH_FONT))
                    .padding(theme::PADDING)
                    .width(Length::Fill)
                    .style(theme::surface_alt),
                button(text("Retry"))
                    .on_press(retry)
                    .padding(12)
                    .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
            ]
            .spacing(f32::from(theme::SPACING)),
        )
        .style(theme::advisory_card)
        .padding(theme::PADDING)
        .width(Length::Fill)
        .max_width(560.0),
    )
    .center_x(Length::Fill)
    .into()
}
