//! The parts every screen is built from. Ported from noid_gui (Apache-2.0,
//! Copyright (C) 2026 Paranoid Zero): `view/mod.rs` (`header`,
//! `live_status`, `status_value`, `separator`), `view/present.rs`
//! (`terminal_meter`, `telemetry_value`, `amount_stat`, `account_separator`,
//! the `utxo_table` pieces) and `view/mod.rs`'s `node_log_terminal`.
//!
//! A screen file never builds a container style of its own. Shapes drifting
//! from screen to screen is exactly what made the wallet look unfinished
//! next to Parano1d (sub-project F spec §2).

use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, responsive, row, scrollable, text, Space};
use iced::{Alignment, Color, Element, Length};

use alphanumeric_gui::activity::{Coverage, Dir, Display, RowStatus, Sign};
use alphanumeric_gui::model::format_coins;

use crate::app::Message;
use crate::theme;

// ---- gauges and telemetry ---------------------------------------------------

/// Bars a gauge draws. 10 bars of 4 px keep a meter cell inside the ~165 px a
/// grid column has at the 760 px minimum window width.
pub const METER_CELLS: usize = 10;

/// Bars lit for `ratio`, clamped to 0..=1 and rounded up so any real load
/// shows at least one bar.
pub fn lit_cells(ratio: f32, cells: usize) -> usize {
    if !ratio.is_finite() || ratio <= 0.0 {
        return 0;
    }
    ((ratio.min(1.0) * cells as f32).ceil() as usize).min(cells)
}

/// `LABEL [||||      ] 11.4%`. `ratio: None` draws no bars -- an unknown
/// load is not an idle one -- and the caller passes `—` as `value`.
pub fn meter(
    label: &'static str,
    ratio: Option<f32>,
    colour: Color,
    value: String,
) -> Element<'static, Message> {
    let lit = ratio.map(|r| lit_cells(r, METER_CELLS)).unwrap_or(0);
    let mut cells = row![].spacing(0);
    for index in 0..METER_CELLS {
        cells = cells.push(
            text(if index < lit { "|" } else { " " })
                .size(15)
                .color(colour)
                .width(Length::Fixed(4.0)),
        );
    }
    row![
        text(label)
            .size(13)
            .color(theme::CYAN)
            .wrapping(Wrapping::None)
            .width(Length::Fixed(56.0)),
        text("[").size(15).color(theme::DIM),
        cells,
        Space::new().width(Length::Fill),
        text(value)
            .size(13)
            .color(theme::MUTED)
            .wrapping(Wrapping::None),
        text("]").size(15).color(theme::DIM),
    ]
    .spacing(2)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .into()
}

