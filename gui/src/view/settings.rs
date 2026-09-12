//! F7: node settings and the wallet's own file -- split between Owned and
//! External, with a summary of what is actually in effect right now, and a
//! control to switch between Owned and External modes.
//!
//! Widget assembly only. Settings assembly reuses `setup::node_settings()`
//! and `setup::node_source_picker()` unchanged, so this screen and the setup
//! screen never drift into different presentations of the same controls.

use std::path::Path;

use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, text, text_input};
use iced::{Element, Length};

use crate::app::{App, MasterRevealState, Message, NodeSource};
use crate::theme;
use crate::view::{kit, setup};

/// Render the "currently in effect" summary: which source is selected, the URL
/// being polled, and (in Owned mode only) the data directory.
///
/// The source is named by `NodeSource::label` -- the picker's own button
/// text -- so the summary speaks the user's language, not the enum, and
/// cannot drift from the buttons beneath it.
///
/// `app.node_url` is the URL in effect — in Owned mode it was set from the
/// explorer port at node start, so it may disagree with the editable
/// `explorer_port_input` field below if the user has typed a new port but not
/// restarted the node yet. That gap is exactly what the summary is for, so
/// show the current state, not the pending one.
fn summary_rows(
    source: NodeSource,
    url: &str,
    data_dir: Option<String>,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![
        ("SOURCE", source.label().to_string()),
        ("NODE URL", url.to_string()),
    ];

    // In External mode, this wallet doesn't run the node, so its data dir
    // would name a local folder for a node that isn't local — a lie. Omit it
    // when source is External, matching view::node's own External branch.
    if source == NodeSource::Owned {
        rows.push((
            "DATA DIRECTORY",
            data_dir.unwrap_or_else(|| "—".to_string()),
        ));
    }

    rows
}

/// The WALLET panel's facts (spec G §3.1, extended by spec H §3.5).
/// PREVIOUS WALLET only after an import this session set one aside.
fn wallet_rows(
    path: &Path,
    derived: usize,
    imported: usize,
    previous: Option<&Path>,
) -> Vec<(&'static str, String)> {
    let addresses = if imported == 0 {
        derived.to_string()
    } else {
        format!("{derived} derived · {imported} imported")
    };
    let mut rows = vec![
        ("FILE", path.display().to_string()),
        ("ADDRESSES", addresses),
    ];
    if let Some(previous) = previous {
        rows.push(("PREVIOUS WALLET", previous.display().to_string()));
    }
    rows
}

/// Spec H §3.5: the master seed stops being a complete backup the moment a
/// key comes in from outside.
///
/// M7: driven by `shown.max(stored)`, not `shown` alone -- `shown` is the row
/// count (what `wallet_rows`' "N derived - N imported" line counts too), but
/// a stored seed that no longer parses is skipped when `addresses` is built
/// (see `wallet_panel`) and so never becomes a row. Without the `max`, the
/// panel would say the master seed covers everything while the file still
/// holds a key it does not.
pub(crate) fn backup_warning(shown: usize, stored: usize) -> Option<String> {
    match shown.max(stored) {
        0 => None,
        1 => Some(
            "1 imported address is not covered by your master seed. Export the wallet file to \
             back it up."
                .to_string(),
        ),
        many => Some(format!(
            "{many} imported addresses are not covered by your master seed. Export the wallet \
             file to back them up."
        )),
    }
}

/// Spec G §3.4's confirm sentence.
pub(crate) fn import_confirm_text(archive: &Path) -> String {
    format!(
        "The wallet now open will be closed. Its file is not deleted: when the imported \
         wallet is saved, the current file moves to {}. Until then you can cancel and come \
         back to this wallet.",
        archive.display()
    )
}

const MASTER_WARNING: &str =
    "This one seed can spend every address in this wallet. Anyone who sees it can take the funds.";

