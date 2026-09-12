//! 주소별 이력 스트림을 하나의 시간 역순 목록으로 합친다.
//!
//! `model.rs` 와 같은 규율이다: `iced` 를 import 하지 않고 I/O 도 하지
//! 않는다. 그래서 창 없이, 노드 없이 테스트된다. 병합기는 *"이 주소의 이
//! 커서로 한 페이지 더 필요하다"* 고 말할 뿐이고, 실제 요청은 `app.rs` 가
//! 보낸다.

use std::collections::VecDeque;

use crate::backend::{AddressPage, Cursor, TxEntry};

/// 한 번에 받아 오는 페이지 크기. 노드 기본값과 같지만 명시해서 보낸다.
pub const PAGE_LIMIT: u32 = 50;

/// 코인베이스 유입의 상대방으로 노드가 쓰는 이름.
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
    /// 지갑의 두 주소 사이 이동. 두 스트림에서 같은 거래를 본 결과다.
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

/// 병합기를 더 진행시키려면 무엇이 필요한가.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// `streams[index]` 의 다음 페이지를 이 커서로 받아 와야 한다.
    ///
    /// `address` 는 그 스트림이 이미 들고 있는 주소다. 호출자가 `index` 로
    /// 다시 찾지 않게 하려고 같이 실어 보낸다 -- 화면이 열려 있는 동안
    /// `Message::AddressStoreSaved` 가 지갑의 주소 목록에 새 주소를 밀어
    /// 넣을 수 있고, 그러면 목록과 병합기가 서로 다른 것을 본다.
    Page {
        index: usize,
        address: String,
        before: Option<Cursor>,
    },
    /// 요청한 만큼 냈거나, 모든 스트림이 소진됐거나, 정지된(stalled) 스트림이
    /// 있어 더 진행할 수 없다. 세 경우를 구분하지 않는다 -- `Idle` 은 "다
    /// 냈다" 가 아니다. 호출자는 `is_done()` 과 `stalled_address()` 로 어느
    /// 경우인지 갈라야 한다. 그러지 않으면 대답 못 한 주소를 목록이
    /// 완결된 것으로 잘못 읽는다.
    Idle,
}

/// 주소 하나의 스트림.
struct Stream {
    address: String,
    /// 아직 안 낸 항목들, 내림차순.
    buffer: VecDeque<TxEntry>,
    /// 다음 페이지 커서. 첫 요청 전에는 `None` 이고, 그 구분은 `answered` 가 한다.
    next: Option<Cursor>,
    /// 한 번이라도 페이지를 받았는가.
    answered: bool,
    /// 노드가 이 주소에 대해 더 줄 것이 없다고 말했는가.
    finished: bool,
    /// 인덱스가 대답할 수 없다고 답했는가(`transactions: null`). 소진과 다르고
    /// 재시도 대상도 아니다 -- 다시 물어도 같은 답이 오고, 그 되물음이
    /// `app.rs` 의 재귀를 타고 노드를 무한히 두드린다.
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

    /// 모든 스트림이 최소 한 번 대답했고, 더 줄 페이지가 없다고 말했고,
    /// 버퍼도 비었다. 정지된(stalled) 스트림은 절대 이 조건을 만족하지
    /// 못한다 -- 대답 못 한 주소를 끝난 것으로 세면 그 주소의 거래가
    /// 조용히 사라진다.
    pub fn is_done(&self) -> bool {
        self.streams
            .iter()
            .all(|stream| stream.answered && stream.finished && stream.buffer.is_empty())
    }

    /// 인덱스가 대답할 수 없다고 답한 주소. 있으면 `advance` 는 계속
    /// `Need::Idle` 을 돌려주고, 그 상태는 "다 냈다"(`is_done`) 와 다르다 --
    /// 화면이 이 값을 봐서 둘을 갈라야 한다.
    pub fn stalled_address(&self) -> Option<&str> {
        self.streams
            .iter()
            .find(|stream| stream.stalled)
            .map(|stream| stream.address.as_str())
    }

    /// 가장 뒤처진 스트림의 인덱스 높이. 하나라도 아직 대답하지 않았으면
    /// `None` -- 그 주소가 최신인지 뒤처진 것인지 아직 모른다.
    pub fn lowest_index_height(&self) -> Option<u64> {
        self.streams
            .iter()
            .map(|stream| stream.index_height)
            .min()
            .flatten()
    }

