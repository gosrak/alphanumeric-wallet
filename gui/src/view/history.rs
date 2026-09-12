//! Transaction history merged across all addresses.
//!
//! Widget layout only. Order, collapsing, and the cursor are decided
//! headlessly by `alphanumeric_gui::history`; fetching is `app.rs`'s job.

use chrono::{DateTime, Local};
use iced::widget::{button, column, container, text};
use iced::{Element, Length};

use alphanumeric_gui::activity;
use alphanumeric_gui::history::{Row, RowKind};
use alphanumeric_gui::model::format_coins;

use crate::app::{AddressEntry, App, Message};
use crate::theme;
use crate::view::kit;

/// Characters of the widest realistic value plus one for the gap (see
/// `view/wallet.rs`'s `ADDRESS_COLUMNS`). The one full-width table, so
/// realistic rows fit on one line even at the 760 px minimum
/// (`realistic_figures_fit_the_history_table_at_both_widths`).
const HISTORY_COLUMNS: [(&str, u16); 8] = [
    ("TIME", 12),
    ("HEIGHT", 8),
    ("ADDR", 8),
    ("DIR", 6),
    ("COUNTERPARTY", 18),
    ("AMOUNT", 14),
    ("FEE", 11),
    ("STATUS", 10),
];

/// `[3]` for the wallet address a row came from; `[0]>[3]` for a move
/// between two of this wallet's addresses; `?` if the address is no longer
/// in the list.
fn addr_label(entry: &Row, addresses: &[AddressEntry]) -> String {
    let index_of = |address: &str| {
        addresses
            .iter()
            .find(|e| e.address == address)
            .map(|e| format!("[{}]", e.label()))
            .unwrap_or_else(|| "?".to_string())
    };
    match &entry.kind {
        RowKind::Internal { from, to } if from != to => {
            format!("{}>{}", index_of(from), index_of(to))
        }
        _ => index_of(&entry.owner),
    }
}

fn counterparty_cell(entry: &Row) -> String {
    match &entry.kind {
        RowKind::Mining => "MINING_REWARDS".to_string(),
        RowKind::In { from } => kit::short_address(from),
        RowKind::Out { to } => kit::short_address(to),
        RowKind::Internal { from, to } if from == to => "fee only".to_string(),
        RowKind::Internal { .. } => "own addresses".to_string(),
    }
}

