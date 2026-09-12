//! How far along the node is when the wallet is opened.
//!
//! Pure -- no `iced`, no I/O. `app.rs` fetches the state and the log; this
//! module only turns that into a phase.
//!
//! A measurement (2026-09-10, spec §2.5) settled this module's shape: there
//! are two phases. `index_ready` was true from the first response, on both a
//! fresh install and a restart, so indexing is not a phase the user waits
//! on -- it's checked, but not set up as its own step.

use crate::backend::NodeStatus;
use crate::node::NodeState;

/// Lagging by this much or less counts as "synced".
///
/// Not 0, and that's from measurement: in a healthy steady state,
/// `blocks_behind` keeps swinging between 0 and 4 (a new block arrives, gets
/// absorbed). Using 0 as the line would leave the screen and the banner
/// flickering forever.
pub const SYNCED_SLACK: u64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The node is coming up. The explorer isn't answering yet. On a fresh
    /// install, the snapshot download falls inside this span, and that part
    /// alone depends on the connection.
    Starting {
        last_line: Option<String>,
        percent: Option<u8>,
    },
    CatchingUp {
        height: u64,
        network_height: u64,
        remaining: u64,
    },
    Ready,
    Failed {
        message: String,
    },
}

/// `node` is `None` when we do not own this node -- no process to observe,
/// so the status is the only truth. That arm falls through to the same
/// status-driven judgment as `Some(NodeState::Running { .. })`.
pub fn phase(node: Option<&NodeState>, status: Option<&NodeStatus>, log_tail: &[String]) -> Phase {
    match node {
        Some(NodeState::Failed { message }) => {
            return Phase::Failed {
                message: message.clone(),
            }
        }
        Some(NodeState::Exited { code }) => {
            let tail = log_tail.join("\n");
            let code = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "a signal".to_string());
            return Phase::Failed {
                message: format!("The node stopped ({code}).\n{tail}"),
            };
        }
        Some(NodeState::Stopped) => {
            return Phase::Failed {
                message: "The node was stopped.".to_string(),
            }
        }
        Some(NodeState::Starting) => return starting(log_tail),
        Some(NodeState::Running { .. }) | None => {}
    }

    // All three have to be present to judge anything. A missing
    // `network_height` means "no beacon seen yet", not "caught up".
    let Some(status) = status else {
        return starting(log_tail);
    };
    let (Some(height), Some(network_height), Some(behind)) =
        (status.height, status.network_height, status.blocks_behind)
    else {
        return starting(log_tail);
    };

    if behind > SYNCED_SLACK {
        return Phase::CatchingUp {
            height,
            network_height,
            remaining: behind,
        };
    }
    if !status.index_ready {
        // Always true in measurement. If false, that's an anomaly rather
        // than catching up, so it isn't let in as `Ready`.
        return Phase::CatchingUp {
            height,
            network_height,
            remaining: 0,
        };
    }
    Phase::Ready
}

fn starting(log_tail: &[String]) -> Phase {
    let last_line = log_tail
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .cloned();
    let percent = last_line.as_deref().and_then(parse_percent);
    Phase::Starting { last_line, percent }
}

/// Finds `NN%` in a line. **Decoration only** -- the screen must still be
/// correct without it, and if correctness depended on this, the screen would
/// quietly start lying the moment the node changes its wording.
pub fn parse_percent(line: &str) -> Option<u8> {
    let idx = line.find('%')?;
    let digits: String = line[..idx]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse::<u16>()
        .ok()
        .filter(|v| *v <= 100)
        .map(|v| v as u8)
}

/// Recent `(seconds, height)` readings while the node catches up, for the
/// startup screen's speed and time-left line.
#[derive(Debug, Clone, Default)]
pub struct SyncRate {
    samples: std::collections::VecDeque<(f64, u64)>,
    /// The first height seen since the last `clear` -- where this catch-up
    /// began. Not windowed: progress is about the whole catch-up.
    start: Option<u64>,
}

/// Samples older than this are dropped: the speed is about now.
const SYNC_WINDOW_SECS: f64 = 90.0;
/// Below this span the rate is noise -- the node takes blocks in bursts.
const SYNC_MIN_SPAN_SECS: f64 = 20.0;