/// `LABEL ......... [value]`.
pub fn telemetry(label: &'static str, value: String, colour: Color) -> Element<'static, Message> {
    row![
        text(label)
            .size(13)
            .color(theme::CYAN)
            .wrapping(Wrapping::None)
            .width(Length::Fill),
        text(format!("[{value}]"))
            .size(14)
            .color(colour)
            .wrapping(Wrapping::None),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .into()
}

// ---- header ----------------------------------------------------------------

/// One piece of a header capsule.
#[derive(Debug, Clone)]
pub enum Chip {
    /// A coloured state with a dot: `● SYNCED`. Never dropped for width.
    Live(String, Color),
    /// A dim label and a value: `PEERS 11`.
    Value(&'static str, String),
}

/// `Value` chips a narrow header gives up, first to last, after the
/// identity text. VERSION goes before anything that describes the node's
/// state; `Live` chips (sync, mining) are never on the list.
pub const HEADER_DROP_ORDER: &[&str] = &["VERSION", "NODE", "PEERS"];

// Widths for `header_plan`. Chip text is the default face (Noto Sans Mono,
// 0.6 em for every glyph, `—` and `…` included) at 13 px; the identity's
// "alphanumeric" is Noto Sans Bold at 15 px, 6.963 em measured from the
// bundled font, and "wallet" is mono at 15 px.
const CHIP_CHAR_PX: f32 = 13.0 * 0.6;
const IDENTITY_PX: f32 = 6.963 * 15.0 + 8.0 + 6.0 * 15.0 * 0.6;
const HEADER_PADDING_X: f32 = 32.0;
const HEADER_SPACING: f32 = 8.0;
/// Divider plus the spacing either side of it, between two chips.
const CHIP_GAP_PX: f32 = 10.0 + 1.0 + 10.0;
/// A capsule's padding and border, both sides.
const CAPSULE_FRAME_PX: f32 = 24.0 + 2.0;
/// Room left over, so a rounding error drops a chip rather than cuts one.
const HEADER_SLACK_PX: f32 = 8.0;

fn chars_px(text: &str) -> f32 {
    text.chars().count() as f32 * CHIP_CHAR_PX
}

fn chip_px(chip: &Chip) -> f32 {
    match chip {
        Chip::Live(label, _) => 9.0 + 8.0 + chars_px(label),
        Chip::Value(label, value) => chars_px(label) + 6.0 + chars_px(value),
    }
}

fn capsule_px(chips: &[Chip]) -> f32 {
    if chips.is_empty() {
        return 0.0;
    }
    let gaps = (chips.len() - 1) as f32 * CHIP_GAP_PX;
    chips.iter().map(chip_px).sum::<f32>() + gaps + CAPSULE_FRAME_PX
}

/// `chips` without the first `hidden` labels of `drop_order`.
pub(crate) fn visible_chips(chips: &[Chip], drop_order: &[&str], hidden: usize) -> Vec<Chip> {
    let gone = &drop_order[..hidden.min(drop_order.len())];
    chips
        .iter()
        .filter(|c| !matches!(c, Chip::Value(label, _) if gone.contains(label)))
        .cloned()
        .collect()
}

/// What a header `width` px wide shows.
#[derive(Debug, PartialEq, Eq)]
pub struct HeaderPlan {
    /// The "alphanumeric wallet" text.
    pub identity: bool,
    /// How many labels of the drop order are left out.
    pub hidden: usize,
    /// False only when even the last step does not fit; it is drawn anyway.
    pub fits: bool,
}

/// The first of: everything; no identity; no identity and one more label
/// of `drop_order` gone at a time -- that fits `width`. A chip is shown
/// whole or not at all, so capsules never wrap or half-draw one.
pub(crate) fn header_plan(
    width: f32,
    left: &[Chip],
    right: &[Chip],
    drop_order: &[&str],
) -> HeaderPlan {
    let room = width - HEADER_PADDING_X - HEADER_SLACK_PX;
    let needed = |identity: bool, hidden: usize| {
        let capsules = capsule_px(&visible_chips(left, drop_order, hidden))
            + capsule_px(&visible_chips(right, drop_order, hidden));
        // Items: [identity, "wallet"], the fill space, two capsules.
        if identity {
            IDENTITY_PX + HEADER_SPACING + HEADER_SPACING * 2.0 + capsules
        } else {
            HEADER_SPACING * 2.0 + capsules
        }
    };
    if needed(true, 0) <= room {
        return HeaderPlan {
            identity: true,
            hidden: 0,
            fits: true,
        };
    }
    for hidden in 0..=drop_order.len() {
        if needed(false, hidden) <= room {
            return HeaderPlan {
                identity: false,
                hidden,
                fits: true,
            };
        }
    }
    HeaderPlan {
        identity: false,
        hidden: drop_order.len(),
        fits: false,
    }
}

fn chip(chip: Chip) -> Element<'static, Message> {
    match chip {
        Chip::Live(label, colour) => row![
            container(Space::new())
                .width(Length::Fixed(9.0))
                .height(Length::Fixed(9.0))
                .style(theme::status_dot(colour)),
            text(label).size(13).color(colour).wrapping(Wrapping::None),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into(),
        Chip::Value(label, value) => row![
            text(label)
                .size(13)
                .color(theme::DIM)
                .wrapping(Wrapping::None),
            text(value)
                .size(13)
                .color(theme::TEXT)
                .wrapping(Wrapping::None),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
    }
}

fn capsule(chips: Vec<Chip>) -> Element<'static, Message> {
    let mut line = row![].spacing(10).align_y(Alignment::Center);
    for (index, c) in chips.into_iter().enumerate() {
        if index > 0 {
            line = line.push(
                container(Space::new())
                    .width(Length::Fixed(1.0))
                    .height(Length::Fixed(18.0))
                    .style(theme::divider),
            );
        }
        line = line.push(chip(c));
    }
    container(line)
        .padding([6, 12])
        .style(theme::status_capsule)
        .into()
}

/// The top bar: identity left, two chip capsules right. noid: `header`.
///
/// Laid out by the width it actually gets (`responsive`): at 760 px the
/// full bar does not fit, and an unmeasured row pushed the MINING chip off
/// the window. `header_plan` decides what goes, in `HEADER_DROP_ORDER`.
pub fn header(left: Vec<Chip>, right: Vec<Chip>) -> Element<'static, Message> {
    responsive(move |size| {
        let plan = header_plan(size.width, &left, &right, HEADER_DROP_ORDER);
        let mut line = row![].spacing(HEADER_SPACING).align_y(Alignment::Center);
        if plan.identity {
            line = line
                .push(
                    text("alphanumeric")
                        .size(15)
                        .font(theme::BRAND_FONT)
                        .color(theme::TEXT)
                        .wrapping(Wrapping::None),
                )
                .push(
                    text("wallet")
                        .size(15)
                        .color(theme::MUTED)
                        .wrapping(Wrapping::None),
                );
        }
        line = line
            .push(Space::new().width(Length::Fill))
            .push(capsule(visible_chips(
                &left,
                HEADER_DROP_ORDER,
                plan.hidden,
            )))
            .push(capsule(visible_chips(
                &right,
                HEADER_DROP_ORDER,
                plan.hidden,
            )));
        container(line)
            .height(Length::Fixed(48.0))
            .padding([0, 16])
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .clip(true)
            .style(theme::top_bar)
            .into()
    })
    .height(Length::Fixed(48.0))
    .into()
}

// ---- narrow layout -----------------------------------------------------------

/// Below this width a two-panel screen stacks its panels instead of sitting
/// them side by side (R1). The header does not read this: it measures its
/// own chips and drops them independently.
pub const NARROW_BELOW: f32 = 900.0;

/// A two-panel screen's layout: side by side with `FillPortion`s when there
/// is room for both, stacked full width one above the other when there is
/// not. F1, F2, F3 (`view`), F6 and F7 all go through this, so the width
/// threshold only has to be right in one place.
pub fn split<'a>(
    narrow: bool,
    left: Element<'a, Message>,
    left_portion: u16,
    right: Element<'a, Message>,
    right_portion: u16,
) -> Element<'a, Message> {
    if narrow {
        column![
            container(left).width(Length::Fill),
            container(right).width(Length::Fill),
        ]
        .spacing(f32::from(theme::SPACING))
        .into()
    } else {
        row![
            container(left).width(Length::FillPortion(left_portion)),
            container(right).width(Length::FillPortion(right_portion)),
        ]
        .spacing(f32::from(theme::SPACING))
        .into()
    }
}

