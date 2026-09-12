//! The screen shown while the wallet's own node is coming up.
//!
//! Purely a rendering of `Phase` (`alphanumeric_gui::startup`) -- no state of
//! its own, no I/O. `app.rs` decides what phase it is; this only draws it.

use iced::widget::{column, container, progress_bar, row, text};
use iced::{Color, Element, Length};

use crate::app::Message;
use crate::theme;
use crate::view::kit;
use alphanumeric_gui::startup::{catch_up_line, Phase, SyncRate};

/// The one control every phase gets: a way out to the node settings (source,
/// binary, ports). Without it a taken port, a missing binary, no network, or
/// a stuck index build each brick the wallet on this screen.
fn node_settings_escape() -> Element<'static, Message> {
    kit::action(
        "NODE SETTINGS",
        kit::Act::Plain,
        Some(Message::OpenSetupSettings),
    )
}

pub fn startup_view<'a>(phase: &Phase, rate: &SyncRate, log: &[String]) -> Element<'a, Message> {
    let (tag, colour, body): (&str, Color, Element<'a, Message>) = match phase {
        Phase::Starting { last_line, percent } => {
            let mut items = column![text(
                "The wallet runs its own node. On a new machine it fetches the current \
                 snapshot first (about 173 MB) -- this can take anywhere from a few \
                 seconds to a few minutes depending on your connection."
            )
            .size(f32::from(theme::BODY))]
            .spacing(f32::from(theme::SPACING));
            if let Some(pct) = percent {
                items = items.push(progress_bar(0.0..=100.0, f32::from(*pct)));
            }
            let lines: Vec<String> = last_line.iter().cloned().collect();
            items = items
                .push(kit::terminal(
                    &lines,
                    "Waiting for the node's first line...",
                ))
                .push(node_settings_escape());
            (
                if percent.is_some() {
                    "DOWNLOADING"
                } else {
                    "STARTING"
                },
                theme::WARNING,
                items.into(),
            )
        }
        // `remaining == 0` while still `CatchingUp` is the `index_ready ==
        // false` case: the chain is caught up and the address index is
        // being built. A full bar would claim the node is done.
        Phase::CatchingUp { remaining: 0, .. } => (
            "INDEXING",
            theme::CYAN,
            column![
                text("The chain is caught up. Building the address index...")
                    .size(f32::from(theme::BODY)),
                node_settings_escape(),
            ]
            .spacing(f32::from(theme::SPACING))
            .into(),
        ),
        Phase::CatchingUp {
            height,
            network_height,
            remaining,
        } => {
            // Measured from where this catch-up began (`SyncRate::progress`).
            // The old bar compared `network - height` with `remaining`, the
            // same number, and so was always empty.
            let progress = rate.progress(*height, *network_height).unwrap_or(0.0);
            let per_sec = rate.blocks_per_sec();
            (
                "CATCHING UP",
                theme::CYAN,
                column![
                    row![
                        text("HEIGHT").size(12).color(theme::DIM),
                        text(format!("{height} / {network_height}"))
                            .font(theme::TECH_FONT)
                            .size(f32::from(theme::BODY))
                            .color(theme::VALUE),
                    ]
                    .spacing(10),
                    progress_bar(0.0..=1.0, progress),
                    text(catch_up_line(
                        *remaining,
                        per_sec,
                        rate.eta_secs(*remaining)
                    ))
                    .size(f32::from(theme::SMALL))
                    .color(theme::MUTED),
                    kit::terminal(log, "—"),
                    node_settings_escape(),
                ]
                .spacing(f32::from(theme::SPACING))
                .into(),
            )
        }
        Phase::Ready => (
            "READY",
            theme::ACCENT,
            column![node_settings_escape()].into(),
        ),
        Phase::Failed { message } => (
            // True for all three ways here: could not start, ended by itself,
            // stopped by us.
            "NOT RUNNING",
            theme::DANGER,
            column![
                kit::terminal(std::slice::from_ref(message), "—"),
                row![
                    kit::action("TRY AGAIN", kit::Act::Go, Some(Message::RetryNode)),
                    node_settings_escape(),
                ]
                .spacing(f32::from(theme::SPACING)),
            ]
            .spacing(f32::from(theme::SPACING))
            .into(),
        ),
    };

    container(
        column![
            row![
                text("alphanumeric").size(20).font(theme::BRAND_FONT),
                text("wallet").size(20).color(theme::MUTED),
            ]
            .spacing(8),
            kit::tag_panel(tag, colour, body),
        ]
        .spacing(f32::from(theme::SPACING))
        .max_width(560.0),
    )
    .center_x(Length::Fill)
    .padding(theme::PADDING)
    .into()
}
