//! The top two layers: the status strip and a 3-row by 4-column metrics grid.
//!
//! An absent value is `—`, not 0 -- on this screen "0 peers" means isolated,
//! a completely different story from "unknown".

use iced::widget::{column, container, row, text};
use iced::{Element, Length};

use crate::app::Message;
use crate::theme;
use crate::view::kit;
use alphanumeric_gui::backend;

/// Amount units (i128) as a coin string with its unit, `1.5 ALPHA`. Delegates
/// to the one formatter this codebase already has (`model::format_coins`)
/// so an amount never reads differently on two screens. The mining screen's
/// WALLET MATURING (`view/mining.rs`) is where it is used.
pub(crate) fn fmt_units(units: i128) -> String {
    format!("{} ALPHA", alphanumeric_gui::model::format_coins(units))
}

pub fn or_dash<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "—".to_string())
}

/// `VmRSS`, reported in KiB by `/proc`, as a human-scaled string.
///
/// `pub(crate)`: the node screen (`view/node.rs`) shows the same owned
/// process's memory this grid does.
pub(crate) fn fmt_kib(kib: u64) -> String {
    let mib = kib as f64 / 1024.0;
    if mib >= 1024.0 {
        format!("{:.2} GiB", mib / 1024.0)
    } else {
        format!("{mib:.1} MiB")
    }
}

/// A byte count as a human-scaled string.
///
/// `pub(crate)`: the node screen (`view/node.rs`) shows the same data
/// directory size this grid does.
pub(crate) fn fmt_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub struct ConsoleData<'a> {
    pub status: Option<&'a backend::NodeStatus>,
    pub stats: Option<&'a backend::NodeStats>,
    pub supply_units: Option<i128>,
    pub maturing_units: Option<i128>,
    /// How busy the node is against ONE core, 0..=1 (`proc::core_share`).
    /// `None` in External mode and whenever the process is not `Running`.
    pub cpu_share: Option<f32>,
    /// How far the memory bar is drawn: RSS against `proc::RSS_FULL_KIB`,
    /// 0..=1. Same `None` cases. The figure beside it comes from `rss_kib`.
    pub mem_share: Option<f32>,
    /// The node's resident memory, for the figure beside the memory bar. A
    /// percentage there said nothing a person could act on.
    pub rss_kib: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub mining: Option<(bool, Option<f64>, Option<String>)>,
    /// The wallet reads a node it does not run. See `grid_note`.
    pub external: bool,
}

/// `1 015 872` -- noid's grouping (a space, not a comma).
pub(crate) fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(digit);
    }
    out
}

/// Circulating supply for the grid: millions to two places, truncated
/// (`10.04M`); below a million, whole coins grouped. No unit (plan ruling 2).
pub(crate) fn fmt_supply_compact(units: i128) -> String {
    let coins = units / alphanumeric_gui::tx::MONEY_SCALE;
    if coins >= 1_000_000 {
        let hundredths = coins / 10_000;
        format!("{}.{:02}M", hundredths / 100, hundredths % 100)
    } else {
        group_digits(u64::try_from(coins.max(0)).unwrap_or(0))
    }
}

/// A coin amount cut to four places (truncated), trailing zeros trimmed.
pub(crate) fn fmt_coins_short(units: i128) -> String {
    alphanumeric_gui::model::format_coins(units - units % 10_000)
}

/// Local height over network height, 0..=1. `None` unless both are known.
pub(crate) fn sync_ratio(height: Option<u64>, network: Option<u64>) -> Option<f32> {
    match (height, network) {
        (Some(h), Some(n)) if n > 0 => Some((h as f64 / n as f64).min(1.0) as f32),
        _ => None,
    }
}

/// In External mode, half the grid is permanently `—`: there's no process
/// to measure CPU/MEMORY/DISK, and PEERS, HASHRATE, MEMPOOL, AVG BLOCK,
/// DIFFICULTY, and BLOCK REWARD come from the node's `/stats` port, which
/// someone else's node won't hand out. The dash itself is honest, but
/// without a reason it reads as "broken" -- so we say so in one line. In
/// Owned mode the dash means "not known yet" and fills in soon, so it goes
/// unexplained.
fn grid_note(external: bool) -> Option<&'static str> {
    external.then_some(
        "External node: the process figures and the node's own /stats figures \
         are read only from a node this wallet runs, so those cells stay —.",
    )
}

