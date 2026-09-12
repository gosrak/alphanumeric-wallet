//! Setup screen: the first thing anyone sees before a wallet exists.
//!
//! Widget layout only. The logic it drives -- the backup gate, seed decoding,
//! the async create/restore/unlock flow -- lives in `app.rs`, where it can be
//! tested without a window.

use std::path::{Path, PathBuf};

use iced::widget::text::Wrapping;
use iced::widget::{
    button, canvas, column, container, row, scrollable, stack, text, text_input, Space,
};
use iced::{Alignment, Element, Length};

use alphanumeric_gui::node;

use crate::app::{App, Message, NodeSource, PendingWallet, Screen, SetupStage};
use crate::theme;
use crate::view::kit;

pub fn view(app: &App) -> Element<'_, Message> {
    let mut content = column![text("alphanumeric wallet")
        .size(f32::from(theme::TITLE))
        .font(theme::BRAND_FONT)]
    .spacing(f32::from(theme::SPACING));

    // This screen is now reachable with a wallet already loaded --
    // Wallet -> RetryNode -> Startup -> "Node settings" lands here (Major 1)
    // -- and `self.setup` is whatever stale stage was last on screen before
    // the wallet was installed (typically `Unlock`, mid-"Unlocking...").
    // Nothing else on this screen can get back to the wallet in that case,
    // so it needs its own way back rather than stranding the user on a
    // stage that no longer describes anything they need to do.
    if app.wallet.is_some() {
        content = content.push(
            button(text("Back to wallet"))
                .on_press(Message::Show(Screen::Wallet))
                .padding(10)
                .style(|_, status| theme::button(theme::ButtonKind::Ghost, status)),
        );
    }

    // Import mode (spec G §3.5): the wallet that was open is closed but its
    // file is untouched until the new one is saved.
    if app.importing.is_some() {
        content = content.push(kit::action(
            "CANCEL IMPORT",
            kit::Act::Plain,
            (!app.setup_writing()).then_some(Message::ImportAbort),
        ));
    }

    // `Choose` is excluded here: it is the one stage where the Owned/External
    // question has not been answered yet, so it owns its own ordering below
    // -- `node_source_picker` first, `node_settings` after it -- instead of
    // showing settings for a choice the user has not made.
    match &app.setup {
        // Unlock's settings are pushed after the unlock panel, behind a
        // toggle -- below.
        SetupStage::Unlock { .. } => {}
        SetupStage::Restore { .. } | SetupStage::SetPassphrase { .. } => {
            content = content.push(node_settings(app, true));
        }
        _ => {}
    }

    let stage: Element<'_, Message> = match &app.setup {
        SetupStage::Choose if app.importing.is_some() => import_choose_view(),
        SetupStage::Choose => choose_view(app),
        SetupStage::Unlock {
            passphrase,
            error,
            busy,
        } => unlock_view(passphrase, error.as_deref(), *busy),
        SetupStage::ImportFile {
            path,
            passphrase,
            busy,
            error,
            preview,
        } => import_file_view(
            path.as_deref(),
            passphrase,
            *busy,
            error.as_deref(),
            preview.as_ref(),
        ),
        SetupStage::ConfirmBackup {
            shown,
            quiz,
            saved_to,
            save_error,
            ..
        } => confirm_backup_view(
            shown,
            quiz.as_ref(),
            saved_to.as_deref(),
            save_error.as_deref(),
        ),
        SetupStage::Restore {
            seed_input,
            seed_error,
            photo,
            photo_busy,
            photo_error,
        } => restore_view(
            seed_input,
            seed_error.as_deref(),
            photo.as_deref(),
            *photo_busy,
            photo_error.as_deref(),
        ),
        SetupStage::SetPassphrase {
            pending,
            passphrase,
            confirm,
            busy,
            error,
            node_unreachable,
        } => match node_unreachable {
            // Restoring needs the node to know how many addresses to bring
            // back; this is the same first-class guidance the wallet screen
            // shows later, not an error string, because it is the identical
            // state (spec 4.1) reached one step earlier.
            //
            // Branches on `node_source`, the same way `wallet.rs`'s
            // `no_node_banner` does (commit b5a8cf1): the External guidance
            // below -- a terminal command to start a node -- is wrong for a
            // restore against the wallet's own node. This is now the
            // ordinary first-restore experience (`StartRestore` starts the
            // owned node, and the scan above routinely lands here while its
            // snapshot is still downloading), so following that command
            // starts a second, unrelated node instead of waiting for the one
            // already coming up -- whose settings this stage already renders
            // just above (`node_settings`, pushed by the match at the top of
            // this function).
            Some(message) => match app.node_source {
                NodeSource::External => crate::view::no_node_guidance(
                    message,
                    &crate::app::node_launch_command(&app.node_url),
                    Message::ConfirmNewPassphrase,
                ),
                NodeSource::Owned => container(
                    container(
                        column![
                            text("No node reachable").size(f32::from(theme::HEADING)),
                            text(message.to_string()).size(f32::from(theme::BODY)),
                            // Not "the wallet will keep trying": nothing on
                            // this stage polls in the background (see
                            // `App::subscription`, gated to Startup/Wallet/
                            // Receive/Send) -- Retry below is the only thing
                            // that asks the node again, so the copy has to
                            // say that rather than imply this will resolve
                            // on its own.
                            text(
                                "This is the wallet's own node. It is still coming up -- there \
                                 is no need to start one yourself. Wait a moment, then press \
                                 Retry."
                            )
                            .size(f32::from(theme::BODY)),
                            button(text("Retry"))
                                .on_press(Message::ConfirmNewPassphrase)
                                .padding(12)
                                .style(|_, status| theme::button(
                                    theme::ButtonKind::Primary,
                                    status
                                )),
                        ]
                        .spacing(f32::from(theme::SPACING)),
                    )
                    .style(theme::advisory_card)
                    .padding(theme::PADDING)
                    .width(Length::Fill)
                    .max_width(560.0),
                )
                .center_x(Length::Fill)
                .into(),
            },
            None => set_passphrase_view(pending, passphrase, confirm, *busy, error.as_deref()),
        },
    };
    content = content.push(stage);
    // Unlock is what nearly every visit needs; the node settings are what a
    // broken node needs. They sit behind a toggle, and the startup screen's
    // escape (`Message::OpenSetupSettings`) opens it (plan ruling 6). The
    // source picker comes with them: switching to "a node I already run" is
    // how a returning user with a broken owned node keeps going (Major 1).
    if matches!(app.setup, SetupStage::Unlock { .. }) {
        content = content.push(kit::action(
            if app.setup_settings_open {
                "HIDE NODE SETTINGS"
            } else {
                "NODE SETTINGS"
            },
            kit::Act::Plain,
            Some(Message::ToggleSetupSettings),
        ));
        if app.setup_settings_open {
            // In a panel like the unlock form above it, not bare on the
            // backdrop.
            content = content.push(kit::tag_panel(
                "NODE SETTINGS",
                theme::CYAN,
                column![node_source_picker(app), node_settings(app, true)]
                    .spacing(f32::from(theme::SPACING)),
            ));
        }
    }

    // The backdrop is drawn behind every stage rather than inside one, so a
    // stage change does not flash a bare background between them. It carries
    // no clock and no subscription (see widgets/interface_backdrop.rs), so it
    // costs one redraw when something else already caused a redraw.
    container(stack![
        canvas(crate::widgets::interface_backdrop::InterfaceBackdrop)
            .width(Length::Fill)
            .height(Length::Fill),
        // `scrollable` defaults to `Shrink`, but a bare `Shrink` still
        // encloses to `Fill` around any `Fill`-width child -- and the node
        // URL field's text_input is `Fill` by default. Left alone, that Fill
        // gives the outer `center_x` no slack to act on: a scrollable
        // positions its content at a fixed offset rather than centring it
        // (unlike `container`), so a Fill-width scrollable leaves its
        // 560-wide column pinned to its own left edge. The explicit
        // `Length::Shrink` below is a plain setter, not another enclose: it
        // overrides that back to the content's real width (capped by
        // `max_width` above) so `center_x` has something narrower than the
        // window to centre.
        container(
            scrollable(content.padding(theme::PADDING).max_width(560.0))
                .width(Length::Shrink)
                .style(theme::scrollable)
        )
        .center_x(Length::Fill),
    ])
    .style(theme::root)
    .padding(theme::PADDING)
    .into()
}

