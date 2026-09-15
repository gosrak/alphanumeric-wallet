//! F5: the miner -- its controls, what it is doing, every device's figures,
//! and where the rewards land.
//!
//! Widget assembly only. The figures come from `backend::NodeStatus` (the
//! wallet's own poll), the controls edit `App::settings.mining` through
//! `Message::Mining*`, and START/STOP restart the owned node with the
//! result. The layout follows the sibling wallet's mining screen: the
//! miner's state on the left, its controls on the right, then the device
//! table and the rewards, full width.

use iced::widget::text::Wrapping;
use iced::widget::{column, container, pick_list, row, text, Space};
use iced::{Alignment, Color, Element, Length};

use alphanumeric_gui::activity::{self, Recent};
use alphanumeric_gui::backend::{MiningDevice, NodeStatus};
use alphanumeric_gui::history::RowKind;
use alphanumeric_gui::model;
use alphanumeric_gui::settings::Backend;

use crate::app::{AddressEntry, App, Message, Screen};
use crate::theme;
use crate::view::console::{fmt_units, or_dash};
use crate::view::{field, kit};

/// What to say about where the coinbase reward lands.
///
/// `mining_payout_rotation` being true means the reward goes to a schedule
/// address that changes with height (`ALPHANUMERIC_COINBASE_PAYOUTS`) --
/// `mining_address` is then NOT where it lands (the node's own comment on
/// `NodeStatus::mining_address` says the two diverge under pool operation).
/// Showing `mining_address` next to "payout" while rotation is on would tell
/// the user their reward goes somewhere it does not, so rotation wins
/// outright: it is checked first, and an address (present or not) never
/// gets a line of its own while it is true.
fn payout_line(address: Option<String>, rotating: bool) -> String {
    if rotating {
        return "Payout rotates by height under the pool payout schedule -- there is no \
                 single address to show."
            .to_string();
    }
    address.unwrap_or_else(|| "—".to_string())
}

/// Rows read to find this wallet's rewards -- the newest 50, already fetched.
const MINED_WANT: usize = 50;

const MINED_COLUMNS: [(&str, u16); 4] =
    [("HEIGHT", 3), ("ADDR", 2), ("REWARD", 4), ("MATURES IN", 4)];

/// The device table: the name gets the room, the figures are short.
const DEVICE_COLUMNS: [(&str, u16); 7] = [
    ("DEVICE", 6),
    ("HASHRATE", 3),
    ("CORE", 3),
    ("VRAM", 3),
    ("TEMP", 2),
    ("POWER", 2),
    ("HASHES", 2),
];