// ---- panels and tables -----------------------------------------------------

/// A coloured tag (`ACTIVE ADDRESS`) sitting on top of a panel.
pub fn tag_panel<'a>(
    tag: impl Into<String>,
    colour: Color,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        container(
            text(tag.into())
                .size(12)
                .font(theme::TECH_FONT)
                .color(theme::INK)
        )
        .padding([3, 9])
        .style(theme::tag(colour)),
        container(body)
            .padding(10)
            .width(Length::Fill)
            .style(theme::surface),
    ]
    .width(Length::Fill)
    .into()
}

/// A table's header row. `(label, portion)` pairs; rows must use the same portions.
///
/// Wraps rather than overflowing, like the cells below it: an unwrapped
/// label wider than its column draws over the next one.
pub fn table_head(columns: &[(&str, u16)]) -> Element<'static, Message> {
    let mut line = row![].align_y(Alignment::Center);
    for (label, portion) in columns {
        line = line.push(
            text(label.to_string())
                .size(12)
                .color(theme::MUTED)
                .wrapping(Wrapping::WordOrGlyph)
                .width(Length::FillPortion(*portion)),
        );
    }
    container(line)
        .padding([6, 8])
        .width(Length::Fill)
        .style(theme::table_head)
        .into()
}

/// Text size of a table cell; headers are one step smaller.
const TABLE_CELL_SIZE: f32 = 13.0;