fn node_url_field(app: &App) -> Element<'_, Message> {
    column![
        kit::field_label("NODE ADDRESS"),
        text_input("http://127.0.0.1:8095", &app.node_url)
            .on_input(Message::NodeUrlChanged)
            .padding(8)
            .style(theme::text_input),
    ]
    .spacing(4.0)
    .into()
}

/// One port field: label, the input kept exactly as typed (spec 4, the brief
/// pins this in `an_unusable_port_falls_back_to_the_default`), and a note
/// when what is typed will not parse -- the fallback happens silently in
/// `resolved_ports`, so this is the only place the user is told about it.
/// The placeholder is the default itself, so the greyed-out number in an
/// empty field is always the port that empty field will actually use.
fn port_field<'a>(
    label: &'static str,
    value: &'a str,
    default: u16,
    on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    let mut field = column![
        kit::field_label(label),
        text_input(&default.to_string(), value)
            .on_input(on_input)
            .padding(10)
            .style(theme::text_input),
    ]
    .spacing(f32::from(theme::SPACING));

    let trimmed = value.trim();
    if !trimmed.is_empty() && trimmed.parse::<u16>().is_err() {
        field = field.push(
            text(format!("Not a port -- {default} will be used."))
                .size(f32::from(theme::SMALL))
                .color(theme::WARNING),
        );
    }

    field.into()
}