/// Only the sync state gets a color -- everything else is a plain fact
/// that needs no emphasis. Not having seen a beacon yet (`None`) is a
/// different fact from "caught up", so it must not collapse into SYNCED,
/// or into its color.
fn sync_label(behind: Option<u64>) -> (String, iced::Color) {
    match behind {
        Some(n) if n <= alphanumeric_gui::startup::SYNCED_SLACK => {
            ("SYNCED".to_string(), theme::ACCENT)
        }
        Some(n) => (format!("BEHIND {n}"), theme::ADVISORY),
        None => ("SYNC —".to_string(), theme::ADVISORY),
    }
}

/// A hashrate reported in H/s (the wire unit) for the header's `MINING …`
/// chip, in the unit it deserves: GH/s for a GPU, MH/s for a CPU session
/// (which read "0.0 GH/s" before), kH/s below that.
pub(crate) fn fmt_hashrate(hps: f64) -> String {
    if hps >= 1e9 {
        format!("{:.1} GH/s", hps / 1e9)
    } else if hps >= 1e6 {
        format!("{:.1} MH/s", hps / 1e6)
    } else {
        format!("{:.0} kH/s", hps / 1e3)
    }
}

/// When the node isn't mining, it omits every `mining_*` field entirely
/// (`mining` itself included). "Not mining" (`Some(false, ..)`) and "never
/// asked" (`None`) are different facts, so neither should collapse into
/// "MINING OFF".
fn mining_label(mining: &Option<(bool, Option<f64>, Option<String>)>) -> (String, iced::Color) {
    match mining {
        Some((true, hps, _)) => (
            match hps {
                Some(h) => format!("MINING {}", fmt_hashrate(*h)),
                None => "MINING ON".to_string(),
            },
            theme::ACCENT,
        ),
        Some((false, _, _)) => ("MINING OFF".to_string(), theme::MUTED),
        None => ("MINING —".to_string(), theme::MUTED),
    }
}

/// The gap between two heights -- FINALIZED (tip vs. the finality checkpoint)
/// and INDEX (tip vs. the address index) are the same computation on two
/// different pairs: `None` unless both are known, otherwise the tip's lead
/// over the other, floored at 0 so a checkpoint or index that is briefly
/// AHEAD of a `height` read taken a moment earlier cannot read as negative.
fn gap(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_sub(b)),
        _ => None,
    }
}

/// Index lag: 0 reads as the word "current", not "-0"; unknown is a dash.
fn lag_label(lag: Option<u64>) -> String {
    lag.map(|n| {
        if n == 0 {
            "current".to_string()
        } else {
            format!("−{n}")
        }
    })
    .unwrap_or_else(|| "—".to_string())
}

/// Identity and two chip capsules: `● SYNCED │ PEERS │ HEIGHT` and
/// `VERSION │ NODE │ ● MINING`. noid's `header`.
pub fn header_bar<'a>(data: &ConsoleData<'a>) -> Element<'a, Message> {
    let st = data.status;
    let sx = data.stats;
    let (sync_text, sync_colour) = sync_label(st.and_then(|s| s.blocks_behind));
    let (mining_text, mining_colour) = mining_label(&data.mining);
    kit::header(
        vec![
            kit::Chip::Live(sync_text, sync_colour),
            kit::Chip::Value("PEERS", or_dash(sx.and_then(|s| s.peers))),
            kit::Chip::Value(
                "HEIGHT",
                st.and_then(|s| s.height)
                    .map(group_digits)
                    .unwrap_or_else(|| "—".into()),
            ),
        ],
        vec![
            kit::Chip::Value(
                "VERSION",
                st.map(|s| s.version.clone()).unwrap_or_else(|| "—".into()),
            ),
            kit::Chip::Value(
                "NODE",
                if data.external { "EXTERNAL" } else { "OWNED" }.to_string(),
            ),
            kit::Chip::Live(mining_text, mining_colour),
        ],
    )
}