/// Arithmetic the table tests use to check that realistic figures fit their
/// columns. The default face is Noto Sans Mono (`main.rs`), whose every
/// glyph advances 0.6 em, so a string's width is its length times that.
#[cfg(test)]
pub(crate) mod sizing {
    /// One character in a table cell (13 px) and in a header (12 px).
    const CELL_CHAR_PX: f32 = 13.0 * 0.6;
    const HEAD_CHAR_PX: f32 = 12.0 * 0.6;
    /// `screen`'s padding, both sides.
    const SCREEN_PADDING_X: f32 = 32.0;
    /// `theme::SPACING` between two panels side by side.
    const PANEL_GAP: f32 = 12.0;
    /// `tag_panel`'s body padding plus `table_row`'s, both sides.
    const PANEL_AND_ROW_PADDING_X: f32 = 20.0 + 16.0;

    /// The width a table's cells share, for a panel taking `share` of a
    /// row of `panels` panels in a window `window` px wide.
    pub(crate) fn row_width(window: f32, panels: u16, share: f32) -> f32 {
        let gaps = f32::from(panels.saturating_sub(1)) * PANEL_GAP;
        (window - SCREEN_PADDING_X - gaps) * share - PANEL_AND_ROW_PADDING_X
    }

    fn column_width(columns: &[(&str, u16)], index: usize, row_width: f32) -> f32 {
        let total: u16 = columns.iter().map(|(_, portion)| portion).sum();
        row_width * f32::from(columns[index].1) / f32::from(total)
    }

    /// Every sample fits its column on one line with a character's gap
    /// before the next column (cells abut), and every header label fits.
    /// Panics naming the first that does not.
    pub(crate) fn assert_fits(columns: &[(&str, u16)], samples: &[&str], row_width: f32) {
        assert_eq!(columns.len(), samples.len());
        for (index, ((label, _), sample)) in columns.iter().zip(samples).enumerate() {
            let width = column_width(columns, index, row_width);
            let needed = (sample.chars().count() + 1) as f32 * CELL_CHAR_PX;
            assert!(
                needed <= width,
                "{label}: {sample:?} needs {needed:.1} px, the column has {width:.1} px"
            );
            let head = label.chars().count() as f32 * HEAD_CHAR_PX;
            assert!(
                head <= width,
                "header {label} needs {head:.1} px, the column has {width:.1} px"
            );
        }
    }

    /// Header labels alone fit (a narrower width, where cells may wrap).
    pub(crate) fn assert_heads_fit(columns: &[(&str, u16)], row_width: f32) {
        for (index, (label, _)) in columns.iter().enumerate() {
            let width = column_width(columns, index, row_width);
            let head = label.chars().count() as f32 * HEAD_CHAR_PX;
            assert!(
                head <= width,
                "header {label} needs {head:.1} px, the column has {width:.1} px"
            );
        }
    }
}

