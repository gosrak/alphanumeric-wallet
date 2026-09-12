//! F1: the active address and its balance on top; the wallet's addresses
//! and its latest transactions side by side below. noid's main screen, with
//! the UTXO table's place taken by addresses and LIVE STATE's by recent
//! activity (spec §4.2).
//!
//! Widget layout only. Polling, discovery and the add-address flow live in
//! `app.rs`, where they can be tested without a window.

use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, text, text_input, Space};
use iced::{Alignment, Element, Length};

use alphanumeric_gui::activity::{self, Coverage, Recent};
use alphanumeric_gui::backend::NodeStatus;
use alphanumeric_gui::model;

use crate::app::{
    AddressEntry, AddressSource, App, ImportKeyState, Message, NodeSource, RemoveKey, Screen,
    Spendable, WalletState,
};
use crate::theme;
use crate::view::kit;

/// Rows RECENT ACTIVITY shows.
const RECENT_ROWS: usize = 8;

/// Portions are characters of the widest realistic value plus one for the
/// gap to the next column (`kit::table_row`'s cells abut): a 17-character
/// short address, a 14-character balance such as `38738.16499623`.
/// `realistic_figures_fit_the_wallet_tables_at_1000_px` checks the sums.
const ADDRESS_COLUMNS: [(&str, u16); 5] = [
    ("IDX", 4),
    ("ADDRESS", 18),
    ("BALANCE", 15),
    ("SPENDABLE", 15),
    ("STATUS", 9),
];

const ACTIVITY_COLUMNS: [(&str, u16); 4] =
    [("HEIGHT", 8), ("DIR", 6), ("AMOUNT", 14), ("STATUS", 10)];

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(wallet) = &app.wallet else {
        return kit::screen(text("No wallet loaded."));
    };

    // "No node reachable" is a first-class state (spec 4.1), but a BANNER
    // over a still-usable screen, not a takeover: every address here is
    // derived from the seed with no network involved.
    let mut content = column![].spacing(f32::from(theme::SPACING));
    if wallet.node_status.is_none() {
        content = content.push(no_node_banner(app));
    }
    content = content.push(kit::tag_panel(
        "ACTIVE ADDRESS",
        theme::ACCENT,
        active_block(app, wallet),
    ));

    let recent = app.recent_activity(RECENT_ROWS);
    content = content.push(kit::split(
        app.narrow(),
        kit::tag_panel("ADDRESSES", theme::CYAN, address_table(app, wallet)),
        3,
        kit::tag_panel(
            "RECENT ACTIVITY",
            theme::LAVENDER,
            activity_table(&recent, wallet.node_status.as_ref()),
        ),
        2,
    ));

    // Pushed only while there is a panel to show: `column![]` still takes a
    // `spacing` gap from its neighbours, which would otherwise leave a bare
    // gap on screen every frame the panel is `Idle`.
    if !matches!(wallet.import_key, ImportKeyState::Idle) {
        content = content.push(import_key_panel(&wallet.import_key));
    }
    if let Some(removing) = &wallet.removing_key {
        content = content.push(remove_key_panel(removing));
    }

    if let Some(error) = &wallet.save_error {
        content = content.push(
            text(error.clone())
                .size(f32::from(theme::SMALL))
                .color(theme::DANGER),
        );
    }
    if let Some(warning) = &wallet.node_status_error {
        content = content.push(
            text(format!("Node status is stale: {warning}"))
                .size(f32::from(theme::CAPTION))
                .color(theme::MUTED),
        );
    }
    kit::screen(content)
}