    /// 직전 `Need::Page` 가 지목한 스트림에 페치 결과를 먹인다. `index` 와
    /// `before` 는 그 `Need::Page` 가 준 것과 같아야 한다.
    ///
    /// `before` 는 이 페이지를 어느 커서로 요청했는지다. 노드는 그 커서
    /// *아래*만 돌려줘야 하고(실측: 커서 (985776,0) 의 다음 페이지는
    /// (985774,0) 부터 시작한다), 그래서 커서 이상인 항목은 버린다.
    /// 이것이 없으면 `before` 를 무시하고 1페이지를 다시 내주는 노드가
    /// 이미 내보낸 행보다 큰 키로 버퍼를 채우고, `pick_highest` 가 그것을
    /// 낮은 행 아래에 붙인다 -- 중복된 데다 순서까지 틀린 목록이 오류
    /// 하나 없이 그려진다. `node_url` 은 사용자가 고칠 수 있으므로 남의
    /// 노드도 사정권이다.
    ///
    /// 버린 뒤 실제로 버퍼에 넣은 항목 수를 돌려준다. 호출자는 이 값이 0
    /// 인데 `next` 가 살아 있는 경우를 잡아야 한다 -- 페이지에 항목이
    /// 있었더라도 전부 버려졌다면 진행이 없는 것이고, 그대로 두면 같은
    /// 커서로 무한히 다시 요청한다.
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
                        // 첫 페이지(`before: None`)는 전부 받는다.
                        if (entry.height, entry.position)
                            >= (cursor.before_height, cursor.before_pos)
                        {
                            continue;
                        }
                    }
                    stream.buffer.push_back(entry);
                    kept += 1;
                }
                // `next` 가 없다는 것이 "이 주소는 끝"의 유일한 신호다.
                stream.finished = page.next.is_none();
            }
            // 인덱스가 대답할 수 없는 상태다. 빈 이력이 아니므로 소진으로
            // 세지 않는다 -- 그렇게 세면 이 주소의 거래가 조용히 사라진다.
            // 재시도도 하지 않는다: 같은 답이 돌아오고, 그 되물음이 노드를
            // 무한히 두드린다.
            None => stream.stalled = true,
        }
        kept
    }

    /// 지금까지 쌓인 행이 `want` 개에 이를 때까지 낼 수 있는 만큼 내고, 그
    /// 다음 필요를 돌려준다. `want` 는 누적 목표치다 -- 이미 낸 행 위에
    /// 더하는 증분이 아니라 `rows().len()` 이 다다라야 할 총량이다.
    /// 증분으로 읽으면 목표를 이미 채운 뒤에도 호출마다 더 요구하게 되고,
    /// 이 병합기는 Task 3 의 페치 루프에서 반복 호출되므로 그 오독이 곧
    /// 무한 요청이 된다.
    pub fn advance(&mut self, want: usize) -> Need {
        loop {
            if self.rows.len() >= want {
                return Need::Idle;
            }
            // 불변식: 버퍼가 빈 미소진 스트림이 하나라도 있으면 아무것도 내지
            // 않는다. 아직 안 받아온 주소가 더 높은 키를 들고 있을 수 있고,
            // 어기면 목록이 조용히 순서를 어긴다.
            // 대답 못 한 주소가 하나라도 있으면 아무것도 낼 수 없고, 더 조를
            // 수도 없다. 멈추고 화면이 그 사실을 말한다.
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

    /// 머리가 가장 큰 스트림. 동점은 없다 -- 같은 키는 같은 거래이고,
    /// 그 경우는 `emit` 이 접는다.
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

        // 같은 키를 든 *다른* 스트림은 같은 거래를 반대쪽에서 본 것이다.
        // 내림차순 정렬에서 같은 키는 반드시 인접하므로, 지금 머리들만 보면
        // 된다. 두 줄로 두면 같은 돈이 두 번 움직인 것처럼 읽히고, 한쪽만
        // 버리면 어느 쪽을 버려도 거짓말이 된다.
        //
        // 방금 꺼낸 스트림은 후보에서 뺀다. 한 버퍼 안에 같은 키가 둘
        // 있으면 -- `before` 를 포함(inclusive)으로 해석하는 노드가 페이지
        // 경계에서 한 항목을 되풀이하면 바로 그렇게 된다 -- 스스로와
        // 접혀서 `Internal { from: owner, to: owner }` 가 되고, 진짜 유입이
        // 자기 송금으로, 금액 대신 수수료가 값 칸에 그려진다.
        let twin = self
            .streams
            .iter()
            .enumerate()
            .position(|(other, stream)| other != index && stream.head() == Some(key));
        if let Some(other) = twin {
            // 반대쪽 사본은 버린다. 금액·수수료·시각은 같은 거래이므로
            // 이미 `entry` 에 있다.
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

        // 방향까지 봐야 한다. 상대방 이름만으로는 부족하다:
        // `MINING_REWARDS` 로 *나가는* 결제가 체인에 남을 수 있다.
        // 정규 수령자 검사는 mempool 진입에서만 하고 블록 검증에서는 일부러
        // 하지 않으며(`src/a9/blockchain.rs` 의 `admit_transaction`, RELAY-POLICY
        // 주석 "그런 블록도 완전히
        // 유효하다"), 그 문서가 예로 드는 것이 바로 `"MINING_REWARDS"` 로
        // 보내는 경우다. 그런 거래는 SENDER 로 색인돼 탐색기가
        // `direction: "out"` 으로 내주는데, 방향을 안 보면 나간 돈이 강조색과
        // 양수 금액으로, 수수료까지 감춰진 채 채굴 보상으로 그려진다.
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

    /// 병합기를 끝까지 돌린다. `pages` 는 스트림 인덱스별 페이지 큐다.
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
                    // 요청에 쓴 커서를 그대로 넘긴다 -- `accept` 가 그 아래만
                    // 받아들이므로, 이 헬퍼를 쓰는 모든 테스트가 그 여과를
                    // 덤으로 지나간다.
                    merge.accept(index, before, next);
                }
            }
        }
    }

    // 세 주소의 이력이 하나의 내림차순 목록으로 합쳐진다. 주소별로는 이미
    // 정렬돼 있지만, 그 셋을 섞는 것은 병합기의 일이다.
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

    // 같은 블록 안에서는 position 이 순서를 정한다. height 만 비교하면 같은
    // 높이의 두 거래가 임의 순서로 섞인다.
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

    // 내 주소 A -> 내 주소 B 는 같은 (height, position) 으로 두 스트림에
    // 나타난다. 두 줄로 두면 같은 돈이 두 번 움직인 것처럼 읽힌다.
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

    // 채굴 보상은 이 지갑에서 가장 흔한 행이 될 것이므로 일반 유입과 섞지 않는다.
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

    // 이 계획 전체에서 가장 중요한 테스트다. 아직 한 번도 안 받아온 스트림이
    // 있으면 어떤 행도 내보내면 안 된다 -- 그 스트림이 더 높은 키를 들고 있을
    // 수 있고, 어기면 목록이 예외도 오류도 없이 조용히 순서를 어긴다.
    #[test]
    fn no_row_is_emitted_while_a_stream_has_never_answered() {
        let mut merge = Merge::new(vec!["a".into(), "b".into()]);
        let need = merge.advance(10);
        assert!(matches!(need, Need::Page { .. }));
        assert!(merge.rows().is_empty());

        // a 만 대답했다. b 는 아직 침묵이다 -- 여전히 아무것도 못 낸다.
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
            "b 가 100번 블록의 거래를 들고 있을 수 있다"
        );

        // 이제 b 도 대답했다.
        merge.accept(1, None, page(vec![tx(100, 0, "in", "y")], None));
        merge.advance(10);
        let heights: Vec<u64> = merge.rows().iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![100, 10]);
    }

    // 한 주소가 먼저 끝나도 나머지는 계속 나와야 한다. 소진된 스트림을 계속
    // 기다리면 목록이 그 자리에서 멈춘다.
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

    // 커서는 노드가 준 것을 그대로 되돌려 보낸다. 클라이언트가 마지막 행에서
    // 다시 계산하면 내부 이체를 접은 자리에서 한 건을 건너뛴다.
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
        // 1행을 냈으니 want=2 면 더 필요하다.
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

    // 신선도는 가장 뒤처진 주소가 정한다. 하나라도 뒤처지면 목록 전체가
    // 완결됐다고 말할 수 없다.
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

    // 인덱스가 대답할 수 없는 주소(transactions: null)는 빈 이력이 아니다.
    // 소진으로 처리하면 그 주소의 거래가 조용히 사라진다. 그렇다고 계속
    // 조르면 -- 버퍼가 비었고 끝나지도 않았으니 advance 가 같은 페이지를
    // 영원히 다시 요구하고, app.rs 의 재귀가 그것을 그대로 노드에 쏜다.
    // 멈추고 말하는 것이 유일하게 맞는 답이다.
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
            "같은 페이지를 다시 요구하면 노드를 무한히 두드린다"
        );
        assert!(merge.rows().is_empty());
        assert!(
            !merge.is_done(),
            "대답 못 한 주소를 끝난 것으로 세면 안 된다"
        );
        assert_eq!(merge.stalled_address(), Some("a"));
    }

    // sender == recipient 는 실제 와이어 값이다(`node.rs` 의
    // `explorer_entry_json` 이 `"self"` 를 내고, `blockchain.rs` 의
    // `address_index_ops` 가 그 경우 한 항목에 두 플래그를 다 세운다). "self" 를 In 으로 읽으면 수수료만 태운
    // 거래가 화면에서 자기 자신에게서 돈이 도착한 것처럼 보인다.
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

    // 상대방이 `MINING_REWARDS` 라도 방향이 `out` 이면 채굴 보상이 아니다.
    // 정규 수령자 검사는 mempool 진입에서만 하고 블록 검증에서는 일부러 하지
    // 않으므로(`src/a9/blockchain.rs` 의 `admit_transaction`), `MINING_REWARDS` 로 나가는
    // 결제가 체인에 남을 수 있고 탐색기는 그것을 `direction: "out"` 으로
    // 내준다. 상대방 이름만 보면 나간 돈이 강조색·양수 금액에 수수료까지
    // 감춰진 채 "Mined" 로 그려진다 -- 돈이 나갔는데 들어온 것처럼 읽힌다.
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
            "나가는 결제를 채굴 보상으로 그리면 안 된다"
        );
    }

    // 쌍둥이 검색은 방금 꺼낸 스트림을 제외해야 한다. 한 버퍼 안에 같은 키가
    // 둘 있으면 -- `before` 를 포함으로 해석하는 노드가 페이지 경계에서 한
    // 항목을 되풀이하면 그렇게 된다 -- 스스로와 접혀 `Internal { a, a }` 가
    // 되고, 진짜 유입이 자기 송금으로 둔갑해 금액 대신 수수료가 값 칸에
    // 그려진다. 중복 자체를 없애는 것은 `accept` 의 커서 여과(아래 테스트)
    // 이고, 여기서 막는 것은 잘못된 라벨이다.
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
                "한 스트림이 자기 자신과 접히면 안 된다"
            );
        }
    }

    // `before` 를 무시하고 1페이지를 다시 내주는 노드. 커서 이상인 항목을
    // 그대로 받으면 이미 내보낸 행보다 큰 키가 버퍼에 들어가고,
    // `pick_highest` 가 그것을 낮은 행 아래에 붙인다 -- 중복에 역순인데
    // 오류는 하나도 안 난다.
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
        // 노드가 커서를 무시하고 같은 두 건을 다시 내준다.
        let kept = merge.accept(
            0,
            Some(cursor),
            page(
                vec![tx(100, 0, "in", "x"), tx(90, 0, "in", "x")],
                Some(cursor),
            ),
        );
        assert_eq!(kept, 0, "커서 이상인 항목은 하나도 받지 않는다");
        merge.advance(10);
        let keys: Vec<(u64, u32)> = merge
            .rows()
            .iter()
            .map(|r| (r.height, r.position))
            .collect();
        assert_eq!(keys, vec![(100, 0), (90, 0)], "중복도 역순도 없다");
    }

    // 전부 버려진 페이지는 항목이 있었더라도 진행이 없다. `accept` 가 0 을
    // 돌려주지 않으면 `app.rs` 의 빈-페이지 감시가 이 경우를 못 보고, 같은
    // 커서로 무한히 다시 요청한다.
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

    // `want` 는 누적 목표치다, 증분이 아니다. 버퍼에 낼 것이 두 개 더 남아
    // 있고 커서도 살아 있는 채로(스트림이 finished 도 아닌 채로) 이미 2행을
    // 낸 뒤 advance(2) 를 다시 부른다. 증분으로 읽는 구현이면 남은 두 개를
    // 마저 내서 행 수가 4로 늘거나(버퍼가 딱 맞아떨어지지 않으면) 버퍼가
    // 비고 살아 있는 커서 때문에 Need::Page 를 돌려준다 -- 어느 쪽이든
    // 누적 목표를 어긴 것이다.
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
            "누적 목표를 이미 채웠으면 다시 불러도 더 내면 안 된다"
        );
    }
}