/// Node settings. `Owned` and `External` split here -- setup and wallet share
/// this one function, so the split lives in exactly one place.
///
/// `show_restart` gates the "Changes take effect..." line and the "APPLY AND
/// RESTART NODE" button. On `SetupStage::Choose` no node has ever
/// started and nothing has been chosen yet, so there is nothing to restart
/// and nothing to apply -- pressing the button there would start the node
/// and jump to Startup before the Owned/External question is even answered,
/// which is exactly what `App::new` (spec 3) and `ChooseNodeSource(Owned)`
/// both deliberately avoid doing. Every other caller describes a node that
/// exists or should, where restarting it is the right action.
pub(crate) fn node_settings(app: &App, show_restart: bool) -> Element<'_, Message> {
    match app.node_source {
        NodeSource::External => node_url_field(app),
        NodeSource::Owned => {
            let mut layout = column![
                kit::field_label("NODE BINARY"),
                // The path can be long. `text_input` scrolls its tail out of
                // sight while looking fine (commit 2c28d1e), so the current
                // value is shown wrapped with Glyph and editing is left to the
                // input field alone.
                text(app.node_binary_display())
                    .font(theme::TECH_FONT)
                    .size(f32::from(theme::BODY))
                    .wrapping(iced::widget::text::Wrapping::Glyph),
                text_input(
                    "leave empty to use the wallet's own folder",
                    &app.node_binary_input
                )
                .on_input(Message::NodeBinaryChanged)
                .padding(10)
                .style(theme::text_input),
                row![
                    port_field(
                        "P2P PORT",
                        &app.p2p_port_input,
                        node::DEFAULT_P2P_PORT,
                        Message::P2pPortChanged,
                    ),
                    port_field(
                        "EXPLORER PORT",
                        &app.explorer_port_input,
                        node::DEFAULT_EXPLORER_PORT,
                        Message::ExplorerPortChanged,
                    ),
                    port_field(
                        "STATS PORT",
                        &app.stats_port_input,
                        node::DEFAULT_STATS_PORT,
                        Message::StatsPortChanged,
                    ),
                ]
                .spacing(f32::from(theme::SPACING)),
            ]
            .spacing(f32::from(theme::SPACING));

            if show_restart {
                layout = layout.push(
                    text("Changes take effect the next time the node starts.")
                        .size(f32::from(theme::SMALL))
                        .color(theme::MUTED),
                );
                // Without this, editing a port here had no way to ever take
                // effect (Major 1): nothing else on this screen restarts the
                // node. Reuses `RetryNode` -- it already drops the old
                // supervisor and starts a new one from whatever is currently
                // typed above, which is exactly "apply these settings".
                layout = layout.push(kit::action(
                    "APPLY AND RESTART NODE",
                    kit::Act::Care,
                    Some(Message::RetryNode),
                ));
            }

            layout.into()
        }
    }
}