pub(crate) fn fmt_mhz(v: Option<u32>) -> String {
    v.map(|m| format!("{m} MHz"))
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn fmt_temp(v: Option<u32>) -> String {
    v.map(|t| format!("{t} °C"))
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn fmt_power(v: Option<f64>) -> String {
    v.map(|w| format!("{w:.0} W"))
        .unwrap_or_else(|| "—".to_string())
}

/// A hash count with a unit, three significant figures: `4.00 G`, `12.3 M`.
pub(crate) fn fmt_hashes(h: u64) -> String {
    let h = h as f64;
    let (value, unit) = if h >= 1e12 {
        (h / 1e12, " T")
    } else if h >= 1e9 {
        (h / 1e9, " G")
    } else if h >= 1e6 {
        (h / 1e6, " M")
    } else if h >= 1e3 {
        (h / 1e3, " k")
    } else {
        return format!("{h:.0}");
    };
    if value >= 100.0 {
        format!("{value:.0}{unit}")
    } else if value >= 10.0 {
        format!("{value:.1}{unit}")
    } else {
        format!("{value:.2}{unit}")
    }
}

/// Seconds to a block as the node estimates it: `~12s`, `~3m`, `~1.2h`, `~2.3d`.
pub(crate) fn fmt_eta(secs: Option<f64>) -> String {
    let Some(s) = secs else {
        return "—".to_string();
    };
    if !s.is_finite() || s <= 0.0 {
        return "—".to_string();
    }
    if s < 90.0 {
        format!("~{s:.0}s")
    } else if s < 5400.0 {
        format!("~{:.0}m", s / 60.0)
    } else if s < 172_800.0 {
        format!("~{:.1}h", s / 3600.0)
    } else {
        format!("~{:.1}d", s / 86_400.0)
    }
}

/// A device's hashrate in the unit it deserves: the CPU miner does tens of
/// MH/s and must not read `0.0 GH/s`.
pub(crate) fn fmt_device_hashrate(hps: f64) -> String {
    if hps <= 0.0 {
        "—".to_string()
    } else if hps >= 1e9 {
        format!("{:.2} GH/s", hps / 1e9)
    } else if hps >= 1e6 {
        format!("{:.1} MH/s", hps / 1e6)
    } else {
        format!("{:.0} kH/s", hps / 1e3)
    }
}

pub fn view(app: &App) -> Element<'_, Message> {
    let status = app.wallet.as_ref().and_then(|w| w.node_status.as_ref());

    let (badge, badge_colour) = match status.and_then(|s| s.mining) {
        Some(true) => ("MINING", theme::ACCENT),
        Some(false) => ("NOT MINING", theme::MUTED),
        None => ("—", theme::DIM),
    };
    // The unit the rate deserves: a CPU session at 197 MH/s must not read
    // "0.2 GH/s". The header chip keeps GH/s; this is the exact figure.
    let hashrate = status
        .and_then(|s| s.mining_hps)
        .map(fmt_device_hashrate)
        .unwrap_or_else(|| "—".to_string());
    let blocks_this_session = or_dash(status.and_then(|s| s.mining_blocks));
    let backend_name = status
        .and_then(|s| s.mining_backend.clone())
        .unwrap_or_else(|| "—".to_string());
    let payout = payout_line(
        status.and_then(|s| s.mining_address.clone()),
        status
            .and_then(|s| s.mining_payout_rotation)
            .unwrap_or(false),
    );
    let maturing = app
        .maturing_units()
        .map(fmt_units)
        .unwrap_or_else(|| "—".to_string());
    let difficulty = or_dash(status.and_then(|s| s.mining_difficulty));
    // 2^(difficulty/16) hashes per block on average -- the node's own rule.
    let expected_work = status
        .and_then(|s| s.mining_difficulty)
        .map(|d| fmt_hashes(2f64.powi((d / 16).min(255) as i32) as u64))
        .unwrap_or_else(|| "—".to_string());
    let block_in = fmt_eta(status.and_then(|s| s.mining_expected_block_secs));
    let session_hashes = status
        .and_then(|s| s.mining_hashes)
        .map(fmt_hashes)
        .unwrap_or_else(|| "—".to_string());

    let miner = column![
        row![
            kit::badge(badge, badge_colour),
            Space::new().width(Length::Fill),
            text("BACKEND").size(12).color(theme::DIM),
            text(backend_name).size(13).color(theme::TEXT),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            kit::stat("HASHRATE", hashrate, theme::VALUE, true),
            kit::stat_separator(),
            kit::stat("SESSION BLOCKS", blocks_this_session, theme::TEXT, false),
            kit::stat_separator(),
            kit::stat("BLOCK IN", block_in, theme::TEXT, false),
        ]
        .spacing(16)
        .align_y(Alignment::End),
        payout_block(app, status, payout),
        kit::kv_row("DIFFICULTY", difficulty),
        kit::kv_row("EXPECTED WORK", format!("{expected_work} hashes")),
        kit::kv_row("SESSION HASHES", session_hashes),
        kit::kv_row("WALLET MATURING", maturing),
    ]
    .spacing(12);

    let addresses: &[AddressEntry] = app
        .wallet
        .as_ref()
        .map(|w| w.addresses.as_slice())
        .unwrap_or(&[]);
    let recent = app.recent_activity(MINED_WANT);
    let devices: &[MiningDevice] = status.map(|s| s.mining_devices.as_slice()).unwrap_or(&[]);
    let mining_now = status.and_then(|s| s.mining).unwrap_or(false);

    kit::screen(
        column![
            kit::split(
                app.narrow(),
                kit::tag_panel("INTERNAL MINER", theme::ACCENT, miner),
                3,
                kit::tag_panel("MINER CONTROL", theme::CYAN, controls(app, status)),
                2,
            ),
            kit::tag_panel(
                "DEVICES",
                theme::LAVENDER,
                device_table(devices, mining_now)
            ),
            kit::tag_panel(
                "MINED TO THIS WALLET",
                theme::LAVENDER,
                mined_table(addresses, &recent, status),
            ),
        ]
        .spacing(f32::from(theme::SPACING)),
    )
}

/// PAYOUT on the miner panel: which of this wallet's addresses the node is
/// mining to, the address itself and a COPY beside it. Under a rotating
/// payout schedule, or with nothing mining, the plain sentence or dash of
/// `payout_line` instead -- there is no single address to name then.
fn payout_block<'a>(
    app: &'a App,
    status: Option<&'a NodeStatus>,
    fallback: String,
) -> Element<'a, Message> {
    let rotating = status
        .and_then(|s| s.mining_payout_rotation)
        .unwrap_or(false);
    let address = status.and_then(|s| s.mining_address.clone());
    let Some(address) = address.filter(|_| !rotating) else {
        // Stacked, not a key/value row: the rotation sentence is long.
        return field("PAYOUT", fallback);
    };
    let (tag, tag_colour) = match app.payout_label(&address) {
        Some(label) => (label, theme::LAVENDER),
        None => ("NOT IN THIS WALLET".to_string(), theme::ADVISORY),
    };
    column![
        row![kit::field_label("PAYOUT"), kit::badge(tag, tag_colour),]
            .spacing(8)
            .align_y(Alignment::Center),
        row![
            text(address.clone())
                .size(13)
                .font(theme::TECH_FONT)
                .color(theme::TEXT)
                .wrapping(Wrapping::WordOrGlyph)
                .width(Length::Fill),
            kit::action("COPY", kit::Act::Plain, Some(Message::CopyPayout(address))),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(6)
    .into()
}

/// The controls: payout address, backend, threads or GPUs, START/STOP.
/// Every edit lands in `App::settings.mining` at once; the node only sees
/// it on START, STOP or APPLY.
fn controls<'a>(app: &'a App, status: Option<&'a NodeStatus>) -> Element<'a, Message> {
    let m = &app.settings.mining;
    let gpu_built = status.map(|s| s.gpu_built).unwrap_or(true);

    let mut body = column![].spacing(f32::from(theme::SPACING));

    // Payout address: one drop-down, however many addresses the wallet has.
    body = body.push(kit::field_label("PAYOUT ADDRESS"));
    let choices = app.mining_address_choices();
    if choices.is_empty() {
        body = body.push(text("No addresses yet.").size(12).color(theme::MUTED));
    } else {
        body = body.push(
            pick_list(choices, app.mining_selected_choice(), |c| {
                Message::MiningAddressPicked(c.address)
            })
            .placeholder("Choose the address the rewards go to")
            .font(theme::TECH_FONT)
            .text_size(14)
            .padding(10)
            .width(Length::Fill)
            .style(theme::pick_list)
            .menu_style(theme::pick_list_menu),
        );
    }

    // Backend.
    body = body.push(kit::field_label("BACKEND"));
    let gpu_detail = if gpu_built {
        "wgpu: Vulkan, DX12 or Metal. Every usable GPU at once."
    } else {
        "This node was built without the GPU miner."
    };
    body = body.push(
        row![
            kit::choice_card(
                "GPU",
                gpu_detail,
                m.backend == Backend::Gpu,
                Message::MiningBackendPicked(Backend::Gpu),
            ),
            kit::choice_card(
                "CPU",
                "BLAKE3 on the CPU. Leave two cores free.",
                m.backend == Backend::Cpu,
                Message::MiningBackendPicked(Backend::Cpu),
            ),
        ]
        .spacing(f32::from(theme::SPACING)),
    );

    match m.backend {
        Backend::Cpu => {
            // A stepper: the count as typed, minus and plus beside it.
            let shown = if app.mining_threads_input.is_empty() {
                "auto".to_string()
            } else {
                app.mining_threads_input.clone()
            };
            let current = m.cpu_threads.unwrap_or(0);
            let minus =
                (current > 1).then(|| Message::MiningThreadsChanged((current - 1).to_string()));
            let plus = Some(Message::MiningThreadsChanged(
                (current.saturating_add(1)).max(1).to_string(),
            ));
            body = body.push(kit::field_label("CPU THREADS"));
            body = body.push(
                row![
                    kit::action("−", kit::Act::Plain, minus),
                    // A fixed width: "auto" and "1" differ by three glyphs,
                    // and a stepper whose "+" moves under the cursor turns a
                    // second click into a miss.
                    text(shown)
                        .size(20)
                        .font(theme::TECH_FONT)
                        .color(theme::VALUE)
                        .width(Length::Fixed(64.0))
                        .align_x(iced::alignment::Horizontal::Center),
                    kit::action("+", kit::Act::Plain, plus),
                    kit::action(
                        "AUTO",
                        kit::Act::Plain,
                        Some(Message::MiningThreadsChanged(String::new()))
                    ),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
            body = body.push(
                text("auto = every core but two, so the node keeps up with the chain.")
                    .size(11)
                    .color(theme::MUTED),
            );
        }
        Backend::Gpu => {
            body = body.push(kit::field_label("GPUS"));
            if app.known_gpus.is_empty() {
                body = body.push(
                    text("No GPU reported yet -- the node lists them once it is up.")
                        .size(12)
                        .color(theme::MUTED),
                );
            }
            for gpu in &app.known_gpus {
                let on = !m.disabled_gpus.contains(&gpu.index);
                body = body.push(
                    row![
                        text(format!("[{}] {}", gpu.index, gpu.name))
                            .size(13)
                            .color(if on { theme::TEXT } else { theme::MUTED })
                            .width(Length::Fill),
                        kit::action(
                            if on { "ON" } else { "OFF" },
                            if on { kit::Act::Look } else { kit::Act::Plain },
                            Some(Message::MiningGpuToggled(gpu.index, !on)),
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
        }
    }

    // START / STOP / APPLY.
    let mut buttons = row![].spacing(8);
    if m.enabled {
        buttons = buttons.push(kit::action(
            "STOP MINING",
            kit::Act::Care,
            Some(Message::MiningStop),
        ));
        if app.mining_dirty() {
            buttons = buttons.push(kit::action(
                "APPLY",
                kit::Act::Go,
                Some(Message::MiningStart),
            ));
        }
    } else {
        buttons = buttons.push(kit::action(
            "START MINING",
            kit::Act::Go,
            Some(Message::MiningStart),
        ));
    }
    body = body.push(buttons);
    body = body.push(
        text("Mining loads the GPU or every CPU core fully. Watch heat and power.")
            .size(11)
            .color(theme::ADVISORY),
    );
    if let Some(e) = &app.mining_error {
        body = body.push(text(e.clone()).size(12).color(theme::DANGER));
    }
    if let Some(e) = &app.settings_error {
        body = body.push(text(e.clone()).size(12).color(theme::DANGER));
    }
    body.into()
}

/// One row per device while mining. A dash for every figure the node or
/// the driver does not have.
fn device_table(devices: &[MiningDevice], mining_now: bool) -> Element<'static, Message> {
    let mut body = column![kit::table_head(&DEVICE_COLUMNS)];
    for (i, d) in devices.iter().enumerate() {
        body = body.push(kit::table_row(
            vec![
                kit::cell(d.name.clone(), DEVICE_COLUMNS[0].1, theme::TEXT),
                kit::cell(
                    fmt_device_hashrate(d.hps),
                    DEVICE_COLUMNS[1].1,
                    theme::VALUE,
                ),
                kit::cell(fmt_mhz(d.core_mhz), DEVICE_COLUMNS[2].1, theme::CYAN),
                kit::cell(fmt_mhz(d.mem_mhz), DEVICE_COLUMNS[3].1, theme::CYAN),
                kit::cell(fmt_temp(d.temp_c), DEVICE_COLUMNS[4].1, theme::ADVISORY),
                kit::cell(fmt_power(d.power_w), DEVICE_COLUMNS[5].1, theme::ADVISORY),
                kit::cell(fmt_hashes(d.hashes), DEVICE_COLUMNS[6].1, theme::MUTED),
            ],
            i % 2 == 1,
            false,
            None,
        ));
    }
    if devices.is_empty() {
        let note = if mining_now {
            "Waiting for the first dispatch…"
        } else {
            "Not mining."
        };
        body = body.push(container(text(note).size(12).color(theme::MUTED)).padding([8, 8]));
    }
    body.into()
}

/// `89 blocks`, `1 block`, `SPENDABLE`, or `—` without a tip.
fn matures_in(height: u64, tip: Option<u64>) -> (String, Color) {
    let Some(tip) = tip else {
        return ("—".to_string(), theme::DIM);
    };
    match activity::blocks_to_spendable(height, tip) {
        0 => ("SPENDABLE".to_string(), theme::ACCENT),
        1 => ("1 block".to_string(), theme::ADVISORY),
        n => (format!("{n} blocks"), theme::ADVISORY),
    }
}

fn mined_table(
    addresses: &[AddressEntry],
    recent: &Recent,
    status: Option<&NodeStatus>,
) -> Element<'static, Message> {
    let tip = status.and_then(|s| s.height);
    let mut body = column![kit::table_head(&MINED_COLUMNS)];
    let mut shown = 0;
    for entry in recent.rows.iter().filter(|r| r.kind == RowKind::Mining) {
        let index = addresses
            .iter()
            .find(|a| a.address == entry.owner)
            .map(|a| format!("[{}]", a.label()))
            .unwrap_or_else(|| "?".to_string());
        let (left, left_colour) = matures_in(entry.height, tip);
        body = body.push(kit::table_row(
            vec![
                kit::cell(entry.height.to_string(), MINED_COLUMNS[0].1, theme::CYAN),
                kit::cell(index, MINED_COLUMNS[1].1, theme::LAVENDER),
                kit::cell(
                    format!("+{}", model::format_coins(entry.amount_units)),
                    MINED_COLUMNS[2].1,
                    theme::ACCENT,
                ),
                kit::cell(left, MINED_COLUMNS[3].1, left_colour),
            ],
            shown % 2 == 1,
            false,
            None,
        ));
        shown += 1;
    }
    // The newest MINED_WANT rows, not the whole history -- see receive.rs's
    // incoming table for why an empty filter must say how far it looked.
    let coverage = recent.coverage(status);
    if shown == 0 {
        body = body.push(
            container(
                text(kit::filtered_empty(
                    "No mining rewards",
                    "No mining rewards have reached this wallet's addresses.",
                    coverage,
                    MINED_WANT,
                ))
                .size(12)
                .color(theme::MUTED)
                .wrapping(Wrapping::WordOrGlyph),
            )
            .padding([8, 8]),
        );
    }
    body = body.push(kit::table_foot(
        text(kit::filtered_foot("REWARDS", shown, coverage, MINED_WANT))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 회전 중일 때 주소 한 줄만 보여주면 거짓말이 된다. 회전 여부에 따라
    /// 문구가 달라지는지만 순수 함수로 검사한다.
    #[test]
    fn a_rotating_payout_says_so_instead_of_naming_one_address() {
        let rotating = payout_line(Some("6aea8e1e".into()), true);
        assert!(rotating.contains("rotates"));
        // The claim is "never names an address" -- not just "says rotates
        // somewhere alongside one". Pin the address's absence, not only the
        // word's presence.
        assert!(!rotating.contains("6aea8e1e"));
        assert!(payout_line(Some("6aea8e1e".into()), false).contains("6aea8e1e"));
        assert_eq!(payout_line(None, false), "—");
    }

    /// 회전 중이면 주소가 없어도(`mining_address` 자체가 `None`이어도) 여전히
    /// 회전 사실을 말해야 한다 -- 주소가 없다고 그냥 대시로 접으면 "회전
    /// 중"이라는, 아는 사실을 숨기게 된다.
    #[test]
    fn a_rotating_payout_says_so_even_with_no_address() {
        assert!(payout_line(None, true).contains("rotates"));
    }

    #[test]
    fn device_cells_read_as_units_or_a_dash() {
        assert_eq!(fmt_mhz(Some(2565)), "2565 MHz");
        assert_eq!(fmt_mhz(None), "—");
        assert_eq!(fmt_temp(Some(53)), "53 °C");
        assert_eq!(fmt_temp(None), "—");
        assert_eq!(fmt_power(Some(103.87)), "104 W");
        assert_eq!(fmt_power(None), "—");
        assert_eq!(fmt_hashes(4_000_000_000), "4.00 G");
        assert_eq!(fmt_hashes(12_300_000), "12.3 M");
        assert_eq!(fmt_hashes(950), "950");
        assert_eq!(fmt_hashes(1_500_000_000_000), "1.50 T");
        assert_eq!(fmt_eta(Some(12.0)), "~12s");
        // Minutes up to 90 of them, the same cut the node's own progress
        // bar makes (`gpu_miner::format_eta`), so the two never disagree.
        assert_eq!(fmt_eta(Some(4500.0)), "~75m");
        assert_eq!(fmt_eta(Some(7200.0)), "~2.0h");
        assert_eq!(fmt_eta(Some(200_000.0)), "~2.3d");
        assert_eq!(fmt_eta(None), "—");
        assert_eq!(fmt_eta(Some(f64::INFINITY)), "—");
    }

    /// A hashrate below a GH/s must not read "0.0 GH/s": the CPU miner
    /// runs at tens of MH/s.
    #[test]
    fn a_device_hashrate_picks_its_unit() {
        assert_eq!(fmt_device_hashrate(2.65e9), "2.65 GH/s");
        assert_eq!(fmt_device_hashrate(31_683_636.0), "31.7 MH/s");
        assert_eq!(fmt_device_hashrate(0.0), "—");
    }

    /// The device table at both widths, full-width panel.
    #[test]
    fn realistic_device_rows_fit_at_both_widths() {
        for window in [1000.0, 760.0] {
            kit::sizing::assert_fits(
                &DEVICE_COLUMNS,
                &[
                    "NVIDIA GeForce RTX 5090",
                    "17.7 GH/s",
                    "2790 MHz",
                    "14001 MHz",
                    "87 °C",
                    "600 W",
                    "1.58 T",
                ],
                kit::sizing::row_width(window, 1, 1.0),
            );
        }
    }

    /// Fits on one line at both widths: the table has the room.
    #[test]
    fn realistic_figures_fit_the_mined_table_at_both_widths() {
        for window in [1000.0, 760.0] {
            kit::sizing::assert_fits(
                &MINED_COLUMNS,
                &["1015860", "[12]", "+1.23456789", "89 blocks"],
                kit::sizing::row_width(window, 2, 3.0 / 5.0),
            );
        }
    }

    /// The node's rule, not ours: spendable once the tip reaches height + 99
    /// (`activity::blocks_to_spendable`). One block left must not read "1 blocks".
    #[test]
    fn matures_in_counts_down_to_spendable() {
        assert_eq!(matures_in(1_000_000, Some(1_000_010)).0, "89 blocks");
        assert_eq!(matures_in(1_000_000, Some(1_000_098)).0, "1 block");
        assert_eq!(matures_in(1_000_000, Some(1_000_099)).0, "SPENDABLE");
        assert_eq!(matures_in(1_000_000, None).0, "—");
    }
}