pub struct Cell {
    pub text: String,
    pub portion: u16,
    pub colour: Color,
}

pub fn cell(text: impl Into<String>, portion: u16, colour: Color) -> Cell {
    Cell {
        text: text.into(),
        portion,
        colour,
    }
}

/// One table row. `on_press: None` rows are still drawn as ordinary rows
/// (`theme::table_row_button`).
///
/// A cell too wide for its column wraps and the row grows. It is never
/// clipped and never drawn over the next column: a clipped amount is a
/// plausible wrong number, and an overlapped one is unreadable. The column
/// portions are sized so realistic figures fit on one line at the 1000 px
/// default (each table's own test); at the 760 px minimum some wrap.
pub fn table_row(
    cells: Vec<Cell>,
    alternate: bool,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let mut line = row![].align_y(Alignment::Center);
    for c in cells {
        line = line.push(
            text(c.text)
                .size(TABLE_CELL_SIZE)
                .color(c.colour)
                .wrapping(Wrapping::WordOrGlyph)
                .width(Length::FillPortion(c.portion)),
        );
    }
    button(line)
        .on_press_maybe(on_press)
        .width(Length::Fill)
        .padding([6, 8])
        .style(move |_, status| theme::table_row_button(alternate, selected, status))
        .into()
}

/// The strip under a table: counts on the left, controls on the right.
pub fn table_foot<'a>(
    left: Element<'a, Message>,
    right: Element<'a, Message>,
) -> Element<'a, Message> {
    container(row![left, Space::new().width(Length::Fill), right].align_y(Alignment::Center))
        .padding([6, 8])
        .width(Length::Fill)
        .into()
}

/// `KEY ........ value` with a hairline under it.
pub fn kv_row(key: &str, value: impl Into<String>) -> Element<'static, Message> {
    column![
        row![
            text(key.to_string())
                .size(12)
                .color(theme::MUTED)
                .width(Length::FillPortion(2)),
            text(value.into())
                .size(13)
                .color(theme::VALUE)
                .wrapping(Wrapping::WordOrGlyph)
                .width(Length::FillPortion(3)),
        ]
        .align_y(Alignment::Center),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(theme::divider),
    ]
    .spacing(4)
    .into()
}

/// The small cyan caption over an input field.
pub fn field_label(label: &str) -> Element<'static, Message> {
    text(label.to_string()).size(11).color(theme::CYAN).into()
}

/// A labelled figure in a balance row. `big` for the headline number.
pub fn stat(
    label: &str,
    value: impl Into<String>,
    colour: Color,
    big: bool,
) -> Element<'static, Message> {
    column![
        text(label.to_string())
            .size(11)
            .color(theme::DIM)
            .wrapping(Wrapping::None),
        text(value.into())
            .size(if big { 26 } else { 16 })
            .font(theme::TECH_FONT)
            .color(colour)
            .wrapping(Wrapping::None),
    ]
    .spacing(4)
    .into()
}

pub fn stat_separator() -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(38.0))
        .style(theme::divider)
        .into()
}

pub fn badge(label: impl Into<String>, colour: Color) -> Element<'static, Message> {
    container(text(label.into()).size(12).color(colour))
        .padding([3, 8])
        .style(theme::badge(colour))
        .into()
}

/// A big either/or card.
pub fn choice_card(
    title: &str,
    detail: &str,
    selected: bool,
    on_press: Message,
) -> Element<'static, Message> {
    button(
        column![
            text(title.to_string()).size(14).color(if selected {
                theme::ACCENT
            } else {
                theme::TEXT
            }),
            text(detail.to_string())
                .size(12)
                .color(theme::MUTED)
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(4),
    )
    .on_press(on_press)
    .padding(12)
    .width(Length::Fill)
    .style(move |_, status| theme::choice_card(selected, status))
    .into()
}