/// The question of where the node comes from. On `SetupStage::Choose`, the
/// screen before a wallet exists (Task 7's F-E ruling), it is a one-time
/// question: no node is running yet, so nothing has been downloaded by the
/// time it is answered. It also renders on `SetupStage::Unlock` (Major 1):
/// there the node may already be running (or stuck, or missing its binary),
/// and picking `External` there is how a returning user with a broken owned
/// node keeps using the wallet without waiting on it. It also renders on
/// `Screen::Settings` (Task 8, following the coordinator's ruling): the most
/// consequential node setting is the choice of source itself, so it belongs on
/// the settings tab even after a wallet exists.
pub(crate) fn node_source_picker(app: &App) -> Element<'_, Message> {
    column![
        text("Where should this wallet get its blockchain data?").size(f32::from(theme::BODY)),
        row![
            kit::choice_card(
                NodeSource::Owned.label(),
                "The wallet runs its own node. The first start downloads about 173 MB and \
                 unpacks it to about 1 GB on disk.",
                app.node_source == NodeSource::Owned,
                Message::ChooseNodeSource(NodeSource::Owned),
            ),
            kit::choice_card(
                NodeSource::External.label(),
                "Point it at the explorer address of a node you already run.",
                app.node_source == NodeSource::External,
                Message::ChooseNodeSource(NodeSource::External),
            ),
        ]
        .spacing(f32::from(theme::SPACING)),
    ]
    .spacing(f32::from(theme::SPACING))
    .into()
}

fn choose_view(app: &App) -> Element<'_, Message> {
    let mut layout = column![
        text("No wallet was found on this machine yet."),
        node_source_picker(app),
        // After the question, not before it (ruling on Task 8): `Owned`'s
        // settings only mean something once the user has actually said
        // `Owned`, which on this stage happens right above.
        node_settings(app, false),
        row![
            kit::choice_card(
                "CREATE NEW",
                "A new master seed (a9m1..., 76 characters) that you save before anything else.",
                false,
                Message::StartCreate,
            ),
            kit::choice_card(
                "RESTORE",
                "From a master seed you type in or a photo of it.",
                false,
                Message::StartRestore,
            ),
            kit::choice_card(
                "IMPORT FILE",
                "A wallet file exported from another machine. It opens with its own passphrase.",
                false,
                Message::StartImportFile,
            ),
        ]
        .spacing(f32::from(theme::SPACING)),
    ]
    .spacing(f32::from(theme::SPACING));

    if let Some(hint) = archive_hint(&app.archives_found) {
        layout = layout.push(
            text(hint)
                .color(theme::ADVISORY)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }

    kit::tag_panel("NO WALLET YET", theme::ACCENT, layout)
}

