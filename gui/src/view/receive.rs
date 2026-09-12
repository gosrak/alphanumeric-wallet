//! Receive screen: the active address, large and as a QR code, and the gate
//! that exports its spendable seed.
//!
//! Widget layout only. The QR encoding itself is built and kept in `app.rs`
//! alongside the address it encodes, not rebuilt on every frame here.

use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, qr_code, row, text, text_input};
use iced::{Alignment, Element, Length};

use alphanumeric_gui::activity;
use alphanumeric_gui::backend::NodeStatus;
use alphanumeric_gui::history::{Row, RowKind};
use alphanumeric_gui::model;

use crate::app::{AddressEntry, App, Message, RevealState};
use crate::theme;
use crate::view::kit;

/// Rows read to find this address's incoming ones -- the newest 50 across the
/// wallet, already fetched.
const INCOMING_WANT: usize = 50;

/// Characters of the widest realistic value plus one for the gap (see
/// `view/wallet.rs`'s `ADDRESS_COLUMNS`): FROM holds `own c0ffee11…66778899`.
const INCOMING_COLUMNS: [(&str, u16); 4] =
    [("HEIGHT", 8), ("FROM", 22), ("AMOUNT", 14), ("STATUS", 10)];

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(wallet) = &app.wallet else {
        return kit::screen(text("No wallet loaded."));
    };
    let Some(entry) = wallet.addresses.get(wallet.active) else {
        return kit::screen(text("No address selected."));
    };
    let next = (wallet.addresses.len() > 1).then(|| (wallet.active + 1) % wallet.addresses.len());

    let mut code = row![].spacing(14).align_y(Alignment::Start);
    if let Some(data) = &wallet.qr {
        code = code.push(
            container(qr_code(data).cell_size(5))
                .style(theme::surface)
                .padding(10),
        );
    }
    code = code.push(
        column![
            kit::action("COPY ADDRESS", kit::Act::Go, Some(Message::CopyAddress)),
            kit::action(
                "NEXT ADDRESS",
                kit::Act::Plain,
                next.map(Message::SetActiveAddress),
            ),
        ]
        .spacing(8),
    );

    let address = column![
        row![
            text(format!("[{}]", entry.label()))
                .size(13)
                .color(theme::LAVENDER),
            kit::field_label("ADDRESS"),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        // Text wrapped by glyph, like the revealed seed below -- not a
        // read-only `text_input`. In a half-width panel at 760 px the input
        // showed ~34 of the 40 characters and scrolled the tail out of sight
        // (the failure commit 2c28d1e warned about), on the screen whose job
        // is showing the address. COPY ADDRESS is the way to copy it.
        text(entry.address.clone())
            .size(16)
            .font(theme::TECH_FONT)
            .color(theme::VALUE)
            .wrapping(Wrapping::Glyph),
        code,
    ]
    .spacing(10);

    let recent = app.recent_activity(INCOMING_WANT);
    let incoming = incoming_table(&entry.address, &recent, wallet.node_status.as_ref());

    let (receive_title, receive_color) = if entry.is_imported() {
        ("IMPORTED ADDRESS", theme::LAVENDER)
    } else {
        ("RECEIVE", theme::ACCENT)
    };
    kit::screen(kit::split(
        app.narrow(),
        column![
            kit::tag_panel(receive_title, receive_color, address),
            kit::tag_panel(
                "ADDRESS SEED",
                theme::ADVISORY,
                reveal_section(&wallet.reveal, entry),
            ),
        ]
        .spacing(f32::from(theme::SPACING))
        .into(),
        1,
        kit::tag_panel(
            format!("INCOMING TO [{}]", entry.label()),
            theme::LAVENDER,
            incoming,
        ),
        1,
    ))
}

/// Money that arrived at `address` (see the test for the cases).
fn is_incoming(entry: &Row, address: &str) -> bool {
    match &entry.kind {
        RowKind::Mining | RowKind::In { .. } => entry.owner == address,
        RowKind::Internal { from, to } => to == address && from != to,
        RowKind::Out { .. } => false,
    }
}