/// The active address in full, its figures, and the two main actions.
fn active_block<'a>(app: &'a App, wallet: &'a WalletState) -> Element<'a, Message> {
    let entry = wallet.addresses.get(wallet.active);
    let index = entry.map(|e| e.label()).unwrap_or_else(|| "—".into());
    let address = entry
        .map(|e| e.address.clone())
        .unwrap_or_else(|| "—".into());
    let bar = row![
        text(format!("[{index}]")).size(13).color(theme::LAVENDER),
        text(address)
            .size(14)
            .font(theme::TECH_FONT)
            .color(theme::VALUE)
            .wrapping(Wrapping::None),
        Space::new().width(Length::Fill),
        kit::action("COPY", kit::Act::Plain, entry.map(|_| Message::CopyAddress)),
        kit::action(
            "NEXT",
            kit::Act::Plain,
            next_address(wallet).map(Message::SetActiveAddress),
        ),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    // A figure after the first carries its separator with it, so when the
    // row wraps the separator goes along instead of dangling at a line end.
    let after_separator = |stat: Element<'static, Message>| -> Element<'static, Message> {
        row![kit::stat_separator(), stat]
            .spacing(16)
            .align_y(Alignment::End)
            .into()
    };
    // The figures wrap onto a second line rather than run under the
    // buttons: a 14-character balance at 26 px, the other three and the two
    // buttons need ~790 px, and the panel has ~710 at the 760 px minimum.
    // `kit::stat` never wraps its own text, so without this the row would
    // overflow the panel and draw money past its edge.
    let figures = row![
        kit::stat(
            "ALPHA BALANCE",
            entry
                .and_then(|e| e.balance_units)
                .map(model::format_coins)
                .unwrap_or_else(|| "—".into()),
            theme::VALUE,
            true,
        ),
        after_separator(kit::stat(
            "SPENDABLE",
            entry
                .map(|e| spendable_text(e.spendable))
                .unwrap_or_else(|| "—".into()),
            theme::TEXT,
            false,
        )),
        after_separator(kit::stat(
            "MATURING",
            entry
                .and_then(address_maturing)
                .map(model::format_coins)
                .unwrap_or_else(|| "—".into()),
            theme::ADVISORY,
            false,
        )),
        after_separator(kit::stat(
            "ADDRESSES",
            wallet.addresses.len().to_string(),
            theme::TEXT,
            false,
        )),
    ]
    .spacing(16)
    .align_y(Alignment::End)
    .width(Length::Fill)
    .wrap()
    .vertical_spacing(10);

    let mut buttons = row![
        kit::action(
            "RECEIVE",
            kit::Act::Look,
            Some(Message::Show(Screen::Receive))
        ),
        kit::action("SEND", kit::Act::Go, Some(Message::Show(Screen::Send))),
    ]
    .spacing(16)
    .align_y(Alignment::End);
    if let Some(entry) = wallet.addresses.get(wallet.active) {
        if let AddressSource::Imported(slot) = entry.source {
            buttons = buttons.push(kit::action(
                "REMOVE",
                kit::Act::Care,
                (app.key_change_blocker().is_none() && wallet.removing_key.is_none())
                    .then_some(Message::RemoveKeyStart(slot)),
            ));
        }
    }

    let hero = row![figures, buttons].spacing(16).align_y(Alignment::End);

    column![bar, hero].spacing(12).into()
}

/// The position after the active one, wrapping; `None` with a single address.
fn next_address(wallet: &WalletState) -> Option<usize> {
    (wallet.addresses.len() > 1).then(|| (wallet.active + 1) % wallet.addresses.len())
}

/// This address's balance not yet spendable -- the grid's wallet-wide
/// MATURING (`backend::maturing_units`) for one address.
fn address_maturing(entry: &AddressEntry) -> Option<i128> {
    let spendable = match entry.spendable {
        Spendable::Known(units) => Some(units),
        Spendable::Pending | Spendable::Unavailable => None,
    };
    alphanumeric_gui::backend::maturing_units(&[(entry.balance_units?, spendable)])
}

/// `Unavailable` (the node answered but could not compute the overlay) keeps
/// its own word: rendering it like `Pending` is how "this node cannot tell
/// you right now" gets mistaken for "still loading".
fn spendable_text(spendable: Spendable) -> String {
    match spendable {
        Spendable::Pending => "...".to_string(),
        Spendable::Unavailable => "unavailable".to_string(),
        Spendable::Known(units) => model::format_coins(units),
    }
}

fn address_status(entry: &AddressEntry, active: bool) -> (&'static str, iced::Color) {
    if entry.error.is_some() {
        ("ERROR", theme::DANGER)
    } else if entry.loading {
        // Only while `PollTick` really has another attempt coming
        // (`apply_address_result`'s `scheduled_retry`).
        ("UPDATING", theme::MUTED)
    } else if active {
        ("ACTIVE", theme::ACCENT)
    } else {
        ("", theme::MUTED)
    }
}

fn address_table(app: &App, wallet: &WalletState) -> Element<'static, Message> {
    let mut body = column![kit::table_head(&ADDRESS_COLUMNS)];
    for (position, entry) in wallet.addresses.iter().enumerate() {
        let active = position == wallet.active;
        let (status, colour) = address_status(entry, active);
        body = body.push(kit::table_row(
            vec![
                kit::cell(
                    entry.label(),
                    ADDRESS_COLUMNS[0].1,
                    if entry.is_imported() {
                        theme::LAVENDER
                    } else {
                        theme::CYAN
                    },
                ),
                kit::cell(
                    kit::short_address(&entry.address),
                    ADDRESS_COLUMNS[1].1,
                    if entry.is_imported() {
                        theme::LAVENDER
                    } else {
                        theme::TEXT
                    },
                ),
                kit::cell(
                    entry
                        .balance_units
                        .map(model::format_coins)
                        .unwrap_or_else(|| "...".into()),
                    ADDRESS_COLUMNS[2].1,
                    theme::VALUE,
                ),
                kit::cell(
                    spendable_text(entry.spendable),
                    ADDRESS_COLUMNS[3].1,
                    theme::TEXT,
                ),
                kit::cell(status, ADDRESS_COLUMNS[4].1, colour),
            ],
            position % 2 == 1,
            active,
            Some(Message::SetActiveAddress(position)),
        ));
    }
    // Per-row problems keep the words the old address card showed.
    for entry in &wallet.addresses {
        if let Some(error) = &entry.error {
            body = body.push(
                text(format!("[{}] {error}", entry.label()))
                    .size(12)
                    .color(theme::DANGER)
                    .wrapping(Wrapping::WordOrGlyph),
            );
        }
        if entry.spendable == Spendable::Unavailable {
            body = body.push(
                text(format!(
                    "[{}] This node cannot compute a spendable amount right now.",
                    entry.label()
                ))
                .size(12)
                .color(theme::MUTED),
            );
        }
    }
    let derived_count = wallet.addresses.iter().filter(|e| !e.is_imported()).count();
    let imported_count = wallet.addresses.iter().filter(|e| e.is_imported()).count();
    body = body.push(kit::table_foot(
        text(owned_footer(derived_count, imported_count))
            .size(12)
            .color(theme::MUTED)
            .into(),
        row![
            kit::action(
                if wallet.adding_address {
                    "ADDING..."
                } else {
                    "+ ADD"
                },
                kit::Act::Plain,
                (!wallet.adding_address).then_some(Message::AddAddress),
            ),
            kit::action(
                "IMPORT",
                kit::Act::Plain,
                (app.key_change_blocker().is_none()
                    && matches!(wallet.import_key, ImportKeyState::Idle))
                .then_some(Message::ImportKeyStart),
            ),
            kit::action(
                if wallet.fetch_in_flight {
                    "REFRESHING..."
                } else {
                    "REFRESH"
                },
                kit::Act::Plain,
                (!wallet.fetch_in_flight).then_some(Message::RefreshAll),
            ),
        ]
        .spacing(6)
        .into(),
    ));
    // I3: a blocked `+ ADD`/`IMPORT`/`REMOVE` must say why, the way
    // `wallet_panel` shows `import_blocker`'s reason under IMPORT WALLET --
    // otherwise a press while blocked looks like it did nothing at all.
    if let Some(reason) = app.key_change_blocker() {
        body = body.push(
            text(reason)
                .size(12)
                .color(theme::ADVISORY)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    body.into()
}

/// `OWNED [2]`, or `OWNED [2] · IMPORTED [1]` once a key was brought in.
pub(crate) fn owned_footer(derived: usize, imported: usize) -> String {
    if imported == 0 {
        format!("OWNED [{derived}]")
    } else {
        format!("OWNED [{derived}] · IMPORTED [{imported}]")
    }
}

/// Spec H §3.2. The seed is masked, CHECK only derives, and ADD appears
/// after the address has been shown.
fn import_key_panel(state: &ImportKeyState) -> Element<'_, Message> {
    match state {
        // M5: unreachable -- the only caller (`view` above) already guards on
        // `!matches!(wallet.import_key, ImportKeyState::Idle)` before calling
        // this at all. Kept rather than deleted so the match stays exhaustive
        // by inspection if `ImportKeyState` ever grows a variant, without
        // silently relying on the caller's guard to keep this arm dead.
        ImportKeyState::Idle => column![].into(),
        ImportKeyState::Asking { seed, error } => {
            let mut body = column![
                text(
                    "Paste the 64-character address seed the node's export-seed gave you. This \
                     key is not part of your master seed: it lives only in this wallet's file, \
                     so back the file up after adding it."
                )
                .size(13)
                .wrapping(Wrapping::WordOrGlyph),
                text_input("Address seed", seed)
                    .secure(true)
                    .on_input(Message::ImportKeySeedChanged)
                    .on_submit(Message::ImportKeyCheck)
                    .padding(8)
                    .style(theme::text_input),
            ]
            .spacing(8);
            if let Some(error) = error {
                body = body.push(
                    text(error.clone())
                        .size(12)
                        .color(theme::DANGER)
                        .wrapping(Wrapping::WordOrGlyph),
                );
            }
            kit::tag_panel(
                "IMPORT AN ADDRESS",
                theme::LAVENDER,
                body.push(
                    row![
                        kit::action("CHECK", kit::Act::Look, Some(Message::ImportKeyCheck)),
                        kit::action("CANCEL", kit::Act::Plain, Some(Message::ImportKeyCancel)),
                    ]
                    .spacing(8),
                ),
            )
        }
        ImportKeyState::Checked { address, .. } => kit::tag_panel(
            "IMPORT AN ADDRESS",
            theme::LAVENDER,
            column![
                kit::kv_row("THIS SEED GIVES", address.clone()),
                row![
                    kit::action(
                        "ADD THIS ADDRESS",
                        kit::Act::Go,
                        Some(Message::ImportKeyAdd)
                    ),
                    kit::action("CANCEL", kit::Act::Plain, Some(Message::ImportKeyCancel)),
                ]
                .spacing(8),
            ]
            .spacing(8),
        ),
        ImportKeyState::Busy => kit::tag_panel(
            "IMPORT AN ADDRESS",
            theme::LAVENDER,
            text("Saving...").size(13),
        ),
    }
}

/// Spec H §3.6: typing the address back is the gate, the way a payment's
/// confirmation is.
fn remove_key_panel(removing: &RemoveKey) -> Element<'_, Message> {
    let mut body = column![
        text(format!(
            "Removing {} deletes its key from this wallet's file. Unless the node or a copy of \
             that seed still has it, the address cannot be used again. Type the address to \
             confirm.",
            removing.address
        ))
        .size(13)
        .wrapping(Wrapping::WordOrGlyph),
        text_input("Type the address", &removing.typed)
            .on_input(Message::RemoveKeyTypedChanged)
            .on_submit(Message::RemoveKeyConfirm)
            .padding(8)
            .style(theme::text_input),
    ]
    .spacing(8);
    if let Some(error) = &removing.error {
        body = body.push(text(error.clone()).size(12).color(theme::DANGER));
    }
    kit::tag_panel(
        "REMOVE AN IMPORTED ADDRESS",
        theme::ADVISORY,
        body.push(
            row![
                kit::action("REMOVE", kit::Act::Care, Some(Message::RemoveKeyConfirm)),
                kit::action("CANCEL", kit::Act::Plain, Some(Message::RemoveKeyCancel)),
            ]
            .spacing(8),
        ),
    )
}

