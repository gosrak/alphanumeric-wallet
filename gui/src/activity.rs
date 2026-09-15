//! What the screens say about a transaction beyond the merger's bare row:
//! its direction and signed value, whether it is final or still maturing,
//! and the short "latest" list the main screen shows.
//!
//! Same discipline as `history.rs` and `model.rs`: no `iced`, no I/O.

use crate::backend::{AddressPage, NodeStatus};
use crate::history::{Merge, Row, RowKind};
use crate::startup;

/// Coinbase maturity in blocks -- the node's `MINING_REWARD_MATURITY`
/// (`src/a9/blockchain.rs`). This crate cannot import the node; the test
/// `the_maturity_constants_match_the_node_source` reads the node's source
/// and fails the moment the two disagree.
pub const MINING_REWARD_MATURITY: u64 = 100;

/// Below this height the node applies no maturity (`MATURITY_ACTIVATION_HEIGHT`).
pub const MATURITY_ACTIVATION_HEIGHT: u64 = 1500;

/// Blocks until a reward mined at `reward_height` can be spent, as of `tip`.
/// `0` = spendable now. The node's rule (`WalletBalanceBreakdown::maturing`):
/// a reward at `rh` leaves the maturing set once the tip reaches
/// `rh + MINING_REWARD_MATURITY − 1`.
///
/// No maturity while the next block, `tip + 1`, is below
/// `MATURITY_ACTIVATION_HEIGHT`: the node checks the height a spend would
/// land at, not the reward's (`immature_coinbase_details`).
pub fn blocks_to_spendable(reward_height: u64, tip: u64) -> u64 {
    if tip.saturating_add(1) < MATURITY_ACTIVATION_HEIGHT {
        return 0;
    }
    (reward_height + MINING_REWARD_MATURITY - 1).saturating_sub(tip)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// At or below the node's finality checkpoint.
    Final,
    /// In the chain, above the checkpoint.
    Confirmed,
    /// A mining reward that cannot be spent yet.
    Maturing { blocks_left: u64 },
    /// The chain height is not known, so nothing can be said.
    Unknown,
}