fn master_reveal_section(
    state: &MasterRevealState,
    imported: usize,
    stored: usize,
) -> Element<'_, Message> {
    match state {
        MasterRevealState::Idle => column![].into(),
        MasterRevealState::Asking {
            passphrase,
            error,
            busy,
            ..
        } => {
            let mut section = column![
                text(MASTER_WARNING)
                    .size(13)
                    .color(theme::ADVISORY)
                    .wrapping(Wrapping::WordOrGlyph),
                text_input("Passphrase", passphrase)
                    .secure(true)
                    .on_input(Message::MasterRevealPassphraseChanged)
                    .on_submit(Message::MasterRevealConfirm)
                    .padding(8)
                    .style(theme::text_input),
            ]
            .spacing(8);
            if let Some(error) = error {
                section = section.push(text(error.clone()).size(12).color(theme::DANGER));
            }
            section
                .push(
                    row![
                        kit::action(
                            if *busy { "CHECKING..." } else { "SHOW" },
                            kit::Act::Care,
                            (!busy).then_some(Message::MasterRevealConfirm),
                        ),
                        kit::action("CANCEL", kit::Act::Plain, Some(Message::MasterRevealHide)),
                    ]
                    .spacing(8),
                )
                .into()
        }
        // Borrowed straight out of the Zeroizing string, as receive.rs does
        // for an address seed -- no second, unwiped copy for the widget.
        MasterRevealState::Revealed { seed } => {
            let mut section = column![
                container(
                    text(seed.as_str())
                        .font(theme::TECH_FONT)
                        .size(14)
                        .wrapping(Wrapping::Glyph),
                )
                .padding(12)
                .width(Length::Fill)
                .style(theme::verdict_card(theme::ADVISORY)),
                text(MASTER_WARNING)
                    .size(12)
                    .color(theme::ADVISORY)
                    .wrapping(Wrapping::WordOrGlyph),
            ]
            .spacing(8);
            if let Some(warning) = backup_warning(imported, stored) {
                section = section.push(
                    text(warning)
                        .size(12)
                        .color(theme::ADVISORY)
                        .wrapping(Wrapping::WordOrGlyph),
                );
            }
            section
                .push(
                    text("Clear the clipboard when you are done: anything running on this machine can read it.")
                        .size(12)
                        .color(theme::MUTED)
                        .wrapping(Wrapping::WordOrGlyph),
                )
                .push(
                    row![
                        kit::action("COPY", kit::Act::Plain, Some(Message::CopyMasterSeed)),
                        kit::action(
                            "CLEAR CLIPBOARD",
                            kit::Act::Plain,
                            Some(Message::ClearClipboard)
                        ),
                        kit::action("HIDE", kit::Act::Plain, Some(Message::MasterRevealHide)),
                    ]
                    .spacing(8),
                )
                .into()
        }
    }
}

