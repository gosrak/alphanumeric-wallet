//! 모든 주소를 합친 거래 이력.
//!
//! 위젯 배치만 한다. 순서·접기·커서는 `alphanumeric_gui::history` 가 창 없이
//! 결정하고, 페치는 `app.rs` 가 한다.

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

    // 노드의 인덱스 쓰기는 fail-open 이라 tip 뒤에 남을 수 있다. 그 상태의
    // 목록을 완결된 것으로 보여주면 안 된다 -- 없는 거래가 아니라 아직 안 보이는
    // 거래다.
    //
    // 배너 문구가 비교의 *기준*을 함께 말한다. `index_height` 는 페이지마다
    // 새로 오지만 체인 높이는 이 화면에 들어올 때 한 번 읽는다
    // (`Message::HistoryStatusFetched`) -- `ConsoleTick` 이 그 뒤로도 같은
    // 값(`wallet.node_status`)을 10초마다 갱신하긴 하지만, 화면을 막 연
    // 사람에게 그 10초를 기다리라고 할 이유는 없다. 그래도 화면을 열어 둔
    // 채 "더 보기"를 오래 누르면 그 기준이 다음 갱신 전까지 늙을 수 있다 --
    // 기준을 적어 두면 낡은 기준이 눈에 보이고, 안 적으면 낡은 기준으로
    // 내린 "완결" 판정이 조용히 통과한다.
    //
    // "the last time the node was asked" 이지 "이 화면을 열 때" 가 아니다.
    // 진입 시 상태 읽기가 실패하면 `apply_status` 는 직전 값을 그대로 두므로
    // (재시도 가능한 오류는 조용히, 아닌 오류도 `node_status` 는 건드리지
    // 않는다) 화면에 남는 높이가 그 시점의 것이 아닐 수 있다. 기준을
    // 정직하게 만들려고 넣은 문장이 지킬 수 없는 약속을 하면 같은 결함이다.
    if let Some(indexed) = history.merge.lowest_index_height() {
        // 노드에 물어본 적이 없는 것과, 물어봤지만 아직 높이가 없는 것을
        // 여기서는 구분하지 않는다 -- 어느 쪽이든 비교가 성립하지 않는다는
        // 점이 똑같다.
        let stale = match wallet.node_status.as_ref().and_then(|status| status.height) {
            Some(chain_height) if indexed < chain_height => Some(format!(
                "The node's address index had reached block {indexed}, and the chain was at {chain_height} \
                 the last time the node was asked. Anything newer is not in this list yet.",
            )),
            Some(_) => None,
            // 체인 높이를 못 읽었다 -- 노드가 답을 안 했거나, 답했지만 아직
            // 높이가 없거나. 어느 쪽이든 비교가 성립하지 않는다.
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

    // 인덱스가 이 주소에 대해 대답 자체를 못 하는 상태. 빈 이력과 전혀 다른
    // 뜻이므로 빈 목록으로 그리지 않는다.
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
                    // 두 문장으로 갈린다. 정지는 보통 첫 페이지에서 나고,
                    // 그러면 `advance` 는 아무 행도 내지 못한 채 멈춘다 --
                    // 그 화면에서 "the list below" 라고 말하면 아래에 아무
                    // 것도 없는데 무언가를 보라고 시키는 거짓말이 된다.
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
                    // 목록은 여기서 멈춰 있다. 아래에 이미 나온 행은 맞지만
                    // 완결이 아니다.
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