fn incoming_table(
    address: &str,
    recent: &activity::Recent,
    status: Option<&NodeStatus>,
) -> iced::Element<'static, Message> {
    let tip = status.and_then(|s| s.height);
    let finalized = status.and_then(|s| s.finalized_height);
    let mut body = column![kit::table_head(&INCOMING_COLUMNS)];
    let mut shown = 0;
    for entry in recent.rows.iter().filter(|r| is_incoming(r, address)) {
        let from = match &entry.kind {
            RowKind::Mining => "MINING_REWARDS".to_string(),
            RowKind::In { from } => kit::short_address(from),
            RowKind::Internal { from, .. } => format!("own {}", kit::short_address(from)),
            RowKind::Out { .. } => String::new(),
        };
        let (state, state_colour) = kit::status_cell(activity::row_status(entry, tip, finalized));
        body = body.push(kit::table_row(
            vec![
                kit::cell(entry.height.to_string(), INCOMING_COLUMNS[0].1, theme::CYAN),
                kit::cell(from, INCOMING_COLUMNS[1].1, theme::TEXT),
                kit::cell(
                    format!("+{}", model::format_coins(entry.amount_units)),
                    INCOMING_COLUMNS[2].1,
                    theme::ACCENT,
                ),
                kit::cell(state, INCOMING_COLUMNS[3].1, state_colour),
            ],
            shown % 2 == 1,
            false,
            None,
        ));
        shown += 1;
    }
    // Only the newest INCOMING_WANT rows were read. Finding nothing in them
    // is "none in the latest 50", not "nothing ever" -- a wallet whose latest
    // 50 rows are all another address's rewards would otherwise say this
    // address never received anything beside a nonzero balance.
    let coverage = recent.coverage(status);
    if shown == 0 {
        body = body.push(
            container(
                text(kit::filtered_empty(
                    "No incoming transfers",
                    "Nothing has arrived at this address yet.",
                    coverage,
                    INCOMING_WANT,
                ))
                .size(12)
                .color(theme::MUTED)
                .wrapping(Wrapping::WordOrGlyph),
            )
            .padding([8, 8]),
        );
    }
    body = body.push(kit::table_foot(
        text(kit::filtered_foot(
            "RECEIVED",
            shown,
            coverage,
            INCOMING_WANT,
        ))
        .size(12)
        .color(theme::MUTED)
        .into(),
        kit::action(
            "F4 HISTORY",
            kit::Act::Plain,
            Some(Message::Show(crate::app::Screen::History)),
        ),
    ));
    body.into()
}