/// Four columns of three: chain, this machine, economy, network (spec §4.1).
pub fn meter_grid<'a>(data: &ConsoleData<'a>) -> Element<'a, Message> {
    let st = data.status;
    let sx = data.stats;
    let dash = || "—".to_string();
    let pct = |r: Option<f32>| r.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(dash);
    let sync = st.and_then(|s| sync_ratio(s.height, s.network_height));
    let finalized = st.and_then(|s| gap(s.height, s.finalized_height));
    let index_lag = st.and_then(|s| gap(s.height, s.index_height));

    let chain = column![
        kit::meter("SYNC", sync, theme::ACCENT, pct(sync)),
        kit::telemetry(
            "FINALIZED",
            finalized.map(|n| format!("tip −{n}")).unwrap_or_else(dash),
            theme::ACCENT,
        ),
        kit::telemetry("INDEX", lag_label(index_lag), theme::ACCENT),
    ]
    .spacing(5);
    let machine = column![
        kit::meter("CPU", data.cpu_share, theme::ACCENT, pct(data.cpu_share)),
        kit::meter(
            "MEMORY",
            data.mem_share,
            theme::WARNING,
            data.rss_kib.map(fmt_kib).unwrap_or_else(dash),
        ),
        kit::telemetry(
            "DISK",
            data.disk_bytes.map(fmt_bytes).unwrap_or_else(dash),
            theme::ACCENT,
        ),
    ]
    .spacing(5);
    let economy = column![
        kit::telemetry(
            "CIRC SUPPLY",
            data.supply_units
                .map(fmt_supply_compact)
                .unwrap_or_else(dash),
            theme::ACCENT,
        ),
        kit::telemetry(
            "BLOCK REWARD",
            sx.and_then(|s| s.block_reward)
                .map(|r| format!("{r:.2}"))
                .unwrap_or_else(dash),
            theme::ACCENT,
        ),
        kit::telemetry(
            "MATURING",
            data.maturing_units
                .map(fmt_coins_short)
                .unwrap_or_else(dash),
            theme::ADVISORY,
        ),
    ]
    .spacing(5);
    let network = column![
        kit::telemetry(
            "NETWORK",
            sx.and_then(|s| s.hashrate_ths)
                .map(|h| format!("{h:.2} TH/s"))
                .unwrap_or_else(dash),
            theme::ACCENT,
        ),
        kit::telemetry(
            "AVG BLOCK TIME",
            sx.and_then(|s| s.avg_block_time_secs)
                .map(|t| format!("{t:.1}s"))
                .unwrap_or_else(dash),
            theme::ACCENT,
        ),
        kit::telemetry(
            "DIFFICULTY",
            sx.and_then(|s| s.difficulty)
                .map(|d| format!("{d:.0}"))
                .unwrap_or_else(dash),
            theme::WARNING,
        ),
    ]
    .spacing(5);

    let mut body = column![row![
        container(chain).width(Length::FillPortion(1)),
        container(machine).width(Length::FillPortion(1)),
        container(economy).width(Length::FillPortion(1)),
        container(network).width(Length::FillPortion(1)),
    ]
    .spacing(14)]
    .spacing(6);
    if let Some(note) = grid_note(data.external) {
        body = body.push(text(note).size(12).color(theme::MUTED));
    }
    container(body)
        .padding([8, 12])
        .width(Length::Fill)
        .style(theme::status_panel)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A CPU session runs at tens of MH/s; "MINING 0.0 GH/s" reads as broken.
    #[test]
    fn the_header_hashrate_picks_its_unit() {
        assert_eq!(fmt_hashrate(2.5e9), "2.5 GH/s");
        assert_eq!(fmt_hashrate(18_100_000.0), "18.1 MH/s");
        assert_eq!(fmt_hashrate(950_000.0), "950 kH/s");
    }

    #[test]
    fn fmt_units_reuses_the_shared_coin_formatter() {
        assert_eq!(fmt_units(150_000_000), "1.5 ALPHA");
        assert_eq!(fmt_units(0), "0 ALPHA");
    }

    /// An absent value is `—`, not 0. Drawing it as 0 would lie: "0 peers"
    /// (isolated), "0 hashrate" (nobody is mining).
    #[test]
    fn an_absent_value_is_a_dash_not_a_zero() {
        assert_eq!(or_dash(None::<u64>), "—");
        assert_eq!(or_dash(Some(0u64)), "0");
        assert_eq!(or_dash(Some(11u64)), "11");
    }

    /// External's permanently empty cells carry a reason; Owned's dash
    /// means "will fill in soon", so it carries no explanation -- one
    /// would be a lie at every moment during startup.
    #[test]
    fn only_an_external_node_explains_its_empty_cells() {
        assert_eq!(grid_note(false), None);
        let note = grid_note(true).expect("external explains itself");
        assert!(
            note.contains("/stats"),
            "names where the missing figures come from"
        );
    }

    /// Grid values carry no unit (plan ruling 2): at the 760 px minimum a
    /// column is ~165 px, and `CIRC SUPPLY [10.04M ALPHA]` needs ~204.
    /// Truncated, never rounded up -- the grid must not show more coins than
    /// exist.
    #[test]
    fn supply_in_the_grid_is_millions_truncated_to_two_places() {
        assert_eq!(fmt_supply_compact(1_004_867_253_439_378), "10.04M");
        assert_eq!(fmt_supply_compact(141_496_332_920_427), "1.41M");
        assert_eq!(fmt_supply_compact(100_000_000 * 999), "999");
        assert_eq!(fmt_supply_compact(99_999_999), "0");
    }

    /// MATURING in the grid is a summary; the exact figure is on F1.
    #[test]
    fn short_coins_keep_four_places_truncated() {
        assert_eq!(fmt_coins_short(12_345_678_901), "123.4567");
        assert_eq!(fmt_coins_short(90_000_000), "0.9");
        assert_eq!(fmt_coins_short(0), "0");
    }

    #[test]
    fn heights_are_grouped_like_noid_does() {
        assert_eq!(group_digits(1_015_872), "1 015 872");
        assert_eq!(group_digits(80_295), "80 295");
        assert_eq!(group_digits(7), "7");
    }

    #[test]
    fn the_sync_gauge_needs_both_heights_and_never_passes_full() {
        assert_eq!(sync_ratio(Some(50), Some(100)), Some(0.5));
        assert_eq!(sync_ratio(Some(120), Some(100)), Some(1.0));
        assert_eq!(sync_ratio(None, Some(100)), None);
        assert_eq!(sync_ratio(Some(5), None), None);
        assert_eq!(sync_ratio(Some(5), Some(0)), None);
    }

    #[test]
    fn fmt_kib_scales_to_mib_and_gib() {
        assert_eq!(fmt_kib(512), "0.5 MiB");
        assert_eq!(fmt_kib(1024 * 1024), "1.00 GiB");
    }

    #[test]
    fn fmt_bytes_scales_from_bytes_up() {
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
    }

    /// No beacon seen yet is not the same fact as "caught up" -- it must not
    /// render as SYNCED, and it must not borrow SYNCED's colour either.
    #[test]
    fn an_unknown_sync_state_is_not_shown_as_synced() {
        let (text, colour) = sync_label(None);
        assert_eq!(text, "SYNC —");
        assert_ne!(colour, theme::ACCENT);
    }

    #[test]
    fn within_slack_reads_synced() {
        let (text, colour) = sync_label(Some(alphanumeric_gui::startup::SYNCED_SLACK));
        assert_eq!(text, "SYNCED");
        assert_eq!(colour, theme::ACCENT);
    }

    #[test]
    fn past_slack_reads_behind_with_the_count() {
        let (text, colour) = sync_label(Some(alphanumeric_gui::startup::SYNCED_SLACK + 1));
        assert_eq!(
            text,
            format!("BEHIND {}", alphanumeric_gui::startup::SYNCED_SLACK + 1)
        );
        assert_eq!(colour, theme::ADVISORY);
    }

    /// The node omits every `mining_*` field (and `mining` itself) when it is
    /// not mining -- but that is a different fact from "we asked and it said
    /// no". `None` must not collapse into "MINING OFF".
    #[test]
    fn no_mining_status_yet_is_not_shown_as_mining_off() {
        let (text, colour) = mining_label(&None);
        assert_eq!(text, "MINING —");
        assert_ne!(colour, theme::ACCENT);
    }

    #[test]
    fn mining_off_is_distinct_from_unknown() {
        let (text, _) = mining_label(&Some((false, None, None)));
        assert_eq!(text, "MINING OFF");
    }

    #[test]
    fn mining_with_a_known_rate_shows_it_in_gh_per_s() {
        let (text, colour) = mining_label(&Some((true, Some(2.5e9), None)));
        assert_eq!(text, "MINING 2.5 GH/s");
        assert_eq!(colour, theme::ACCENT);
    }

    #[test]
    fn mining_with_an_unknown_rate_still_says_on() {
        let (text, _) = mining_label(&Some((true, None, None)));
        assert_eq!(text, "MINING ON");
    }

    /// An index lag of exactly 0 reads as "current" prose, not "−0" -- but an
    /// *unknown* lag must not fall into either branch.
    #[test]
    fn lag_label_distinguishes_current_behind_and_unknown() {
        assert_eq!(lag_label(Some(0)), "current");
        assert_eq!(lag_label(Some(3)), "−3");
        assert_eq!(lag_label(None), "—");
    }

    /// FINALIZED and INDEX are the same gap computation on two different
    /// pairs -- one function, tested once, rather than the same match
    /// duplicated (and untested) at each call site.
    #[test]
    fn gap_is_the_lead_of_a_over_b_when_both_are_known() {
        assert_eq!(gap(Some(100), Some(97)), Some(3));
    }

    #[test]
    fn gap_is_none_when_either_side_is_unknown() {
        assert_eq!(gap(None, Some(97)), None);
        assert_eq!(gap(Some(100), None), None);
        assert_eq!(gap(None, None), None);
    }

    /// `b` briefly ahead of `a` (a checkpoint or index read a moment after
    /// `height`) must read as caught up, not wrap around to a huge number.
    #[test]
    fn gap_floors_at_zero_rather_than_wrapping() {
        assert_eq!(gap(Some(5), Some(9)), Some(0));
    }
}