/// `LAST 8`, or `PARTIAL` while an address has not answered or the node's own
/// address index has fallen behind its chain (`Recent::coverage`) -- the
/// newest rows may be missing.
fn activity_foot(coverage: Coverage, shown: usize) -> String {
    match coverage {
        Coverage::Waiting | Coverage::Behind => "PARTIAL".to_string(),
        Coverage::Latest | Coverage::All => format!("LAST {shown}"),
    }
}

fn activity_table(recent: &Recent, status: Option<&NodeStatus>) -> Element<'static, Message> {
    let tip = status.and_then(|s| s.height);
    let finalized = status.and_then(|s| s.finalized_height);
    let coverage = recent.coverage(status);
    let mut body = column![kit::table_head(&ACTIVITY_COLUMNS)];
    if recent.rows.is_empty() {
        body = body.push(
            container(
                // `Latest` cannot be empty (it means `want` rows were read);
                // it is mapped for completeness, not reached.
                text(kit::filtered_empty(
                    "No transactions",
                    "No transactions yet.",
                    coverage,
                    RECENT_ROWS,
                ))
                .size(12)
                .color(theme::MUTED)
                .wrapping(Wrapping::WordOrGlyph),
            )
            .padding([8, 8]),
        );
    }
    for (position, entry) in recent.rows.iter().enumerate() {
        let shown = activity::display(entry);
        let (state, state_colour) = kit::status_cell(activity::row_status(entry, tip, finalized));
        body = body.push(kit::table_row(
            vec![
                kit::cell(entry.height.to_string(), ACTIVITY_COLUMNS[0].1, theme::CYAN),
                kit::cell(
                    shown.dir.label(),
                    ACTIVITY_COLUMNS[1].1,
                    kit::dir_colour(shown.dir),
                ),
                kit::cell(
                    kit::signed_amount(&shown),
                    ACTIVITY_COLUMNS[2].1,
                    kit::amount_colour(&shown),
                ),
                kit::cell(state, ACTIVITY_COLUMNS[3].1, state_colour),
            ],
            position % 2 == 1,
            false,
            None,
        ));
    }
    body = body.push(kit::table_foot(
        text(activity_foot(coverage, recent.rows.len()))
            .size(12)
            .color(theme::MUTED)
            .into(),
        kit::action(
            "F4 HISTORY",
            kit::Act::Plain,
            Some(Message::Show(Screen::History)),
        ),
    ));
    body.into()
}

