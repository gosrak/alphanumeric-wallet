//! Merges per-address history streams into one reverse-chronological list.
//!
//! Same discipline as `model.rs`: no `iced` import, no I/O. So it can be
//! tested with no window and no node. The merger only ever says *"I need
//! one more page for this address at this cursor"* -- the actual request
//! is sent by `app.rs`.

use std::collections::VecDeque;

use crate::backend::{AddressPage, Cursor, TxEntry};

/// Page size fetched at a time. Matches the node's default, but sent explicitly.
pub const PAGE_LIMIT: u32 = 50;

/// The counterparty name the node uses for coinbase inflows.
const MINING_COUNTERPARTY: &str = "MINING_REWARDS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Mining,
    In {
        from: String,
    },
    Out {
        to: String,
    },
    /// A transfer between two of the wallet's own addresses. The result of
    /// seeing the same transaction on both streams.
    Internal {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub height: u64,
    pub position: u32,
    pub timestamp: u64,
    pub amount_units: i128,
    pub fee_units: i128,
    pub kind: RowKind,
    /// The wallet address whose stream this row was taken from. For an
    /// `Internal` row (the same transaction seen on two of this wallet's
    /// streams) it is the stream the row was popped from.
    pub owner: String,
}

/// What the merger needs in order to make more progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// The next page of `streams[index]` needs to be fetched with this cursor.
    ///
    /// `address` is the address that stream already holds. It rides along
    /// so the caller does not have to look it back up by `index` -- while
    /// the screen is open, `Message::AddressStoreSaved` can push a new
    /// address into the wallet's address list, and then the list and the
    /// merger would be looking at different things.
    Page {
        index: usize,
        address: String,
        before: Option<Cursor>,
    },
    /// Either as many rows were emitted as asked for, all streams are
    /// exhausted, or a stalled stream blocks further progress. The three
    /// cases are not distinguished here -- `Idle` does not mean "all done".
    /// The caller must tell them apart with `is_done()` and
    /// `stalled_address()`; otherwise an address that never answered gets
    /// misread as the list being complete.
    Idle,
}

/// A single address's stream.
struct Stream {
    address: String,
    /// Entries not yet emitted, descending order.
    buffer: VecDeque<TxEntry>,
    /// Cursor for the next page. `None` before the first request too --
    /// `answered` is what tells the two apart.
    next: Option<Cursor>,
    /// Whether a page has ever been received.
    answered: bool,
    /// Whether the node has said it has nothing more to give for this address.
    finished: bool,
    /// Whether the index answered that it cannot answer (`transactions:
    /// null`). Different from exhausted, and not something to retry --
    /// asking again gets the same answer, and that re-ask rides `app.rs`'s
    /// recursion into hammering the node forever.
    stalled: bool,
    index_height: Option<u64>,
}

impl Stream {
    fn needs_a_page(&self) -> bool {
        self.buffer.is_empty() && !self.finished && !self.stalled
    }

    fn head(&self) -> Option<(u64, u32)> {
        self.buffer.front().map(|tx| (tx.height, tx.position))
    }
}

pub struct Merge {
    streams: Vec<Stream>,
    rows: Vec<Row>,
}