/// `tip` is `NodeStatus::height`; `finalized` is `NodeStatus::finalized_height`,
/// already `None` for the node's "not seeded yet" `0` (`parse_status`).
pub fn row_status(row: &Row, tip: Option<u64>, finalized: Option<u64>) -> RowStatus {
    let Some(tip) = tip else {
        return RowStatus::Unknown;
    };
    if row.kind == RowKind::Mining {
        let blocks_left = blocks_to_spendable(row.height, tip);
        if blocks_left > 0 {
            return RowStatus::Maturing { blocks_left };
        }
    }
    match finalized {
        Some(checkpoint) if row.height <= checkpoint => RowStatus::Final,
        _ => RowStatus::Confirmed,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Mined,
    In,
    Out,
    /// Sender and recipient are the same address: only a fee was burned.
    SelfSend,
    /// Between two different addresses of this wallet.
    Internal,
}

impl Dir {
    pub fn label(self) -> &'static str {
        match self {
            Dir::Mined => "MINED",
            Dir::In => "IN",
            Dir::Out => "OUT",
            Dir::SelfSend => "SELF",
            Dir::Internal => "MOVE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Plus,
    Minus,
    /// Nets to zero across the wallet (a move between its own addresses).
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub dir: Dir,
    /// What the amount column shows, in units, unsigned.
    pub value_units: i128,
    pub sign: Sign,
    /// `Some` only when this wallet paid a fee the amount column does not
    /// already show.
    pub fee_units: Option<i128>,
    /// The other side. Empty for a mining reward.
    pub counterparty: String,
}

/// The money-direction rules every screen draws a row with (F1, F2, F4).
/// Moved out of `view/history.rs` so the screens cannot drift apart:
///
/// - Mining and incoming: the fee on the wire was paid by the other side
///   (for a reward, not by anyone in this wallet) -- no fee is shown, or it
///   reads as a deduction that never touched this balance.
/// - Outgoing: amount and the fee this wallet paid.
/// - Self-send (`direction: "self"`): the node writes the amount unchanged
///   and positive, but nothing left the wallet. The fee is the only number
///   that changed the balance, so it takes the amount slot, and no second
///   fee line is shown.
/// - Between two different addresses of this wallet: the amount nets to zero
///   across the wallet, but the fee is a real loss and must be there to
///   reconcile the list against the balance.
pub fn display(row: &Row) -> Display {
    match &row.kind {
        RowKind::Mining => Display {
            dir: Dir::Mined,
            value_units: row.amount_units,
            sign: Sign::Plus,
            fee_units: None,
            counterparty: String::new(),
        },
        RowKind::In { from } => Display {
            dir: Dir::In,
            value_units: row.amount_units,
            sign: Sign::Plus,
            fee_units: None,
            counterparty: from.clone(),
        },
        RowKind::Out { to } => Display {
            dir: Dir::Out,
            value_units: row.amount_units,
            sign: Sign::Minus,
            fee_units: Some(row.fee_units),
            counterparty: to.clone(),
        },
        RowKind::Internal { from, to } if from == to => Display {
            dir: Dir::SelfSend,
            value_units: row.fee_units,
            sign: Sign::Minus,
            fee_units: None,
            counterparty: from.clone(),
        },
        RowKind::Internal { from, to } => Display {
            dir: Dir::Internal,
            value_units: row.amount_units,
            sign: Sign::Neutral,
            fee_units: Some(row.fee_units),
            counterparty: format!("{from} -> {to}"),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub rows: Vec<Row>,
    /// False when something newer than the last row may be missing: an
    /// address not fetched yet, or one whose index cannot answer. The merger
    /// stops rather than guess, and the screen says "partial".
    ///
    /// True does NOT mean "the whole history": only that the newest `want`
    /// rows are exact. See `exhaustive`.
    pub complete: bool,
    /// Every address's stream was read to its end (`Merge::is_done`): `rows`
    /// is everything, not just the newest part. Only then may a filtered
    /// list that comes up empty say "nothing has arrived".
    pub exhaustive: bool,
}

/// How much of the truth a `Recent` list is, for the screen's footer and
/// empty-state wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Some address has not answered: nothing can be said yet.
    Waiting,
    /// The node's OWN address index has not caught up to its chain (by more
    /// than `startup::SYNCED_SLACK`): rows newer than the index may be
    /// missing, no matter which address's page happened to answer most
    /// recently.
    Behind,
    /// Exact, but only the newest `want` rows were read.
    Latest,
    /// Every address's history, read to its end.
    All,
}

impl Recent {
    /// The node's own index lag: `height` behind `index_height` by more than
    /// `startup::SYNCED_SLACK`, the same slack the rest of the wallet already
    /// uses for "caught up". Unknown on either side is not "behind": there
    /// is nothing to compare.
    ///
    /// Deliberately NOT a function of which address's page is stalest
    /// (that was the first wave's `lowest_index_height`, since removed):
    /// only the ACTIVE address is repolled every 10s, so for any wallet with
    /// more than one address that reading was behind almost all the time --
    /// a label that is always on says nothing.
    fn node_index_behind(status: Option<&NodeStatus>) -> bool {
        let Some(status) = status else {
            return false;
        };
        matches!(
            (status.height, status.index_height),
            (Some(height), Some(index_height))
                if height.saturating_sub(index_height) > startup::SYNCED_SLACK
        )
    }

    pub fn coverage(&self, status: Option<&NodeStatus>) -> Coverage {
        if !self.complete {
            Coverage::Waiting
        } else if Self::node_index_behind(status) {
            Coverage::Behind
        } else if self.exhaustive {
            Coverage::All
        } else {
            Coverage::Latest
        }
    }
}

/// The newest `want` rows across `addresses`, from the first page each
/// address's last refresh already fetched (`AddressState::as_page`). No
/// request is made here. The history merger does the ordering and folds the
/// two sides of an internal transfer into one row, so this list and F4
/// agree row for row.
pub fn recent_rows(addresses: &[String], pages: &[Option<AddressPage>], want: usize) -> Recent {
    let mut merge = Merge::new(addresses.to_vec());
    for (index, page) in pages.iter().enumerate() {
        if let Some(page) = page {
            merge.accept(index, None, page.clone());
        }
    }
    let _ = merge.advance(want);
    let exhaustive = merge.is_done();
    let complete = merge.rows().len() >= want || exhaustive;
    Recent {
        rows: merge.rows().to_vec(),
        complete,
        exhaustive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Cursor, TxEntry};

    fn tx(counterparty: &str, direction: &str, height: u64, position: u32) -> TxEntry {
        TxEntry {
            amount_units: 100,
            fee_units: 7,
            counterparty: counterparty.into(),
            direction: direction.into(),
            height,
            position,
            timestamp: height,
        }
    }

    fn page(entries: Vec<TxEntry>, next: Option<Cursor>) -> Option<AddressPage> {
        Some(AddressPage {
            transactions: Some(entries),
            next,
            history_available: true,
            index_ready: true,
            index_height: Some(2_000_000),
        })
    }

    fn row(kind: RowKind, height: u64) -> Row {
        Row {
            height,
            position: 0,
            timestamp: 0,
            amount_units: 100,
            fee_units: 7,
            kind,
            owner: "a".into(),
        }
    }

    #[test]
    fn a_reward_is_spendable_once_the_tip_reaches_height_plus_99() {
        assert_eq!(blocks_to_spendable(1_000_000, 1_000_000), 99);
        assert_eq!(blocks_to_spendable(1_000_000, 1_000_098), 1);
        assert_eq!(blocks_to_spendable(1_000_000, 1_000_099), 0);
        assert_eq!(blocks_to_spendable(1_000_000, 2_000_000), 0);
        assert_eq!(blocks_to_spendable(1_000, 1_000), 0, "below activation");
    }

    /// The node gates on the height a spend would land at, tip + 1
    /// (`immature_coinbase_details(address, spend_height, ..)` returns nothing
    /// while `spend_height < MATURITY_ACTIVATION_HEIGHT`), not on the
    /// reward's own height. A reward mined just below activation is still
    /// maturing once the chain has passed it.
    #[test]
    fn the_activation_gate_is_on_the_spend_height_not_the_reward_height() {
        assert_eq!(blocks_to_spendable(1_450, 1_510), 39);
        assert_eq!(blocks_to_spendable(1_000, 1_000), 0);
        assert_eq!(blocks_to_spendable(1_450, 1_498), 0, "spend at 1_499");
        assert_eq!(blocks_to_spendable(1_450, 1_499), 50, "spend at 1_500");
    }

    #[test]
    fn status_says_maturing_final_confirmed_or_unknown() {
        let mined = row(RowKind::Mining, 1_000_000);
        assert_eq!(
            row_status(&mined, Some(1_000_010), Some(999_000)),
            RowStatus::Maturing { blocks_left: 89 }
        );
        assert_eq!(
            row_status(&mined, Some(1_000_200), Some(1_000_100)),
            RowStatus::Final
        );
        let out = row(RowKind::Out { to: "x".into() }, 1_000_000);
        assert_eq!(
            row_status(&out, Some(1_000_050), Some(999_999)),
            RowStatus::Confirmed
        );
        assert_eq!(
            row_status(&out, Some(1_000_050), None),
            RowStatus::Confirmed
        );
        assert_eq!(row_status(&out, None, Some(1)), RowStatus::Unknown);
    }

    #[test]
    fn display_keeps_the_money_direction_rules() {
        let d = display(&row(RowKind::Mining, 1));
        assert_eq!(
            (d.dir, d.sign, d.fee_units, d.value_units),
            (Dir::Mined, Sign::Plus, None, 100)
        );
        let d = display(&row(RowKind::In { from: "x".into() }, 1));
        assert_eq!((d.dir, d.sign, d.fee_units), (Dir::In, Sign::Plus, None));
        let d = display(&row(RowKind::Out { to: "x".into() }, 1));
        assert_eq!(
            (d.dir, d.sign, d.fee_units),
            (Dir::Out, Sign::Minus, Some(7))
        );
        let d = display(&row(
            RowKind::Internal {
                from: "a".into(),
                to: "a".into(),
            },
            1,
        ));
        assert_eq!(
            (d.dir, d.value_units, d.sign, d.fee_units),
            (Dir::SelfSend, 7, Sign::Minus, None),
            "a self-send shows the fee in the amount slot and no second fee"
        );
        let d = display(&row(
            RowKind::Internal {
                from: "a".into(),
                to: "b".into(),
            },
            1,
        ));
        assert_eq!(
            (d.dir, d.sign, d.fee_units),
            (Dir::Internal, Sign::Neutral, Some(7))
        );
    }

    #[test]
    fn recent_rows_are_newest_first_across_addresses() {
        let addresses = vec!["a".to_string(), "b".to_string()];
        let pages = vec![
            page(vec![tx("x", "in", 30, 0), tx("x", "in", 10, 0)], None),
            page(vec![tx("y", "out", 20, 0)], None),
        ];
        let recent = recent_rows(&addresses, &pages, 8);
        let heights: Vec<u64> = recent.rows.iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![30, 20, 10]);
        assert!(recent.complete, "every stream finished");
    }

    #[test]
    fn an_address_not_fetched_yet_makes_the_list_partial_not_short() {
        let addresses = vec!["a".to_string(), "b".to_string()];
        let pages = vec![page(vec![tx("x", "in", 30, 0)], None), None];
        let recent = recent_rows(&addresses, &pages, 8);
        assert!(
            recent.rows.is_empty(),
            "the merger will not order without b"
        );
        assert!(!recent.complete);
    }

    #[test]
    fn a_first_page_with_more_behind_it_is_complete_only_up_to_want() {
        let addresses = vec!["a".to_string()];
        let more = Some(Cursor {
            before_height: 10,
            before_pos: 0,
        });
        let pages = vec![page(vec![tx("x", "in", 30, 0), tx("x", "in", 20, 0)], more)];
        assert!(recent_rows(&addresses, &pages, 2).complete);
        assert!(!recent_rows(&addresses, &pages, 3).complete);
    }

    /// `complete` only says the newest `want` rows are exact. A screen that
    /// filters them (F2's incoming) and finds nothing must not
    /// read that as "nothing ever": only `exhaustive` says every address's
    /// history was read to its end.
    #[test]
    fn exhaustive_only_when_every_address_was_read_to_its_end() {
        let addresses = vec!["a".to_string(), "b".to_string()];
        let done = vec![
            page(vec![tx("x", "in", 30, 0)], None),
            page(vec![tx("y", "out", 20, 0)], None),
        ];
        let recent = recent_rows(&addresses, &done, 8);
        assert!(recent.complete && recent.exhaustive);

        let more = Some(Cursor {
            before_height: 10,
            before_pos: 0,
        });
        let paged = vec![
            page(vec![tx("x", "in", 30, 0), tx("x", "in", 25, 0)], more),
            page(vec![tx("y", "out", 20, 0)], None),
        ];
        let recent = recent_rows(&addresses, &paged, 2);
        assert!(recent.complete, "the newest two are exact");
        assert!(!recent.exhaustive, "a has more behind its first page");

        let missing = vec![page(vec![tx("x", "in", 30, 0)], None), None];
        assert!(!recent_rows(&addresses, &missing, 8).exhaustive);
    }

    /// A `NodeStatus` with only `height` and `index_height` set -- everything
    /// `Recent::node_index_behind` reads.
    fn status_at(height: Option<u64>, index_height: Option<u64>) -> NodeStatus {
        NodeStatus {
            height,
            network_height: None,
            blocks_behind: None,
            index_ready: true,
            index_height,
            version: "8.0.0".into(),
            finalized_height: None,
            mining: None,
            mining_address: None,
            mining_backend: None,
            mining_hps: None,
            mining_blocks: None,
            mining_payout_rotation: None,
            gpu_built: false,
            gpu_devices: Vec::new(),
            mining_hashes: None,
            mining_difficulty: None,
            mining_expected_block_secs: None,
            mining_threads: None,
            mining_devices: Vec::new(),
        }
    }

    fn recent_at(complete: bool, exhaustive: bool) -> Recent {
        Recent {
            rows: Vec::new(),
            complete,
            exhaustive,
        }
    }

    /// R2: PARTIAL is the node's OWN index lag (`height` vs `index_height`,
    /// past `startup::SYNCED_SLACK`), not which address's page happened to
    /// answer last. The previous `lowest_index_height` reading made a
    /// multi-address wallet see PARTIAL almost permanently, since only the
    /// active address is repolled every 10s.
    #[test]
    fn coverage_is_partial_on_the_nodes_own_index_lag_not_the_pages_age() {
        let partial = |coverage| matches!(coverage, Coverage::Waiting | Coverage::Behind);

        // complete + index current -> not partial
        assert!(!partial(
            recent_at(true, true).coverage(Some(&status_at(Some(1_000), Some(1_000))))
        ));
        // complete + index 9 behind (past the 8-block slack) -> partial
        assert!(partial(
            recent_at(true, true).coverage(Some(&status_at(Some(1_000), Some(991))))
        ));
        // complete + index 8 behind (exactly the slack) -> not partial
        assert!(!partial(
            recent_at(true, true).coverage(Some(&status_at(Some(1_000), Some(992))))
        ));
        // incomplete -> partial regardless of the index
        assert!(partial(
            recent_at(false, false).coverage(Some(&status_at(Some(1_000), Some(1_000))))
        ));
        // no status -> partial only if incomplete
        assert!(!partial(recent_at(true, true).coverage(None)));
        assert!(partial(recent_at(false, false).coverage(None)));
    }

    /// Unknown on either side of the comparison is not "behind": there is
    /// nothing to compare, so it must not read as a lagging index.
    #[test]
    fn an_unknown_height_or_index_is_not_behind() {
        let recent = recent_at(true, true);
        assert!(!matches!(
            recent.coverage(Some(&status_at(None, Some(1_000)))),
            Coverage::Behind
        ));
        assert!(!matches!(
            recent.coverage(Some(&status_at(Some(1_000), None))),
            Coverage::Behind
        ));
    }

    /// The exact four variants, not just partial/not-partial -- `Waiting` and
    /// `Behind` read differently (`kit::filtered_empty`), and not answering
    /// must outrank being behind, not merely coexist with it.
    #[test]
    fn coverage_says_waiting_behind_latest_or_all() {
        let current = status_at(Some(1_000), Some(1_000));
        let behind = status_at(Some(1_000), Some(900));

        assert_eq!(
            recent_at(false, false).coverage(Some(&current)),
            Coverage::Waiting
        );
        assert_eq!(
            recent_at(false, false).coverage(Some(&behind)),
            Coverage::Waiting,
            "not answering outranks being behind"
        );
        assert_eq!(
            recent_at(true, true).coverage(Some(&behind)),
            Coverage::Behind
        );
        assert_eq!(
            recent_at(true, false).coverage(Some(&behind)),
            Coverage::Behind
        );
        assert_eq!(
            recent_at(true, false).coverage(Some(&current)),
            Coverage::Latest
        );
        assert_eq!(
            recent_at(true, true).coverage(Some(&current)),
            Coverage::All
        );
    }

    #[test]
    fn one_transfer_between_own_addresses_is_one_row() {
        let addresses = vec!["a".to_string(), "b".to_string()];
        let pages = vec![
            page(vec![tx("b", "out", 30, 1)], None),
            page(vec![tx("a", "in", 30, 1)], None),
        ];
        let recent = recent_rows(&addresses, &pages, 8);
        assert_eq!(recent.rows.len(), 1);
        assert!(matches!(recent.rows[0].kind, RowKind::Internal { .. }));
    }

    /// The node's source, read at compile time. Test-only; the crate still
    /// does not depend on the node.
    const NODE_BLOCKCHAIN_SRC: &str = include_str!("../../src/a9/blockchain.rs");

    #[test]
    fn the_maturity_constants_match_the_node_source() {
        assert!(NODE_BLOCKCHAIN_SRC.contains(&format!(
            "pub const MINING_REWARD_MATURITY: u32 = {MINING_REWARD_MATURITY};"
        )));
        assert!(NODE_BLOCKCHAIN_SRC.contains(&format!(
            "pub const MATURITY_ACTIVATION_HEIGHT: u32 = {MATURITY_ACTIVATION_HEIGHT};"
        )));
        assert!(NODE_BLOCKCHAIN_SRC.contains("leaves this set once the tip reaches"));
        assert!(
            NODE_BLOCKCHAIN_SRC.contains("rh + MINING_REWARD_MATURITY \u{2212} 1"),
            "the node's statement of the rule `blocks_to_spendable` implements"
        );
    }

    const NODE_MGMT_SRC: &str = include_str!("../../src/a9/mgmt.rs");

    /// The comments above can stay put while the code moves. The node's own
    /// display countdown (`blocks_until_mature`) is the same arithmetic as
    /// `blocks_to_spendable`, and the activation gate reads the spend height.
    #[test]
    fn the_maturity_arithmetic_matches_the_node_code() {
        assert!(
            NODE_MGMT_SRC.contains(".saturating_add(MINING_REWARD_MATURITY as u64 - 1)"),
            "the node's countdown: reward height + MINING_REWARD_MATURITY - 1"
        );
        assert!(
            NODE_BLOCKCHAIN_SRC.contains("if (spend_height as u32) < MATURITY_ACTIVATION_HEIGHT {"),
            "the node's activation gate is on the spend height (tip + 1)"
        );
    }
}