/// What a button means, not how it looks: the four meanings are the only
/// four looks (spec §3).
#[derive(Debug, Clone, Copy)]
pub enum Act {
    /// Send, confirm, unlock: green fill.
    Go,
    /// Receive, review, next: cyan outline.
    Look,
    /// Restart, reveal a seed: orange outline.
    Care,
    /// Everything else.
    Plain,
}

pub fn action(label: &str, act: Act, on_press: Option<Message>) -> Element<'static, Message> {
    let kind = match act {
        Act::Go => theme::ButtonKind::Primary,
        Act::Look => theme::ButtonKind::Outline,
        Act::Care => theme::ButtonKind::Caution,
        Act::Plain => theme::ButtonKind::Secondary,
    };
    button(text(label.to_string()).size(13))
        .on_press_maybe(on_press)
        .padding([6, 12])
        .style(move |_, status| theme::button(kind, status))
        .into()
}

// ---- terminal ----------------------------------------------------------------

/// Removes ANSI escape sequences (`ESC [ … final-byte`). The node writes its
/// banner in 24-bit colour and `node.log` keeps the raw escapes; drawn as-is
/// they show up as `[0m[38;2;165;225;236m`.
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn level_colour(line: &str) -> Color {
    if line.contains("ERROR") || line.contains("FATAL") {
        theme::DANGER
    } else if line.contains("WARN") {
        theme::WARNING
    } else {
        theme::MUTED
    }
}

/// Log lines in a dark box, escapes stripped, coloured by level.
pub fn terminal(lines: &[String], empty: &str) -> Element<'static, Message> {
    let mut body = column![].spacing(2);
    if lines.is_empty() {
        body = body.push(text(empty.to_string()).size(12).color(theme::DIM));
    }
    for line in lines {
        let clean = strip_ansi(line);
        let colour = level_colour(&clean);
        body = body.push(
            text(clean)
                .size(12)
                .font(theme::TECH_FONT)
                .color(colour)
                .wrapping(Wrapping::Glyph),
        );
    }
    container(body)
        .padding(10)
        .width(Length::Fill)
        .style(theme::terminal)
        .into()
}

// ---- a whole screen body ---------------------------------------------------

/// Root background, padding, vertical scroll -- every tab's body.
pub fn screen<'a>(body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(
        scrollable(container(body).padding(theme::PADDING).width(Length::Fill))
            .style(theme::scrollable),
    )
    .style(theme::root)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ---- how a transaction row reads (shared by F1, F2, F4) -------------------