/// The explorer API is opt-in behind an environment variable; this is where
/// nearly everyone gets stuck on first run if nothing explains it (spec 4.1).
///
/// A banner, not the screen's early return it used to be: everything below it
/// (addresses, Receive) works without a node, and there is no navigation back
/// to the setup screen to fix a wrong address from there, so the field to
/// correct it has to live here too.
///
/// Branches on `node_source` (Moderate 3): the External guidance below --
/// start a node yourself with this environment variable -- is actively wrong
/// for an Owned wallet. That node is the wallet's own job; a user who follows
/// terminal instructions for it starts a second, unrelated node in whatever
/// directory they happen to be in, while the one the wallet actually reads
/// stays dead. Owned gets its own message and, via `node_settings`, a button
/// that restarts the node the wallet is already responsible for.
fn no_node_banner(app: &App) -> Element<'_, Message> {
    match app.node_source {
        NodeSource::External => {
            let command = crate::app::node_launch_command(&app.node_url);
            column![
                container(crate::view::setup::node_settings(app, true))
                    .style(theme::advisory_card)
                    .padding(theme::PADDING)
                    .width(Length::Fill),
                crate::view::no_node_guidance(
                    "The explorer API is off by default. Start the node with this environment \
                     variable set:",
                    &command,
                    Message::RefreshAll,
                ),
            ]
            .spacing(f32::from(theme::SPACING))
            .into()
        }
        // `node_settings` already carries the "APPLY AND RESTART NODE"
        // button for `Owned` (Major 1, part 1.3) -- it sends `RetryNode`,
        // which drops the old supervisor, starts a new one, and moves to the
        // startup screen so the user watches it come back. No second restart
        // button is added here; two stacked ones would just be sloppy.
        NodeSource::Owned => column![
            container(
                column![
                    text("Your wallet's node is not answering").size(f32::from(theme::HEADING)),
                    text(
                        "It may still be starting, or it may have stopped. The settings below \
                         can restart it."
                    )
                    .size(f32::from(theme::BODY)),
                ]
                .spacing(f32::from(theme::SPACING))
            )
            .style(theme::advisory_card)
            .padding(theme::PADDING)
            .width(Length::Fill),
            container(crate::view::setup::node_settings(app, true))
                .style(theme::advisory_card)
                .padding(theme::PADDING)
                .width(Length::Fill),
        ]
        .spacing(f32::from(theme::SPACING))
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AddressSource;

    fn entry(balance: Option<i128>, spendable: Spendable) -> AddressEntry {
        AddressEntry {
            source: AddressSource::Derived(0),
            address: "a".into(),
            balance_units: balance,
            spendable,
            error: None,
            loading: false,
            recent: None,
        }
    }

    /// Same definition as the grid's wallet-wide MATURING, for one address:
    /// unknown spendable means unknown maturing, never "all of it".
    #[test]
    fn one_address_maturing_is_balance_minus_spendable_or_nothing() {
        assert_eq!(
            address_maturing(&entry(Some(100), Spendable::Known(60))),
            Some(40)
        );
        assert_eq!(
            address_maturing(&entry(Some(100), Spendable::Pending)),
            None
        );
        assert_eq!(
            address_maturing(&entry(Some(100), Spendable::Unavailable)),
            None
        );
        assert_eq!(address_maturing(&entry(None, Spendable::Known(60))), None);
    }

    /// "The node cannot tell you" must not read as "still loading".
    #[test]
    fn spendable_words_keep_pending_and_unavailable_apart() {
        assert_eq!(spendable_text(Spendable::Pending), "...");
        assert_eq!(spendable_text(Spendable::Unavailable), "unavailable");
        assert_eq!(spendable_text(Spendable::Known(150_000_000)), "1.5");
    }

    /// A lagging index means the newest rows may be missing even though the
    /// merge had every page it asked for.
    #[test]
    fn recent_activity_is_partial_while_waiting_or_behind() {
        assert_eq!(activity_foot(Coverage::Waiting, 0), "PARTIAL");
        assert_eq!(activity_foot(Coverage::Behind, 8), "PARTIAL");
        assert_eq!(activity_foot(Coverage::Latest, 8), "LAST 8");
        assert_eq!(activity_foot(Coverage::All, 3), "LAST 3");
    }

    /// Money is never clipped or drawn over the next column
    /// (`kit::table_row` wraps). At the 1000 px default, realistic figures
    /// sit on one line.
    #[test]
    fn realistic_figures_fit_the_wallet_tables_at_1000_px() {
        kit::sizing::assert_fits(
            &ADDRESS_COLUMNS,
            &[
                "12",
                "c0ffee11\u{2026}66778899",
                "38738.16499623",
                "38738.16499623",
                "UPDATING",
            ],
            kit::sizing::row_width(1000.0, 2, 3.0 / 5.0),
        );
        kit::sizing::assert_fits(
            &ACTIVITY_COLUMNS,
            &["1015860", "MINED", "+1.23456789", "CONFIRMED"],
            kit::sizing::row_width(1000.0, 2, 2.0 / 5.0),
        );
    }

    #[test]
    fn the_wallet_table_headers_fit_at_760_px() {
        kit::sizing::assert_heads_fit(
            &ADDRESS_COLUMNS,
            kit::sizing::row_width(760.0, 2, 3.0 / 5.0),
        );
        kit::sizing::assert_heads_fit(
            &ACTIVITY_COLUMNS,
            kit::sizing::row_width(760.0, 2, 2.0 / 5.0),
        );
    }

    #[test]
    fn the_footer_counts_imported_rows_only_when_there_are_some() {
        assert_eq!(owned_footer(2, 0), "OWNED [2]");
        assert_eq!(owned_footer(2, 1), "OWNED [2] · IMPORTED [1]");
    }

    #[test]
    fn an_error_outranks_loading_and_active() {
        let mut e = entry(None, Spendable::Pending);
        e.loading = true;
        e.error = Some("boom".into());
        assert_eq!(address_status(&e, true).0, "ERROR");
        e.error = None;
        assert_eq!(address_status(&e, true).0, "UPDATING");
        e.loading = false;
        assert_eq!(address_status(&e, true).0, "ACTIVE");
        assert_eq!(address_status(&e, false).0, "");
    }
}