/// Spec G §3.5 in import mode. Two cards, not three: seed and photo are one
/// existing stage (`SetupStage::Restore`), and a card each would open it twice.
fn import_choose_view<'a>() -> Element<'a, Message> {
    kit::tag_panel(
        "IMPORT A WALLET",
        theme::ADVISORY,
        column![
            text(
                "Choose what to import. The wallet you had stays on disk, untouched, until the \
                 imported one is saved -- CANCEL IMPORT goes back to it."
            )
            .wrapping(Wrapping::WordOrGlyph),
            row![
                kit::choice_card(
                    "WALLET FILE",
                    "A wallet file exported from an alphanumeric wallet. It opens with its own passphrase.",
                    false,
                    Message::StartImportFile,
                ),
                kit::choice_card(
                    "MASTER SEED OR PHOTO",
                    "The a9m1... master seed, or the photo the wallet was made from.",
                    false,
                    Message::StartRestore,
                ),
            ]
            .spacing(f32::from(theme::SPACING)),
        ]
        .spacing(f32::from(theme::SPACING)),
    )
}

/// Spec G §3.5, the WALLET FILE card: choose, open with that file's own
/// passphrase, check what it holds, use it.
fn import_file_view<'a>(
    path: Option<&'a Path>,
    passphrase: &'a str,
    busy: bool,
    error: Option<&'a str>,
    preview: Option<&'a crate::app::OpenedFile>,
) -> Element<'a, Message> {
    let chosen = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "No file chosen.".to_string());
    // Controller ruling, Task 6 fix round 1: `on_input` is left off entirely
    // while busy, so the field cannot be edited mid-OPEN or mid-USE -- not
    // merely ignored, disabled. `on_submit` stays: `ImportOpen` already
    // refuses to run a second time while busy.
    let mut passphrase_field = text_input("Passphrase of that file", passphrase)
        .secure(true)
        .on_submit(Message::ImportOpen)
        .padding(8)
        .style(theme::text_input);
    if !busy {
        passphrase_field = passphrase_field.on_input(Message::ImportPassphraseChanged);
    }
    let mut layout = column![
        text(
            "A wallet file exported from an alphanumeric wallet. It opens with its own passphrase."
        )
        .wrapping(Wrapping::WordOrGlyph),
        row![
            text(chosen)
                .font(theme::TECH_FONT)
                .size(f32::from(theme::SMALL))
                .wrapping(Wrapping::Glyph)
                .width(Length::Fill),
            kit::action(
                "CHOOSE FILE",
                kit::Act::Plain,
                (!busy).then_some(Message::ImportPickFile)
            ),
        ]
        .spacing(f32::from(theme::SPACING))
        .align_y(Alignment::Center),
        passphrase_field,
    ]
    .spacing(f32::from(theme::SPACING));
    if let Some(error) = error {
        layout = layout.push(
            text(error.to_string())
                .color(theme::DANGER)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    let back = kit::action(
        "BACK",
        kit::Act::Plain,
        (!busy).then_some(Message::BackToChoose),
    );
    layout = match preview {
        None => layout.push(
            row![
                kit::action(
                    if busy { "OPENING..." } else { "OPEN" },
                    kit::Act::Look,
                    (path.is_some() && !busy).then_some(Message::ImportOpen),
                ),
                back,
            ]
            .spacing(f32::from(theme::SPACING)),
        ),
        Some(opened) => layout
            .push(kit::kv_row("ADDRESSES", opened.file.next_index.to_string()))
            .push(kit::kv_row(
                "FIRST ADDRESS",
                opened.file.first_address.clone(),
            ))
            .push(
                row![
                    kit::action(
                        if busy { "SAVING..." } else { "USE THIS WALLET" },
                        kit::Act::Go,
                        (!busy).then_some(Message::ImportUse),
                    ),
                    back,
                ]
                .spacing(f32::from(theme::SPACING)),
            ),
    };
    kit::tag_panel("IMPORT A WALLET FILE", theme::CYAN, layout)
}

/// Spec G §3.6: wallets an import set aside, found on a first run -- the
/// safety net for a crash between setting the old file aside and writing the
/// new one. Named when there is one; counted when there are several, because
/// a same-second `-2` suffix sorts before the name it follows.
pub(crate) fn archive_hint(archives: &[PathBuf]) -> Option<String> {
    match archives {
        [] => None,
        [one] => Some(format!(
            "An archived wallet was found: {}. Import it with IMPORT FILE to use it again.",
            one.display()
        )),
        several => Some(format!(
            "{} archived wallets were found in {}. Import one with IMPORT FILE to use it again.",
            several.len(),
            several[0]
                .parent()
                .map(|dir| dir.display().to_string())
                .unwrap_or_default()
        )),
    }
}

fn unlock_view<'a>(
    passphrase: &'a str,
    error: Option<&'a str>,
    busy: bool,
) -> Element<'a, Message> {
    let mut layout = column![
        text("Enter the passphrase to unlock your wallet."),
        text_input("Passphrase", passphrase)
            .secure(true)
            .on_input(Message::UnlockPassphraseChanged)
            .on_submit(Message::Unlock)
            .padding(8)
            .style(theme::text_input),
    ]
    .spacing(f32::from(theme::SPACING));

    if let Some(error) = error {
        layout = layout.push(text(error.to_string()).color(theme::DANGER));
    }

    layout = layout.push(kit::action(
        if busy { "UNLOCKING..." } else { "UNLOCK" },
        kit::Act::Go,
        (!busy).then_some(Message::Unlock),
    ));

    kit::tag_panel("UNLOCK", theme::ACCENT, layout)
}