fn wallet_panel(app: &App) -> Element<'_, Message> {
    let Some(wallet) = &app.wallet else {
        return column![].into();
    };
    // Not `wallet.imported.len()`: a stored seed that no longer parses is
    // kept in `imported` (so a later save cannot lose it) but skipped when
    // `install_wallet` builds `addresses`, so the two counts can differ.
    // Counting rows directly describes what the user can actually see, and
    // cannot underflow the way subtracting a seed count from a row count did.
    let derived = wallet.addresses.iter().filter(|e| !e.is_imported()).count();
    let imported = wallet.addresses.iter().filter(|e| e.is_imported()).count();
    let mut body = column![].spacing(8);
    for (label, value) in wallet_rows(
        &wallet.wallet_path,
        derived,
        imported,
        app.last_archive.as_deref(),
    ) {
        body = body.push(kit::kv_row(label, value));
    }
    if let Some(warning) = backup_warning(imported, wallet.imported.len()) {
        body = body.push(
            text(warning)
                .size(12)
                .color(theme::ADVISORY)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    let blocker = app.import_blocker();
    body = body.push(
        row![
            kit::action(
                if wallet.exporting {
                    "EXPORTING..."
                } else {
                    "EXPORT WALLET FILE"
                },
                kit::Act::Plain,
                (!wallet.exporting).then_some(Message::ExportWalletFile),
            ),
            kit::action(
                "SHOW MASTER SEED",
                kit::Act::Care,
                matches!(wallet.master_reveal, MasterRevealState::Idle)
                    .then_some(Message::MasterRevealStart),
            ),
            kit::action(
                "IMPORT WALLET",
                kit::Act::Care,
                (blocker.is_none() && wallet.import_confirm.is_none())
                    .then_some(Message::ImportStart),
            ),
        ]
        .spacing(8)
        .wrap(),
    );
    if let Some(reason) = blocker {
        body = body.push(
            text(reason)
                .size(12)
                .color(theme::ADVISORY)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    match &wallet.export_result {
        Some(Ok(path)) => {
            body = body.push(
                text(format!("Saved to {}", path.display()))
                    .size(12)
                    .color(theme::MUTED)
                    .wrapping(Wrapping::WordOrGlyph),
            );
        }
        Some(Err(message)) => {
            body = body.push(
                text(message.clone())
                    .size(12)
                    .color(theme::DANGER)
                    .wrapping(Wrapping::WordOrGlyph),
            );
        }
        None => {}
    }
    body = body.push(master_reveal_section(
        &wallet.master_reveal,
        imported,
        wallet.imported.len(),
    ));
    if let Some(archive) = &wallet.import_confirm {
        body = body.push(kit::tag_panel(
            "IMPORT WALLET",
            theme::ADVISORY,
            column![
                text(import_confirm_text(archive))
                    .size(13)
                    .wrapping(Wrapping::WordOrGlyph),
                row![
                    kit::action("CONTINUE", kit::Act::Care, Some(Message::ImportContinue)),
                    kit::action(
                        "CANCEL",
                        kit::Act::Plain,
                        Some(Message::ImportCancelConfirm)
                    ),
                ]
                .spacing(8),
            ]
            .spacing(8),
        ));
    }
    kit::tag_panel("WALLET", theme::CYAN, body)
}

pub fn view(app: &App) -> Element<'_, Message> {
    let data_dir = app.data_dir().map(|p| p.display().to_string());
    let mut in_effect = column![].spacing(8);
    for (label, value) in summary_rows(app.node_source, &app.node_url, data_dir) {
        in_effect = in_effect.push(kit::kv_row(label, value));
    }
    // The summary describes the node actually running; the fields beside it
    // can already disagree (a typed port not yet applied) without either one
    // being wrong.
    in_effect = in_effect.push(
        text("This is what is running now. The fields beside it take effect on the next restart.")
            .size(12)
            .color(theme::MUTED),
    );

    // Half and half, not a third: `kv_row` gives its value three fifths of
    // the panel, and in a third of a 760 px window that was ~16 characters
    // -- the data path ran to many short lines and the node URL broke as
    // `http://127.0.0.1` / `:8095`. At half width the URL fits on a line and
    // the path takes two; the node-source panel's three port fields still
    // get ~100 px each. Below `kit::NARROW_BELOW` the two stack instead
    // (R1), where each gets the full window width.
    let left = column![
        kit::tag_panel("IN EFFECT", theme::ACCENT, in_effect),
        wallet_panel(app),
    ]
    .spacing(f32::from(theme::SPACING));
    kit::screen(kit::split(
        app.narrow(),
        left.into(),
        1,
        kit::tag_panel(
            "NODE SOURCE",
            theme::CYAN,
            column![
                setup::node_source_picker(app),
                setup::node_settings(app, true)
            ]
            .spacing(f32::from(theme::SPACING)),
        ),
        1,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wallet_panel_names_the_file_the_count_and_any_previous_wallet() {
        let rows = wallet_rows(Path::new("/h/.alphanumeric-gui/seed.enc"), 3, 0, None);
        assert_eq!(
            rows,
            vec![
                ("FILE", "/h/.alphanumeric-gui/seed.enc".to_string()),
                ("ADDRESSES", "3".to_string()),
            ]
        );
        let rows = wallet_rows(
            Path::new("/h/.alphanumeric-gui/seed.enc"),
            1,
            0,
            Some(Path::new("/h/.alphanumeric-gui/seed-20260912-031500.enc")),
        );
        assert_eq!(
            rows[2],
            (
                "PREVIOUS WALLET",
                "/h/.alphanumeric-gui/seed-20260912-031500.enc".to_string()
            )
        );
    }

    #[test]
    fn the_wallet_panel_counts_both_kinds_and_warns_only_when_imported() {
        let rows = wallet_rows(Path::new("/h/seed.enc"), 2, 0, None);
        assert_eq!(rows[1], ("ADDRESSES", "2".to_string()));
        let rows = wallet_rows(Path::new("/h/seed.enc"), 2, 1, None);
        assert_eq!(rows[1], ("ADDRESSES", "2 derived · 1 imported".to_string()));

        assert_eq!(backup_warning(0, 0), None);
        let warning = backup_warning(1, 1).expect("warned");
        assert!(
            warning.contains("not covered by your master seed"),
            "{warning}"
        );
        assert!(warning.contains("Export"), "{warning}");
        let many = backup_warning(2, 2).expect("warned");
        assert!(many.contains("2 imported addresses"), "{many}");
    }

    // M7: a stored seed that no longer parses is kept in `imported` (so a
    // later save cannot lose it) but is skipped when the row list is built,
    // so `shown` alone can undercount -- and at 0 shown rows, silence what
    // should be a warning that the master seed does not cover everything.
    #[test]
    fn the_backup_warning_still_fires_for_a_stored_seed_no_row_shows() {
        assert_eq!(
            backup_warning(0, 1),
            backup_warning(1, 1),
            "one unshowable stored key warns exactly like one shown row does"
        );
        assert!(backup_warning(0, 1).is_some());
    }

    #[test]
    fn the_confirm_text_says_the_file_is_kept_and_where() {
        let text = import_confirm_text(Path::new("/h/seed-20260912-031500.enc"));
        assert!(text.contains("not deleted"), "{text}");
        assert!(text.contains("/h/seed-20260912-031500.enc"), "{text}");
        assert!(text.contains("cancel"), "{text}");
    }

    /// In Owned mode, the summary includes SOURCE, URL, and DATA DIRECTORY.
    #[test]
    fn owned_summary_has_all_three_rows() {
        let rows = summary_rows(
            NodeSource::Owned,
            "http://127.0.0.1:8095",
            Some("/home/user/.alphanumeric".to_string()),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "SOURCE");
        assert_eq!(rows[1].0, "NODE URL");
        assert_eq!(rows[2].0, "DATA DIRECTORY");
    }

    /// In External mode, the summary excludes DATA DIRECTORY.
    #[test]
    fn external_summary_has_only_source_and_url() {
        let rows = summary_rows(
            NodeSource::External,
            "http://192.168.1.100:8095",
            Some("/home/user/.alphanumeric".to_string()),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "SOURCE");
        assert_eq!(rows[1].0, "NODE URL");
    }

    /// When data directory is None, Owned mode shows a dash, not a missing row.
    #[test]
    fn owned_with_no_data_dir_shows_dash() {
        let rows = summary_rows(NodeSource::Owned, "http://127.0.0.1:8095", None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].0, "DATA DIRECTORY");
        assert_eq!(rows[2].1, "—");
    }

    /// Owned mode source label uses user-facing wording.
    #[test]
    fn owned_source_shows_user_facing_label() {
        let rows = summary_rows(
            NodeSource::Owned,
            "http://127.0.0.1:8095",
            Some("/home/user/.alphanumeric".to_string()),
        );
        assert!(rows[0].1.contains("Run its own node"));
    }

    /// External mode source label uses user-facing wording.
    #[test]
    fn external_source_shows_user_facing_label() {
        let rows = summary_rows(NodeSource::External, "http://192.168.1.100:8095", None);
        assert!(rows[0].1.contains("Point at a node I already run"));
    }
}