/// `Row.timestamp` as the viewer's local date and time. `None` only if the
/// wire ever sent something `i64` cannot hold -- in that case the TIME cell
/// shows `—` rather than a wrong date (the HEIGHT cell beside it still
/// places the row).
fn format_local_time(timestamp: u64) -> Option<String> {
    let seconds = i64::try_from(timestamp).ok()?;
    let utc = DateTime::from_timestamp(seconds, 0)?;
    Some(utc.with_timezone(&Local).format("%m-%d %H:%M").to_string())
}

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(wallet) = &app.wallet else {
        return kit::screen(text("No wallet loaded."));
    };
    let Some(history) = &wallet.history else {
        return kit::screen(text("No history loaded."));
    };

    let mut content = column![].spacing(f32::from(theme::SPACING));

    // The node's index writes are fail-open, so the index can lag behind
    // the tip. A list in that state must not be shown as complete -- these
    // aren't missing transactions, just ones not visible yet.
    //
    // The banner text also states the *baseline* the comparison used.
    // `index_height` arrives fresh on every page, but the chain height is
    // read once, when this screen is entered (`Message::HistoryStatusFetched`)
    // -- `ConsoleTick` keeps refreshing that same value (`wallet.node_status`)
    // every 10 seconds afterward, but there's no reason to make someone who
    // just opened the screen wait those 10 seconds. Still, if the screen
    // stays open and "load more" gets pressed for a while, that baseline can
    // go stale before the next refresh -- writing it down makes a stale
    // baseline visible; leaving it out lets a "complete" verdict built on a
    // stale baseline pass silently.
    //
    // It's "the last time the node was asked", not "when this screen was
    // opened". If the status read on entry fails, `apply_status` leaves the
    // previous value in place (a retryable error silently, and even a
    // non-retryable one leaves `node_status` untouched), so the height left
    // on screen may not be from that moment. A sentence added to keep the
    // baseline honest is the same flaw if it makes a promise it can't keep.
    if let Some(indexed) = history.merge.lowest_index_height() {
        // This doesn't distinguish between never having asked the node and
        // having asked but not yet getting a height -- either way the
        // comparison can't be made, and that's all that matters here.
        let stale = match wallet.node_status.as_ref().and_then(|status| status.height) {
            Some(chain_height) if indexed < chain_height => Some(format!(
                "The node's address index had reached block {indexed}, and the chain was at {chain_height} \
                 the last time the node was asked. Anything newer is not in this list yet.",
            )),
            Some(_) => None,
            // The chain height couldn't be read -- the node didn't answer,
            // or it answered but has no height yet. Either way the
            // comparison can't be made.
            None => Some(format!(
                "The node's address index had reached block {indexed}, but the chain height \
                 could not be read, so this list cannot be judged complete.",
            )),
        };
        if let Some(message) = stale {
            content = content.push(
                container(
                    text(message)
                        .size(f32::from(theme::SMALL))
                        .color(theme::ADVISORY),
                )
                .style(theme::advisory_card)
                .padding(theme::PADDING)
                .width(Length::Fill),
            );
        }
    }

    // The index cannot answer for this address at all -- a completely
    // different state from an empty history, so it isn't drawn as an
    // empty list.
    let has_rows = history.rows_len() > 0;
    if let Some(address) = history.merge.stalled_address() {
        content = content.push(
            container(
                column![
                    text("The node cannot answer for one of your addresses yet")
                        .size(f32::from(theme::BODY))
                        .color(theme::ADVISORY),
                    text(address.to_string())
                        .size(f32::from(theme::CAPTION))
                        .color(theme::MUTED)
                        .wrapping(iced::widget::text::Wrapping::Glyph),
                    // Split into two sentences: a stall usually happens on
                    // the first page, where `advance` stops without
                    // producing any rows at all -- saying "the list below"
                    // on that screen would be a lie, pointing at something
                    // to look at when there's nothing below.
                    text(if has_rows {
                        "Its address index is unbuilt or rebuilding. This is not an empty \
                         history -- the list below is incomplete."
                    } else {
                        "Its address index is unbuilt or rebuilding. This is not an empty \
                         history -- nothing can be listed until the node can answer."
                    })
                    .size(f32::from(theme::SMALL))
                    .color(theme::MUTED),
                ]
                .spacing(f32::from(theme::SPACING)),
            )
            .style(theme::advisory_card)
            .padding(theme::PADDING)
            .width(Length::Fill),
        );
    }

    let tip = wallet.node_status.as_ref().and_then(|s| s.height);
    let finalized = wallet.node_status.as_ref().and_then(|s| s.finalized_height);
    let mut table = column![kit::table_head(&HISTORY_COLUMNS)];
    if history.rows_len() == 0
        && history.error.is_none()
        && !history.in_flight
        && history.merge.stalled_address().is_none()
    {
        table = table.push(
            container(text("No transactions yet.").size(12).color(theme::MUTED)).padding([8, 8]),
        );
    }
    for (position, entry) in history.merge.rows().iter().enumerate() {
        let shown = activity::display(entry);
        let (state, state_colour) = kit::status_cell(activity::row_status(entry, tip, finalized));
        table = table.push(kit::table_row(
            vec![
                kit::cell(
                    format_local_time(entry.timestamp).unwrap_or_else(|| "—".into()),
                    HISTORY_COLUMNS[0].1,
                    theme::MUTED,
                ),
                kit::cell(entry.height.to_string(), HISTORY_COLUMNS[1].1, theme::CYAN),
                kit::cell(
                    addr_label(entry, &wallet.addresses),
                    HISTORY_COLUMNS[2].1,
                    theme::LAVENDER,
                ),
                kit::cell(
                    shown.dir.label(),
                    HISTORY_COLUMNS[3].1,
                    kit::dir_colour(shown.dir),
                ),
                kit::cell(counterparty_cell(entry), HISTORY_COLUMNS[4].1, theme::TEXT),
                kit::cell(
                    kit::signed_amount(&shown),
                    HISTORY_COLUMNS[5].1,
                    kit::amount_colour(&shown),
                ),
                kit::cell(
                    shown
                        .fee_units
                        .map(format_coins)
                        .unwrap_or_else(|| "—".into()),
                    HISTORY_COLUMNS[6].1,
                    theme::FAINT,
                ),
                kit::cell(state, HISTORY_COLUMNS[7].1, state_colour),
            ],
            position % 2 == 1,
            false,
            None,
        ));
    }
    let more: Element<'_, Message> = if history.in_flight {
        text("LOADING...").size(12).color(theme::MUTED).into()
    } else if !history.merge.is_done()
        && history.error.is_none()
        && history.merge.stalled_address().is_none()
    {
        kit::action("LOAD MORE", kit::Act::Plain, Some(Message::HistoryMore))
    } else if history.merge.is_done() {
        text("ALL LOADED").size(12).color(theme::DIM).into()
    } else {
        text("").into()
    };
    table = table.push(kit::table_foot(
        text(format!("LOADED [{}]", history.rows_len()))
            .size(12)
            .color(theme::MUTED)
            .into(),
        more,
    ));
    content = content.push(kit::tag_panel("HISTORY", theme::CYAN, table));

    // Below the rows, not above them. The button that produces this failure
    // ("Load more") is at the bottom of the list, so with a screen's worth of
    // rows a card above them lands off-screen: the button vanishes and the
    // explanation and Retry are a scroll away, out of sight. The stale banner
    // and the stalled card stay above, because they qualify the whole list and
    // say so ("the list below").
    if let Some(message) = &history.error {
        content = content.push(
            container(
                column![
                    // `message` is `ApiError::to_string()`; the common
                    // transport failure is `ApiError::Transport(e.to_string())`,
                    // and reqwest appends " for url (...)" with the full
                    // request URL -- a 40-character address plus cursor
                    // params, one unbreakable ~110-character token. Default
                    // word wrapping leaves it on one line and clips the
                    // tail (see receive.rs); `WordOrGlyph` wraps prose
                    // normally and glyph-splits only the token that does
                    // not fit, so the URL still shows in full.
                    text(message.as_str())
                        .size(f32::from(theme::SMALL))
                        .color(theme::DANGER)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                    // The list is stalled here. The rows already shown
                    // below are real, just not the complete picture.
                    text("The list stops here until this succeeds.")
                        .size(f32::from(theme::CAPTION))
                        .color(theme::MUTED),
                    button(text("Retry"))
                        .on_press(Message::HistoryMore)
                        .padding(10)
                        .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
                ]
                .spacing(f32::from(theme::SPACING)),
            )
            .style(theme::verdict_card(theme::DANGER))
            .padding(theme::PADDING)
            .width(Length::Fill),
        );
    }

    kit::screen(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AddressEntry, AddressSource, Spendable};

    fn entry(index: u32, address: &str) -> AddressEntry {
        AddressEntry {
            source: AddressSource::Derived(index),
            address: address.into(),
            balance_units: None,
            spendable: Spendable::Pending,
            error: None,
            loading: false,
            recent: None,
        }
    }

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

    /// The one full-width table: realistic figures fit on one line even at
    /// the 760 px minimum.
    #[test]
    fn realistic_figures_fit_the_history_table_at_both_widths() {
        for window in [1000.0, 760.0] {
            kit::sizing::assert_fits(
                &HISTORY_COLUMNS,
                &[
                    "09-11 23:42",
                    "1015860",
                    "[0]>[3]",
                    "MINED",
                    "c0ffee11\u{2026}66778899",
                    "+1.23456789",
                    "0.00012345",
                    "CONFIRMED",
                ],
                kit::sizing::row_width(window, 1, 1.0),
            );
        }
    }

    #[test]
    fn the_addr_column_names_the_wallet_address_or_both_ends_of_a_move() {
        let addresses = vec![entry(0, "a"), entry(3, "b")];
        assert_eq!(addr_label(&row(RowKind::Mining, "b"), &addresses), "[3]");
        assert_eq!(
            addr_label(
                &row(
                    RowKind::Internal {
                        from: "a".into(),
                        to: "b".into()
                    },
                    "a"
                ),
                &addresses
            ),
            "[0]>[3]"
        );
        assert_eq!(addr_label(&row(RowKind::Mining, "gone"), &addresses), "?");
    }

    #[test]
    fn the_counterparty_column_never_prints_a_self_send_as_a_payment() {
        assert_eq!(
            counterparty_cell(&row(RowKind::Mining, "a")),
            "MINING_REWARDS"
        );
        assert_eq!(
            counterparty_cell(&row(
                RowKind::Internal {
                    from: "a".into(),
                    to: "a".into()
                },
                "a"
            )),
            "fee only"
        );
        // Short enough for the column at 760 px; ADDR already names both ends.
        assert_eq!(
            counterparty_cell(&row(
                RowKind::Internal {
                    from: "a".into(),
                    to: "b".into()
                },
                "a"
            )),
            "own addresses"
        );
        assert_eq!(
            counterparty_cell(&row(
                RowKind::Out {
                    to: "c0ffee11aabbccddeeff00112233445566778899".into()
                },
                "a"
            )),
            "c0ffee11…66778899"
        );
    }
}
