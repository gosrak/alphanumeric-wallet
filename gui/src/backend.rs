//! The explorer HTTP surface, and nothing else.
//!
//! No `iced` here: response parsing and error classification are the parts worth
//! testing, and they must not need a window to run.

use serde::Deserialize;

use crate::model::parse_units;

#[derive(Debug, Clone, PartialEq)]
pub struct NodeStatus {
    /// 체인이 비어 있으면 `None` (`explorer_status_handler` 의 `tip.as_ref().map`).
    pub height: Option<u64>,
    /// 서명된 비콘을 하나도 못 봤으면 `None`. 기동 직후가 그렇다.
    pub network_height: Option<u64>,
    /// 위와 짝. 둘 중 하나라도 없으면 노드가 이것도 보내지 않는다.
    pub blocks_behind: Option<u64>,
    /// 주소 색인이 쓸 수 있는 상태인가. 잔액과 이력이 여기에 달려 있다.
    pub index_ready: bool,
    pub index_height: Option<u64>,
    pub version: String,
    /// `None` when no checkpoint has been seeded yet. The node's
    /// `/explorer/status` (`node.rs`'s `explorer_status_handler`) sends this
    /// key unconditionally, using `0` as its own documented sentinel for
    /// "not yet seeded" -- NOT a real height. `parse_status` translates that
    /// sentinel to `None` at the wire boundary, so every consumer sees the
    /// same "no checkpoint" answer that `null` gives elsewhere in this file,
    /// rather than each one having to remember to special-case `0` itself.
    pub finalized_height: Option<u64>,
    /// The node omits every `mining_*` field below (and this one) entirely
    /// when it is not mining, rather than sending a false value. Treating an
    /// absent field as required would turn a healthy resting node into a
    /// parse error and blank the whole grid.
    pub mining: Option<bool>,
    pub mining_address: Option<String>,
    pub mining_backend: Option<String>,
    pub mining_hps: Option<f64>,
    pub mining_blocks: Option<u64>,
    pub mining_payout_rotation: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TxEntry {
    pub amount_units: i128,
    pub fee_units: i128,
    pub counterparty: String,
    pub direction: String,
    pub height: u64,
    pub position: u32,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct AddressState {
    pub balance_units: i128,
    /// `None` when the spendable overlay could not be computed. The node
    /// preserves this as `null` deliberately (not a zero) so a reader is not
    /// handed a plausible-looking number for an amount it does not actually
    /// know.
    pub spendable_units: Option<i128>,
    /// `None` when the address index cannot answer for this address yet --
    /// unbuilt, or mid full rebuild -- and NOT the same thing as "no
    /// history". The node serves `null` here specifically so a scanner does
    /// not read an empty page as "never used" (its own words: "so a scanner
    /// fails loudly instead of concluding wrongly").
    pub transactions: Option<Vec<TxEntry>>,
    /// Whether the index has any metadata for this address at all.
    pub history_available: bool,
    /// The wire's `index_ready` -- computed from the same check as
    /// `history_available` today, kept as its own field because the two are
    /// logically distinct questions even though currently always equal.
    pub index_ready: bool,
    /// The chain height the index had been built through as of this
    /// response, absent when there is no index metadata yet. A caller MUST
    /// compare this against the current chain height before trusting a
    /// "no history" answer: the index write is fail-open, so this can sit
    /// behind the tip for as long as the node has been running.
    pub index_height: Option<u64>,
    /// The cursor for the page after this one's `transactions`, exactly as
    /// the node sent it. `None` means this address has no older history.
    pub next: Option<Cursor>,
}

impl AddressState {
    /// This response's history as the first page of the address's stream --
    /// what the history merger consumes. Cloned: the caller keeps the state.
    pub fn as_page(&self) -> AddressPage {
        AddressPage {
            transactions: self.transactions.clone(),
            next: self.next,
            history_available: self.history_available,
            index_ready: self.index_ready,
            index_height: self.index_height,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeeEstimate {
    pub recommended_units: i128,
    pub floor_units: i128,
}

/// 지갑 전체의 성숙 대기 금액. 주소 하나라도 지출가능을 모르면 전체가
/// `None` 이다 -- 부분 합계는 "덜 익은 게 이만큼"이라는 거짓말이 된다.
///
/// 성숙 지연은 코인베이스에만 걸린다 (`src/a9/blockchain.rs` 의
/// `MINING_REWARD_MATURITY`). 일반 송금은 확정되면 바로 쓸 수 있으므로
/// `성숙 대기 = 잔액 − 지출가능`, 지갑의 모든 주소에 대해 합산한다.
pub fn maturing_units(addresses: &[(i128, Option<i128>)]) -> Option<i128> {
    let mut total: i128 = 0;
    for (balance, spendable) in addresses {
        total = total.saturating_add(balance.saturating_sub((*spendable)?));
    }
    Some(total)
}

// Wire shapes. Amounts arrive as STRINGS; the node's companion f64 fields are
// exact only below 2^53 units (~90 million coins), so the strings are what is
// read here.
#[derive(Deserialize)]
struct WireStatus {
    // 셋 다 실제로 `null` 로 온다 -- 비콘 전, 그리고 빈 체인. 필수로 두면
    // 건강한 "아직 뜨는 중" 응답이 ApiError::Malformed 가 된다. `WireAddress`
    // 가 같은 이유로 같은 선택을 이미 하고 있다.
    height: Option<u64>,
    network_height: Option<u64>,
    blocks_behind: Option<u64>,
    #[serde(default)]
    index_ready: bool,
    index_height: Option<u64>,
    version: String,
    finalized_height: Option<u64>,
    mining: Option<bool>,
    mining_address: Option<String>,
    mining_backend: Option<String>,
    mining_hps: Option<f64>,
    mining_blocks: Option<u64>,
    mining_payout_rotation: Option<bool>,
}

#[derive(Deserialize)]
struct WireTx {
    amount_units: String,
    fee_units: String,
    counterparty: String,
    direction: String,
    height: u64,
    position: u32,
    timestamp: u64,
}

#[derive(Deserialize)]
struct WireCursor {
    before_height: u64,
    before_pos: u32,
}

#[derive(Deserialize)]
struct WireAddress {
    balance_units: String,
    // Both `null` on the wire in real, non-error cases (an unbuilt/rebuilding
    // index, or an unresolvable spendable overlay) -- see `AddressState`.
    // Treating either as required is how a healthy "still indexing" response
    // becomes `ApiError::Malformed` instead of the non-answer it actually is.
    spendable_units: Option<String>,
    transactions: Option<Vec<WireTx>>,
    history_available: bool,
    index_ready: bool,
    index_height: Option<u64>,
    next: Option<WireCursor>,
}

#[derive(Deserialize)]
struct WireFee {
    recommended_fee_units: String,
    floor_fee_units: String,
}

pub fn parse_status(json: &str) -> Result<NodeStatus, String> {
    let wire: WireStatus =
        serde_json::from_str(json).map_err(|e| format!("Unexpected status response: {e}"))?;
    Ok(NodeStatus {
        height: wire.height,
        network_height: wire.network_height,
        blocks_behind: wire.blocks_behind,
        index_ready: wire.index_ready,
        index_height: wire.index_height,
        version: wire.version,
        // 노드는 이 필드를 생략하지 않고 0 을 보낸다. 그 0 은 "0번 블록까지
        // 확정"이 아니라 "아직 체크포인트가 없다"는 뜻이다 (`explorer_status_handler`
        // 의 "0 means not yet seeded"). 경계에서 None 으로 바꿔 두면 소비자마다
        // 0 을 기억해 특수 처리할 필요가 없다 -- 한 곳만 잊어도 "0번 블록까지
        // 확정"이라는 거짓이 화면에 샌다.
        finalized_height: wire.finalized_height.filter(|h| *h != 0),
        mining: wire.mining,
        mining_address: wire.mining_address,
        mining_backend: wire.mining_backend,
        // 같은 모양의 센티널이다: 노드는 채굴을 켜기 직전에 `HPS=0` 을 먼저
        // 적는다(`miner.rs` 의 `session_started`, `node.rs` 의
        // `mining_status_json` 문서 "채굴 중인데 아직 측정 전"). 그 0 을
        // 여기서 None 으로 바꾸면 띠는 이미 있는 `MINING ON` 갈래로 간다.
        mining_hps: wire.mining_hps.filter(|h| *h != 0.0),
        mining_blocks: wire.mining_blocks,
        mining_payout_rotation: wire.mining_payout_rotation,
    })
}

pub fn parse_address_state(json: &str) -> Result<AddressState, String> {
    let wire: WireAddress =
        serde_json::from_str(json).map_err(|e| format!("Unexpected address response: {e}"))?;
    let transactions = match wire.transactions {
        Some(entries) => {
            let mut parsed = Vec::with_capacity(entries.len());
            for entry in entries {
                parsed.push(TxEntry {
                    amount_units: parse_units(&entry.amount_units)?,
                    fee_units: parse_units(&entry.fee_units)?,
                    counterparty: entry.counterparty,
                    direction: entry.direction,
                    height: entry.height,
                    position: entry.position,
                    timestamp: entry.timestamp,
                });
            }
            Some(parsed)
        }
        None => None,
    };
    let spendable_units = wire
        .spendable_units
        .as_deref()
        .map(parse_units)
        .transpose()?;
    Ok(AddressState {
        balance_units: parse_units(&wire.balance_units)?,
        spendable_units,
        transactions,
        history_available: wire.history_available,
        index_ready: wire.index_ready,
        index_height: wire.index_height,
        next: wire.next.map(|c| Cursor {
            before_height: c.before_height,
            before_pos: c.before_pos,
        }),
    })
}

/// The node's next-page cursor. Round-tripped exactly as received -- never
/// recomputed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub before_height: u64,
    pub before_pos: u32,
}

/// One page of history. Unlike `AddressState`, this carries a cursor rather
/// than a balance.
#[derive(Debug, Clone)]
pub struct AddressPage {
    pub transactions: Option<Vec<TxEntry>>,
    pub next: Option<Cursor>,
    pub history_available: bool,
    pub index_ready: bool,
    pub index_height: Option<u64>,
}

pub fn parse_address_page(json: &str) -> Result<AddressPage, String> {
    let wire: WireAddress =
        serde_json::from_str(json).map_err(|e| format!("Unexpected address response: {e}"))?;
    let transactions = match wire.transactions {
        Some(entries) => {
            let mut parsed = Vec::with_capacity(entries.len());
            for entry in entries {
                parsed.push(TxEntry {
                    amount_units: parse_units(&entry.amount_units)?,
                    fee_units: parse_units(&entry.fee_units)?,
                    counterparty: entry.counterparty,
                    direction: entry.direction,
                    height: entry.height,
                    position: entry.position,
                    timestamp: entry.timestamp,
                });
            }
            Some(parsed)
        }
        None => None,
    };
    Ok(AddressPage {
        transactions,
        next: wire.next.map(|c| Cursor {
            before_height: c.before_height,
            before_pos: c.before_pos,
        }),
        history_available: wire.history_available,
        index_ready: wire.index_ready,
        index_height: wire.index_height,
    })
}

pub fn parse_fee_estimate(json: &str) -> Result<FeeEstimate, String> {
    let wire: WireFee =
        serde_json::from_str(json).map_err(|e| format!("Unexpected fee response: {e}"))?;
    let floor_units = parse_units(&wire.floor_fee_units)?;
    // `parse_units` accepts any i128, and this number is then arithmetic on a
    // render path and the lower bound of every fee this wallet signs. A floor
    // above the fee this wallet will ever sign is not a relay policy -- the
    // node's own CLI would refuse to pay it either -- so refuse the estimate
    // rather than carry a number that makes every payment unsendable and
    // overflows the dust-threshold computation on the way.
    if !(0..=crate::tx::FEE_SAFETY_LIMIT_UNITS).contains(&floor_units) {
        return Err(format!(
            "The node reported a relay floor of {floor_units} units, which is outside the \
             range any real one has ({} at most). Not using this estimate.",
            crate::tx::FEE_SAFETY_LIMIT_UNITS
        ));
    }
    Ok(FeeEstimate {
        recommended_units: parse_units(&wire.recommended_fee_units)?,
        floor_units,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeStats {
    pub peers: Option<u64>,
    pub hashrate_ths: Option<f64>,
    pub difficulty: Option<f64>,
    pub uptime_secs: Option<u64>,
    /// 아래 셋은 노드가 아직 안 낼 수 있다(계획 Task 3 이 더한다). 없으면
    /// 그 칸만 `—` 이고 나머지 격자는 산다.
    pub mempool: Option<u64>,
    pub avg_block_time_secs: Option<f64>,
    pub block_reward: Option<f64>,
}

#[derive(Deserialize)]
struct WireStats {
    // 전부 Option: 노드 버전에 따라 없을 수 있고, 있어도 null 일 수 있다.
    // null 을 0 으로 접으면 "피어 0"(고립됐다)과 "모른다"가 같아진다.
    peers: Option<u64>,
    hashrate_ths: Option<f64>,
    difficulty: Option<f64>,
    uptime_secs: Option<u64>,
    mempool: Option<u64>,
    avg_block_time_secs: Option<f64>,
    block_reward: Option<f64>,
}

pub fn parse_stats(json: &str) -> Result<NodeStats, String> {
    let wire: WireStats =
        serde_json::from_str(json).map_err(|e| format!("Unexpected stats response: {e}"))?;
    Ok(NodeStats {
        peers: wire.peers,
        // The node sends these as bare numbers, never `null` -- but it sends
        // an honest `0.0` for both on an empty chain (no tip to compute a
        // windowed hashrate or a block reward from: `calculate_network_hashrate`
        // and `current_block_reward` in `blockchain.rs` both return `0.0` when
        // `get_last_block()` is `None`). That is the same shape of sentinel
        // `parse_status` already filters out of `finalized_height` -- a fresh
        // install with an empty chain must not show a confident "0.00 TH/s"
        // or "0.00 ALPHA" beside an honest `HEIGHT —`.
        hashrate_ths: wire.hashrate_ths.filter(|h| *h != 0.0),
        difficulty: wire.difficulty,
        uptime_secs: wire.uptime_secs,
        mempool: wire.mempool,
        avg_block_time_secs: wire.avg_block_time_secs,
        block_reward: wire.block_reward.filter(|r| *r != 0.0),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supply {
    pub height: u64,
    pub supply_units: i128,
}

#[derive(Deserialize)]
struct WireSupply {
    height: u64,
    // A STRING on the wire (`explorer_supply_handler` sends
    // `supply_units.to_string()`), exactly like `balance_units` on
    // `/explorer/address` -- not a bare JSON integer. Read the same way as
    // every other amount in this file.
    supply_units: String,
}

pub fn parse_supply(json: &str) -> Result<Supply, String> {
    let wire: WireSupply =
        serde_json::from_str(json).map_err(|e| format!("Unexpected supply response: {e}"))?;
    Ok(Supply {
        height: wire.height,
        supply_units: parse_units(&wire.supply_units)?,
    })
}

/// What went wrong, in the terms the UI has to act on.
///
/// Whether waiting is the right response is `is_retryable()`, a question for
/// the CALLER. It is deliberately not in the `Display` text: this type has no
/// idea whether the call site it is being rendered at has a retry scheduled,
/// and only one of them does (`PollTick`, which never renders these strings at
/// all -- it sets a flag). See `is_retryable`.
#[derive(Debug, Clone)]
pub enum ApiError {
    /// The chain lock was contended. Waiting is the right response, but the
    /// wait is the caller's to schedule.
    Busy,
    /// The node's submit bucket is empty. As `Busy`, but wait longer.
    RateLimited,
    /// Transaction validation refused this. Show it to the user.
    Rejected(String),
    /// HTTP 409: the node refused because something ALREADY EXISTS -- a key
    /// bound to a different transaction, byte-identical bytes from another
    /// withdrawal, or a payment that reached the node by some other route.
    ///
    /// Never a plain failure. Every one of these means a payment matching this
    /// one may already be on the chain, so the caller must reconcile rather
    /// than re-sign, and rendering it as an ordinary rejection is how someone
    /// is told to send a payment they have already made.
    ///
    /// `status` is the node's machine-readable token, kept SEPARATE from
    /// `message`. The node sends BOTH fields on every one of these bodies, and
    /// `message` prefers the human prose -- which is documentation text that
    /// will be reworded. A caller that branches on the prose is a caller that
    /// silently stops branching.
    Conflict {
        status: Option<String>,
        message: String,
        /// The response body verbatim.
        ///
        /// These bodies carry the identifiers a person needs to look the
        /// payment up -- `tx_id`, `original_tx_id`, `colliding_tx_id`,
        /// `height`, `existing_status` -- and telling someone to check without
        /// them leaves them with nothing to check against. Which fields
        /// identify a payment is not something this layer knows, so the body
        /// is carried whole and the screen that understands payments picks
        /// them out.
        body: String,
    },
    /// The request never reached the handler. A bug on this side.
    Malformed(String),
    /// The node was reached and answered 5xx: it failed INSIDE, after the
    /// request arrived. Kept apart from `Transport` because the two send a
    /// reader to different places -- "could not reach the node" points at DNS,
    /// ports and firewalls, and none of those are wrong when the node itself
    /// answered. Not retryable: a 500 is a fault to look at, not a wait.
    NodeFault(String),
    /// The request never reached the node at all: connection refused, DNS,
    /// TLS, or a timeout with no response.
    Transport(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // No "Retrying." here. These strings are only ever SHOWN where
            // nothing retries -- the send screen's unresolved outcome, the
            // spendable re-check in `ceiling_from_fetch`, the fee error, and the
            // restore scan -- and each of those already says what the user's
            // next move is. The one place that does retry on its own
            // (`PollTick`) renders a flag, not this text. Promising an
            // automatic retry in the type would put the promise on every screen
            // that has none, which is the same lie `scheduled_retry` exists to
            // keep out of the address rows.
            ApiError::Busy => write!(f, "The node is busy and did not answer this."),
            ApiError::RateLimited => write!(
                f,
                "The node is rate limiting requests and did not answer this one."
            ),
            ApiError::Rejected(message) | ApiError::Conflict { message, .. } => {
                write!(f, "{message}")
            }
            ApiError::Malformed(message) => {
                write!(f, "The node could not read the request: {message}")
            }
            ApiError::NodeFault(message) => {
                write!(
                    f,
                    "The node answered but failed while handling this: {message}"
                )
            }
            ApiError::Transport(message) => write!(f, "Could not reach the node: {message}"),
        }
    }
}

impl ApiError {
    /// True when waiting is the right response. A UI that shows these as
    /// failures teaches the operator to distrust a wallet that is working.
    ///
    /// This is the machine-readable half of what the old `Display` wording
    /// asserted in prose. A caller that has a retry scheduled may say so; one
    /// that does not must not, and the string alone cannot tell them apart.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ApiError::Busy | ApiError::RateLimited)
    }
}

/// Turn an HTTP response into something the UI can act on.
///
/// The Content-Type carries the distinction the status code loses: a 400 whose
/// body parses as JSON came from transaction validation and the user must read
/// it; a 400 in plain text came from the HTTP layer before the handler ran and
/// is a bug on this side. `EXPLORER_API.md` says to key off the type, not the
/// status.
pub fn classify(status: u16, content_type: Option<&str>, body: &str) -> ApiError {
    let is_json = content_type
        .map(|value| value.to_ascii_lowercase().contains("application/json"))
        .unwrap_or(false);

    let parsed = if is_json {
        serde_json::from_str::<serde_json::Value>(body).ok()
    } else {
        None
    };
    // The machine-readable token, extracted IN ITS OWN RIGHT. It used to be
    // reachable only as a fallback for `message`, so a body carrying both
    // fields -- which every protected-endpoint conflict does -- lost it
    // entirely, and a caller branching on it silently stopped branching.
    let status_token = parsed
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    // The human-readable string still prefers the prose and falls back to the
    // token: that ordering is right for something a person reads, and it is
    // not a substitute for the token above.
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("error")
                .or_else(|| value.get("status"))
                .and_then(|field| field.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.trim().to_string());
    let message = if message.is_empty() {
        format!("The node returned status {status} with no detail.")
    } else {
        message
    };

    match status {
        503 => ApiError::Busy,
        429 => ApiError::RateLimited,
        // Routed on the STATUS CODE, not on the token inside the body, so a
        // conflict this wallet has never heard of still fails closed as a
        // conflict rather than as an ordinary rejection the user is invited
        // to retry. The token rides along for the caller that has wording for
        // it.
        409 => ApiError::Conflict {
            status: status_token,
            message,
            body: body.to_string(),
        },
        // A 5xx is the node failing, not the user's transaction being refused.
        // Framing it as a rejection tells someone to fix a payment that was
        // never the problem -- the same trust cost the retryable cases avoid.
        // It is `NodeFault` rather than `Transport` because the node WAS
        // reached: rendering it as "could not reach the node" sends the reader
        // to check a port that is demonstrably open.
        500..=599 => ApiError::NodeFault(message),
        _ if is_json => ApiError::Rejected(message),
        _ => ApiError::Malformed(message),
    }
}

/// Months in the one date format HTTP requires a server to GENERATE
/// (RFC 9110's IMF-fixdate). The two obsolete formats a client must merely
/// tolerate are not accepted: a header this parser cannot read reports "clock
/// unknown", which is a truthful answer, where guessing at a loose format
/// would risk reporting a wrong offset as a real one.
const HTTP_DATE_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days from 1970-01-01 to a civil date, for proleptic Gregorian dates
/// (Howard Hinnant's `days_from_civil`). Exact integer arithmetic: no
/// dependency, and nothing to drift.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parse an HTTP `Date` header into unix seconds.
///
/// This is the node's OWN wall clock, read from the machine that will check
/// the timestamp window on a submitted transaction -- which is exactly the
/// clock a send screen has to compare against before it signs anything
/// (spec 4.3). `None` for anything this cannot read with certainty.
pub fn parse_http_date(value: &str) -> Option<u64> {
    // "Tue, 08 Sep 2026 00:28:44 GMT" -- the weekday is redundant with the
    // date itself, so it is dropped rather than checked for agreement.
    let rest = value.split_once(',')?.1;
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    let month = HTTP_DATE_MONTHS
        .iter()
        .position(|candidate| *candidate == month_name)? as i64
        + 1;
    if !(1..=31).contains(&day) || !(1970..=9999).contains(&year) {
        return None;
    }
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // A leap second (`:60`) is folded onto :59 rather than rejected: one
    // second of error is immaterial against a 5-minute future window, and
    // refusing would report "clock unknown" for a header that is perfectly
    // readable.
    let seconds_of_day = hour * 3_600 + minute * 60 + second.min(59);
    u64::try_from(days_from_civil(year, month, day) * 86_400 + seconds_of_day).ok()
}

/// A response body, together with how far the node's clock sits from ours.
pub struct Fetched {
    pub body: String,
    /// The node's clock minus this computer's, in seconds, measured at the
    /// moment the response arrived. Positive means the node is ahead.
    ///
    /// `None` when the response carried no readable `Date`. The difference is
    /// carried rather than the node's raw time because the OFFSET is what
    /// stays meaningful for the rest of a compose session, while a captured
    /// absolute time goes stale the moment it is stored.
    pub clock_offset: Option<i64>,
}

/// This computer's clock in unix seconds. `None` only if the system clock is
/// set before 1970, which is not a state this wallet tries to work around.
pub fn local_unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// A client for one node's explorer API.
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base_url: &str) -> Result<Self, String> {
        let trimmed = base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() || !trimmed.starts_with("http") {
            return Err("Enter a node address like http://127.0.0.1:8095".to_string());
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Could not start the HTTP client: {e}"))?;
        Ok(Self {
            base: trimmed.to_string(),
            http,
        })
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    async fn get(&self, path: &str) -> Result<Fetched, ApiError> {
        self.get_absolute(&self.endpoint(path)).await
    }

    /// As `get`, but `url` is used exactly as given rather than joined to
    /// `self.base` -- for the node's `/stats` server, which lives on a
    /// **different port** from the explorer.
    async fn get_absolute(&self, url: &str) -> Result<Fetched, ApiError> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        // Read before the body is drained, and paired with the local clock
        // right here: an offset measured further downstream would fold in
        // however long the caller took to get around to it.
        let node_unix = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_http_date);
        let clock_offset = node_unix.and_then(|node| {
            let local = local_unix_now()?;
            i64::try_from(node)
                .ok()
                .zip(i64::try_from(local).ok())
                .map(|(node, local)| node - local)
        });
        let body = response
            .text()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;

        if (200..300).contains(&status) {
            Ok(Fetched { body, clock_offset })
        } else {
            Err(classify(status, content_type.as_deref(), &body))
        }
    }

    pub async fn status(&self) -> Result<NodeStatus, ApiError> {
        let fetched = self.get("explorer/status").await?;
        parse_status(&fetched.body).map_err(ApiError::Malformed)
    }

    /// 노드의 stats 서버. 익스플로러와 **다른 포트**라 base 를 따로 받는다.
    pub async fn stats(&self, base: &str) -> Result<NodeStats, ApiError> {
        let url = format!("{}/stats", base.trim_end_matches('/'));
        let fetched = self.get_absolute(&url).await?;
        parse_stats(&fetched.body).map_err(ApiError::Malformed)
    }

    pub async fn supply(&self) -> Result<Supply, ApiError> {
        let fetched = self.get("explorer/supply").await?;
        parse_supply(&fetched.body).map_err(ApiError::Malformed)
    }

    pub async fn address(&self, address: &str) -> Result<AddressState, ApiError> {
        let fetched = self.get(&format!("explorer/address/{address}")).await?;
        parse_address_state(&fetched.body).map_err(ApiError::Malformed)
    }

    /// One page of history. `before` is the cursor the node handed back on
    /// the prior page, echoed back exactly; the client never manufactures
    /// one.
    ///
    /// `limit` is sent explicitly: even though 50 matches the node's own
    /// default, that default is the node's to change, and a page size that
    /// quietly changed would quietly change the merger's buffering behaviour
    /// too. The node clamps to 1..=200 (`explorer_address_handler`), so anything outside
    /// that range would be silently truncated -- enforced here first.
    pub async fn address_page(
        &self,
        address: &str,
        limit: u32,
        before: Option<Cursor>,
    ) -> Result<AddressPage, ApiError> {
        let limit = limit.clamp(1, 200);
        let path = match before {
            Some(cursor) => format!(
                "explorer/address/{address}?limit={limit}&before_height={}&before_pos={}",
                cursor.before_height, cursor.before_pos
            ),
            None => format!("explorer/address/{address}?limit={limit}"),
        };
        let fetched = self.get(&path).await?;
        parse_address_page(&fetched.body).map_err(ApiError::Malformed)
    }

    pub async fn fee_estimate(&self) -> Result<FeeEstimate, ApiError> {
        Ok(self.fee_estimate_with_clock().await?.0)
    }

    /// The fee recommendation, plus the node's clock offset from the same
    /// response.
    ///
    /// One request answers both of the send screen's pre-signing questions
    /// (spec 4.3 steps 2 and the clock-skew warning), and the two readings are
    /// then guaranteed to describe the same moment.
    pub async fn fee_estimate_with_clock(&self) -> Result<(FeeEstimate, Option<i64>), ApiError> {
        let fetched = self.get("explorer/fee-estimate").await?;
        let estimate = parse_fee_estimate(&fetched.body).map_err(ApiError::Malformed)?;
        Ok((estimate, fetched.clock_offset))
    }

    /// POST a prepared submission body to the protected endpoint.
    ///
    /// **The caller supplies the idempotency key**, inside `body`, alongside the
    /// signed transaction -- this function does not generate one. That is why
    /// `/v2/` is used: it lets the NODE tell a retry apart from a colliding
    /// second payment, where `/v1/`'s `already_pending` cannot, because the tx id
    /// is a pure function of the five signed fields and matches either way.
    /// The node REQUIRES the key: a body without one is refused with a 422
    /// naming the missing field, so a caller that forgets it fails loudly rather
    /// than silently getting `/v1/` behaviour at a `/v2/` URL.
    ///
    /// A 2xx here is not automatically a success in the wallet's sense. The node
    /// answers 200 with `status` of `accepted`, `already_pending` or
    /// `already_confirmed`, and the caller has to read it -- this returns the raw
    /// body precisely so that decision stays with the screen that knows what it
    /// asked for.
    pub async fn submit(&self, body: &serde_json::Value) -> Result<String, ApiError> {
        let response = self
            .http
            .post(self.endpoint("explorer/v2/submit-tx"))
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response
            .text()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;

        if (200..300).contains(&status) {
            Ok(text)
        } else {
            Err(classify(status, content_type.as_deref(), &text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maturing_is_balance_minus_spendable_across_addresses() {
        assert_eq!(
            maturing_units(&[(1000, Some(400)), (50, Some(50))]),
            Some(600)
        );
    }

    /// 한 주소의 지출가능을 모르면 합계도 모른다. 그 주소를 빼고 더하면
    /// "덜 익은 게 이만큼"이라고 확언하게 되는데 그건 아는 바가 아니다.
    #[test]
    fn one_unknown_spendable_makes_the_whole_total_unknown() {
        assert_eq!(maturing_units(&[(1000, Some(400)), (50, None)]), None);
    }

    #[test]
    fn nothing_maturing_is_zero_not_none() {
        assert_eq!(maturing_units(&[(1000, Some(1000))]), Some(0));
    }

    // Captured verbatim from a running node, not hand-written. The shapes that
    // matter and are easy to get wrong: *_units are STRINGS, and `next` is null
    // when there is no more history.
    const STATUS_JSON: &str = r#"{"blocks_behind":0,"finality_margin":64,"finalized_height":959431,"height":959513,"index_height":959513,"index_ready":true,"network_height":959513,"network_id":"66b4","ok":true,"tip_hash":"0000","uptime_secs":1059950,"version":"8.0.0"}"#;

    const ADDRESS_JSON: &str = r#"{"address":"84dab431b53e6522fe2e74914eec99f17758f4e3","balance":1234567.89012345,"balance_units":"123456789012345","history_available":true,"index_height":959513,"index_ready":true,"next":{"before_height":959511,"before_pos":0},"spendable":1234000.00000001,"spendable_units":"123400000000001","summary":{"transactions":98765},"transactions":[{"amount":10.0,"amount_units":"1000000000","counterparty":"MINING_REWARDS","direction":"in","fee":0.0005,"fee_units":"50000","height":959513,"position":0,"timestamp":1788816861}]}"#;

    // An address nobody has used still returns 200 with zeros and next: null --
    // NOT a 404. Address discovery depends on that, so it is pinned here.
    const EMPTY_ADDRESS_JSON: &str = r#"{"address":"0000000000000000000000000000000000000000","balance":0.0,"balance_units":"0","history_available":true,"index_height":959516,"index_ready":true,"next":null,"spendable":0.0,"spendable_units":"0","summary":{"transactions":0},"transactions":[]}"#;

    // An unbuilt or mid-rebuild index: `history_available`/`index_ready` are
    // false, `index_height`, `spendable`/`spendable_units`, `summary` and
    // `transactions` are all `null` -- captured from the node's own
    // documented behaviour (node.rs's address handler), not invented. A
    // healthy response like this must still parse; it is a real "cannot
    // answer yet", not malformed input.
    const REBUILDING_ADDRESS_JSON: &str = r#"{"address":"0000000000000000000000000000000000000000","balance":0.0,"balance_units":"0","history_available":false,"index_height":null,"index_ready":false,"next":null,"spendable":null,"spendable_units":null,"summary":null,"transactions":null}"#;

    const FEE_JSON: &str = r#"{"anchor_fee":0.0002,"anchor_fee_units":"20000","auto_cap_fee":0.002,"auto_cap_fee_units":"200000","basis":"quiet","congested":false,"explicit_cap_fee":0.01,"explicit_cap_fee_units":"1000000","floor_fee":0.0001,"floor_fee_units":"10000","next_block_fits":0,"pending_candidates":0,"recommended_fee":0.0002,"recommended_fee_units":"20000"}"#;

    #[test]
    fn status_parses() {
        let status = parse_status(STATUS_JSON).expect("captured status must parse");
        assert_eq!(status.height, Some(959_513));
        assert_eq!(status.network_height, Some(959_513));
        assert_eq!(status.blocks_behind, Some(0));
        assert_eq!(status.version, "8.0.0");
    }

    /// 기동 직후 노드가 실제로 보내는 모양. 비콘을 보기 전까지
    /// network_height 와 blocks_behind 가 null 이다 (`explorer_status_handler`).
    /// 이것을 Malformed 로 취급하면 동기화 화면이 첫 폴링부터 오류를 그린다.
    const STATUS_JSON_NO_BEACON: &str = r#"{"blocks_behind":null,"finality_margin":64,"finalized_height":0,"height":12,"index_height":12,"index_ready":true,"network_height":null,"network_id":"66b4","ok":true,"tip_hash":"0000","uptime_secs":3,"version":"8.0.0"}"#;

    #[test]
    fn a_status_without_a_beacon_parses_instead_of_failing() {
        let status = parse_status(STATUS_JSON_NO_BEACON).expect("a beaconless status is normal");
        assert_eq!(status.height, Some(12));
        assert_eq!(status.network_height, None);
        assert_eq!(status.blocks_behind, None);
        assert!(status.index_ready);
        assert_eq!(status.index_height, Some(12));
    }

    #[test]
    fn a_status_on_an_empty_chain_parses_too() {
        let json = r#"{"blocks_behind":null,"height":null,"index_height":null,"index_ready":false,"network_height":null,"ok":true,"version":"8.0.0"}"#;
        let status = parse_status(json).expect("an empty chain is normal at first boot");
        assert_eq!(status.height, None);
        assert!(!status.index_ready);
    }

    #[test]
    fn a_full_status_still_carries_the_index_fields() {
        let status = parse_status(STATUS_JSON).expect("valid");
        assert!(status.index_ready);
        assert_eq!(status.index_height, Some(959_513));
    }

    // `*_units` is the field to read because it is exact by construction: the
    // node also sends `balance` as a decimal f64, and that path is only correct
    // while the value stays under 2^53 units (about 90 million coins). It does
    // today -- this balance is 1.2 million coins, and a sweep of two million
    // realistic amounts found no divergence between the two paths -- so this is
    // not a bug being prevented, it is a boundary being kept far away for free.
    #[test]
    fn address_units_are_read_from_the_exact_field() {
        let state = parse_address_state(ADDRESS_JSON).expect("captured address must parse");
        assert_eq!(state.balance_units, 123_456_789_012_345);
        assert_eq!(state.spendable_units, Some(123_400_000_000_001));
    }

    #[test]
    fn address_transactions_parse() {
        let state = parse_address_state(ADDRESS_JSON).expect("parse");
        let transactions = state.transactions.expect("history is available");
        assert_eq!(transactions.len(), 1);
        let entry = &transactions[0];
        assert_eq!(entry.amount_units, 1_000_000_000);
        assert_eq!(entry.fee_units, 50_000);
        assert_eq!(entry.counterparty, "MINING_REWARDS");
        assert_eq!(entry.direction, "in");
        assert_eq!(entry.height, 959_513);
        assert_eq!(entry.timestamp, 1_788_816_861);
    }

    /// `/explorer/address` sends the same `next` cursor a history page does.
    /// Dropping it made the first page useless as a stream: the merger reads
    /// "no cursor" as "this address is finished" and would call a 50-row
    /// first page the complete history (`activity::recent_rows`).
    #[test]
    fn an_address_state_keeps_the_next_cursor_and_converts_to_a_page() {
        let json = r#"{"balance_units":"5","spendable_units":"5","transactions":[{"amount_units":"5","fee_units":"1","counterparty":"MINING_REWARDS","direction":"in","height":10,"position":0,"timestamp":1}],"history_available":true,"index_ready":true,"index_height":10,"next":{"before_height":10,"before_pos":0}}"#;
        let state = parse_address_state(json).expect("valid");
        assert_eq!(
            state.next,
            Some(Cursor {
                before_height: 10,
                before_pos: 0
            })
        );
        let page = state.as_page();
        assert_eq!(page.next, state.next);
        assert_eq!(page.transactions.as_ref().map(Vec::len), Some(1));
        assert_eq!(page.index_height, Some(10));
        assert!(page.history_available && page.index_ready);
    }

    // Trimmed from a real /explorer/address response. `position` is always on
    // the wire, and without it `before_pos` cannot be formed, so a page could
    // not be turned.
    const ADDRESS_PAGE_FIXTURE: &str = r#"{
        "address": "711f763bdacfc0e118a7479f3d705c2429f81859",
        "balance_units": "3873816499623",
        "spendable_units": "3849816499623",
        "history_available": true,
        "index_ready": true,
        "index_height": 984090,
        "next": { "before_height": 983961, "before_pos": 0 },
        "transactions": [
            { "amount_units": "1000000000", "fee_units": "50000",
              "counterparty": "MINING_REWARDS", "direction": "in",
              "height": 984144, "position": 0, "timestamp": 1788951090 },
            { "amount_units": "250000000", "fee_units": "50000",
              "counterparty": "abc0000000000000000000000000000000000001", "direction": "out",
              "height": 984142, "position": 3, "timestamp": 1788951001 }
        ]
    }"#;

    #[test]
    fn an_address_page_carries_position_and_the_next_cursor() {
        let page = parse_address_page(ADDRESS_PAGE_FIXTURE).expect("fixture parses");
        let txs = page.transactions.expect("the fixture has transactions");
        assert_eq!(txs[0].position, 0);
        assert_eq!(txs[1].position, 3);
        assert_eq!(txs[1].height, 984142);
        assert_eq!(
            page.next,
            Some(Cursor {
                before_height: 983961,
                before_pos: 0
            })
        );
        assert_eq!(page.index_height, Some(984090));
    }

    // A last page has no `next`. That absence is the only signal that this
    // address is done.
    //
    // The live node sends `"next": null` explicitly -- verified against the
    // running node -- so that is the shape under test here. The omitted-key
    // shape is asserted too, one line, because `Option` treats the two the
    // same and nothing should quietly start depending on which one arrives.
    #[test]
    fn a_last_page_has_no_cursor() {
        let json = r#"{
            "balance_units": "0",
            "spendable_units": "0",
            "history_available": true,
            "index_ready": true,
            "index_height": 10,
            "next": null,
            "transactions": []
        }"#;
        let page = parse_address_page(json).expect("parses");
        assert_eq!(page.next, None);
        assert_eq!(page.transactions.as_deref(), Some(&[][..]));

        let omitted = r#"{
            "balance_units": "0",
            "spendable_units": "0",
            "history_available": true,
            "index_ready": true,
            "index_height": 10,
            "transactions": []
        }"#;
        assert_eq!(parse_address_page(omitted).expect("parses").next, None);
    }

    // When the index does not exist yet, the node gives `transactions` as
    // null. That is a different meaning from an empty array -- an empty
    // array is "no history", null is "cannot answer".
    #[test]
    fn a_null_transactions_field_is_not_an_empty_history() {
        let json = r#"{
            "balance_units": "0",
            "spendable_units": null,
            "history_available": false,
            "index_ready": false,
            "index_height": null,
            "transactions": null
        }"#;
        let page = parse_address_page(json).expect("parses");
        assert!(page.transactions.is_none());
        assert!(!page.history_available);
    }

    // Discovery scans indices until it finds unused ones. If an unused address
    // 404'd, the scan would have to treat an error as "empty" and could not tell
    // that apart from a node being down.
    #[test]
    fn an_unused_address_is_a_success_with_zeros() {
        let state = parse_address_state(EMPTY_ADDRESS_JSON).expect("empty address must parse");
        assert_eq!(state.balance_units, 0);
        assert_eq!(state.spendable_units, Some(0));
        assert_eq!(state.transactions.as_ref().map(Vec::len), Some(0));
        assert!(state.history_available);
        assert!(state.index_ready);
        assert_eq!(state.index_height, Some(959_516));
    }

    // The node serves this shape while its address index is unbuilt or
    // mid-rebuild -- a real, healthy response, not an error. `transactions`
    // and `spendable_units` arriving as `null` must parse as `None`, not fail
    // the whole response: before this was fixed, a non-optional `Vec`/`String`
    // here turned "still indexing" into `ApiError::Malformed`, and a restore's
    // discovery scan would abort on the very first index against any node
    // that had not finished indexing yet.
    #[test]
    fn an_unbuilt_index_parses_as_unanswered_rather_than_failing() {
        let state =
            parse_address_state(REBUILDING_ADDRESS_JSON).expect("a null index must still parse");
        assert_eq!(state.balance_units, 0);
        assert_eq!(state.spendable_units, None);
        assert!(state.transactions.is_none());
        assert!(!state.history_available);
        assert!(!state.index_ready);
        assert_eq!(state.index_height, None);
    }

    #[test]
    fn fee_estimate_parses() {
        let fee = parse_fee_estimate(FEE_JSON).expect("captured fee estimate must parse");
        assert_eq!(fee.recommended_units, 20_000);
        assert_eq!(fee.floor_units, 10_000);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_status("{").is_err());
        assert!(parse_address_state("null").is_err());
        assert!(parse_fee_estimate(r#"{"recommended_fee_units":"not a number"}"#).is_err());
    }

    // The node uses Content-Type to say what kind of failure this is: a 400 that
    // parses as JSON came from transaction validation and the user needs to read
    // it; a 400 in plain text came from the HTTP layer before the handler ran and
    // means the request was malformed. Keying off the status alone conflates them.
    #[test]
    fn a_json_400_is_a_rejection_and_a_plain_400_is_malformed() {
        let rejected = classify(
            400,
            Some("application/json"),
            r#"{"error":"transaction rejected: below the relay floor"}"#,
        );
        assert!(matches!(rejected, ApiError::Rejected(ref m) if m.contains("relay floor")));

        let malformed = classify(400, Some("text/plain; charset=utf-8"), "bad body");
        assert!(matches!(malformed, ApiError::Malformed(_)));
    }

    // `floor_units` is the lower bound of every fee this wallet signs and the
    // input to a dust threshold computed on a render path. `parse_units` accepts
    // any i128, so an absurd floor used to travel all the way in. A floor above
    // the most this wallet will ever sign is not a relay policy -- the node's own
    // CLI could not pay it either -- so the estimate is refused rather than
    // carried.
    #[test]
    fn an_impossible_relay_floor_is_refused_rather_than_carried() {
        let with_floor = |floor: &str| {
            format!(
                r#"{{"recommended_fee_units":"20000","floor_fee_units":"{floor}","recommended_fee":0.0002,"floor_fee":0.0001}}"#
            )
        };

        assert_eq!(
            parse_fee_estimate(&with_floor("10000"))
                .expect("the real node's floor")
                .floor_units,
            10_000
        );
        // The boundary itself is still accepted: this rejects the impossible,
        // not a node that has raised its floor to the wallet's own ceiling.
        assert_eq!(
            parse_fee_estimate(&with_floor(&crate::tx::FEE_SAFETY_LIMIT_UNITS.to_string()))
                .expect("a floor at the wallet's own ceiling")
                .floor_units,
            crate::tx::FEE_SAFETY_LIMIT_UNITS
        );

        for impossible in [
            (crate::tx::FEE_SAFETY_LIMIT_UNITS + 1).to_string(),
            i128::MAX.to_string(),
        ] {
            let error = parse_fee_estimate(&with_floor(&impossible))
                .expect_err("an impossible floor must not be carried");
            assert!(
                error.contains("relay floor"),
                "the error must name what it refused: {error}"
            );
        }
    }

    // Whether a retry is coming is a fact about the CALL SITE, and only one
    // call site has one. `Display` is rendered on the send screen's unresolved
    // outcome, on the fee error and in the restore scan -- none of which
    // schedule anything -- so a string that says "Retrying." is a promise the
    // screen showing it cannot keep. The retryability is carried by
    // `is_retryable()` instead, which the poller reads and the send screen does
    // not.
    #[test]
    fn no_error_text_promises_a_retry_that_the_caller_has_not_scheduled() {
        for error in [
            ApiError::Busy,
            ApiError::RateLimited,
            ApiError::Rejected("below the relay floor".into()),
            ApiError::Malformed("missing field".into()),
            ApiError::NodeFault("boom".into()),
            ApiError::Transport("connection refused".into()),
            ApiError::Conflict {
                status: Some("idempotency_conflict".into()),
                message: "already bound".into(),
                body: "{}".into(),
            },
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(
                !rendered.contains("retrying") && !rendered.contains("will retry"),
                "{error:?} renders as {rendered:?}, which promises a retry no call site schedules"
            );
        }
        // The fact itself did not move -- it is just no longer in the prose.
        assert!(ApiError::Busy.is_retryable());
        assert!(ApiError::RateLimited.is_retryable());
    }

    // 503 and 429 are "later", not "failed". A UI that shows them as errors
    // teaches the operator to distrust a wallet that is working correctly.
    #[test]
    fn busy_and_rate_limited_are_retryable_and_nothing_else_is() {
        assert!(
            classify(503, Some("application/json"), r#"{"error":"chain busy"}"#).is_retryable()
        );
        assert!(
            classify(429, Some("application/json"), r#"{"error":"rate_limited"}"#).is_retryable()
        );

        assert!(!classify(
            400,
            Some("application/json"),
            r#"{"error":"transaction rejected: x"}"#
        )
        .is_retryable());
        assert!(!classify(422, Some("text/plain"), "missing field").is_retryable());
        assert!(!classify(
            409,
            Some("application/json"),
            r#"{"ok":false,"status":"idempotency_conflict","error":"this idempotency_key is already bound to a different transaction"}"#
        )
        .is_retryable());
        // A 5xx moved from `Transport` to `NodeFault`; it was not retryable
        // before and must not have become retryable by moving.
        assert!(!classify(500, Some("application/json"), r#"{"error":"boom"}"#).is_retryable());
        assert!(!ApiError::Transport("connection refused".into()).is_retryable());
        assert!(!ApiError::NodeFault("boom".into()).is_retryable());
    }

    // A 500 means the node WAS reached and then failed inside itself. Saying
    // "could not reach the node" for it sends the reader off to check DNS, a
    // port and a firewall, none of which can be wrong when the node answered.
    // The two states get separate variants so the wording cannot be shared
    // back together by accident.
    #[test]
    fn a_node_that_answered_is_never_described_as_unreachable() {
        let fault = classify(
            500,
            Some("application/json"),
            r#"{"error":"index poisoned"}"#,
        );
        let rendered = fault.to_string();
        assert!(
            !rendered.to_ascii_lowercase().contains("reach"),
            "a 5xx must not be worded as unreachable: {rendered}"
        );
        assert!(rendered.contains("index poisoned"));

        // The genuine case keeps the wording that is right for it.
        assert!(ApiError::Transport("connection refused".into())
            .to_string()
            .contains("Could not reach the node"));
    }

    // A 422 with a plain-text body is what the live node actually returns for a
    // malformed submit -- pinned from an observed response, not invented.
    #[test]
    fn the_observed_422_shape_classifies_as_malformed() {
        let error = classify(
            422,
            Some("text/plain; charset=utf-8"),
            "Failed to deserialize the JSON body into the target type: missing field `idempotency_key`",
        );
        assert!(matches!(error, ApiError::Malformed(_)));
    }

    // The seam this variant exists to close. Every protected-endpoint conflict
    // body carries BOTH a machine-readable `status` and a human `error`, and
    // the message picks `error` first -- correctly, for something a person
    // reads. When the token was reachable only as that pick's FALLBACK, a body
    // with both fields lost it entirely, and a caller branching on the token
    // silently stopped branching. Both fields are pinned here, together, on a
    // body copied from `src/a9/node.rs`.
    #[test]
    fn a_conflict_keeps_its_status_token_even_when_the_body_also_explains_itself() {
        let error = classify(
            409,
            Some("application/json"),
            r#"{"ok":false,"status":"transaction_collision","idempotency_key":"k","colliding_tx_id":"t","error":"another withdrawal key already submitted byte-identical transaction bytes"}"#,
        );
        let ApiError::Conflict {
            status,
            message,
            body,
        } = error
        else {
            panic!("a 409 is a conflict, not an ordinary rejection: {error:?}");
        };
        assert_eq!(status.as_deref(), Some("transaction_collision"));
        assert!(message.contains("byte-identical"), "{message}");
        // The body is carried whole so the screen can pull out the
        // identifiers a person needs to look the payment up.
        assert!(body.contains(r#""colliding_tx_id":"t""#), "{body}");

        // A 409 is routed on the STATUS CODE, so one with a token this wallet
        // has never seen -- or with no token at all -- is still a conflict.
        for body in [
            r#"{"ok":false,"status":"some_future_conflict","error":"prose"}"#,
            r#"{"ok":false,"error":"prose only"}"#,
        ] {
            assert!(
                matches!(
                    classify(409, Some("application/json"), body),
                    ApiError::Conflict { .. }
                ),
                "{body} must fail closed as a conflict"
            );
        }
        // And a 400 is not swept up with them: it really is an ordinary
        // rejection the user can correct.
        assert!(matches!(
            classify(
                400,
                Some("application/json"),
                r#"{"error":"transaction rejected: below the relay floor"}"#
            ),
            ApiError::Rejected(_)
        ));
    }

    // A rejection message reaches the user, so it must not be empty when the body
    // is JSON without the field we expect.
    #[test]
    fn a_json_body_without_an_error_field_still_says_something() {
        let error = classify(400, Some("application/json"), r#"{"unexpected":true}"#);
        match error {
            ApiError::Rejected(message) | ApiError::Malformed(message) => {
                assert!(!message.trim().is_empty())
            }
            other => panic!("expected a message-bearing variant, got {other:?}"),
        }
    }

    // A 5xx is the node failing. Presenting it as a rejection would tell someone
    // to fix a payment that was never wrong -- and a framework that always emits
    // JSON error bodies makes that the default outcome without this arm.
    #[test]
    fn server_errors_are_not_presented_as_rejections() {
        for status in [500u16, 502, 504] {
            let error = classify(status, Some("application/json"), r#"{"error":"upstream"}"#);
            assert!(
                matches!(error, ApiError::NodeFault(_)),
                "status {status} should read as a node fault, got {error:?}"
            );
        }
        // 503 keeps its own meaning: busy, and retryable.
        assert!(
            classify(503, Some("application/json"), r#"{"error":"chain busy"}"#).is_retryable()
        );
    }

    // A base URL with or without a trailing slash must produce the same endpoint.
    // Getting this wrong yields a 404 that looks like a node problem.
    #[test]
    fn endpoints_are_built_consistently() {
        for base in ["http://127.0.0.1:8095", "http://127.0.0.1:8095/"] {
            let client = Client::new(base).expect("valid base");
            assert_eq!(
                client.endpoint("explorer/status"),
                "http://127.0.0.1:8095/explorer/status"
            );
            assert_eq!(
                client.endpoint("explorer/address/abc"),
                "http://127.0.0.1:8095/explorer/address/abc"
            );
        }
    }

    // The node requires before_height and before_pos together, and sends a
    // 400 if only one arrives. Without a test pinning the assembly, that 400
    // would only show up at runtime.
    #[test]
    fn a_history_page_url_sends_both_cursor_halves_or_neither() {
        let client = Client::new("http://127.0.0.1:8095").expect("valid base");
        assert_eq!(
            client.endpoint("explorer/address/abc?limit=50"),
            "http://127.0.0.1:8095/explorer/address/abc?limit=50"
        );
        assert_eq!(
            client.endpoint("explorer/address/abc?limit=50&before_height=7&before_pos=2"),
            "http://127.0.0.1:8095/explorer/address/abc?limit=50&before_height=7&before_pos=2"
        );
    }

    #[test]
    fn an_unusable_base_url_is_refused_at_construction() {
        assert!(Client::new("").is_err());
        assert!(Client::new("not a url").is_err());
    }

    // The node's `Date` header is the only reading of the clock that will
    // actually judge a transaction's timestamp window, so the send screen
    // warns off it before signing. A parser that quietly returned a WRONG
    // instant would be worse than none: it would report a skew that is not
    // there, or hide one that is. Expectations here are external -- computed
    // with `date -u -d ... +%s`, not from this code.
    #[test]
    fn the_node_clock_reads_exactly_off_an_http_date() {
        // RFC 9110's own IMF-fixdate example.
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(
            parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(0),
            "the epoch itself must not be confused with a parse failure"
        );
        // A leap day, and a century year that IS a leap year.
        assert_eq!(
            parse_http_date("Tue, 29 Feb 2000 12:00:00 GMT"),
            Some(951_825_600)
        );
        // The day after a century year that is NOT a leap year.
        assert_eq!(
            parse_http_date("Mon, 01 Mar 2100 00:00:00 GMT"),
            Some(4_107_542_400)
        );
        // Captured from this repository's own running node.
        assert_eq!(
            parse_http_date("Tue, 08 Sep 2026 00:28:44 GMT"),
            Some(1_788_827_324)
        );
    }

    // Anything this cannot read with certainty reports "clock unknown", which
    // the screen shows as a missing check rather than a passing one. The two
    // obsolete HTTP date formats are deliberately among these: a server is
    // required to generate IMF-fixdate, and a loose parser that half-read
    // `06-Nov-94` would be a source of wrong offsets, not extra coverage.
    #[test]
    fn an_unreadable_date_is_no_reading_rather_than_a_wrong_one() {
        for value in [
            "",
            "GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",  // RFC 850, obsolete
            "Sun Nov  6 08:49:37 1994",        // asctime, obsolete
            "Sun, 06 Nov 1994 08:49:37",       // no zone
            "Sun, 06 Nov 1994 08:49:37 +0100", // not GMT
            "Sun, 06 Xyz 1994 08:49:37 GMT",   // no such month
            "Sun, 06 Nov 1994 24:49:37 GMT",   // hour out of range
            "Sun, 06 Nov 1994 08:60:37 GMT",   // minute out of range
            "Sun, 06 Nov 1994 08:49 GMT",      // no seconds
            "Sun, 32 Nov 1994 08:49:37 GMT",   // day out of range
            "Sun, 06 Nov 1969 08:49:37 GMT",   // before the epoch
            "Sun, 06 Nov 1994 08:49:37 GMT extra",
        ] {
            assert_eq!(
                parse_http_date(value),
                None,
                "{value:?} must read as no clock at all"
            );
        }
    }

    /// 채굴 중이 아니면 노드가 mining_* 를 아예 보내지 않는다(생략 규칙).
    /// 그것을 Malformed 로 취급하면 안 캐는 노드에서 격자가 통째로 죽는다.
    #[test]
    fn a_status_from_a_node_that_is_not_mining_still_parses() {
        let json = r#"{"ok":true,"height":10,"network_height":10,"blocks_behind":0,"index_ready":true,"index_height":10,"finalized_height":9,"version":"8.0.0","mining":false}"#;
        let s = parse_status(json).expect("a resting node is normal");
        assert_eq!(s.mining, Some(false));
        assert_eq!(s.mining_hps, None);
        assert_eq!(s.finalized_height, Some(9));
    }

    /// 회전 중이면 mining_address 한 줄이 거짓이 된다 -- 그 사실이 파싱되어야
    /// 화면이 거짓말을 피할 수 있다.
    #[test]
    fn a_rotating_payout_is_carried_through() {
        let json = r#"{"ok":true,"height":10,"network_height":10,"blocks_behind":0,"index_ready":true,"version":"8.0.0","mining":true,"mining_hps":2.78e10,"mining_payout_rotation":true}"#;
        let s = parse_status(json).expect("valid");
        assert_eq!(s.mining_payout_rotation, Some(true));
        assert_eq!(s.mining_hps, Some(2.78e10));
    }

    /// 노드는 채굴을 시작하는 순간 `HPS=0` 을 먼저 적고 `MINING=true` 를 켠다
    /// (`miner.rs` 의 `session_started`). 그 `0` 은 노드 자신의 문서대로
    /// "채굴 중인데 아직 측정 전"이지 잰 값이 아니다. 띠가 `MINING 0.0 GH/s`
    /// 라고 하면 "채굴기가 죽었다"로 읽힌다 -- 측정 전은 `MINING ON` 이다.
    #[test]
    fn a_hashrate_not_yet_measured_is_absent_not_zero() {
        let json = r#"{"ok":true,"height":10,"network_height":10,"blocks_behind":0,"index_ready":true,"version":"8.0.0","mining":true,"mining_hps":0,"mining_payout_rotation":false}"#;
        let s = parse_status(json).expect("valid");
        assert_eq!(
            s.mining,
            Some(true),
            "still mining -- only the rate is unknown"
        );
        assert_eq!(
            s.mining_hps, None,
            "0 is the node's 'not measured yet', not a measured zero"
        );
    }

    #[test]
    fn an_unseeded_finality_checkpoint_is_absent_not_block_zero() {
        let json = r#"{"ok":true,"height":1005522,"network_height":1005522,"blocks_behind":0,"index_ready":true,"index_height":1005522,"finalized_height":0,"version":"8.0.0"}"#;
        let s = parse_status(json).expect("valid");
        assert_eq!(
            s.finalized_height, None,
            "0 is the node's sentinel for 'no checkpoint yet', not a real height"
        );
    }

    /// 손으로 쓴 예시다(`node.rs`의 실제 `stats_handler`는 `ok` 를 보내지
    /// 않고, `difficulty`/`peers`/`uptime_secs` 는 정수, `version` 은
    /// `"rust-<NETWORK_VERSION>"` 형태다) -- 여기서 못박는 것은 "노드
    /// 버전에 따라 세 필드가 아직 없을 수 있고, 그래도 파싱되어야 한다"는
    /// 생략 규칙이지 이 바이트 그대로의 캡처가 아니다. 이것이 Malformed 가
    /// 되면 Task 3 을 배포하기 전까지 격자가 통째로 죽는다.
    const STATS_TODAY: &str = r#"{"ok":true,"height":1005522,"difficulty":464.0,"hashrate_ths":27.8,"peers":11,"uptime_secs":86400,"version":"8.0.0"}"#;

    #[test]
    fn a_stats_response_without_the_new_fields_still_parses() {
        let s = parse_stats(STATS_TODAY).expect("today's node");
        assert_eq!(s.peers, Some(11));
        assert_eq!(s.hashrate_ths, Some(27.8));
        assert_eq!(s.difficulty, Some(464.0));
        assert_eq!(s.uptime_secs, Some(86_400));
        assert_eq!(s.mempool, None);
        assert_eq!(s.avg_block_time_secs, None);
        assert_eq!(s.block_reward, None);
    }

    #[test]
    fn a_stats_response_with_the_new_fields_parses_them() {
        let json = r#"{"ok":true,"height":1,"difficulty":464.0,"hashrate_ths":27.8,"peers":11,"uptime_secs":1,"version":"8.0.1","mempool":42,"avg_block_time_secs":22.3,"block_reward":50.0}"#;
        let s = parse_stats(json).expect("tomorrow's node");
        assert_eq!(s.mempool, Some(42));
        assert_eq!(s.avg_block_time_secs, Some(22.3));
        assert_eq!(s.block_reward, Some(50.0));
    }

    /// null 은 "잴 수 없었다"이고 0 이 아니다. 피어가 null 인데 0 으로
    /// 읽으면 "고립됐다"는 거짓말을 하게 된다.
    #[test]
    fn nulls_stay_none_rather_than_becoming_zero() {
        let json =
            r#"{"ok":true,"peers":null,"hashrate_ths":null,"difficulty":null,"uptime_secs":null}"#;
        let s = parse_stats(json).expect("nulls are normal");
        assert_eq!(s.peers, None);
        assert_eq!(s.hashrate_ths, None);
    }

    #[test]
    fn a_non_json_stats_body_is_an_error_not_a_panic() {
        assert!(parse_stats("<html>nope</html>").is_err());
    }

    /// An empty chain has no tip to compute a hashrate from, and the node
    /// sends its honest `0.0` for it rather than `null`. That `0.0` must read
    /// as absent, the same as `finalized_height`'s `0` -- otherwise a fresh
    /// install shows "0.00 TH/s" next to an honest `HEIGHT —`.
    #[test]
    fn an_empty_chain_hashrate_is_absent_not_block_zero() {
        let json = r#"{"ok":true,"height":0,"difficulty":1.0,"hashrate_ths":0.0,"peers":0,"uptime_secs":1,"version":"8.0.0"}"#;
        let s = parse_stats(json).expect("valid");
        assert_eq!(
            s.hashrate_ths, None,
            "0.0 is what an empty chain reports, not a measured zero hashrate"
        );
    }

    /// Same sentinel, for the block reward: `current_block_reward()` returns
    /// `0.0` when there is no tip to reward, not because the reward really is
    /// zero.
    #[test]
    fn an_empty_chain_block_reward_is_absent_not_block_zero() {
        let json = r#"{"ok":true,"height":0,"difficulty":1.0,"peers":0,"uptime_secs":1,"version":"8.0.0","block_reward":0.0}"#;
        let s = parse_stats(json).expect("valid");
        assert_eq!(
            s.block_reward, None,
            "0.0 is what an empty chain reports, not a real zero-ALPHA reward"
        );
    }

    #[test]
    fn supply_is_read_as_units() {
        // `supply_units` is a STRING on the wire (`node.rs`'s
        // `explorer_supply_handler` sends `supply_units.to_string()`, exactly
        // like `balance_units`/`spendable_units` on `/explorer/address`), not
        // a bare JSON integer -- confirmed by reading the handler, since the
        // node cannot be run here.
        let json = r#"{"ok":true,"height":1005522,"supply":"4021000.00000000","supply_units":"402100000000000"}"#;
        let s = parse_supply(json).expect("valid supply");
        assert_eq!(s.height, 1_005_522);
        assert_eq!(s.supply_units, 402_100_000_000_000);
    }
}