fn confirm_backup_view<'a>(
    shown: &'a str,
    quiz: Option<&'a crate::app::BackupQuiz>,
    saved_to: Option<&'a std::path::Path>,
    save_error: Option<&'a str>,
) -> Element<'a, Message> {
    // The only irreversible step on this screen, so this notice alone carries
    // WARNING. Phase two has no seed on screen, where "save this" would be
    // pointing at nothing.
    let notice = if quiz.is_none() {
        "Save this. It is the only copy of your wallet -- nothing else can recover it."
    } else {
        "The seed is hidden now. If you did not actually save it, go back -- nothing \
         else can recover this wallet."
    };
    let mut layout = column![container(text(notice).size(16).color(theme::WARNING))
        .style(theme::advisory_card)
        .padding(theme::PADDING)
        .width(Length::Fill),]
    .spacing(f32::from(theme::SPACING));

    match quiz {
        // Phase one: the seed is on screen, with the two ways anyone actually
        // keeps a 76-character hex string.
        None => {
            layout = layout.push(
                // Glyph wrapping, not the default word wrapping. A master seed
                // is one unbroken 76-character token, so `Wrapping::Word` has
                // nothing to break on and leaves it on a single line the box
                // clips -- which turned the one screen that says "this is the
                // only copy" into a screen showing about two thirds of it.
                container(
                    text(shown)
                        .size(18)
                        .wrapping(iced::widget::text::Wrapping::Glyph),
                )
                .padding(theme::PADDING)
                .width(Length::Fill)
                .style(container::bordered_box),
            );
            layout = layout.push(
                row![
                    button(text("Copy"))
                        .on_press(Message::BackupCopy)
                        .padding(10)
                        .style(|_, status| theme::button(theme::ButtonKind::Secondary, status)),
                    button(text("Save to file..."))
                        .on_press(Message::BackupSaveToFile)
                        .padding(10)
                        .style(|_, status| theme::button(theme::ButtonKind::Secondary, status)),
                ]
                .spacing(f32::from(theme::SPACING)),
            );
            if let Some(path) = saved_to {
                layout = layout.push(
                    text(format!("Saved to {}", path.display()))
                        .size(f32::from(theme::SMALL))
                        .color(theme::ACCENT)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                );
            }
            if let Some(message) = save_error {
                layout = layout.push(
                    text(message.to_string())
                        .size(f32::from(theme::SMALL))
                        .color(theme::DANGER)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                );
            }
            layout = layout.push(
                text(
                    "A password manager or a printed page is how a key this long is kept. \
                     Once you continue, this screen does not come back.",
                )
                .size(f32::from(theme::SMALL))
                .color(theme::MUTED),
            );
            layout = layout.push(
                button(text("I have saved it"))
                    .on_press(Message::BackupSavedIt)
                    .padding(12)
                    .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
            );
        }
        // Phase two: the seed is gone from the screen, so the only place to
        // read the answer is wherever it was just saved. That is the whole
        // proof -- a check you can answer off the display above it is not one.
        Some(quiz) => {
            let asked = quiz
                .positions
                .iter()
                .map(|at| (at + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            layout = layout.push(
                text("Open your saved copy and read these characters out of it.")
                    .size(f32::from(theme::BODY)),
            );
            layout = layout.push(
                text(format!("Characters {asked} of the seed, in that order"))
                    .size(f32::from(theme::BODY))
                    .color(theme::VALUE),
            );
            layout = layout.push(
                text_input("e.g. 4f2a", &quiz.typed)
                    .on_input(Message::BackupTypedChanged)
                    .on_submit(Message::ConfirmBackup)
                    .padding(8)
                    .style(theme::text_input),
            );
            if quiz.mismatch {
                layout = layout.push(
                    text("That does not match. Count from the first character, including `a9m1`.")
                        .color(theme::DANGER)
                        .size(f32::from(theme::SMALL)),
                );
            }
            layout = layout.push(
                row![
                    button(text("Confirm backup"))
                        .on_press(Message::ConfirmBackup)
                        .padding(12)
                        .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
                    // Without this the screen is a dead end: press "I have
                    // saved it" by mistake and there is no seed to read, no way
                    // to answer, and nothing to do but restart and lose the
                    // wallet that was never written. Going back clears the
                    // quiz, so returning draws fresh positions -- you can never
                    // hold the questions and the seed at the same time.
                    button(text("Show the seed again"))
                        .on_press(Message::BackupShowAgain)
                        .padding(12)
                        .style(|_, status| theme::button(theme::ButtonKind::Ghost, status)),
                ]
                .spacing(f32::from(theme::SPACING)),
            );
        }
    }

    kit::tag_panel("BACKUP", theme::ADVISORY, layout)
}

fn restore_view<'a>(
    seed_input: &'a str,
    seed_error: Option<&'a str>,
    photo: Option<&'a alphanumeric_gui::photo::PhotoSecret>,
    photo_busy: bool,
    photo_error: Option<&'a str>,
) -> Element<'a, Message> {
    let mut seed_column = column![
        text("Restore from a master seed").size(16),
        text_input("a9m1...", seed_input)
            .on_input(Message::RestoreSeedInputChanged)
            .padding(8)
            .style(theme::text_input),
    ]
    .spacing(f32::from(theme::SPACING));
    if let Some(error) = seed_error {
        seed_column = seed_column.push(text(error.to_string()).color(theme::DANGER));
    }
    seed_column = seed_column.push(
        button(text("Restore from this seed"))
            .on_press_maybe((!seed_input.trim().is_empty()).then_some(Message::UseSeedInput))
            .padding(12)
            .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
    );

    let mut photo_column = column![
        text("Restore from a photo").size(16),
        // A warning about losing access to money, not a neutral fact: the
        // frame and the text must tell the same story, so this is ADVISORY
        // like the card border, not MUTED.
        container(
            text(
                "A photograph is NOT a backup: re-saving, resizing, or sending it through a \
             messenger changes its pixels and derives a different wallet."
            )
            .size(f32::from(theme::CAPTION))
            .color(theme::ADVISORY)
        )
        .style(theme::advisory_card)
        .padding(theme::PADDING)
        .width(Length::Fill),
        button(text(if photo_busy {
            "Choosing..."
        } else {
            "Choose a photo"
        }))
        .on_press_maybe((!photo_busy).then_some(Message::ChoosePhoto))
        .padding(12)
        .style(|_, status| theme::button(theme::ButtonKind::Secondary, status)),
    ]
    .spacing(f32::from(theme::SPACING));

    if let Some(secret) = photo {
        photo_column = photo_column.push(
            column![
                text(format!("Photo: {}", secret.name)),
                text(format!("Fingerprint: {}", secret.key_id)).size(f32::from(theme::SMALL)),
            ]
            .spacing(2.0),
        );
        photo_column = photo_column.push(
            button(text("Restore from this photo"))
                .on_press(Message::UsePhoto)
                .padding(12)
                .style(|_, status| theme::button(theme::ButtonKind::Primary, status)),
        );
    }
    if let Some(error) = photo_error {
        photo_column = photo_column.push(text(error.to_string()).color(theme::DANGER));
    }

    let layout = column![
        seed_column,
        Space::new().height(f32::from(theme::SPACING)),
        photo_column,
        text(
            "A restore brings back the addresses this wallet derived. An address imported from \
             somewhere else lives only in the wallet file.",
        )
        .size(12)
        .color(theme::MUTED)
        .wrapping(Wrapping::WordOrGlyph),
        button(text("Back"))
            .on_press(Message::BackToChoose)
            .padding(8)
            .style(|_, status| theme::button(theme::ButtonKind::Ghost, status)),
    ]
    .spacing(f32::from(theme::SPACING));

    kit::tag_panel("RESTORE", theme::CYAN, layout)
}