/// `c0ffee11…66778899`. Tables are too narrow for 40 hex characters; the full
/// address is always on screen elsewhere (the active-address bar, Receive).
pub fn short_address(address: &str) -> String {
    if address.chars().count() <= 19 {
        return address.to_string();
    }
    let head: String = address.chars().take(8).collect();
    let tail: String = address
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

pub fn status_cell(status: RowStatus) -> (String, Color) {
    match status {
        RowStatus::Final => ("FINAL".into(), theme::ACCENT),
        RowStatus::Confirmed => ("CONFIRMED".into(), theme::TEXT),
        RowStatus::Maturing { .. } => ("MATURING".into(), theme::ADVISORY),
        RowStatus::Unknown => ("—".into(), theme::DIM),
    }
}

pub fn dir_colour(dir: Dir) -> Color {
    match dir {
        Dir::Mined => theme::LAVENDER,
        Dir::In => theme::ACCENT,
        Dir::Out => theme::ADVISORY,
        Dir::SelfSend => theme::MUTED,
        Dir::Internal => theme::CYAN,
    }
}

/// `+1.5`, `-0.5`, or unsigned for a move that nets to zero.
pub fn signed_amount(display: &Display) -> String {
    let coins = format_coins(display.value_units);
    match display.sign {
        Sign::Plus => format!("+{coins}"),
        Sign::Minus => format!("-{coins}"),
        Sign::Neutral => coins,
    }
}

/// What an F1/F2 list says while some address has not answered.
pub const WAITING_FOR_EVERY_ADDRESS: &str = "Waiting for the node to answer for every address.";

/// What a list filtered out of the newest `want` rows (F2's incoming) says when the filter finds nothing. `what` names the missing
/// thing ("No incoming transfers"); `never` is the sentence for a history
/// read to its end, the only case that may claim nothing ever came.
pub fn filtered_empty(what: &str, never: &str, coverage: Coverage, want: usize) -> String {
    match coverage {
        Coverage::Waiting => WAITING_FOR_EVERY_ADDRESS.to_string(),
        Coverage::Behind => format!(
            "{what} found. The node's address index is behind the chain, so the newest \
             transactions may be missing."
        ),
        Coverage::Latest => format!("{what} in the latest {want} transactions."),
        Coverage::All => never.to_string(),
    }
}

/// The footer count of such a list: `PARTIAL` while it cannot be trusted,
/// and how far it looked when it did not read everything.
pub fn filtered_foot(label: &str, shown: usize, coverage: Coverage, want: usize) -> String {
    match coverage {
        Coverage::Waiting | Coverage::Behind => "PARTIAL".to_string(),
        Coverage::Latest => format!("{label} [{shown}] IN LATEST {want}"),
        Coverage::All => format!("{label} [{shown}]"),
    }
}

pub fn amount_colour(display: &Display) -> Color {
    match display.sign {
        Sign::Plus => theme::ACCENT,
        Sign::Minus => theme::TEXT,
        Sign::Neutral => theme::MUTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gauge_lights_whole_bars_and_never_more_than_it_has() {
        assert_eq!(lit_cells(0.0, 10), 0);
        assert_eq!(lit_cells(0.001, 10), 1, "any real load shows");
        assert_eq!(lit_cells(0.5, 10), 5);
        assert_eq!(lit_cells(1.0, 10), 10);
        assert_eq!(lit_cells(3.0, 10), 10);
        assert_eq!(lit_cells(f32::NAN, 10), 0);
    }

    fn chips(sync: &str, mining: &str) -> (Vec<Chip>, Vec<Chip>) {
        (
            vec![
                Chip::Live(sync.into(), theme::ACCENT),
                Chip::Value("PEERS", "11".into()),
                Chip::Value("HEIGHT", "1 015 872".into()),
            ],
            vec![
                Chip::Value("VERSION", "8.0.0".into()),
                Chip::Value("NODE", "EXTERNAL".into()),
                Chip::Live(mining.into(), theme::ACCENT),
            ],
        )
    }

    /// The header's own width: the window less `App::view`'s 8 px each side.
    fn header_width(window: f32) -> f32 {
        window - 16.0
    }

    #[test]
    fn a_wide_header_shows_everything() {
        let (left, right) = chips("SYNCED", "MINING 27.8 GH/s");
        let plan = header_plan(header_width(1000.0), &left, &right, HEADER_DROP_ORDER);
        assert_eq!(
            plan,
            HeaderPlan {
                identity: true,
                hidden: 0,
                fits: true
            }
        );
    }

    /// Below ~900 px the identity goes first, then VERSION -- never MINING,
    /// which is a live chip and not in the drop order at all.
    #[test]
    fn a_narrow_header_drops_identity_then_version_and_keeps_mining() {
        let (left, right) = chips("BEHIND 9234", "MINING 27.8 GH/s");
        let mid = header_plan(header_width(880.0), &left, &right, HEADER_DROP_ORDER);
        assert!(!mid.identity, "{mid:?}");
        let narrow = header_plan(header_width(760.0), &left, &right, HEADER_DROP_ORDER);
        assert_eq!(
            narrow,
            HeaderPlan {
                identity: false,
                hidden: 1,
                fits: true
            }
        );
        assert_eq!(HEADER_DROP_ORDER[0], "VERSION");
        assert!(right
            .iter()
            .any(|c| matches!(c, Chip::Live(label, _) if label.starts_with("MINING"))));
    }

    /// A short state at the minimum width keeps VERSION when it fits.
    #[test]
    fn the_header_drops_only_what_it_must() {
        let (left, right) = chips("SYNCED", "MINING —");
        let plan = header_plan(header_width(760.0), &left, &right, HEADER_DROP_ORDER);
        assert!(plan.fits);
        assert!(plan.hidden <= 1);
        let shown = visible_chips(&right, HEADER_DROP_ORDER, plan.hidden);
        assert!(shown.iter().any(|c| matches!(c, Chip::Live(..))));
    }

    #[test]
    fn ansi_colour_is_stripped_and_text_kept() {
        assert_eq!(
            strip_ansi("\u{1b}[0m\u{1b}[38;2;165;225;236m    ++++  Quantum DSS"),
            "    ++++  Quantum DSS"
        );
        assert_eq!(strip_ansi("plain line"), "plain line");
        assert_eq!(strip_ansi("a\u{1b}b"), "ab", "a lone ESC is dropped");
    }

    #[test]
    fn log_levels_get_their_colour() {
        assert_eq!(level_colour("[ERROR] bind failed"), theme::DANGER);
        assert_eq!(level_colour("Headless WARN something"), theme::WARNING);
        assert_eq!(level_colour("Headless mode enabled."), theme::MUTED);
    }

    #[test]
    fn a_long_address_is_shortened_in_the_middle() {
        assert_eq!(
            short_address("c0ffee11aabbccddeeff00112233445566778899"),
            "c0ffee11…66778899"
        );
        assert_eq!(short_address("MINING_REWARDS"), "MINING_REWARDS");
    }

    #[test]
    fn amounts_carry_their_direction() {
        let make = |sign| Display {
            dir: Dir::In,
            value_units: 150_000_000,
            sign,
            fee_units: None,
            counterparty: String::new(),
        };
        assert_eq!(signed_amount(&make(Sign::Plus)), "+1.5");
        assert_eq!(signed_amount(&make(Sign::Minus)), "-1.5");
        assert_eq!(signed_amount(&make(Sign::Neutral)), "1.5");
    }

    /// A list filtered out of the newest rows (F2's incoming)
    /// that finds nothing has only looked at those rows. "Nothing has
    /// arrived" is for a history read to its end.
    #[test]
    fn an_empty_filtered_list_says_how_far_it_looked() {
        let never = "Nothing has arrived at this address yet.";
        let say = |coverage| filtered_empty("No incoming transfers", never, coverage, 50);
        assert_eq!(
            say(Coverage::Latest),
            "No incoming transfers in the latest 50 transactions."
        );
        assert_eq!(say(Coverage::All), never);
        assert_eq!(
            say(Coverage::Waiting),
            "Waiting for the node to answer for every address."
        );
        let behind = say(Coverage::Behind);
        assert!(behind.contains("behind"), "{behind}");
        assert_ne!(behind, never);
    }

    #[test]
    fn a_filtered_list_footer_is_partial_or_counts_what_it_read() {
        let foot = |shown, coverage| filtered_foot("RECEIVED", shown, coverage, 50);
        assert_eq!(foot(0, Coverage::Waiting), "PARTIAL");
        assert_eq!(foot(3, Coverage::Behind), "PARTIAL");
        assert_eq!(foot(3, Coverage::Latest), "RECEIVED [3] IN LATEST 50");
        assert_eq!(foot(3, Coverage::All), "RECEIVED [3]");
    }

    #[test]
    fn an_unknown_status_is_a_dash() {
        assert_eq!(status_cell(RowStatus::Unknown).0, "—");
        assert_eq!(
            status_cell(RowStatus::Maturing { blocks_left: 3 }).0,
            "MATURING"
        );
    }
}