impl Merge {
    pub fn new(addresses: Vec<String>) -> Self {
        Self {
            streams: addresses
                .into_iter()
                .map(|address| Stream {
                    address,
                    buffer: VecDeque::new(),
                    next: None,
                    answered: false,
                    finished: false,
                    stalled: false,
                    index_height: None,
                })
                .collect(),
            rows: Vec::new(),
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Every stream has answered at least once, has said it has no more
    /// pages to give, and its buffer is empty. A stalled stream can never
    /// satisfy this -- counting an address that never answered as finished
    /// would make that address's transactions vanish silently.
    pub fn is_done(&self) -> bool {
        self.streams
            .iter()
            .all(|stream| stream.answered && stream.finished && stream.buffer.is_empty())
    }

    /// The address the index answered that it cannot answer. When set,
    /// `advance` keeps returning `Need::Idle`, and that state is different
    /// from "all done" (`is_done`) -- the screen must check this value to
    /// tell the two apart.
    pub fn stalled_address(&self) -> Option<&str> {
        self.streams
            .iter()
            .find(|stream| stream.stalled)
            .map(|stream| stream.address.as_str())
    }

    /// The index height of the most-behind stream. `None` if even one
    /// stream has not answered yet -- whether that address is current or
    /// behind is still unknown.
    pub fn lowest_index_height(&self) -> Option<u64> {
        self.streams
            .iter()
            .map(|stream| stream.index_height)
            .min()
            .flatten()
    }

    /// Feeds a fetch result into the stream the last `Need::Page` pointed
    /// at. `index` and `before` must match what that `Need::Page` gave.
    ///
    /// `before` is the cursor this page was requested at. The node must
    /// only return entries *below* that cursor (measured: the page after
    /// cursor (985776,0) starts at (985774,0)), so entries at or above the
    /// cursor are dropped. Without this, a node that ignores `before` and
    /// hands back page 1 again fills the buffer with keys higher than rows
    /// already emitted, and `pick_highest` tacks those onto the bottom of
    /// lower rows -- a duplicated, out-of-order list drawn with no error at
    /// all. `node_url` is user-editable, so someone else's node is in scope
    /// too.
    ///
    /// Returns how many entries actually made it into the buffer after
    /// dropping. The caller must catch this being 0 while `next` is still
    /// alive -- even if the page had entries, if all of them were dropped
    /// there was no progress, and leaving it as-is re-requests the same
    /// cursor forever.
    pub fn accept(&mut self, index: usize, before: Option<Cursor>, page: AddressPage) -> usize {
        let Some(stream) = self.streams.get_mut(index) else {
            return 0;
        };
        stream.answered = true;
        stream.index_height = page.index_height;
        stream.next = page.next;
        let mut kept = 0;
        match page.transactions {
            Some(entries) => {
                for entry in entries {
                    if let Some(cursor) = before {
                        // The first page (`before: None`) is accepted in full.
                        if (entry.height, entry.position)
                            >= (cursor.before_height, cursor.before_pos)
                        {
                            continue;
                        }
                    }
                    stream.buffer.push_back(entry);
                    kept += 1;
                }
                // The absence of `next` is the only signal that this
                // address is done.
                stream.finished = page.next.is_none();
            }
            // The index cannot answer. This is not an empty history, so it
            // does not count as exhausted -- counting it that way would
            // make this address's transactions vanish silently. It is not
            // retried either: the same answer comes back, and that re-ask
            // hammers the node forever.
            None => stream.stalled = true,
        }
        kept
    }

    /// Emits as many rows as it can until the rows accumulated so far reach
    /// `want`, then returns the next need. `want` is a cumulative target --
    /// not an increment on top of rows already emitted, but the total
    /// `rows().len()` must reach. Reading it as an increment would keep
    /// demanding more on every call even after the target is already met,
    /// and since this merger is called repeatedly from Task 3's fetch loop,
    /// that misreading turns straight into infinite requests.
    pub fn advance(&mut self, want: usize) -> Need {
        loop {
            if self.rows.len() >= want {
                return Need::Idle;
            }
            // Invariant: if any unexhausted stream has an empty buffer,
            // nothing is emitted. An address not yet fetched could be
            // holding a higher key, and breaking this silently puts the
            // list out of order.
            // If any address never answered, nothing can be emitted and
            // none can be pressed further. Stop, and let the screen say so.
            if self.streams.iter().any(|stream| stream.stalled) {
                return Need::Idle;
            }
            if let Some(index) = self.streams.iter().position(Stream::needs_a_page) {
                return Need::Page {
                    index,
                    address: self.streams[index].address.clone(),
                    before: self.streams[index].next,
                };
            }
            let Some(index) = self.pick_highest() else {
                return Need::Idle;
            };
            self.emit(index);
        }
    }

    /// The stream whose head is largest. There are no ties -- the same key
    /// is the same transaction, and that case is folded by `emit`.
    fn pick_highest(&self) -> Option<usize> {
        self.streams
            .iter()
            .enumerate()
            .filter_map(|(index, stream)| stream.head().map(|key| (index, key)))
            .max_by_key(|(_, key)| *key)
            .map(|(index, _)| index)
    }

    fn emit(&mut self, index: usize) {
        let Some(entry) = self.streams[index].buffer.pop_front() else {
            return;
        };
        let key = (entry.height, entry.position);
        let owner = self.streams[index].address.clone();
        let row_owner = owner.clone();

        // A *different* stream holding the same key is the same transaction
        // seen from the other side. Since equal keys are always adjacent in
        // descending order, checking just the current heads is enough.
        // Leaving both as separate rows reads as the same money moving
        // twice, and dropping just one is a lie no matter which one you
        // drop.
        //
        // The stream just popped from is excluded from the candidates. If
        // one buffer holds the same key twice -- which is exactly what
        // happens when a node that treats `before` as inclusive repeats one
        // entry at a page boundary -- it would fold with itself into
        // `Internal { from: owner, to: owner }`, turning a genuine inflow
        // into a self-transfer, with the fee drawn in the amount column
        // instead of the actual amount.
        let twin = self
            .streams
            .iter()
            .enumerate()
            .position(|(other, stream)| other != index && stream.head() == Some(key));
        if let Some(other) = twin {
            // The copy on the other side is discarded. Amount, fee, and
            // timestamp are the same transaction, so they are already in
            // `entry`.
            self.streams[other].buffer.pop_front();
            let (from, to) = if entry.direction == "out" {
                (owner, self.streams[other].address.clone())
            } else {
                (self.streams[other].address.clone(), owner)
            };
            self.rows.push(Row {
                height: entry.height,
                position: entry.position,
                timestamp: entry.timestamp,
                amount_units: entry.amount_units,
                fee_units: entry.fee_units,
                kind: RowKind::Internal { from, to },
                owner: row_owner,
            });
            return;
        }

        // Direction has to be checked too. The counterparty name alone is
        // not enough: a payment *out* to `MINING_REWARDS` can end up on
        // chain. The canonical-recipient check runs only at mempool
        // admission and is deliberately skipped at block validation
        // (`admit_transaction` in `src/a9/blockchain.rs`, RELAY-POLICY
        // comment "such a block is still fully valid"), and that same
        // comment gives sending to `"MINING_REWARDS"` as its example. Such
        // a transaction is indexed under SENDER, so the explorer reports it
        // as `direction: "out"` -- and without checking direction, money
        // that left gets drawn as a mining reward, in the accent color,
        // with a positive amount, and the fee hidden.
        let kind = if entry.counterparty == MINING_COUNTERPARTY && entry.direction == "in" {
            RowKind::Mining
        } else if entry.direction == "self" {
            // sender == recipient (node.rs 7057 / blockchain.rs 3037-3047):
            // only a fee was burned, nothing left the wallet. Without this
            // arm ahead of the "out" check, "self" falls to the final
            // `else` below and reads as `In` -- money arriving from the
            // address itself, a reversed direction on a money screen.
            // `Internal { from: owner, to: owner }` already means "did not
            // leave this wallet", which is exactly true here.
            RowKind::Internal {
                from: owner.clone(),
                to: owner,
            }
        } else if entry.direction == "out" {
            RowKind::Out {
                to: entry.counterparty.clone(),
            }
        } else {
            RowKind::In {
                from: entry.counterparty.clone(),
            }
        };
        self.rows.push(Row {
            height: entry.height,
            position: entry.position,
            timestamp: entry.timestamp,
            amount_units: entry.amount_units,
            fee_units: entry.fee_units,
            kind,
            owner: row_owner,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(height: u64, position: u32, direction: &str, counterparty: &str) -> TxEntry {
        TxEntry {
            amount_units: 1_000_000_000,
            fee_units: 50_000,
            counterparty: counterparty.to_string(),
            direction: direction.to_string(),
            height,
            position,
            timestamp: 1_788_000_000 + height,
        }
    }

    fn page(txs: Vec<TxEntry>, next: Option<Cursor>) -> AddressPage {
        AddressPage {
            transactions: Some(txs),
            next,
            history_available: true,
            index_ready: true,
            index_height: Some(1_000),
        }
    }

    /// F4's ADDR column and F2's "incoming to this address" both need to know
    /// which of the wallet's addresses a row came from. The merger knew (the
    /// stream it popped) and threw it away.
    #[test]
    fn every_row_names_the_address_it_came_from() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        merge.accept(0, None, page(vec![tx(5, 0, "in", "x")], None));
        merge.accept(1, None, page(vec![tx(4, 0, "out", "y")], None));
        merge.advance(10);
        let owners: Vec<&str> = merge.rows().iter().map(|r| r.owner.as_str()).collect();
        assert_eq!(owners, vec!["a", "b"]);
    }

    /// Drives the merger to completion. `pages` is a per-stream-index page queue.
    fn drive(merge: &mut Merge, mut pages: Vec<Vec<AddressPage>>, want: usize) {
        loop {
            match merge.advance(want) {
                Need::Idle => return,
                Need::Page { index, before, .. } => {
                    let queue = &mut pages[index];
                    let next = if queue.is_empty() {
                        page(vec![], None)
                    } else {
                        queue.remove(0)
                    };
                    // Passes through the same cursor the request used --
                    // since `accept` only accepts entries below it, every
                    // test using this helper exercises that filtering for
                    // free.
                    merge.accept(index, before, next);
                }
            }
        }
    }

    // Three addresses' histories merge into one descending list. Each is
    // already sorted on its own, but interleaving the three is the
    // merger's job.
    #[test]
    fn three_streams_merge_into_one_descending_list() {
        let mut merge = Merge::new(vec!["a".into(), "b".into(), "c".into()]);
        drive(
            &mut merge,
            vec![
                vec![page(
                    vec![tx(100, 0, "in", "x"), tx(70, 0, "in", "x")],
                    None,
                )],
                vec![page(vec![tx(90, 0, "in", "y"), tx(60, 0, "in", "y")], None)],
                vec![page(vec![tx(80, 0, "in", "z")], None)],
            ],
            100,
        );
        let heights: Vec<u64> = merge.rows().iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![100, 90, 80, 70, 60]);
        assert!(merge.is_done());
    }

    // Within the same block, position decides the order. Comparing height
    // alone would let two transactions at the same height end up in
    // arbitrary order.
    #[test]
    fn ties_on_height_are_broken_by_position_descending() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        drive(
            &mut merge,
            vec![
                vec![page(vec![tx(50, 1, "in", "x")], None)],
                vec![page(vec![tx(50, 7, "in", "y")], None)],
            ],
            100,
        );
        let keys: Vec<(u64, u32)> = merge
            .rows()
            .iter()
            .map(|r| (r.height, r.position))
            .collect();
        assert_eq!(keys, vec![(50, 7), (50, 1)]);
    }

    // My address A -> my address B shows up at the same (height, position)
    // on both streams. Leaving it as two rows reads as the same money
    // moving twice.
    #[test]
    fn a_transfer_between_my_own_addresses_folds_into_one_row() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        drive(
            &mut merge,
            vec![
                vec![page(vec![tx(50, 2, "out", "b")], None)],
                vec![page(vec![tx(50, 2, "in", "a")], None)],
            ],
            100,
        );
        assert_eq!(merge.rows().len(), 1);
        assert_eq!(
            merge.rows()[0].kind,
            RowKind::Internal {
                from: "a".into(),
                to: "b".into()
            }
        );
    }

    // Mining rewards will be the most common row in this wallet, so they
    // are not mixed in with ordinary inflows.
    #[test]
    fn a_mining_reward_is_its_own_kind() {
        let mut merge = Merge::new(vec!["a".into()]);
        drive(
            &mut merge,
            vec![vec![page(vec![tx(50, 0, "in", "MINING_REWARDS")], None)]],
            100,
        );
        assert_eq!(merge.rows()[0].kind, RowKind::Mining);
    }

    // The single most important test in this whole plan. If any stream has
    // never been fetched, no row may be emitted -- that stream could be
    // holding a higher key, and breaking this silently puts the list out
    // of order with no exception and no error.
    #[test]
    fn no_row_is_emitted_while_a_stream_has_never_answered() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        let need = merge.advance(10);
        assert!(matches!(need, Need::Page { .. }));
        assert!(merge.rows().is_empty());

        // Only a has answered. b is still silent -- still nothing can be
        // emitted.
        merge.accept(0, None, page(vec![tx(10, 0, "in", "x")], None));
        let need = merge.advance(10);
        assert_eq!(
            need,
            Need::Page {
                index: 1,
                address: "b".into(),
                before: None
            }
        );
        assert!(
            merge.rows().is_empty(),
            "b could be holding a transaction from block 100"
        );

        // Now b has answered too.
        merge.accept(1, None, page(vec![tx(100, 0, "in", "y")], None));
        merge.advance(10);
        let heights: Vec<u64> = merge.rows().iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![100, 10]);
    }

    // Even if one address finishes first, the rest must keep coming out.
    // Waiting on an exhausted stream forever would stall the list right
    // there.
    #[test]
    fn an_exhausted_stream_does_not_stall_the_others() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        drive(
            &mut merge,
            vec![
                vec![page(vec![tx(100, 0, "in", "x")], None)],
                vec![
                    page(
                        vec![tx(90, 0, "in", "y")],
                        Some(Cursor {
                            before_height: 90,
                            before_pos: 0,
                        }),
                    ),
                    page(vec![tx(80, 0, "in", "y"), tx(70, 0, "in", "y")], None),
                ],
            ],
            100,
        );
        let heights: Vec<u64> = merge.rows().iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![100, 90, 80, 70]);
    }

    // The cursor is echoed back exactly as the node sent it. If the client
    // recomputed it from the last row instead, it would skip one entry
    // right where an internal transfer got folded.
    #[test]
    fn the_cursor_is_echoed_back_exactly_as_the_node_sent_it() {
        let mut merge = Merge::new(vec!["a".into()]);
        assert_eq!(
            merge.advance(1),
            Need::Page {
                index: 0,
                address: "a".into(),
                before: None
            }
        );
        merge.accept(
            0,
            None,
            page(
                vec![tx(100, 4, "in", "x")],
                Some(Cursor {
                    before_height: 42,
                    before_pos: 9,
                }),
            ),
        );
        // One row has been emitted, so want=2 still needs more.
        assert_eq!(
            merge.advance(2),
            Need::Page {
                index: 0,
                address: "a".into(),
                before: Some(Cursor {
                    before_height: 42,
                    before_pos: 9
                })
            }
        );
    }

    // Freshness is decided by the most-behind address. If even one is
    // behind, the whole list cannot be called complete.
    #[test]
    fn the_lowest_index_height_is_what_freshness_is_judged_on() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        assert_eq!(merge.lowest_index_height(), None);
        merge.advance(10);
        merge.accept(
            0,
            None,
            AddressPage {
                transactions: Some(vec![]),
                next: None,
                history_available: true,
                index_ready: true,
                index_height: Some(900),
            },
        );
        merge.advance(10);
        merge.accept(
            1,
            None,
            AddressPage {
                transactions: Some(vec![]),
                next: None,
                history_available: true,
                index_ready: true,
                index_height: Some(1_000),
            },
        );
        assert_eq!(merge.lowest_index_height(), Some(900));
    }

    // An address the index cannot answer for (transactions: null) is not
    // an empty history. Treating it as exhausted would make that address's
    // transactions vanish silently. Pressing it anyway is no better --
    // since the buffer is empty and it never finishes, `advance` would
    // demand the same page forever, and `app.rs`'s recursion would fire
    // that straight at the node. Stopping and saying so is the only
    // correct answer.
    #[test]
    fn an_unanswerable_address_stops_the_merge_instead_of_being_retried() {
        let mut merge = Merge::new(vec!["a".into()]);
        assert_eq!(
            merge.advance(10),
            Need::Page {
                index: 0,
                address: "a".into(),
                before: None
            }
        );
        merge.accept(
            0,
            None,
            AddressPage {
                transactions: None,
                next: None,
                history_available: false,
                index_ready: false,
                index_height: None,
            },
        );
        assert_eq!(
            merge.advance(10),
            Need::Idle,
            "demanding the same page again would hammer the node forever"
        );
        assert!(merge.rows().is_empty());
        assert!(
            !merge.is_done(),
            "an address that never answered must not count as finished"
        );
        assert_eq!(merge.stalled_address(), Some("a"));
    }

    // sender == recipient is a real value on the wire (`node.rs`'s
    // `explorer_entry_json` emits `"self"`, and `blockchain.rs`'s
    // `address_index_ops` sets both flags on one entry in that case).
    // Reading "self" as In would make a transaction that only burned a fee
    // look, on screen, like money arrived from itself.
    #[test]
    fn a_self_send_is_internal_with_the_same_address_on_both_sides() {
        let mut merge = Merge::new(vec!["a".into()]);
        drive(
            &mut merge,
            vec![vec![page(vec![tx(50, 0, "self", "a")], None)]],
            100,
        );
        assert_eq!(
            merge.rows()[0].kind,
            RowKind::Internal {
                from: "a".into(),
                to: "a".into()
            }
        );
    }

    // Even if the counterparty is `MINING_REWARDS`, it is not a mining
    // reward when the direction is `out`. The canonical-recipient check
    // runs only at mempool admission and is deliberately skipped at block
    // validation (`admit_transaction` in `src/a9/blockchain.rs`), so a
    // payment *out* to `MINING_REWARDS` can end up on chain, and the
    // explorer reports it as `direction: "out"`. Looking only at the
    // counterparty name would draw money that left as "Mined" -- in the
    // accent color, with a positive amount, fee hidden -- reading as money
    // that arrived when it actually left.
    #[test]
    fn a_payment_out_to_the_mining_counterparty_is_not_a_reward() {
        let mut merge = Merge::new(vec!["a".into()]);
        drive(
            &mut merge,
            vec![vec![page(vec![tx(50, 0, "out", "MINING_REWARDS")], None)]],
            100,
        );
        assert_eq!(
            merge.rows()[0].kind,
            RowKind::Out {
                to: "MINING_REWARDS".into()
            },
            "an outgoing payment must not be drawn as a mining reward"
        );
    }

    // The twin search must exclude the stream just popped from. If one
    // buffer holds the same key twice -- which is what happens when a node
    // that treats `before` as inclusive repeats one entry at a page
    // boundary -- it would fold with itself into `Internal { a, a }`,
    // turning a genuine inflow into a self-transfer with the fee drawn in
    // the amount column instead. Removing the duplicate itself is
    // `accept`'s cursor filtering (tested below); what is guarded here is
    // the mislabeling.
    #[test]
    fn two_entries_with_one_key_in_one_stream_do_not_fold_into_internal() {
        let mut merge = Merge::new(vec!["a".into()]);
        drive(
            &mut merge,
            vec![vec![page(
                vec![tx(50, 2, "in", "x"), tx(50, 2, "in", "x")],
                None,
            )]],
            100,
        );
        assert_eq!(merge.rows().len(), 2);
        for row in merge.rows() {
            assert_eq!(
                row.kind,
                RowKind::In { from: "x".into() },
                "a stream must not fold with itself"
            );
        }
    }

    // A node that ignores `before` and hands back page 1 again. Accepting
    // entries at or above the cursor as-is would put keys higher than rows
    // already emitted into the buffer, and `pick_highest` would tack those
    // onto the bottom of lower rows -- duplicated and out of order, with
    // not a single error raised.
    #[test]
    fn a_page_at_or_above_the_cursor_it_was_asked_for_is_dropped() {
        let mut merge = Merge::new(vec!["a".into()]);
        let cursor = Cursor {
            before_height: 90,
            before_pos: 0,
        };
        assert_eq!(
            merge.advance(10),
            Need::Page {
                index: 0,
                address: "a".into(),
                before: None
            }
        );
        merge.accept(
            0,
            None,
            page(
                vec![tx(100, 0, "in", "x"), tx(90, 0, "in", "x")],
                Some(cursor),
            ),
        );
        assert_eq!(
            merge.advance(3),
            Need::Page {
                index: 0,
                address: "a".into(),
                before: Some(cursor)
            }
        );
        // The node ignores the cursor and hands back the same two entries
        // again.
        let kept = merge.accept(
            0,
            Some(cursor),
            page(
                vec![tx(100, 0, "in", "x"), tx(90, 0, "in", "x")],
                Some(cursor),
            ),
        );
        assert_eq!(kept, 0, "nothing at or above the cursor is accepted");
        merge.advance(10);
        let keys: Vec<(u64, u32)> = merge
            .rows()
            .iter()
            .map(|r| (r.height, r.position))
            .collect();
        assert_eq!(
            keys,
            vec![(100, 0), (90, 0)],
            "no duplicates, no reversed order"
        );
    }

    // A page dropped in its entirety made no progress even though it had
    // entries. If `accept` did not return 0 here, `app.rs`'s empty-page
    // guard would miss this case and re-request the same cursor forever.
    #[test]
    fn a_fully_dropped_page_reports_no_progress() {
        let mut merge = Merge::new(vec!["a".into()]);
        let cursor = Cursor {
            before_height: 50,
            before_pos: 0,
        };
        merge.advance(10);
        let kept = merge.accept(
            0,
            Some(cursor),
            page(vec![tx(70, 0, "in", "x")], Some(cursor)),
        );
        assert_eq!(kept, 0);
        assert!(merge.rows().is_empty());
    }

    // `want` is a cumulative target, not an increment. Two more entries
    // are still sitting in the buffer, and with the cursor still alive
    // (the stream is not `finished`), advance(2) is called again after 2
    // rows have already been emitted. An implementation that reads it as
    // an increment would either emit the remaining two and grow the row
    // count to 4, or (if the buffer does not line up exactly) empty the
    // buffer and return `Need::Page` because the cursor is still alive --
    // either way, the cumulative target was violated.
    #[test]
    fn advance_want_is_a_cumulative_target_not_an_increment() {
        let mut merge = Merge::new(vec!["a".into()]);
        drive(
            &mut merge,
            vec![vec![page(
                vec![
                    tx(100, 0, "in", "x"),
                    tx(90, 0, "in", "x"),
                    tx(80, 0, "in", "x"),
                    tx(70, 0, "in", "x"),
                ],
                Some(Cursor {
                    before_height: 70,
                    before_pos: 0,
                }),
            )]],
            2,
        );
        assert_eq!(merge.rows().len(), 2);
        assert_eq!(merge.advance(2), Need::Idle);
        assert_eq!(
            merge.rows().len(),
            2,
            "once the cumulative target is met, calling again must not emit more"
        );
    }
}