/// The three states of the seed gate, in DANGER throughout.
///
/// Every screen on which a spendable key can appear is framed the same colour,
/// so the frame itself is the signal -- `send.rs` uses DANGER for a payment
/// whose outcome is unknown, and nothing else in the wallet borrows it.
fn reveal_section<'a>(reveal: &'a RevealState, entry: &AddressEntry) -> Element<'a, Message> {
    match reveal {
        RevealState::Idle => column![
            text(
                "A key that can spend this address. It is shown only after the \
                 passphrase is entered again, and cleared when you leave this screen."
            )
            .size(f32::from(theme::SMALL))
            .color(theme::MUTED),
            kit::action(
                "REVEAL SEED",
                kit::Act::Care,
                Some(Message::RevealSeedStart)
            ),
        ]
        .spacing(f32::from(theme::SPACING))
        .into(),

        RevealState::Asking {
            passphrase,
            error,
            busy,
        } => {
            let mut card = column![
                text("This shows a key that can spend this address")
                    .size(f32::from(theme::BODY))
                    .color(theme::DANGER),
                // The node says the same thing at its own export-seed prompt.
                // Worth repeating rather than shortening: this is the only
                // sentence between the user and handing over the funds.
                text(
                    "Anyone who sees it owns what is in it. Enter your passphrase to continue -- \
                     the one you unlocked with is not reused here."
                )
                .size(f32::from(theme::SMALL))
                .color(theme::MUTED),
                text_input("Passphrase", passphrase)
                    .secure(true)
                    .on_input(Message::RevealPassphraseChanged)
                    .on_submit(Message::RevealSeedConfirm)
                    .padding(8)
                    .style(theme::text_input),
            ]
            .spacing(f32::from(theme::SPACING));

            if let Some(message) = error {
                card = card.push(
                    text(message)
                        .size(f32::from(theme::SMALL))
                        .color(theme::DANGER),
                );
            }

            card = card.push(
                row![
                    button(text(if *busy { "Checking..." } else { "Show seed" }))
                        // No press while the derivation is running: a second
                        // Argon2id pass would be started against a passphrase
                        // field the first one is already using.
                        .on_press_maybe((!*busy).then_some(Message::RevealSeedConfirm))
                        .padding(10)
                        .style(|_, status| theme::colored_primary(theme::DANGER, status)),
                    button(text("Cancel"))
                        .on_press(Message::RevealSeedCancel)
                        .padding(10)
                        .style(|_, status| theme::button(theme::ButtonKind::Ghost, status)),
                ]
                .spacing(f32::from(theme::SPACING)),
            );

            container(card)
                .style(theme::verdict_card(theme::DANGER))
                .padding(theme::PADDING)
                .into()
        }

        RevealState::Revealed { address, seed_hex } => {
            let mut card = column![
                text("Seed for this address")
                    .size(f32::from(theme::BODY))
                    .color(theme::DANGER),
                // The address it belongs to, spelled out. A seed is 64
                // characters of nothing, and the one mistake that costs money
                // here is exporting the wrong row.
                text(address.as_str())
                    .size(f32::from(theme::CAPTION))
                    .color(theme::MUTED),
                // A wrapping `text` in a bordered box, exactly as the backup
                // screen shows the master seed -- and for the same reason. A
                // `text_input` holds 64 characters on one line and simply
                // scrolls the tail out of view: it looked fine and showed
                // about fifty of them, so the one thing the user needs to be
                // able to do here -- check that what they are about to paste
                // is what they meant -- was the thing it prevented.
                container(
                    text(seed_hex.as_str())
                        .size(f32::from(theme::BODY))
                        // `Wrapping::Word`, the default, has nothing to break
                        // on: a seed is 64 hex characters with no spaces in it,
                        // so word wrapping leaves it on one line and the box
                        // clips the tail. Glyph wrapping is what makes all 64
                        // visible.
                        .wrapping(iced::widget::text::Wrapping::Glyph),
                )
                .padding(theme::PADDING)
                .width(Length::Fill)
                .style(iced::widget::container::bordered_box),
            ]
            .spacing(f32::from(theme::SPACING));

            if entry.is_imported() {
                card = card.push(
                    text(
                        "This key is not part of your master seed. A master-seed restore will \
                         not bring this address back -- keep the wallet file backed up.",
                    )
                    .size(12)
                    .color(theme::ADVISORY)
                    .wrapping(Wrapping::WordOrGlyph),
                );
            }

            card = card
                .push(
                    text(
                        "Paste it at the node's `import-seed` prompt. Clear the clipboard when \
                         you are done: anything running on this machine can read it.",
                    )
                    .size(f32::from(theme::SMALL))
                    .color(theme::MUTED),
                )
                .push(
                    row![
                        button(text("Copy seed"))
                            .on_press(Message::CopySeed)
                            .padding(10)
                            .style(|_, status| theme::colored_primary(theme::DANGER, status)),
                        button(text("Clear clipboard"))
                            .on_press(Message::ClearClipboard)
                            .padding(10)
                            .style(|_, status| theme::button(theme::ButtonKind::Secondary, status)),
                        button(text("Hide"))
                            .on_press(Message::RevealSeedCancel)
                            .padding(10)
                            .style(|_, status| theme::button(theme::ButtonKind::Ghost, status)),
                    ]
                    .spacing(f32::from(theme::SPACING)),
                );

            container(card)
                .style(theme::verdict_card(theme::DANGER))
                .padding(theme::PADDING)
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alphanumeric_gui::history::{Row, RowKind};

    fn row(kind: RowKind, owner: &str) -> Row {
        Row {
            height: 1,
            position: 0,
            timestamp: 0,
            amount_units: 100,
            fee_units: 7,
            kind,
            owner: owner.into(),
        }
    }

    #[test]
    fn realistic_figures_fit_the_incoming_table_at_1000_px() {
        kit::sizing::assert_fits(
            &INCOMING_COLUMNS,
            &[
                "1015860",
                "own c0ffee11\u{2026}66778899",
                "+1.23456789",
                "CONFIRMED",
            ],
            kit::sizing::row_width(1000.0, 2, 0.5),
        );
        kit::sizing::assert_heads_fit(&INCOMING_COLUMNS, kit::sizing::row_width(760.0, 2, 0.5));
    }

    /// Money that arrived at this address: a reward or payment its own
    /// stream recorded, or the receiving side of a move between two of this
    /// wallet's addresses. A self-send brought nothing in.
    #[test]
    fn incoming_means_money_arrived_at_this_address() {
        assert!(is_incoming(&row(RowKind::Mining, "a"), "a"));
        assert!(is_incoming(
            &row(RowKind::In { from: "x".into() }, "a"),
            "a"
        ));
        assert!(!is_incoming(
            &row(RowKind::In { from: "x".into() }, "b"),
            "a"
        ));
        assert!(
            !is_incoming(&row(RowKind::Mining, "b"), "a"),
            "another address's reward did not arrive here"
        );
        assert!(!is_incoming(
            &row(RowKind::Out { to: "x".into() }, "a"),
            "a"
        ));
        assert!(is_incoming(
            &row(
                RowKind::Internal {
                    from: "b".into(),
                    to: "a".into()
                },
                "b"
            ),
            "a"
        ));
        assert!(!is_incoming(
            &row(
                RowKind::Internal {
                    from: "a".into(),
                    to: "a".into()
                },
                "a"
            ),
            "a"
        ));
    }
}