impl SyncRate {
    pub fn push(&mut self, at_secs: f64, height: u64) {
        if let Some(&(_, last)) = self.samples.back() {
            if height < last {
                self.clear();
            }
        }
        if self.start.is_none() {
            self.start = Some(height);
        }
        self.samples.push_back((at_secs, height));
        while let Some(&(t0, _)) = self.samples.front() {
            if at_secs - t0 > SYNC_WINDOW_SECS {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn blocks_per_sec(&self) -> Option<f64> {
        let (&(t0, h0), &(t1, h1)) = (self.samples.front()?, self.samples.back()?);
        let span = t1 - t0;
        if span < SYNC_MIN_SPAN_SECS || h1 <= h0 {
            return None;
        }
        Some((h1 - h0) as f64 / span)
    }

    pub fn eta_secs(&self, remaining: u64) -> Option<u64> {
        let rate = self.blocks_per_sec()?;
        Some((remaining as f64 / rate).ceil() as u64)
    }

    /// How far this catch-up has come, 0..=1: from its start height to the
    /// network height.
    pub fn progress(&self, height: u64, network: u64) -> Option<f32> {
        let start = self.start?;
        if network <= start {
            return None;
        }
        Some((height.saturating_sub(start) as f32 / (network - start) as f32).clamp(0.0, 1.0))
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.start = None;
    }
}

/// "about 40 s", "about 25 min", "about 2 h 10 min".
pub fn fmt_eta(secs: u64) -> String {
    if secs < 60 {
        format!("about {secs} s")
    } else if secs < 3_600 {
        format!("about {} min", secs.div_ceil(60))
    } else {
        format!("about {} h {} min", secs / 3_600, (secs % 3_600) / 60)
    }
}

/// The line under the catch-up progress bar.
pub fn catch_up_line(remaining: u64, rate: Option<f64>, eta: Option<u64>) -> String {
    match (rate, eta) {
        (Some(rate), Some(eta)) => {
            format!(
                "{remaining} blocks behind · ~{rate:.1} blocks/s · {}",
                fmt_eta(eta)
            )
        }
        _ => format!("{remaining} blocks behind · measuring speed…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(height: Option<u64>, network: Option<u64>, behind: Option<u64>) -> NodeStatus {
        NodeStatus {
            height,
            network_height: network,
            blocks_behind: behind,
            index_ready: true,
            index_height: height,
            version: "8.0.0".into(),
            finalized_height: None,
            mining: None,
            mining_address: None,
            mining_backend: None,
            mining_hps: None,
            mining_blocks: None,
            mining_payout_rotation: None,
        }
    }

    #[test]
    fn before_the_explorer_answers_it_is_starting() {
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), None, &[]),
            Phase::Starting {
                last_line: None,
                percent: None
            }
        );
    }

    #[test]
    fn while_starting_the_last_log_line_is_carried_verbatim() {
        let log = vec!["Loading wallets...".to_string()];
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), None, &log),
            Phase::Starting {
                last_line: Some("Loading wallets...".to_string()),
                percent: None
            }
        );
    }

    #[test]
    fn a_percentage_in_the_line_is_picked_up_as_decoration() {
        let log = vec!["Bootstrap download 40% (120 / 300 MB)".to_string()];
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), None, &log),
            Phase::Starting {
                last_line: Some("Bootstrap download 40% (120 / 300 MB)".to_string()),
                percent: Some(40)
            }
        );
    }

    /// Correctness isn't staked on a regex. The screen must stay right even
    /// if the node changes its wording.
    #[test]
    fn a_line_without_a_percentage_still_produces_a_correct_phase() {
        let log = vec!["something nobody planned for".to_string()];
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), None, &log),
            Phase::Starting {
                last_line: Some("something nobody planned for".to_string()),
                percent: None
            }
        );
    }

    #[test]
    fn a_nonsense_percentage_is_ignored_rather_than_clamped_wrongly() {
        assert_eq!(parse_percent("done 1000% of it"), None);
        assert_eq!(parse_percent("100% there"), Some(100));
        assert_eq!(parse_percent("no digits here"), None);
    }

    /// Before a beacon, `network_height` is null. That means "not known
    /// yet", not "caught up", so this must not fall through to `Ready`.
    #[test]
    fn a_status_without_a_beacon_is_still_starting() {
        let s = status(Some(10), None, None);
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), Some(&s), &[]),
            Phase::Starting {
                last_line: None,
                percent: None
            }
        );
    }

    #[test]
    fn a_node_that_is_far_behind_is_catching_up() {
        let s = status(Some(997_175), Some(997_498), Some(323));
        assert_eq!(
            phase(Some(&NodeState::Running { pid: 1 }), Some(&s), &[]),
            Phase::CatchingUp {
                height: 997_175,
                network_height: 997_498,
                remaining: 323
            }
        );
    }

    /// Measured: in a healthy steady state, `blocks_behind` keeps swinging
    /// between 0 and 4. Using 0 as the line would flicker the screen
    /// forever.
    #[test]
    fn a_small_gap_counts_as_ready() {
        for behind in 0..=SYNCED_SLACK {
            let s = status(Some(100), Some(100 + behind), Some(behind));
            assert_eq!(
                phase(Some(&NodeState::Running { pid: 1 }), Some(&s), &[]),
                Phase::Ready,
                "blocks_behind {behind} should read as in sync"
            );
        }
    }

    #[test]
    fn one_block_past_the_slack_is_catching_up() {
        let behind = SYNCED_SLACK + 1;
        let s = status(Some(100), Some(100 + behind), Some(behind));
        assert!(matches!(
            phase(Some(&NodeState::Running { pid: 1 }), Some(&s), &[]),
            Phase::CatchingUp { .. }
        ));
    }

    /// Without the index, neither balance nor history can be trusted. It was
    /// always true in measurement, so false is an anomaly, not catching up.
    #[test]
    fn an_unready_index_is_not_ready_even_when_the_height_is_caught_up() {
        let mut s = status(Some(100), Some(100), Some(0));
        s.index_ready = false;
        assert!(!matches!(
            phase(Some(&NodeState::Running { pid: 1 }), Some(&s), &[]),
            Phase::Ready
        ));
    }

    #[test]
    fn a_failed_node_carries_its_message() {
        let node = NodeState::Failed {
            message: "no binary".into(),
        };
        assert_eq!(
            phase(Some(&node), None, &[]),
            Phase::Failed {
                message: "no binary".into()
            }
        );
    }

    /// A port conflict arrives in this shape. The log tail has to be
    /// attached for the user to know why.
    #[test]
    fn a_node_that_exited_reports_it_with_the_log_tail() {
        let node = NodeState::Exited { code: Some(1) };
        let log = vec!["Address already in use".to_string()];
        match phase(Some(&node), None, &log) {
            Phase::Failed { message } => {
                assert!(message.contains("Address already in use"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_starting_node_is_starting_even_with_a_stale_status() {
        let s = status(Some(100), Some(100), Some(0));
        assert!(matches!(
            phase(Some(&NodeState::Starting), Some(&s), &[]),
            Phase::Starting { .. }
        ));
    }

    /// A node we brought down ourselves must read differently from "it
    /// crashed" -- `Exited` attaches the log tail as if it were the cause,
    /// but a node stopped on request has no cause to attach.
    #[test]
    fn a_stopped_node_is_reported_as_stopped_not_as_a_crash() {
        let log = vec!["something unrelated".to_string()];
        match phase(Some(&NodeState::Stopped), None, &log) {
            Phase::Failed { message } => {
                assert!(
                    !message.contains("something unrelated"),
                    "a deliberate stop must not present the last log line as a cause: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// `None` means we do not own this node -- no process to observe, so the
    /// status is the only truth, judged the same way a `Running` node is.
    #[test]
    fn no_node_state_is_judged_by_the_status_alone() {
        let synced = status(Some(100), Some(100), Some(0));
        assert_eq!(phase(None, Some(&synced), &[]), Phase::Ready);
        assert_eq!(
            phase(None, None, &[]),
            Phase::Starting {
                last_line: None,
                percent: None
            }
        );
    }

    /// Measured 2026-09-11: the node takes a few hundred blocks, pauses, takes
    /// more (1006320 → 1006591 → pause → 1006631 → 1006818 in 80 s). A rate
    /// over two samples swings between 0 and ~30/s; it must span a while.
    #[test]
    fn the_sync_rate_waits_for_a_span_before_saying_anything() {
        let mut rate = SyncRate::default();
        rate.push(0.0, 1_006_320);
        rate.push(10.0, 1_006_591);
        assert_eq!(rate.blocks_per_sec(), None, "10 s is too short to trust");
        rate.push(40.0, 1_006_631);
        rate.push(80.0, 1_006_818);
        let r = rate.blocks_per_sec().expect("80 s of samples");
        assert!((r - 6.225).abs() < 0.01, "{r}");
        assert_eq!(rate.eta_secs(9_000), Some(1_446));
    }

    #[test]
    fn the_sync_rate_forgets_samples_older_than_its_window() {
        let mut rate = SyncRate::default();
        rate.push(0.0, 0);
        rate.push(1_000.0, 100);
        rate.push(1_030.0, 400);
        let r = rate.blocks_per_sec().expect("30 s inside the window");
        assert!((r - 10.0).abs() < 1e-9, "{r}");
    }

    #[test]
    fn a_height_that_goes_backwards_restarts_the_measurement() {
        let mut rate = SyncRate::default();
        rate.push(0.0, 500);
        rate.push(30.0, 800);
        rate.push(31.0, 100);
        assert_eq!(rate.blocks_per_sec(), None);
    }

    /// The old catch-up bar drew `span - remaining` over `span`, where
    /// `span = network - height` and `remaining = behind` -- the same number --
    /// so it was always empty. Progress is measured from where this
    /// catch-up began.
    #[test]
    fn progress_is_measured_from_where_catching_up_began() {
        let mut rate = SyncRate::default();
        assert_eq!(rate.progress(1_300, 2_000), None, "no start yet");
        rate.push(0.0, 1_000);
        rate.push(30.0, 1_300);
        assert_eq!(rate.progress(1_300, 2_000), Some(0.3));
        assert_eq!(
            rate.progress(900, 1_000),
            None,
            "network at or below the start"
        );
        rate.clear();
        assert_eq!(rate.progress(1_300, 2_000), None);
    }

    #[test]
    fn eta_reads_like_a_person_would_say_it() {
        assert_eq!(fmt_eta(40), "about 40 s");
        assert_eq!(fmt_eta(1_446), "about 25 min");
        assert_eq!(fmt_eta(7_800), "about 2 h 10 min");
    }

    #[test]
    fn the_catch_up_line_says_what_is_known() {
        assert_eq!(
            catch_up_line(9_234, None, None),
            "9234 blocks behind · measuring speed…"
        );
        assert_eq!(
            catch_up_line(9_234, Some(6.2), Some(1_489)),
            "9234 blocks behind · ~6.2 blocks/s · about 25 min"
        );
    }
}
