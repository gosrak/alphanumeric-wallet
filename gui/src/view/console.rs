//! 맨 위 두 층: 상태 띠와 3줄 × 4칸 지표 격자.
//!
//! 값이 없으면 `—` 다. 0 이 아니다 -- 이 화면에서 "피어 0" 은 고립됐다는
//! 뜻이고 "모른다" 와 전혀 다른 이야기다.

use iced::widget::{column, container, row, text};
use iced::{Element, Length};

use crate::app::Message;
use crate::theme;
use crate::view::kit;
use alphanumeric_gui::backend;

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

/// External 모드에서는 격자의 절반이 영구히 `—` 다: CPU·MEMORY·DISK 는 잴
/// 프로세스가 없고, PEERS·HASHRATE·MEMPOOL·AVG BLOCK·DIFFICULTY·BLOCK REWARD
/// 는 노드의 `/stats` 포트에서 오는데 남의 노드는 그 포트를 알려주지 않는다.
/// 대시 자체는 정직하지만 이유가 없으면 "고장났다"로 읽힌다 -- 한 줄로 말한다.
/// Owned 에서 대시는 "아직 모른다"이고 곧 채워지므로 설명하지 않는다.
fn grid_note(external: bool) -> Option<&'static str> {
    external.then_some(
        "External node: the process figures and the node's own /stats figures \
         are read only from a node this wallet runs, so those cells stay —.",
    )
}

/// 동기화 여부만 색을 갖는다 -- 나머지는 사실이라 강조가 필요 없다. 비콘을
/// 아직 못 본 것(`None`)은 "따라잡았다"와 다른 사실이므로 SYNCED 로도, 그
/// 색으로도 접히면 안 된다.
fn sync_label(behind: Option<u64>) -> (String, iced::Color) {
    match behind {
        Some(n) if n <= alphanumeric_gui::startup::SYNCED_SLACK => {
            ("SYNCED".to_string(), theme::ACCENT)
        }
        Some(n) => (format!("BEHIND {n}"), theme::ADVISORY),
        None => ("SYNC —".to_string(), theme::ADVISORY),
    }
}

/// A hashrate reported in H/s (the wire unit) as GH/s, for the header's
/// `MINING … GH/s` chip.
pub(crate) fn fmt_hashrate(hps: f64) -> String {
    format!("{:.1} GH/s", hps / 1e9)
}

/// 노드는 채굴 중이 아니면 `mining_*` 필드 전부(그리고 `mining` 자신)를 아예
/// 생략한다. "안 캔다"(`Some(false, ..)`)와 "물어본 적 없다"(`None`)는 다른
/// 사실이라 둘 다 "MINING OFF" 로 접으면 안 된다.
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

/// 색인 지연: 0 은 "current" 라는 문장이지 "−0" 이 아니고, 모르면 대시다.
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

    /// 없는 값은 0 이 아니라 `—` 다. 0 으로 그리면 "피어 0"(고립됐다),
    /// "해시레이트 0"(아무도 안 캔다) 같은 거짓말을 하게 된다.
    #[test]
    fn an_absent_value_is_a_dash_not_a_zero() {
        assert_eq!(or_dash(None::<u64>), "—");
        assert_eq!(or_dash(Some(0u64)), "0");
        assert_eq!(or_dash(Some(11u64)), "11");
    }

    /// External 에서 영구히 빈 칸들은 이유를 달고, Owned 의 대시는 "곧
    /// 채워진다"이므로 설명을 달지 않는다 -- 달면 모든 기동 순간에 거짓이다.
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