fn set_passphrase_view<'a>(
    pending: &'a PendingWallet,
    passphrase: &'a str,
    confirm: &'a str,
    busy: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let title = match pending {
        PendingWallet::Fresh(_) => "Choose a passphrase to encrypt your new wallet.",
        PendingWallet::Restored(_) => "Choose a passphrase to encrypt the restored wallet.",
    };

    let mut layout = column![
        text(title),
        text_input("Passphrase", passphrase)
            .secure(true)
            .on_input(Message::NewPassphraseChanged)
            .padding(8)
            .style(theme::text_input),
        text_input("Confirm passphrase", confirm)
            .secure(true)
            .on_input(Message::NewPassphraseConfirmChanged)
            .on_submit(Message::ConfirmNewPassphrase)
            .padding(8)
            .style(theme::text_input),
    ]
    .spacing(f32::from(theme::SPACING));

    if let Some(error) = error {
        layout = layout.push(text(error.to_string()).color(theme::DANGER));
    }

    let mut continue_button = button(text(if busy { "Working..." } else { "Continue" }))
        .padding(12)
        .style(|_, status| theme::button(theme::ButtonKind::Primary, status));
    if !busy {
        continue_button = continue_button.on_press(Message::ConfirmNewPassphrase);
    }
    layout = layout.push(continue_button);

    kit::tag_panel("PASSPHRASE", theme::CYAN, layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_archive_is_named_and_several_are_counted() {
        assert_eq!(archive_hint(&[]), None);
        let one = archive_hint(&[PathBuf::from("/h/seed-20260912-031500.enc")]).expect("hint");
        assert!(one.contains("/h/seed-20260912-031500.enc"), "{one}");
        assert!(one.contains("IMPORT FILE"), "{one}");
        let two = archive_hint(&[
            PathBuf::from("/h/seed-20260101-000000.enc"),
            PathBuf::from("/h/seed-20260912-031500.enc"),
        ])
        .expect("hint");
        assert!(two.starts_with("2 archived wallets"), "{two}");
        assert!(two.contains("/h"), "{two}");
    }
}
