//! Send screen: the one place in this wallet where money moves.
//!
//! Everything here that decides whether a payment is safe to sign is a pure
//! function with no `iced` in it -- `parse_coin_amount`, `prepare_payment`,
//! `clock_check`, `build_submission`, `verdict` -- because a defect in any of
//! them signs a wrong payment against a live chain, and a widget tree is not
//! somewhere that can be tested.
//!
//! The order of the checks is spec 4.3's order, and it is deliberate: the
//! spendable ceiling is established BEFORE a signature exists, not after, so
//! there is never a signed transaction lying around that the wallet has since
//! decided it should not have made.

use iced::widget::{column, container, row, text, text_input};
use iced::{Element, Length};

use alphanumeric_gui::backend::{AddressState, ApiError, FeeEstimate};
use alphanumeric_gui::{model, tx};

use crate::app::{
    AddressEntry, AddressSource, App, Message, Screen, SendStage, SendState, Spendable, WalletState,
};
use crate::theme;
use crate::view::kit;

/// Build the body for `/explorer/v2/submit-tx`.
///
/// The signed string and the JSON numbers are derived from the same integer
/// units. A float between them is how a signature stops matching the message the
/// node re-derives, and the transaction is rejected with nothing obviously wrong.
pub fn build_submission(
    seed: &[u8; 32],
    sender: &str,
    recipient: &str,
    amount_units: i128,
    fee_units: i128,
    timestamp: u64,
) -> serde_json::Value {
    use alphanumeric_gui::tx;

    let message = tx::signing_message(sender, recipient, amount_units, fee_units, timestamp);
    let signature = tx::sign_message(seed, message.as_bytes());
    let public_key = tx::public_key_from_seed(seed);

    serde_json::json!({
        "idempotency_key": uuid::Uuid::new_v4().to_string(),
        "transaction": {
            "sender": sender,
            "recipient": recipient,
            "amount": wire_amount(amount_units),
            "fee": wire_amount(fee_units),
            "timestamp": timestamp,
            "signature": hex::encode(&signature),
            "pub_key": hex::encode(&public_key),
            "sig_hash": tx::sig_hash(&signature),
        }
    })
}

/// The wire's decimal `amount`/`fee` field, derived from the SAME integer
/// units the signature was taken over. The float is the documented wire
/// format, and the node re-quantises it back to units on arrival.
///
/// The parse is total over every string `units_to_amount_string` can produce
/// -- it emits `-?\d+\.\d{8}` and nothing else -- and
/// `the_wire_float_is_total_over_what_this_app_can_sign` pins that rather than
/// asserting it in a comment. The unreachable branch still yields `null`
/// rather than `0.0` on purpose: `0.0` is a perfectly valid amount, so it
/// would ship a body that signs for one figure and transmits another, and the
/// node would refuse a signature that is in fact correct. `null` cannot
/// deserialise into the transaction shape at all, so the node names the field
/// it could not read and nothing ambiguous is ever admitted.
fn wire_amount(units: i128) -> serde_json::Value {
    match tx::units_to_amount_string(units).parse::<f64>() {
        Ok(value) => serde_json::json!(value),
        Err(_) => serde_json::Value::Null,
    }
}

/// The node's own `Transaction::to_units` (`src/a9/blockchain.rs`), replicated
/// so this screen can verify the round trip it depends on rather than assume
/// it. The node reads the wire float, quantises it with exactly this, and then
/// re-derives the signed string from the result -- so if this does not return
/// the units that were signed for, the signature cannot match.
fn node_units_from_wire(amount: f64) -> i128 {
    const SCALE: f64 = 100_000_000.0;
    if !amount.is_finite() {
        return 0;
    }
    (((amount * SCALE).round() / SCALE) * SCALE).round() as i128
}

/// True when the float the node will read re-quantises to exactly the integer
/// units this wallet signed for.
///
/// A PROXY for the property that has to hold, not that property itself. What
/// has to hold is that the node's RE-DERIVED signed string equals the one this
/// wallet signed: the node reads the wire float, quantises it with `to_units`,
/// and then rebuilds the amount field as `format!("{:.8}", from_units(units))`
/// (`src/a9/blockchain.rs::get_message`). This checks only the first half of
/// that -- `units -> f64 -> units` -- and stops before the formatting step.
///
/// The proxy suffices across everything reachable here, which is a measured
/// claim and not an assumed one. `the_proxy_is_the_node_s_own_answer_up_to_2_53`
/// pins the two ends of it: the two agree at 2^53 units (90071992.54740992
/// coins), and the first amount at which this returns true while the node's
/// re-derived string differs is 9007199254740996 units -- four units past
/// 2^53, where an f64 stops carrying consecutive integers at all. Sampling
/// 10^8 amounts spread over `[1, 2^53]` found no disagreement anywhere below
/// it.
///
/// The gap is also the safe direction, which is why it suffices rather than
/// merely being far away. Where the proxy is a false positive the node
/// re-derives a DIFFERENT amount string, so the signature does not verify and
/// the payment is refused whole. It cannot be admitted for the wrong amount --
/// the failure is a rejection, not a mis-sent payment.
///
/// It is still checked per payment rather than inferred from that range: the
/// alternative to checking is discovering at the far end of an i128 that a
/// signature over `90071992.54740993` is being verified against a float that
/// no longer says that.
fn survives_the_wire(units: i128) -> bool {
    match tx::units_to_amount_string(units).parse::<f64>() {
        Ok(value) => node_units_from_wire(value) == units,
        Err(_) => false,
    }
}

/// Why an amount as typed is not a number of coins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountProblem {
    Empty,
    NotANumber,
    TooPrecise,
    TooLarge,
}

/// Read an amount typed in COINS into exact atomic units.
///
/// Integer arithmetic throughout, with no float anywhere on the path (spec 4.3
/// step 4): these units are what both the signed string and the JSON number
/// are derived from, so a rounding step here would put the two out of step
/// before anything else in this file had a chance to.
///
/// Accepts `\d+` or `\d+.\d{1,8}` after trimming, and nothing else. A partial
/// entry like `1.` or `.5` is refused rather than guessed at -- mid-typing the
/// Review button is simply not offered yet, which costs nothing, where
/// guessing at an unfinished number costs a wrong payment.
pub fn parse_coin_amount(input: &str) -> Result<i128, AmountProblem> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AmountProblem::Empty);
    }
    let (whole_text, fraction_text) = match trimmed.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (trimmed, ""),
    };
    // No second point, no sign, no exponent, no separators. `i128::from_str`
    // would take `+5` and `-5`, and a stray character silently flipping an
    // amount's meaning is exactly what this refuses to allow.
    if whole_text.is_empty()
        || !whole_text.bytes().all(|byte| byte.is_ascii_digit())
        || (trimmed.contains('.') && fraction_text.is_empty())
        || !fraction_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(AmountProblem::NotANumber);
    }
    let scale_digits = tx::MONEY_SCALE.to_string().len() - 1;
    if fraction_text.len() > scale_digits {
        return Err(AmountProblem::TooPrecise);
    }
    let mut padded = fraction_text.to_string();
    while padded.len() < scale_digits {
        padded.push('0');
    }
    let whole: i128 = whole_text.parse().map_err(|_| AmountProblem::TooLarge)?;
    let fraction: i128 = if padded.is_empty() {
        0
    } else {
        padded.parse().map_err(|_| AmountProblem::TooLarge)?
    };
    whole
        .checked_mul(tx::MONEY_SCALE)
        .and_then(|units| units.checked_add(fraction))
        .ok_or(AmountProblem::TooLarge)
}

/// The recipient in the only form the node treats as canonical: 40 characters
/// of lowercase hex (`is_canonical_user_address`, `src/a9/blockchain.rs`).
///
/// The case fold is applied to the input rather than to the signed string:
/// the signature is taken over the recipient verbatim, so lowercasing anywhere
/// downstream of `signing_message` would sign one address and send another.
/// Here it happens before anything is prepared, and the confirmation screen
/// shows the folded form, so what the user approves is what gets signed.
pub fn normalise_recipient(input: &str) -> Option<String> {
    let candidate = input.trim().to_ascii_lowercase();
    let canonical = candidate.len() == 40
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    canonical.then_some(candidate)
}

/// Why this payment cannot be signed, in the order the checks run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// The wallet has never had a spendable figure for this address.
    SpendableUnknown,
    /// The node answered but could not compute the spendable overlay.
    SpendableUnavailable,
    NoRecipient,
    BadRecipient,
    SelfPayment,
    Amount(AmountProblem),
    ZeroAmount,
    NoFeeEstimate,
    /// `clamp_fee_units` found no fee that is both at or above the node's
    /// relay floor and below the whisper band -- the amount is dust.
    ///
    /// Carries the floor that was in force, because the smallest sendable
    /// amount is a function of it and the explanation quotes that number. A
    /// fixed 0.00000889 in the text would tell someone to send an amount that
    /// is still dust on a node that raised its floor.
    DustAmount {
        floor_units: i128,
    },
    /// The amount cannot survive the wire's decimal round trip intact.
    NotRepresentable,
    InsufficientSpendable {
        total_units: i128,
        spendable_units: i128,
    },
}

impl Blocker {
    /// What to tell the user, in terms of the thing they can change.
    pub fn explain(&self) -> String {
        match self {
            // Neither of the two non-answers is read as a number. Treating
            // either as zero would block every payment; treating either as
            // "fine" would sign one the node is going to refuse. The only
            // honest answer is that the ceiling is not known right now.
            Blocker::SpendableUnknown => "This wallet does not know yet how much of this \
                 address can be spent -- the node has not answered with that figure. It is \
                 the ceiling a payment is checked against, so nothing can be signed until \
                 it arrives."
                .to_string(),
            Blocker::SpendableUnavailable => "The node answered, but could not work out how \
                 much of this address is spendable -- its address index is still building. \
                 That figure is the ceiling a payment is checked against, so nothing can be \
                 signed until the node can produce it. This normally clears on its own."
                .to_string(),
            Blocker::NoRecipient => "Enter the address to pay.".to_string(),
            Blocker::BadRecipient => "An address is 40 hexadecimal characters. Check what \
                 was pasted -- a truncated address is not a payment that can be recovered."
                .to_string(),
            Blocker::SelfPayment => "That is this address itself. Paying it moves nothing \
                 and still spends the fee."
                .to_string(),
            Blocker::Amount(AmountProblem::Empty) => "Enter an amount in coins.".to_string(),
            Blocker::Amount(AmountProblem::NotANumber) => {
                "Enter an amount in coins, like 1.5.".to_string()
            }
            Blocker::Amount(AmountProblem::TooPrecise) => "One coin divides into 100,000,000 \
                 units, so an amount carries at most 8 decimal places."
                .to_string(),
            Blocker::Amount(AmountProblem::TooLarge) => {
                "That amount is larger than this wallet can represent.".to_string()
            }
            Blocker::ZeroAmount => "Enter an amount greater than zero.".to_string(),
            Blocker::NoFeeEstimate => "The fee has not been read from the node yet. A \
                 payment is not signed without it -- a guessed fee either never propagates \
                 or is read as a message."
                .to_string(),
            Blocker::DustAmount { floor_units } => format!(
                "{} coins is the smallest amount that can carry a valid fee at this node's \
                 relay floor. Below it every fee high enough to be relayed already lands \
                 inside the whisper band, where the payment is displayed as a four-letter \
                 message instead. Send more.",
                model::format_coins(tx::smallest_sendable_units(*floor_units))
            ),
            Blocker::NotRepresentable => "That amount cannot be written on the wire without \
                 losing a unit, so the signature would not match what the node reads back. \
                 Send it in smaller parts."
                .to_string(),
            Blocker::InsufficientSpendable {
                total_units,
                spendable_units,
            } => format!(
                "This costs {} coins with the fee, and only {} is spendable from this \
                 address right now. Mining rewards that have not matured and payments \
                 already pending are not spendable.",
                model::format_coins(*total_units),
                model::format_coins(*spendable_units),
            ),
        }
    }
}

/// A payment that has passed every check and is ready to be confirmed. Still
/// unsigned: this is arithmetic, and holding it does not commit anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub sender: String,
    /// Where the sender's signing key comes from, not its position in the
    /// address list. The signing seed comes from this, and the two are
    /// deliberately distinct types in this codebase.
    pub sender_source: AddressSource,
    pub recipient: String,
    pub amount_units: i128,
    pub fee_units: i128,
    pub total_units: i128,
}

/// Everything spec 4.3 asks for before a signature exists, in its order.
///
/// The two states that mean "the spendable ceiling is not known" come first,
/// before anything the user typed is even looked at: they are not caused by
/// the input and there is no point in reporting a recipient problem to
/// someone whose wallet cannot check a payment at all.
pub fn prepare_payment(
    sender: &str,
    sender_source: AddressSource,
    spendable: Spendable,
    recipient_input: &str,
    amount_input: &str,
    fee_estimate: Option<&FeeEstimate>,
) -> Result<Prepared, Blocker> {
    // 1. The ceiling, or a reason there is none.
    let spendable_units = match spendable {
        Spendable::Pending => return Err(Blocker::SpendableUnknown),
        Spendable::Unavailable => return Err(Blocker::SpendableUnavailable),
        Spendable::Known(units) => units,
    };

    // 2. Who is being paid.
    if recipient_input.trim().is_empty() {
        return Err(Blocker::NoRecipient);
    }
    let recipient = normalise_recipient(recipient_input).ok_or(Blocker::BadRecipient)?;
    if recipient == sender {
        return Err(Blocker::SelfPayment);
    }

    // 3. How much, quantised to integer units first (spec 4.3 step 4).
    let amount_units = parse_coin_amount(amount_input).map_err(Blocker::Amount)?;
    if amount_units <= 0 {
        return Err(Blocker::ZeroAmount);
    }

    // 4. The fee: the node's recommendation, put through the clamp against the
    //    node's own relay floor and against this wallet's absolute safety
    //    limit. BOTH wire numbers come from the same estimate -- the
    //    recommendation and the floor are one answer, and taking the
    //    recommendation from the node while clamping against a compiled-in
    //    floor is how a relay-policy change stops reaching this wallet. The
    //    safety limit is the one bound that is deliberately NOT the node's to
    //    set; see `tx::FEE_SAFETY_LIMIT_UNITS`. `None` from the clamp is not
    //    "use the recommendation unclamped": it means no valid fee exists for
    //    this amount at all.
    let estimate = fee_estimate.ok_or(Blocker::NoFeeEstimate)?;
    let floor_units = estimate.floor_units;
    let fee_units =
        tx::clamp_fee_units(amount_units, estimate.recommended_units, Some(floor_units))
            .ok_or(Blocker::DustAmount { floor_units })?;

    // The node itself uses a checked add here (`has_valid_regular_amounts`);
    // matching it means a total that would overflow is refused on this side
    // rather than wrapping into a small, plausible-looking number.
    let total_units = amount_units
        .checked_add(fee_units)
        .ok_or(Blocker::Amount(AmountProblem::TooLarge))?;

    if !survives_the_wire(amount_units) || !survives_the_wire(fee_units) {
        return Err(Blocker::NotRepresentable);
    }

    // 5. The ceiling, applied. `spendable` is already net of immature mining
    //    rewards and pending debits, and the node admits on
    //    `confirmed - pending >= amount + fee`, so the fee belongs inside the
    //    comparison rather than beside it.
    if total_units > spendable_units {
        return Err(Blocker::InsufficientSpendable {
            total_units,
            spendable_units,
        });
    }

    Ok(Prepared {
        sender: sender.to_string(),
        sender_source,
        recipient,
        amount_units,
        fee_units,
        total_units,
    })
}

/// The ceiling to check a payment against, taken from an address fetch made
/// for that purpose alone, or a reason there is no ceiling to check against.
///
/// This exists because the ten-second poll covers only the ACTIVE address.
/// A payment prepared from index 0 while index 1 is active would otherwise be
/// re-checked against whatever index 0's row happened to be holding, which
/// could be minutes old and could predate another payment entirely. A check
/// that reads a stale row is not the check requirement 1 asks for -- it is the
/// appearance of it, which is worse, because it is the guarantee this whole
/// screen advertises.
///
/// An error is never folded back onto the cached number: "the node could not
/// be asked" and "the node said you have this much" are not interchangeable,
/// and the first one is a refusal to sign, not a licence to use the second.
pub fn ceiling_from_fetch(result: &Result<AddressState, ApiError>) -> Result<Spendable, String> {
    match result {
        // The same three-state reading the poller uses: a null overlay is a
        // real answer meaning "cannot compute", not a zero and not an error.
        Ok(state) => Ok(match state.spendable_units {
            Some(units) => Spendable::Known(units),
            None => Spendable::Unavailable,
        }),
        Err(error) => Err(format!(
            "Nothing was signed: the node could not be asked how much is spendable from this \
             address right now. {error}"
        )),
    }
}

/// Re-check, at the instant of signing, the one input that can have moved
/// while a confirmation sat on screen.
///
/// The spendable figure is fetched fresh for this check (see
/// `ceiling_from_fetch`); everything else in a `Prepared` is frozen arithmetic
/// the user has already approved. Re-running the whole preparation here would
/// silently swap in a newer fee estimate and sign for something other than
/// what was confirmed.
pub fn recheck_spendable(prepared: &Prepared, spendable: Spendable) -> Result<(), Blocker> {
    match spendable {
        Spendable::Pending => Err(Blocker::SpendableUnknown),
        Spendable::Unavailable => Err(Blocker::SpendableUnavailable),
        Spendable::Known(units) if prepared.total_units > units => {
            Err(Blocker::InsufficientSpendable {
                total_units: prepared.total_units,
                spendable_units: units,
            })
        }
        Spendable::Known(_) => Ok(()),
    }
}

/// Beyond this the two clocks are far enough apart to be worth saying so.
/// Well above the second of granularity an HTTP date carries plus any
/// plausible round trip, so an in-step pair never trips it.
pub const CLOCK_WARN_SECS: i64 = 60;

/// The node will not include a transaction dated further ahead than this
/// (`MAX_BLOCK_FUTURE_TIME`, `src/a9/blockchain.rs`).
const NODE_FUTURE_WINDOW_SECS: i64 = 300;

/// A transaction must be mined within this of its signed timestamp
/// (`MAX_TX_AGE_SECS`, `src/a9/blockchain.rs`).
const NODE_AGE_WINDOW_SECS: i64 = 21_600;

/// How this computer's clock compares with the node's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockCheck {
    /// The node's clock could not be read, so no comparison was made. Said
    /// out loud rather than passed over: a check that did not happen must not
    /// look like a check that passed.
    Unknown,
    InStep,
    Skewed {
        message: String,
        /// True once the skew is large enough that the node's own timestamp
        /// windows will act on it.
        severe: bool,
    },
}

/// Compare the two clocks BEFORE signing (spec 4.3).
///
/// The timestamp is signed into the message, so a skewed clock produces a
/// transaction that is refused or never templated, with nothing in the
/// rejection naming the clock as the cause. `offset` is the node's clock minus
/// this computer's: positive means this computer is BEHIND.
pub fn clock_check(offset: Option<i64>) -> ClockCheck {
    let Some(offset) = offset else {
        return ClockCheck::Unknown;
    };
    if offset.abs() <= CLOCK_WARN_SECS {
        return ClockCheck::InStep;
    }
    if offset > 0 {
        ClockCheck::Skewed {
            message: format!(
                "This computer's clock is {} behind the node's. A payment is stamped with \
                 this computer's time, and the node will not mine one more than 6 hours \
                 old.",
                describe_seconds(offset)
            ),
            severe: offset >= NODE_AGE_WINDOW_SECS,
        }
    } else {
        ClockCheck::Skewed {
            message: format!(
                "This computer's clock is {} ahead of the node's. The node will not include \
                 a payment dated more than 5 minutes ahead of its own clock until it catches \
                 up.",
                describe_seconds(-offset)
            ),
            severe: -offset >= NODE_FUTURE_WINDOW_SECS,
        }
    }
}

fn describe_seconds(seconds: i64) -> String {
    match seconds {
        0..=119 => format!("{seconds} seconds"),
        120..=7_199 => format!("{} minutes", seconds / 60),
        _ => format!("{} hours", seconds / 3_600),
    }
}

/// Which of the node's three success statuses came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    Accepted,
    AlreadyPending,
    AlreadyConfirmed,
}

/// What the node's answer means for the money -- which is not the same
/// question as whether the HTTP call succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The payment is in the node's hands. Terminal, and a success.
    Settled {
        settlement: Settlement,
        tx_id: Option<String>,
        height: Option<u64>,
    },
    /// The node definitively did not admit this payment, and said why. The
    /// signed body can be discarded and a corrected one composed.
    Refused(String),
    /// The outcome is unknown. The SAME signed body must be re-posted; signing
    /// a new one would risk a second payment that the node cannot tell from a
    /// retry.
    Unresolved(String),
    /// The node refused, but a payment matching this one may already exist on
    /// the chain. Neither retry nor re-sign until that is resolved.
    Ambiguous {
        message: String,
        /// What to look the payment up BY. Carried as data rather than folded
        /// into `message`, so the instruction and the identifiers it needs
        /// stay independently checkable -- and so telling someone to check
        /// first is an instruction they can actually follow. Without these,
        /// the only two things a person can do are nothing and send again,
        /// which are the two outcomes the message exists to prevent.
        details: Vec<Detail>,
    },
}

/// One labelled identifier lifted out of a conflict body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub label: String,
    pub value: String,
}

/// The fields a conflict body can carry that identify the payment, with what
/// to call each on screen, in the order they are worth reading.
///
/// Applied by presence rather than by status: a conflict this wallet has no
/// specific wording for still surfaces whatever identifiers it does carry,
/// which is the same fail-closed reasoning that routes every 409 to
/// `Ambiguous` in the first place. Names taken from the bodies in
/// `src/a9/node.rs`.
const CONFLICT_DETAILS: [(&str, &str); 8] = [
    ("tx_id", "Transaction"),
    ("original_tx_id", "Transaction already under this key"),
    ("colliding_tx_id", "Colliding transaction"),
    ("existing_status", "Its state on this node"),
    ("height", "In block"),
    ("last_observed_height", "Last seen at height"),
    ("idempotency_key", "Submission key"),
    ("colliding_idempotency_key", "Colliding submission key"),
];

/// Pull the identifying fields out of a conflict body.
///
/// A `null` is skipped rather than rendered: the node sends `height: null`
/// whenever the existing transaction is pending rather than confirmed, and
/// "In block: null" is worse than saying nothing.
pub fn conflict_details(body: &serde_json::Value) -> Vec<Detail> {
    CONFLICT_DETAILS
        .iter()
        .filter_map(|(field, label)| {
            let value = body.get(field)?;
            let rendered = match value {
                serde_json::Value::String(text) if !text.trim().is_empty() => text.clone(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some(Detail {
                label: (*label).to_string(),
                value: rendered,
            })
        })
        .collect()
}

/// Read the node's answer.
///
/// A 200 is not automatically a success and a failure is not automatically a
/// non-payment: `already_confirmed` is a 200 meaning the money has ALREADY
/// moved, and every 409 means something exists that has to be looked at before
/// anything else is signed. Getting either backwards is how a user is told to
/// retry a payment that already went through.
pub fn verdict(result: &Result<String, ApiError>) -> Verdict {
    match result {
        Ok(body) => read_success_body(body),
        // Dispatched on the node's machine-readable `status` token, which
        // `ApiError::Conflict` carries in its own field. It is NOT read out of
        // the message: the node sends both a token and a sentence, the
        // sentence is documentation text that will be reworded, and a match
        // against the sentence is a match that quietly stops matching.
        Err(ApiError::Conflict {
            status,
            message,
            body,
        }) => conflict_verdict(
            status.as_deref(),
            message,
            &serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
        ),
        Err(ApiError::Rejected(message)) => Verdict::Refused(message.clone()),
        // A body the node could not deserialise never reached the handler, so
        // nothing was admitted -- but it is a bug on this side, not the user's.
        Err(ApiError::Malformed(message)) => Verdict::Refused(format!(
            "The node could not read this submission: {message}"
        )),
        Err(error) => Verdict::Unresolved(error.to_string()),
    }
}

const IDEMPOTENCY_CONFLICT: &str = "The node already has a DIFFERENT payment under this \
     submission's key. This payment was not admitted. Do not re-send until you know what \
     the key is bound to.";

const TRANSACTION_COLLISION: &str = "Another submission already carries a byte-identical \
     payment. This one was not admitted, and re-sending it would be a SECOND payment, not a \
     retry. Check whether the first one is on the chain before sending again.";

const EXISTING_UNATTRIBUTED: &str = "An identical payment reached the node by another route \
     before this submission could claim it. It may already be on the chain. Do not send \
     again until you have checked.";

const HISTORICAL_UNAVAILABLE: &str = "The node once saw this payment confirmed but can no \
     longer prove it. It is most likely already on the chain. Do not send again until you \
     have checked.";

/// Its own constant, like the conflict wordings, so a test can pin it. An arm
/// asserted only by the VARIANT it produces is an arm that can be deleted
/// without anything going red -- this one drops into the same `Unresolved`
/// the catch-all produces, so the variant alone proves nothing about it.
const SUBMISSION_RESERVED: &str = "The node has reserved this payment's key but has not \
     recorded whether it was admitted. Retrying the identical payment is the way to resolve \
     it.";

/// Every 409 is `Ambiguous`, whatever token it carries.
///
/// The known four get wording that says what specifically happened; anything
/// else gets wording that still says the one thing that matters. Failing
/// closed on the whole family rather than on an enumerated list is deliberate:
/// a conflict this wallet has no wording for is still a conflict, and the
/// action it must not invite is a retry.
fn conflict_verdict(status: Option<&str>, message: &str, body: &serde_json::Value) -> Verdict {
    Verdict::Ambiguous {
        message: match status {
            Some("idempotency_conflict") => IDEMPOTENCY_CONFLICT.to_string(),
            Some("transaction_collision") => TRANSACTION_COLLISION.to_string(),
            Some("existing_transaction_unattributed") => EXISTING_UNATTRIBUTED.to_string(),
            Some("historical_outcome_unavailable") => HISTORICAL_UNAVAILABLE.to_string(),
            _ => format!(
                "The node reports a conflict, so a payment matching this one may already \
                 exist. Do not send again until you have checked. {message}"
            ),
        },
        details: conflict_details(body),
    }
}

fn read_success_body(body: &str) -> Verdict {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Verdict::Unresolved(format!(
            "The node answered, but with something this wallet could not read: {}",
            first_line(body)
        ));
    };
    let tx_id = value
        .get("tx_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let height = value.get("height").and_then(serde_json::Value::as_u64);
    match value.get("status").and_then(serde_json::Value::as_str) {
        Some("accepted") => Verdict::Settled {
            settlement: Settlement::Accepted,
            tx_id,
            height: None,
        },
        Some("already_pending") => Verdict::Settled {
            settlement: Settlement::AlreadyPending,
            tx_id,
            height: None,
        },
        Some("already_confirmed") => Verdict::Settled {
            settlement: Settlement::AlreadyConfirmed,
            tx_id,
            height,
        },
        // A 200 that is nonetheless a dead end. The ledger records this
        // withdrawal in a terminal state that will never pay, so offering
        // "retry the same payment" would offer an answer that cannot change:
        // the identical key and body can only return this same status for as
        // long as the binding is kept.
        Some("rejected") => Verdict::Refused(format!(
            "The node recorded this payment as rejected. {}",
            value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("It gave no reason.")
        )),
        // `Expired` is only ever reached from `Reserved` or `Pending`, and a
        // transaction past the freshness window can never be included in a
        // block -- so nothing moved and nothing can.
        Some("expired") => Verdict::Refused(
            "This payment's signed timestamp is now too old for any block to include it, so \
             it can never be mined. Nothing moved."
                .to_string(),
        ),
        // Genuinely unresolved, and the one case where retrying the identical
        // body is exactly the documented remedy: the key is durably reserved
        // and a retry of this same operation may safely attempt admission.
        Some("submission_reserved") => Verdict::Unresolved(SUBMISSION_RESERVED.to_string()),
        // The same thing its 409 twin means, arriving as a replay instead --
        // identifiers and all. Reading it as merely unrecognised here would
        // undo, on this path, the care taken on the other one.
        Some("existing_transaction_unattributed") => Verdict::Ambiguous {
            message: EXISTING_UNATTRIBUTED.to_string(),
            details: conflict_details(&value),
        },
        // Unknown, not failed. Re-posting the identical body is safe under the
        // idempotency key, so this stays retryable rather than being reported
        // as a payment that did not happen. `ok` is true only while a payment
        // is progressing or done, so a false one sharpens the warning without
        // turning an unknown answer into a claimed outcome.
        Some(other) => Verdict::Unresolved(format!(
            "The node answered with a status this wallet does not recognise: {other}. The \
             payment may or may not have been admitted.{}",
            if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
                " The node reports it is not progressing, so retrying may not change it."
            } else {
                ""
            }
        )),
        None => Verdict::Unresolved(format!(
            "The node answered without a status: {}",
            first_line(body)
        )),
    }
}

fn first_line(body: &str) -> String {
    let trimmed = body.trim();
    let line = trimmed.lines().next().unwrap_or("").trim();
    if line.chars().count() > 160 {
        format!("{}...", line.chars().take(160).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Whether leaving this outcome keeps what was typed in the form.
///
/// Only a `Refused` outcome does -- a validation rejection or a body the node
/// could not read, both of which mean nothing was admitted and the user is
/// being asked to correct something and try again. Making them retype a
/// 40-character hex address by hand to do that is its own hazard: a mistyped
/// address is not a refused payment, it is funds sent somewhere nobody can
/// recover them from.
///
/// Every other outcome clears the form. `Settled` obviously must -- a payment
/// that went through has no business sitting one click from being sent again.
/// `Unresolved` and `Ambiguous` must too, and for a sharper reason: in both,
/// the payment MAY already have been admitted, and neither asks the user to
/// correct anything (the answer to an unresolved outcome is to retry the
/// identical body, not to compose a new one). A pre-filled form there would be
/// a short path to a genuine second payment.
pub fn keeps_the_form(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Refused(_))
}

/// The headline and detail for a settled payment.
pub fn describe_settlement(settlement: Settlement, height: Option<u64>) -> (String, String) {
    let headline = match settlement {
        Settlement::Accepted => "Payment sent",
        Settlement::AlreadyPending => "Payment already waiting to be mined",
        // Read as a SUCCESS, and worded so nobody about to retry a payment
        // reads it as one that failed.
        Settlement::AlreadyConfirmed => "Payment already on the chain",
    };
    let detail = match settlement {
        Settlement::Accepted => {
            "The node admitted it and will announce it to the network.".to_string()
        }
        Settlement::AlreadyPending => {
            "This exact payment was already in the node's mempool. Nothing was sent twice."
                .to_string()
        }
        Settlement::AlreadyConfirmed => match height {
            Some(height) => format!("It was mined in block {height}. Nothing was sent twice."),
            None => "It has already been mined. Nothing was sent twice.".to_string(),
        },
    };
    (headline.to_string(), detail)
}

/// The transaction id a settled payment can be looked up by. The RESULT
/// panel shows it on its own row and its COPY button copies it -- both read
/// it from here, so what is copied is what is shown. `None` for every other
/// verdict, and for a settled one the node answered without an id.
pub fn settled_tx_id(verdict: &Verdict) -> Option<&str> {
    match verdict {
        Verdict::Settled { tx_id, .. } => tx_id.as_deref(),
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Widgets. Everything above this line is pure and tested; everything below is
// layout over it.
// --------------------------------------------------------------------------

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(wallet) = &app.wallet else {
        return kit::screen(text("No wallet loaded."));
    };
    let Some(entry) = wallet.addresses.get(wallet.active) else {
        return kit::screen(text("No address selected."));
    };

    // Left: the form (editable only while composing). Right: whatever the
    // stage is -- a live preview, the confirmation, the wait, the verdict.
    // The stage machine is untouched; this only decides where each existing
    // piece is drawn.
    let (form, side_tag, side): (Element<'_, Message>, &str, Element<'_, Message>) =
        match &wallet.send.stage {
            SendStage::Compose => (
                compose(wallet, entry.source, &entry.address, entry.spendable),
                "PREVIEW",
                preview(wallet, entry),
            ),
            SendStage::Confirm(prepared) => (
                locked_form(&wallet.send),
                "REVIEW",
                confirm(wallet, prepared),
            ),
            SendStage::Checking(_) => (
                locked_form(&wallet.send),
                "CHECKING",
                waiting(
                    "Checking your spendable balance",
                    "Asking the node how much can be spent from this address, before \
                     anything is signed.",
                ),
            ),
            SendStage::Sending => (
                locked_form(&wallet.send),
                "SENDING",
                waiting(
                    "Sending...",
                    "Waiting for the node to answer. Do not close the wallet.",
                ),
            ),
            SendStage::Answered(verdict) => (
                locked_form(&wallet.send),
                "RESULT",
                answered(wallet, verdict),
            ),
        };

    kit::screen(kit::split(
        app.narrow(),
        kit::tag_panel("COMPOSE", theme::CYAN, form),
        1,
        kit::tag_panel(side_tag, theme::LAVENDER, side),
        1,
    ))
}

/// What the payment will be, from the same `prepare_payment` the REVIEW
/// button is gated on -- so this panel cannot show a payment the button
/// would refuse. Nothing here is signed or sent.
fn preview<'a>(wallet: &'a WalletState, entry: &'a AddressEntry) -> Element<'a, Message> {
    let send = &wallet.send;
    match prepare_payment(
        &entry.address,
        entry.source,
        entry.spendable,
        &send.recipient,
        &send.amount,
        send.fee.as_ref(),
    ) {
        Ok(payment) => {
            let left = match entry.spendable {
                Spendable::Known(spendable) => model::format_coins(spendable - payment.total_units),
                Spendable::Pending | Spendable::Unavailable => "—".to_string(),
            };
            column![
                kit::kv_row("FROM", kit::short_address(&payment.sender)),
                // In full: the one field a payment can go wrong in, and
                // `kv_row` wraps it.
                kit::kv_row("TO", payment.recipient.clone()),
                kit::kv_row("AMOUNT", model::format_coins(payment.amount_units)),
                kit::kv_row("FEE", model::format_coins(payment.fee_units)),
                kit::kv_row("TOTAL", model::format_coins(payment.total_units)),
                kit::kv_row("LEFT SPENDABLE", left),
            ]
            .spacing(8)
            .into()
        }
        Err(_) => text(
            "The payment appears here as you fill in the form. Nothing is signed until \
             you confirm it.",
        )
        .size(f32::from(theme::SMALL))
        .color(theme::MUTED)
        .into(),
    }
}

/// The form while a payment is reviewed, sent or answered: what was entered,
/// read-only. Editable fields here would describe a payment other than the
/// one on the right.
fn locked_form(send: &SendState) -> Element<'_, Message> {
    let or_dash = |s: &str| {
        if s.trim().is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    column![
        kit::kv_row("TO", or_dash(&send.recipient)),
        kit::kv_row("AMOUNT", or_dash(&send.amount)),
        text("The form is locked while this payment is reviewed or sent.")
            .size(12)
            .color(theme::MUTED),
    ]
    .spacing(8)
    .into()
}

fn compose<'a>(
    wallet: &'a WalletState,
    sender_source: AddressSource,
    sender: &'a str,
    spendable: Spendable,
) -> Element<'a, Message> {
    let send = &wallet.send;
    let prepared = prepare_payment(
        sender,
        sender_source,
        spendable,
        &send.recipient,
        &send.amount,
        send.fee.as_ref(),
    );

    let mut from_label = row![kit::field_label("FROM")]
        .spacing(8)
        .align_y(iced::Alignment::Center);
    if matches!(sender_source, AddressSource::Imported(_)) {
        from_label = from_label.push(kit::badge("IMPORTED", theme::LAVENDER));
    }

    let mut content = column![
        column![
            from_label,
            // Glyph: 40 hex characters have nothing to break on, and the
            // half-width panel at 760 px is narrower than the line.
            text(sender.to_string())
                .size(14)
                .wrapping(iced::widget::text::Wrapping::Glyph),
            text(match spendable {
                Spendable::Known(units) => format!("Spendable {}", model::format_coins(units)),
                Spendable::Unavailable => "Spendable unavailable".to_string(),
                Spendable::Pending => "Spendable not known yet".to_string(),
            })
            .size(f32::from(theme::SMALL)),
        ]
        .spacing(2.0),
        column![
            kit::field_label("TO"),
            text_input("40 hexadecimal characters", &send.recipient)
                .on_input(Message::SendRecipientChanged)
                .padding(8)
                .style(theme::text_input),
        ]
        .spacing(4.0),
        column![
            kit::field_label("AMOUNT (ALPHA)"),
            text_input("1.5", &send.amount)
                .on_input(Message::SendAmountChanged)
                .padding(8)
                .style(theme::text_input),
        ]
        .spacing(4.0),
        fee_line(send),
    ]
    .spacing(f32::from(theme::SPACING));

    // Before signing, not after: the warning is on the screen while the
    // payment is still being composed, and again on the confirmation.
    if let Some(warning) = clock_warning(send.clock_offset) {
        content = content.push(warning);
    }

    match &prepared {
        Ok(payment) => {
            content = content.push(
                text(format!(
                    "Sending {} plus a fee of {} -- {} in total",
                    model::format_coins(payment.amount_units),
                    model::format_coins(payment.fee_units),
                    model::format_coins(payment.total_units),
                ))
                .size(f32::from(theme::SMALL)),
            );
        }
        Err(blocker) => {
            // Not shown while the form is simply still empty: an untouched
            // screen scolding the user for not having typed yet is noise, and
            // noise is what a real blocker has to stand out from.
            if !is_merely_unfinished(blocker) {
                content = content.push(
                    text(blocker.explain())
                        .size(f32::from(theme::SMALL))
                        .color(theme::DANGER),
                );
            }
        }
    }

    // A payment that was approved and then failed its re-check at the instant
    // of signing lands back here, and this is where it says why. Kept separate
    // from the live blocker above: editing the form clears that one on its own,
    // and this one has to survive long enough to be read.
    if let Some(error) = &send.error {
        content = content.push(
            text(format!("This payment was not signed: {error}"))
                .size(f32::from(theme::SMALL))
                .color(theme::DANGER),
        );
    }

    content = content.push(kit::action(
        "REVIEW",
        kit::Act::Look,
        prepared.is_ok().then_some(Message::SendReview),
    ));
    content.into()
}

/// True for the states an untouched form is legitimately in.
fn is_merely_unfinished(blocker: &Blocker) -> bool {
    matches!(
        blocker,
        Blocker::NoRecipient | Blocker::Amount(AmountProblem::Empty)
    )
}

fn fee_line(send: &SendState) -> Element<'_, Message> {
    let mut block = column![kit::field_label("FEE")].spacing(2.0);
    match &send.fee {
        Some(fee) => {
            block = block.push(
                text(format!(
                    "{} recommended by the node",
                    model::format_coins(fee.recommended_units)
                ))
                .size(f32::from(theme::SMALL))
                .color(theme::FAINT),
            );
        }
        None if send.fee_in_flight => {
            block = block.push(
                text("Asking the node...")
                    .size(f32::from(theme::SMALL))
                    .color(theme::MUTED),
            );
        }
        None => {
            block = block.push(
                text("Not read from the node yet.")
                    .size(f32::from(theme::SMALL))
                    .color(theme::MUTED),
            );
        }
    }
    if let Some(error) = &send.fee_error {
        block = block.push(
            text(error.clone())
                .size(f32::from(theme::CAPTION))
                .color(theme::DANGER),
        );
    }
    row![
        block.width(Length::Fill),
        kit::action(
            if send.fee_in_flight {
                "READING..."
            } else {
                "REFRESH FEE"
            },
            kit::Act::Plain,
            (!send.fee_in_flight).then_some(Message::SendFeeRefresh),
        ),
    ]
    .spacing(f32::from(theme::SPACING))
    .into()
}

fn clock_warning<'a>(offset: Option<i64>) -> Option<Element<'a, Message>> {
    match clock_check(offset) {
        ClockCheck::InStep => None,
        ClockCheck::Unknown => Some(
            text("The node's clock could not be read, so a skewed clock here would not be caught.")
                .size(f32::from(theme::CAPTION))
                .color(theme::MUTED)
                .into(),
        ),
        ClockCheck::Skewed { message, severe } => Some(
            text(message)
                .size(f32::from(theme::SMALL))
                .color(if severe { theme::DANGER } else { theme::MUTED })
                .into(),
        ),
    }
}

fn confirm<'a>(wallet: &'a WalletState, prepared: &'a Prepared) -> Element<'a, Message> {
    let mut content = column![
        text("Confirm this payment").size(16),
        detail_row("From", &prepared.sender),
        detail_row("To", &prepared.recipient),
        detail_row("Amount", &model::format_coins(prepared.amount_units)),
        detail_row("Fee", &model::format_coins(prepared.fee_units)),
        detail_row("Total", &model::format_coins(prepared.total_units)),
    ]
    .spacing(f32::from(theme::SPACING));

    // Shown again here because this is the last screen before a signature
    // exists, and this is the one a user actually reads.
    if let Some(warning) = clock_warning(wallet.send.clock_offset) {
        content = content.push(warning);
    }
    if let Some(error) = &wallet.send.error {
        content = content.push(
            text(error.clone())
                .size(f32::from(theme::SMALL))
                .color(theme::DANGER),
        );
    }

    content = content.push(
        row![
            kit::action("SIGN AND SEND", kit::Act::Go, Some(Message::SendConfirm)),
            kit::action("BACK", kit::Act::Plain, Some(Message::SendBackToCompose)),
        ]
        .spacing(f32::from(theme::SPACING)),
    );
    content.into()
}

fn detail_row<'a>(label: &'a str, value: &str) -> Element<'a, Message> {
    kit::kv_row(label, value.to_string())
}

fn waiting<'a>(headline: &'a str, detail: &'a str) -> Element<'a, Message> {
    column![
        text(headline).size(16),
        text(detail)
            .size(f32::from(theme::SMALL))
            .color(theme::MUTED),
    ]
    .spacing(f32::from(theme::SPACING))
    .into()
}

/// The colour a verdict is drawn in.
///
/// Three colours for four verdicts, and the grouping is the point: `Refused`
/// is the only outcome where nothing was admitted, so it is the only one that
/// may look recoverable. `Unresolved` and `Ambiguous` both mean the payment
/// may already be on the chain, and they share DANGER for that reason -- not
/// because they are the same thing (their screens differ) but because the
/// question "is it safe to send again" has the same answer.
fn verdict_colour(verdict: &Verdict) -> iced::Color {
    match verdict {
        Verdict::Settled { .. } => crate::theme::ACCENT,
        Verdict::Refused(_) => crate::theme::ADVISORY,
        Verdict::Unresolved(_) | Verdict::Ambiguous { .. } => crate::theme::DANGER,
    }
}

/// TRANSACTION, the id, and a COPY for it. The id is what the payment is looked
/// up by, and 64 characters is not something to retype by hand.
fn tx_id_row(id: &str) -> Element<'static, Message> {
    row![
        column![
            kit::field_label("TRANSACTION"),
            text(id.to_string())
                .font(theme::TECH_FONT)
                .size(f32::from(theme::SMALL))
                .wrapping(iced::widget::text::Wrapping::Glyph),
        ]
        .spacing(4)
        .width(Length::Fill),
        kit::action("COPY", kit::Act::Plain, Some(Message::CopySentTxId)),
    ]
    .spacing(f32::from(theme::SPACING))
    .align_y(iced::Alignment::Center)
    .into()
}

/// Wrap at words, and break a word by glyph only when it is longer than the
/// line -- a node's message or a verdict's detail can carry a transaction id
/// or an address, which has no space in it.
const WORD_OR_GLYPH: iced::widget::text::Wrapping = iced::widget::text::Wrapping::WordOrGlyph;

fn answered<'a>(wallet: &'a WalletState, verdict: &'a Verdict) -> Element<'a, Message> {
    let mut content = column![].spacing(f32::from(theme::SPACING));
    // The single source every coloured text in this block reads from -- the
    // title and the node's own message both have to agree with the verdict,
    // or a red paragraph under an orange title tells two different stories.
    let colour = verdict_colour(verdict);
    let mut actions = row![].spacing(f32::from(theme::SPACING));

    match verdict {
        Verdict::Settled {
            settlement, height, ..
        } => {
            let (headline, detail) = describe_settlement(*settlement, *height);
            content = content.push(text(headline).size(16).color(colour));
            content = content.push(
                text(detail)
                    .size(f32::from(theme::SMALL))
                    .wrapping(WORD_OR_GLYPH),
            );
            if let Some(id) = settled_tx_id(verdict) {
                content = content.push(tx_id_row(id));
            }
            actions = actions.push(kit::action(
                "SEND ANOTHER",
                kit::Act::Go,
                Some(Message::SendClear),
            ));
        }
        Verdict::Refused(message) => {
            content = content.push(text("The node refused this payment").size(16).color(colour));
            content = content.push(
                text(message.clone())
                    .size(f32::from(theme::SMALL))
                    .color(colour)
                    .wrapping(WORD_OR_GLYPH),
            );
            content = content.push(
                // True of every `Refused` outcome, including the two that
                // arrive as a 200: a validation rejection was never admitted,
                // and an expired payment can no longer be included in any
                // block. Only mining moves funds, so in all of them none did.
                text(
                    "This payment can no longer be mined and no funds moved, so it is safe \
                     to correct it and send again. The recipient and amount are kept for \
                     editing -- retyping a 40-character address by hand is its own way to \
                     lose funds.",
                )
                .size(f32::from(theme::CAPTION))
                .color(theme::MUTED),
            );
            actions = actions.push(kit::action(
                "CORRECT AND TRY AGAIN",
                kit::Act::Look,
                Some(Message::SendClear),
            ));
        }
        Verdict::Unresolved(message) => {
            content = content.push(
                text("This payment's outcome is not known")
                    .size(16)
                    .color(colour),
            );
            content = content.push(
                text(message.clone())
                    .size(f32::from(theme::SMALL))
                    .color(colour)
                    .wrapping(WORD_OR_GLYPH),
            );
            content = content.push(
                text(
                    "Retry re-sends the identical signed payment under the identical key, so \
                     the node treats it as the same withdrawal and cannot pay twice. Signing \
                     a new one could.",
                )
                .size(f32::from(theme::CAPTION))
                .color(theme::MUTED),
            );
            actions = actions.push(kit::action(
                "RETRY THE SAME PAYMENT",
                kit::Act::Care,
                Some(Message::SendRetry),
            ));
            actions = actions.push(kit::action(
                "DISCARD",
                kit::Act::Plain,
                Some(Message::SendClear),
            ));
        }
        Verdict::Ambiguous { message, details } => {
            content = content.push(
                text("Check before sending this again")
                    .size(16)
                    .color(colour),
            );
            content = content.push(
                text(message.clone())
                    .size(f32::from(theme::SMALL))
                    .color(colour)
                    .wrapping(WORD_OR_GLYPH),
            );
            // What to look it up BY. An instruction to check, with nothing to
            // check against, leaves a person with only two moves -- do nothing
            // or send again -- and those are the two this screen exists to
            // prevent.
            if !details.is_empty() {
                content = content.push(
                    text("Look it up by:")
                        .size(f32::from(theme::CAPTION))
                        .color(theme::MUTED),
                );
                for detail in details {
                    content = content.push(detail_row(&detail.label, &detail.value));
                }
            }
            // No retry button here on purpose: a matching payment may already
            // be on the chain, and this is the one verdict where neither
            // retrying nor re-signing is safe.
            actions = actions.push(kit::action(
                "DISCARD",
                kit::Act::Plain,
                Some(Message::SendClear),
            ));
        }
    }

    if wallet.send.outstanding.is_some() {
        content = content.push(
            text("The signed payment is still held, exactly as it was sent.")
                .size(f32::from(theme::CAPTION))
                .color(theme::MUTED),
        );
    }
    actions = actions.push(kit::action(
        "BACK TO WALLET",
        kit::Act::Plain,
        Some(Message::Show(Screen::Wallet)),
    ));
    container(content.push(actions))
        .style(theme::verdict_card(colour))
        .padding(theme::PADDING)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The JSON that goes on the wire must agree with the string that was signed:
    // both come from the same integer units. A float anywhere between them is how
    // a signature stops matching the message a node re-derives.
    #[test]
    fn the_submission_agrees_with_what_was_signed() {
        let seed = [0x07u8; 32];
        let sender = "9e1e860361994891b3165e611dc5aefcdd37dfbf";
        let recipient = "84dab431b53e6522fe2e74914eec99f17758f4e3";
        let value = build_submission(
            &seed,
            sender,
            recipient,
            150_000_000,
            100_000,
            1_783_600_000,
        );

        let tx = &value["transaction"];
        assert_eq!(tx["sender"], sender);
        assert_eq!(tx["recipient"], recipient);
        assert_eq!(tx["timestamp"], 1_783_600_000u64);

        // The signature is over the canonical message, and sig_hash commits to it.
        let message = alphanumeric_gui::tx::signing_message(
            sender,
            recipient,
            150_000_000,
            100_000,
            1_783_600_000,
        );
        let signature = alphanumeric_gui::tx::sign_message(&seed, message.as_bytes());
        assert_eq!(tx["signature"], hex::encode(&signature));
        assert_eq!(tx["sig_hash"], alphanumeric_gui::tx::sig_hash(&signature));
        assert_eq!(
            tx["pub_key"],
            hex::encode(alphanumeric_gui::tx::public_key_from_seed(&seed))
        );

        // A fresh key every call, or the node cannot tell a retry from a second
        // payment.
        let again = build_submission(
            &seed,
            sender,
            recipient,
            150_000_000,
            100_000,
            1_783_600_000,
        );
        assert_ne!(value["idempotency_key"], again["idempotency_key"]);
    }

    // The idempotency key sits at the TOP level, beside `transaction` and not
    // inside it -- the shape `/explorer/v2/submit-tx` requires
    // (docs/EXCHANGE_INTEGRATION.md). A key nested one level too deep is a 422
    // naming a missing field, and the node's length bound is what makes a
    // guessable key impossible in the first place.
    #[test]
    fn the_idempotency_key_is_where_the_node_looks_for_it() {
        let value = build_submission(
            &[0x07u8; 32],
            "9e1e860361994891b3165e611dc5aefcdd37dfbf",
            "84dab431b53e6522fe2e74914eec99f17758f4e3",
            150_000_000,
            100_000,
            1_783_600_000,
        );
        let key = value["idempotency_key"]
            .as_str()
            .expect("the key is a top-level string");
        assert!(
            (16..=128).contains(&key.len()),
            "the node bounds a key at 16..128 printable ASCII, got {}",
            key.len()
        );
        assert!(key.bytes().all(|byte| byte.is_ascii_graphic()));
        assert!(value["transaction"].get("idempotency_key").is_none());
        // Exactly two top-level fields: an extra one is a 422 from the node's
        // deserialiser, not something it ignores.
        let object = value.as_object().expect("an object body");
        assert_eq!(object.len(), 2);
        assert!(object.contains_key("transaction"));
    }

    /// The node's `Transaction::to_units` (`src/a9/blockchain.rs`), written
    /// out here from the node's source rather than called from the code under
    /// test, so this proves the round trip rather than its own consistency.
    fn node_side_to_units(amount: f64) -> i128 {
        const SCALE: f64 = 100_000_000.0;
        (((amount * SCALE).round() / SCALE) * SCALE).round() as i128
    }

    // The `unwrap_or(0.0)` this replaces could not fire -- but if it ever had,
    // the body would have been SIGNED for the real amount and TRANSMITTED as
    // zero, and the node would have refused a signature that was in fact
    // correct. This proves the parse is total over everything this app can
    // sign, and that the float the node reads re-quantises to exactly the
    // units that were signed for.
    #[test]
    fn the_wire_float_is_total_over_what_this_app_can_sign() {
        let mut values: Vec<i128> = vec![
            0,
            1,
            564,
            889,
            10_000,
            94_458,
            100_000,
            150_000_000,
            99_999_999,
            100_000_000,
            123_456_789_012_345,
            1_000_000_000_000_000,
        ];
        // A sweep across every magnitude, including the awkward all-nines and
        // the carry either side of them.
        for exponent in 0..16u32 {
            let decade = 10i128.pow(exponent);
            values.extend([decade - 1, decade, decade + 1, decade * 9 + 7]);
        }
        for units in values {
            let rendered = tx::units_to_amount_string(units);
            let parsed = rendered
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("{rendered} must parse as f64"));
            assert!(
                !wire_amount(units).is_null(),
                "{units} produced a null wire amount"
            );
            assert_eq!(
                node_side_to_units(parsed),
                units,
                "{units} did not survive the wire's decimal round trip"
            );
            assert!(survives_the_wire(units), "{units} failed its own guard");
        }
    }

    /// The node's `Transaction::from_units`, replicated beside
    /// `node_side_to_units` for the same reason: to check the round trip
    /// against the node's arithmetic rather than against this screen's.
    fn node_side_from_units(units: i128) -> f64 {
        const SCALE: f64 = 100_000_000.0;
        ((units as f64 / SCALE) * SCALE).round() / SCALE
    }

    /// The whole property, not the proxy: what the node re-derives as the
    /// amount field of the signed message, given the float this wallet put on
    /// the wire.
    fn node_side_signed_amount(units: i128) -> String {
        let parsed = tx::units_to_amount_string(units)
            .parse::<f64>()
            .expect("this app only signs amounts that parse");
        format!("{:.8}", node_side_from_units(node_side_to_units(parsed)))
    }

    // `survives_the_wire` checks `units -> f64 -> units`. What actually has to
    // hold is `units -> f64 -> the node's re-derived signed string == the
    // string this wallet signed`, and the two are not the same claim -- the
    // node formats the units it recovered back into the message before
    // verifying. Pin where the proxy stops standing in for the real thing, so
    // the range its doc comment cites is checked rather than asserted in prose.
    #[test]
    fn the_proxy_is_the_node_s_own_answer_up_to_2_53() {
        let p53 = 1i128 << 53;

        // Everything the wallet can actually reach. The proxy and the full
        // round trip agree, up to and including 2^53 units.
        for units in [
            1i128,
            889,
            10_000,
            150_000_000,
            100_000_000_000,
            3_355_446_864_845_443,
            p53 - 1,
            p53,
        ] {
            assert!(survives_the_wire(units), "{units} must survive");
            assert_eq!(
                node_side_signed_amount(units),
                tx::units_to_amount_string(units),
                "{units}: the node re-derives a different signed amount"
            );
        }

        // Four units past 2^53 is the first place the proxy says yes and the
        // node's re-derived string says something else. It is unreachable -- an
        // f64 no longer carries consecutive integers there, 2^53 units is over
        // 90 million coins, and the whole chain has issued less than half of
        // that -- but it is the boundary the doc comment names, so it is pinned
        // rather than trusted.
        let first_divergence = 9_007_199_254_740_996i128;
        assert!(survives_the_wire(first_divergence));
        assert_ne!(
            node_side_signed_amount(first_divergence),
            tx::units_to_amount_string(first_divergence),
            "the proxy's first false positive moved; the doc comment above names it"
        );
    }

    // Past the point where an i128 stops fitting an f64 exactly, the guard has
    // to say so rather than sign for one figure and transmit another.
    #[test]
    fn an_amount_that_cannot_survive_the_wire_is_refused() {
        // 2^53 units is where the float can no longer carry every integer.
        let beyond = (1i128 << 53) + 1;
        assert!(!survives_the_wire(beyond));
        assert_eq!(
            prepare_payment(
                "aa".repeat(20).as_str(),
                AddressSource::Derived(0),
                Spendable::Known(i128::MAX / 4),
                &"bb".repeat(20),
                "9007199254740992.99999999",
                Some(&estimate(20_000)),
            ),
            Err(Blocker::NotRepresentable)
        );
    }

    // Coins in, exact units out, with no float on the path: these integers are
    // what BOTH the signed string and the JSON number are derived from.
    #[test]
    fn coins_quantise_to_exact_units() {
        assert_eq!(parse_coin_amount("1.5"), Ok(150_000_000));
        assert_eq!(parse_coin_amount("1"), Ok(100_000_000));
        assert_eq!(parse_coin_amount("0.00000001"), Ok(1));
        assert_eq!(parse_coin_amount("0"), Ok(0));
        assert_eq!(parse_coin_amount("  2.25  "), Ok(225_000_000));
        assert_eq!(parse_coin_amount("007.5"), Ok(750_000_000));
        assert_eq!(
            parse_coin_amount("1234567.89012345"),
            Ok(123_456_789_012_345)
        );
        // A value no f64 could hold without rounding, read exactly.
        assert_eq!(
            parse_coin_amount("90071992.54740993"),
            Ok(9_007_199_254_740_993)
        );
    }

    // Anything that is not unambiguously a number of coins is refused rather
    // than guessed at. A ninth decimal place in particular is REFUSED and not
    // rounded: rounding it would sign for a different amount than was typed.
    #[test]
    fn an_amount_that_is_not_plainly_a_number_is_refused() {
        assert_eq!(parse_coin_amount(""), Err(AmountProblem::Empty));
        assert_eq!(parse_coin_amount("   "), Err(AmountProblem::Empty));
        assert_eq!(
            parse_coin_amount("1.234567891"),
            Err(AmountProblem::TooPrecise)
        );
        for input in [
            "-1", "+1", "1.5.5", "1.", ".5", ".", "abc", "1e9", "1,5", "1 5", "0x10", "１.５",
        ] {
            assert_eq!(
                parse_coin_amount(input),
                Err(AmountProblem::NotANumber),
                "{input:?} must not read as an amount"
            );
        }
        assert_eq!(
            parse_coin_amount(&"9".repeat(40)),
            Err(AmountProblem::TooLarge)
        );
    }

    fn sender() -> String {
        "9e1e860361994891b3165e611dc5aefcdd37dfbf".to_string()
    }

    fn recipient() -> String {
        "84dab431b53e6522fe2e74914eec99f17758f4e3".to_string()
    }

    /// A fee estimate shaped like the live node's: `recommended_fee_units`
    /// beside the relay floor it published in the same answer.
    fn estimate(recommended_units: i128) -> FeeEstimate {
        FeeEstimate {
            recommended_units,
            floor_units: tx::RELAY_FLOOR_UNITS,
        }
    }

    fn prepare(spendable: Spendable, amount: &str) -> Result<Prepared, Blocker> {
        prepare_payment(
            &sender(),
            AddressSource::Derived(0),
            spendable,
            &recipient(),
            amount,
            Some(&estimate(20_000)),
        )
    }

    // Requirement 1. `spendable` is the ceiling and it is checked BEFORE a
    // signature exists -- and the fee is inside the comparison, because the
    // node admits on `confirmed - pending >= amount + fee`.
    #[test]
    fn a_payment_above_spendable_never_reaches_a_signature() {
        // Exactly the spendable balance, minus room for the fee: allowed.
        let ceiling = 100_000_000i128;
        let fee = tx::clamp_fee_units(ceiling - 20_000, 20_000, None).expect("a fee exists");
        let ok = prepare(
            Spendable::Known(ceiling),
            &model::format_coins(ceiling - fee),
        )
        .expect("a payment that fits with its fee must be allowed");
        assert_eq!(ok.total_units, ceiling);

        // One unit more, and the fee no longer fits.
        let over = prepare(
            Spendable::Known(ceiling),
            &model::format_coins(ceiling - fee + 1),
        );
        assert!(matches!(over, Err(Blocker::InsufficientSpendable { .. })));

        // The whole balance, ignoring the fee, is the classic mistake.
        assert!(matches!(
            prepare(Spendable::Known(ceiling), &model::format_coins(ceiling)),
            Err(Blocker::InsufficientSpendable { .. })
        ));
    }

    // Requirement 1, the part a number cannot express. Neither non-answer may
    // be read as zero (which blocks every payment) or as "fine, proceed"
    // (which signs one the node will refuse). Each says its own thing.
    #[test]
    fn a_spendable_the_wallet_does_not_know_blocks_the_send_with_its_own_reason() {
        assert_eq!(
            prepare(Spendable::Pending, "1"),
            Err(Blocker::SpendableUnknown)
        );
        assert_eq!(
            prepare(Spendable::Unavailable, "1"),
            Err(Blocker::SpendableUnavailable)
        );

        // Neither is silently the same as a zero balance, and neither reads
        // like the other.
        let zero = prepare(Spendable::Known(0), "1").expect_err("zero cannot pay 1 coin");
        assert_ne!(zero, Blocker::SpendableUnknown);
        assert_ne!(zero, Blocker::SpendableUnavailable);
        assert_ne!(
            Blocker::SpendableUnknown.explain(),
            Blocker::SpendableUnavailable.explain()
        );
        for blocker in [Blocker::SpendableUnknown, Blocker::SpendableUnavailable] {
            assert!(!blocker.explain().trim().is_empty());
        }

        // And they are checked FIRST: a user whose wallet cannot check a
        // payment at all is told that, not that they mistyped an address.
        assert_eq!(
            prepare_payment(
                &sender(),
                AddressSource::Derived(0),
                Spendable::Pending,
                "nonsense",
                "",
                Some(&estimate(20_000))
            ),
            Err(Blocker::SpendableUnknown)
        );
    }

    // Requirement 2. The fee starts at the node's recommendation and goes
    // through the clamp; `None` from the clamp means the AMOUNT is dust, and
    // the send is blocked rather than sent with an unclamped fee.
    #[test]
    fn the_fee_comes_from_the_node_through_the_clamp_and_dust_is_refused() {
        let payment = prepare(Spendable::Known(1_000_000_000), "1").expect("an ordinary payment");
        assert_eq!(
            payment.fee_units,
            tx::clamp_fee_units(payment.amount_units, 20_000, Some(tx::RELAY_FLOOR_UNITS))
                .expect("clamped"),
        );
        assert!(payment.fee_units >= tx::RELAY_FLOOR_UNITS);
        assert!(payment.fee_units <= tx::max_non_whisper_fee_units(payment.amount_units));

        // Dust: every amount below 889 units has no fee that is both above the
        // relay floor and below the whisper band.
        for units in [1i128, 500, 888] {
            assert_eq!(
                prepare(Spendable::Known(1_000_000_000), &model::format_coins(units)),
                Err(Blocker::DustAmount {
                    floor_units: tx::RELAY_FLOOR_UNITS
                }),
                "{units} units must be refused as dust"
            );
        }
        assert_eq!(
            prepare(Spendable::Known(1_000_000_000), &model::format_coins(889))
                .expect("889 units is the first sendable amount")
                .fee_units,
            tx::RELAY_FLOOR_UNITS
        );
        // The message names the real minimum, not "zero".
        assert!(Blocker::DustAmount {
            floor_units: tx::RELAY_FLOOR_UNITS
        }
        .explain()
        .contains("0.00000889"));
        // And it follows the node's floor rather than that constant: a node
        // that quadrupled its floor makes a larger amount the smallest
        // sendable one, and the text has to say so.
        assert!(Blocker::DustAmount {
            floor_units: 4 * tx::RELAY_FLOOR_UNITS
        }
        .explain()
        .contains(&model::format_coins(tx::smallest_sendable_units(
            4 * tx::RELAY_FLOOR_UNITS
        ))));

        // A recommendation far above the whisper band is clamped DOWN, never
        // passed through: an ordinary payment must not be displayed as a
        // four-letter message.
        let generous = prepare_payment(
            &sender(),
            AddressSource::Derived(0),
            Spendable::Known(1_000_000_000),
            &recipient(),
            "1",
            Some(&estimate(50_000_000)),
        )
        .expect("a high recommendation is clamped, not refused");
        assert!(generous.fee_units <= tx::max_non_whisper_fee_units(generous.amount_units));

        // And with no estimate at all, nothing is signed with a guess.
        assert_eq!(
            prepare_payment(
                &sender(),
                AddressSource::Derived(0),
                Spendable::Known(1_000_000_000),
                &recipient(),
                "1",
                None
            ),
            Err(Blocker::NoFeeEstimate)
        );
    }

    // Requirement 3, end to end: the integers a payment is prepared with are
    // the integers the signed message and the JSON are both built from.
    #[test]
    fn the_prepared_integers_are_what_gets_signed() {
        let payment = prepare(Spendable::Known(1_000_000_000), "1.5").expect("prepared");
        assert_eq!(payment.amount_units, 150_000_000);
        assert_eq!(
            payment.total_units,
            payment.amount_units + payment.fee_units
        );

        let body = build_submission(
            &[0x07u8; 32],
            &payment.sender,
            &payment.recipient,
            payment.amount_units,
            payment.fee_units,
            1_783_600_000,
        );
        let expected = tx::signing_message(
            &payment.sender,
            &payment.recipient,
            payment.amount_units,
            payment.fee_units,
            1_783_600_000,
        );
        let signature = tx::sign_message(&[0x07u8; 32], expected.as_bytes());
        assert_eq!(body["transaction"]["signature"], hex::encode(&signature));
        // The float on the wire quantises back to the very same integers.
        assert_eq!(
            node_side_to_units(body["transaction"]["amount"].as_f64().expect("a number")),
            payment.amount_units
        );
        assert_eq!(
            node_side_to_units(body["transaction"]["fee"].as_f64().expect("a number")),
            payment.fee_units
        );
    }

    // The recipient is folded to the canonical lowercase form BEFORE it is
    // prepared, so what is confirmed on screen is what gets signed. Folding it
    // any later would sign one address and display another, and the node
    // refuses a non-canonical recipient outright.
    #[test]
    fn the_recipient_is_canonical_before_anything_is_signed() {
        let shouted = recipient().to_uppercase();
        let payment = prepare_payment(
            &sender(),
            AddressSource::Derived(0),
            Spendable::Known(1_000_000_000),
            &format!("  {shouted}  "),
            "1",
            Some(&estimate(20_000)),
        )
        .expect("a pasted address in capitals is still that address");
        assert_eq!(payment.recipient, recipient());

        for bad in [
            "",
            "  ",
            &recipient()[..39],
            &format!("{}0", recipient()),
            &"g".repeat(40),
            "MINING_REWARDS",
        ] {
            let blocker = prepare_payment(
                &sender(),
                AddressSource::Derived(0),
                Spendable::Known(1_000_000_000),
                bad,
                "1",
                Some(&estimate(20_000)),
            )
            .expect_err("a non-address must never be prepared");
            assert!(matches!(
                blocker,
                Blocker::NoRecipient | Blocker::BadRecipient
            ));
        }

        // Paying this very address moves nothing and still costs the fee.
        assert_eq!(
            prepare_payment(
                &sender(),
                AddressSource::Derived(0),
                Spendable::Known(1_000_000_000),
                &sender(),
                "1",
                Some(&estimate(20_000))
            ),
            Err(Blocker::SelfPayment)
        );
    }

    // Requirement 6. The node checks the timestamp window, so a skewed clock
    // produces repeated rejections with nothing naming the cause. A reading
    // that could not be taken must not look like one that passed.
    #[test]
    fn clock_skew_is_reported_before_signing_and_names_the_direction() {
        assert_eq!(clock_check(None), ClockCheck::Unknown);
        for offset in [-CLOCK_WARN_SECS, -1, 0, 1, CLOCK_WARN_SECS] {
            assert_eq!(
                clock_check(Some(offset)),
                ClockCheck::InStep,
                "{offset}s apart is within the tolerance"
            );
        }

        // Node ahead of us: this computer is behind, and 6 hours behind is
        // past the chain's freshness window.
        let behind = clock_check(Some(3_600));
        let ClockCheck::Skewed { message, severe } = behind else {
            panic!("an hour of skew must be reported");
        };
        assert!(message.contains("behind"));
        assert!(!severe);
        assert!(matches!(
            clock_check(Some(NODE_AGE_WINDOW_SECS)),
            ClockCheck::Skewed { severe: true, .. }
        ));

        // Node behind us: this computer is ahead, and 5 minutes ahead is past
        // the future window the node will template.
        let ahead = clock_check(Some(-120));
        let ClockCheck::Skewed { message, severe } = ahead else {
            panic!("two minutes of skew must be reported");
        };
        assert!(message.contains("ahead"));
        assert!(!severe);
        assert!(matches!(
            clock_check(Some(-NODE_FUTURE_WINDOW_SECS)),
            ClockCheck::Skewed { severe: true, .. }
        ));
    }

    // A 200 is not automatically a success, and `already_confirmed` in
    // particular is a payment that ALREADY WENT THROUGH. Reading it as a
    // failure is how a user retries a payment they have already made.
    #[test]
    fn the_three_success_statuses_are_told_apart_and_all_read_as_success() {
        let accepted = verdict(&Ok(
            r#"{"ok":true,"status":"accepted","tx_id":"abc"}"#.to_string()
        ));
        assert_eq!(
            accepted,
            Verdict::Settled {
                settlement: Settlement::Accepted,
                tx_id: Some("abc".to_string()),
                height: None,
            }
        );

        let pending = verdict(&Ok(
            r#"{"ok":true,"status":"already_pending","tx_id":"abc"}"#.to_string(),
        ));
        assert!(matches!(
            pending,
            Verdict::Settled {
                settlement: Settlement::AlreadyPending,
                ..
            }
        ));

        let confirmed = verdict(&Ok(
            r#"{"ok":true,"status":"already_confirmed","tx_id":"abc","height":961410,"final":false}"#
                .to_string(),
        ));
        assert_eq!(
            confirmed,
            Verdict::Settled {
                settlement: Settlement::AlreadyConfirmed,
                tx_id: Some("abc".to_string()),
                height: Some(961_410),
            }
        );

        // The words a user reads must not suggest the payment failed or that
        // sending again is called for.
        for (settlement, height) in [
            (Settlement::Accepted, None),
            (Settlement::AlreadyPending, None),
            (Settlement::AlreadyConfirmed, Some(961_410)),
        ] {
            let (headline, detail) = describe_settlement(settlement, height);
            let words = format!("{headline} {detail}").to_ascii_lowercase();
            assert!(!words.contains("failed"), "{words}");
            assert!(!words.contains("could not"), "{words}");
        }
        let (_, detail) = describe_settlement(Settlement::AlreadyConfirmed, Some(7));
        assert!(detail.contains("block 7"));
    }

    // The transaction id moved out of the prose into its own row with a COPY
    // button. Both the row and the button read it from here, so a settled
    // payment's id is what gets shown and what gets copied -- and nothing is
    // offered for a verdict that has none.
    #[test]
    fn a_settled_payment_offers_its_transaction_id_and_nothing_else_does() {
        for settlement in [
            Settlement::Accepted,
            Settlement::AlreadyPending,
            Settlement::AlreadyConfirmed,
        ] {
            let settled = Verdict::Settled {
                settlement,
                tx_id: Some("abc".to_string()),
                height: None,
            };
            assert_eq!(settled_tx_id(&settled), Some("abc"));
        }
        let no_id = Verdict::Settled {
            settlement: Settlement::Accepted,
            tx_id: None,
            height: None,
        };
        assert_eq!(settled_tx_id(&no_id), None);
        assert_eq!(settled_tx_id(&Verdict::Refused("abc".into())), None);
        assert_eq!(settled_tx_id(&Verdict::Unresolved("abc".into())), None);
    }

    // The outcome of a submission is three-valued, not two: some answers mean
    // the payment definitely did not happen, some mean it definitely did, and
    // some mean nobody knows -- and only the last may be retried, with the
    // identical body.
    #[test]
    fn an_unknown_outcome_is_never_reported_as_a_failure() {
        for error in [
            ApiError::Busy,
            ApiError::RateLimited,
            ApiError::Transport("connection reset".into()),
            ApiError::NodeFault("index poisoned".into()),
        ] {
            assert!(
                matches!(verdict(&Err(error.clone())), Verdict::Unresolved(_)),
                "{error:?} leaves the outcome unknown, so the same body must stay retryable"
            );
        }
        // A 200 whose status this wallet does not know is also unknown, not a
        // failure: re-posting the same key is safe, re-signing is not.
        assert!(matches!(
            verdict(&Ok(r#"{"ok":true,"status":"tomorrows_status"}"#.to_string())),
            Verdict::Unresolved(_)
        ));
        assert!(matches!(
            verdict(&Ok("not json at all".to_string())),
            Verdict::Unresolved(_)
        ));
        assert!(matches!(
            verdict(&Ok(r#"{"ok":true}"#.to_string())),
            Verdict::Unresolved(_)
        ));
    }

    // A real rejection is terminal: nothing was admitted, so composing a
    // corrected payment is safe.
    #[test]
    fn a_validation_rejection_is_terminal() {
        assert_eq!(
            verdict(&Err(ApiError::Rejected(
                "transaction rejected: below the relay floor".into()
            ))),
            Verdict::Refused("transaction rejected: below the relay floor".to_string())
        );
        assert!(matches!(
            verdict(&Err(ApiError::Malformed("missing field".into()))),
            Verdict::Refused(_)
        ));
    }

    /// The node's four real 409 bodies, copied from `src/a9/node.rs` rather
    /// than invented: `explorer_v2_existing_transaction_unattributed` (:7888),
    /// `explorer_v2_historical_outcome_unknown` (:7907), and the
    /// `Conflict`/`Collision` arms of `submit_with_idempotency` (:7971,
    /// :7986).
    ///
    /// Every one carries BOTH a `status` token and an `error` sentence. That
    /// pairing is the whole defect these bodies exist to catch: a classifier
    /// that reads `error` first and treats `status` as its fallback throws the
    /// token away exactly when both are present, which is always.
    const CONFLICT_BODIES: [(&str, &str); 4] = [
        (
            "existing_transaction_unattributed",
            r#"{"ok":false,"status":"existing_transaction_unattributed","idempotency_key":"k","tx_id":"t","existing_status":"confirmed","height":961410,"error":"the identical transaction already existed before this node could attribute it to this withdrawal key; do not mark paid and do not re-sign blindly — reconcile it against your withdrawal history"}"#,
        ),
        (
            "historical_outcome_unavailable",
            r#"{"ok":false,"status":"historical_outcome_unavailable","idempotency_key":"k","tx_id":"t","last_observed_height":961410,"error":"this ledger previously observed the transaction as confirmed, but the canonical replay index no longer retains transactions outside the validity window; verify the withdrawal in your own durable history before taking action"}"#,
        ),
        (
            "idempotency_conflict",
            r#"{"ok":false,"status":"idempotency_conflict","idempotency_key":"k","original_tx_id":"t","error":"this idempotency_key is already bound to a different transaction; reuse a key only to retry the exact same withdrawal"}"#,
        ),
        (
            "transaction_collision",
            r#"{"ok":false,"status":"transaction_collision","idempotency_key":"k","colliding_tx_id":"t","colliding_idempotency_key":"k2","error":"another withdrawal key already submitted byte-identical transaction bytes; two distinct withdrawals collided — rebuild and re-sign this one with a new timestamp"}"#,
        ),
    ];

    /// The path a real response takes, end to end: the same 2xx split
    /// `Client::submit` makes, then `classify`, then `verdict`. Hand-building
    /// an `ApiError` here would assert a mapping production cannot produce,
    /// which is how the 409 defect survived a review that claimed to cover it.
    fn verdict_for_response(status: u16, body: &str) -> Verdict {
        let result = if (200..300).contains(&status) {
            Ok(body.to_string())
        } else {
            Err(alphanumeric_gui::backend::classify(
                status,
                Some("application/json"),
                body,
            ))
        };
        verdict(&result)
    }

    // Every 409 means "something already exists", and the two that most likely
    // mean the money already moved must never read as "nothing was admitted,
    // safe to send again" -- that wording plus a form that keeps its fields is
    // two clicks from a genuine second payment.
    #[test]
    fn every_409_says_check_before_sending_again() {
        // Each token must produce ITS OWN wording, not merely some ambiguous
        // wording. Pinned against the exact constant, because the catch-all
        // for an unfamiliar conflict is also `Ambiguous` and also mentions the
        // body's prose -- so an assertion any weaker than this would stay
        // green with every arm of `conflict_verdict` deleted, which is
        // precisely how the original defect went unnoticed.
        let expected = [
            ("existing_transaction_unattributed", EXISTING_UNATTRIBUTED),
            ("historical_outcome_unavailable", HISTORICAL_UNAVAILABLE),
            ("idempotency_conflict", IDEMPOTENCY_CONFLICT),
            ("transaction_collision", TRANSACTION_COLLISION),
        ];
        for (token, body) in CONFLICT_BODIES {
            let wording = expected
                .iter()
                .find(|(name, _)| *name == token)
                .expect("every conflict body has its own wording")
                .1;
            let Verdict::Ambiguous { message, .. } = verdict_for_response(409, body) else {
                panic!("{token} must not read as a plain failure");
            };
            assert_eq!(
                message, wording,
                "{token} did not get its own conflict wording"
            );
        }

        // The four are told apart from each other, so no two conflicts can
        // quietly share an explanation.
        let mut wordings: Vec<&str> = expected.iter().map(|(_, wording)| *wording).collect();
        wordings.sort_unstable();
        let count = wordings.len();
        wordings.dedup();
        assert_eq!(wordings.len(), count, "two conflicts share wording");

        // An unrecognised conflict still fails closed as a conflict, because
        // the routing is on the status CODE, not on a list of tokens.
        let unknown = verdict_for_response(
            409,
            r#"{"ok":false,"status":"some_future_conflict","error":"a conflict from a later node"}"#,
        );
        let Verdict::Ambiguous {
            message: generic, ..
        } = unknown
        else {
            panic!("an unfamiliar 409 must still fail closed");
        };
        assert!(
            generic.to_ascii_lowercase().contains("checked"),
            "the catch-all must still say to check first: {generic}"
        );

        // The two whose money may already have moved say so.
        for token in [
            "existing_transaction_unattributed",
            "historical_outcome_unavailable",
        ] {
            let body = CONFLICT_BODIES
                .iter()
                .find(|(name, _)| *name == token)
                .expect("a body for every token")
                .1;
            let Verdict::Ambiguous { message, .. } = verdict_for_response(409, body) else {
                unreachable!()
            };
            assert!(
                message.to_ascii_lowercase().contains("already"),
                "{token}: {message}"
            );
        }
    }

    // The classification must survive the node rewording its own prose: the
    // `error` string is documentation text, and a match against it is a match
    // that quietly stops matching the day someone improves the sentence.
    #[test]
    fn a_conflict_is_recognised_by_its_token_and_not_by_its_prose() {
        for (token, body) in CONFLICT_BODIES {
            let original = verdict_for_response(409, body);
            let reworded = body.replace(
                body.split(r#""error":""#).nth(1).expect("an error field"),
                r#"some entirely different sentence"}"#,
            );
            assert_eq!(
                verdict_for_response(409, &reworded),
                original,
                "{token} changed meaning when only its prose changed"
            );
            // And the token alone, with no prose and no identifiers at all,
            // still classifies the same way. Only the wording is compared
            // here: a body carrying no identifiers legitimately surfaces none,
            // which is a different thing from being classified differently.
            let Verdict::Ambiguous {
                message: bare,
                details,
            } = verdict_for_response(409, &format!(r#"{{"ok":false,"status":"{token}"}}"#))
            else {
                panic!("{token} must classify from its token alone");
            };
            let Verdict::Ambiguous {
                message: full,
                details: full_details,
            } = original
            else {
                unreachable!()
            };
            assert_eq!(
                bare, full,
                "{token} depends on prose that need not be there"
            );
            assert!(details.is_empty());
            assert!(
                !full_details.is_empty(),
                "{token}'s real body carries identifiers that must reach the screen"
            );
        }
    }

    // Telling somebody to check, and giving them nothing to check against,
    // leaves them two moves: do nothing, or send again. Those are the two
    // outcomes the message exists to prevent, so every conflict has to carry
    // what the payment can be looked up BY -- as data beside the wording, not
    // folded into the sentence.
    #[test]
    fn a_conflict_says_what_to_look_the_payment_up_by() {
        // The identifiers each real body carries, named in `src/a9/node.rs`.
        for (token, expected) in [
            (
                "existing_transaction_unattributed",
                vec![
                    ("Transaction", "t"),
                    ("Its state on this node", "confirmed"),
                    ("In block", "961410"),
                    ("Submission key", "k"),
                ],
            ),
            (
                "historical_outcome_unavailable",
                vec![
                    ("Transaction", "t"),
                    ("Last seen at height", "961410"),
                    ("Submission key", "k"),
                ],
            ),
            (
                "idempotency_conflict",
                vec![
                    ("Transaction already under this key", "t"),
                    ("Submission key", "k"),
                ],
            ),
            (
                "transaction_collision",
                vec![
                    ("Colliding transaction", "t"),
                    ("Submission key", "k"),
                    ("Colliding submission key", "k2"),
                ],
            ),
        ] {
            let body = CONFLICT_BODIES
                .iter()
                .find(|(name, _)| *name == token)
                .expect("a body for every token")
                .1;
            let Verdict::Ambiguous { details, .. } = verdict_for_response(409, body) else {
                unreachable!()
            };
            let actual: Vec<(&str, &str)> = details
                .iter()
                .map(|detail| (detail.label.as_str(), detail.value.as_str()))
                .collect();
            assert_eq!(actual, expected, "{token} withheld its identifiers");
        }

        // An unfamiliar conflict surfaces whatever identifiers it does carry,
        // for the same fail-closed reason its wording still says to check.
        let Verdict::Ambiguous { details, .. } = verdict_for_response(
            409,
            r#"{"ok":false,"status":"some_future_conflict","tx_id":"abc","error":"prose"}"#,
        ) else {
            panic!("an unfamiliar 409 is still a conflict");
        };
        assert_eq!(
            details,
            vec![Detail {
                label: "Transaction".to_string(),
                value: "abc".to_string(),
            }]
        );

        // A null is skipped rather than rendered: the node sends
        // `height: null` whenever the existing transaction is pending rather
        // than confirmed, and "In block: null" is worse than saying nothing.
        let pending = serde_json::json!({
            "status": "existing_transaction_unattributed",
            "tx_id": "abc",
            "existing_status": "pending",
            "height": serde_json::Value::Null,
        });
        assert_eq!(
            conflict_details(&pending),
            vec![
                Detail {
                    label: "Transaction".to_string(),
                    value: "abc".to_string()
                },
                Detail {
                    label: "Its state on this node".to_string(),
                    value: "pending".to_string()
                },
            ]
        );
        // A body with nothing to identify does not invent anything.
        assert!(conflict_details(&serde_json::Value::Null).is_empty());
    }

    // Three of the node's replay statuses are terminal 200s. Offering "retry
    // the same payment" for `rejected` or `expired` offers an answer that can
    // never change, and `existing_transaction_unattributed` arriving as a 200
    // means what it means as a 409.
    #[test]
    fn a_terminal_two_hundred_is_not_offered_a_retry_that_cannot_resolve() {
        let rejected = verdict_for_response(
            200,
            r#"{"status":"rejected","reason":"below the relay floor","ok":false,"idempotency_key":"k","tx_id":"t","idempotent_replay":true}"#,
        );
        let Verdict::Refused(message) = rejected else {
            panic!("a recorded rejection is terminal, not unresolved");
        };
        assert!(message.contains("below the relay floor"), "{message}");

        assert!(matches!(
            verdict_for_response(
                200,
                r#"{"status":"expired","ok":false,"idempotency_key":"k","tx_id":"t","idempotent_replay":true}"#,
            ),
            Verdict::Refused(_)
        ));

        // Reserved is genuinely unresolved, and retrying the identical body is
        // the documented remedy rather than a guess. Pinned against its own
        // WORDING, not just the variant: the catch-all for an unknown status
        // produces the same `Unresolved`, so an assertion on the variant alone
        // would stay green with this arm deleted -- an arm no test can see is
        // an arm that silently stops being true.
        assert_eq!(
            verdict_for_response(
                200,
                r#"{"status":"submission_reserved","ok":false,"idempotency_key":"k","tx_id":"t","idempotent_replay":true}"#,
            ),
            Verdict::Unresolved(SUBMISSION_RESERVED.to_string())
        );

        // The 409's twin, arriving as a replay.
        assert_eq!(
            verdict_for_response(
                200,
                r#"{"status":"existing_transaction_unattributed","ok":false,"idempotency_key":"k","tx_id":"t","idempotent_replay":true}"#,
            ),
            Verdict::Ambiguous {
                message: EXISTING_UNATTRIBUTED.to_string(),
                details: vec![
                    Detail {
                        label: "Transaction".to_string(),
                        value: "t".to_string(),
                    },
                    Detail {
                        label: "Submission key".to_string(),
                        value: "k".to_string(),
                    },
                ],
            }
        );

        // `ok` is true only while a payment is progressing or done, so a false
        // one on a status this wallet has never seen sharpens the warning --
        // without turning an unknown answer into a claimed outcome.
        let Verdict::Unresolved(message) = verdict_for_response(
            200,
            r#"{"status":"a_status_from_a_later_node","ok":false,"tx_id":"t"}"#,
        ) else {
            panic!("an unknown status stays unresolved");
        };
        assert!(message.contains("not progressing"), "{message}");
    }

    // A confirmation can sit on screen while the ten-second poll moves the
    // spendable figure underneath it. The ceiling is re-checked at the instant
    // of signing, against the SAME fee the user approved -- re-preparing here
    // would quietly swap in a newer estimate and sign for something else.
    #[test]
    fn the_ceiling_is_checked_again_at_the_moment_of_signing() {
        let payment = prepare(Spendable::Known(1_000_000_000), "1").expect("prepared");
        assert_eq!(
            recheck_spendable(&payment, Spendable::Known(1_000_000_000)),
            Ok(())
        );
        assert_eq!(
            recheck_spendable(&payment, Spendable::Known(payment.total_units)),
            Ok(())
        );
        assert!(matches!(
            recheck_spendable(&payment, Spendable::Known(payment.total_units - 1)),
            Err(Blocker::InsufficientSpendable { .. })
        ));
        assert_eq!(
            recheck_spendable(&payment, Spendable::Pending),
            Err(Blocker::SpendableUnknown)
        );
        assert_eq!(
            recheck_spendable(&payment, Spendable::Unavailable),
            Err(Blocker::SpendableUnavailable)
        );
    }

    // Every blocker has to say something a user can act on. An empty or
    // duplicated explanation is a dead end in the one screen that must not
    // have one.
    #[test]
    fn every_blocker_explains_itself_distinctly() {
        let blockers = [
            Blocker::SpendableUnknown,
            Blocker::SpendableUnavailable,
            Blocker::NoRecipient,
            Blocker::BadRecipient,
            Blocker::SelfPayment,
            Blocker::Amount(AmountProblem::Empty),
            Blocker::Amount(AmountProblem::NotANumber),
            Blocker::Amount(AmountProblem::TooPrecise),
            Blocker::Amount(AmountProblem::TooLarge),
            Blocker::ZeroAmount,
            Blocker::NoFeeEstimate,
            Blocker::DustAmount {
                floor_units: tx::RELAY_FLOOR_UNITS,
            },
            Blocker::NotRepresentable,
            Blocker::InsufficientSpendable {
                total_units: 100_020_000,
                spendable_units: 5,
            },
        ];
        let mut seen: Vec<String> = Vec::new();
        for blocker in &blockers {
            let explanation = blocker.explain();
            assert!(explanation.len() > 10, "{blocker:?} explains nothing");
            assert!(
                !seen.contains(&explanation),
                "{blocker:?} repeats another blocker's wording"
            );
            seen.push(explanation);
        }
        // The one that has to carry figures actually carries them.
        let short = Blocker::InsufficientSpendable {
            total_units: 100_020_000,
            spendable_units: 5,
        }
        .explain();
        assert!(short.contains("1.0002"));
        assert!(short.contains("0.00000005"));
    }

    // The ten-second poll refreshes only the ACTIVE address, so the row a
    // payment's sender lives in can be minutes old -- and re-checking against
    // it would be the appearance of requirement 1's check rather than the
    // check. The ceiling therefore comes from a fetch made for this purpose,
    // and every way that fetch can fail to produce a number lands in the same
    // refusal, never on the stale figure it was meant to replace.
    #[test]
    fn the_ceiling_comes_from_a_fresh_fetch_and_never_falls_back() {
        let fresh = alphanumeric_gui::backend::parse_address_state(
            r#"{"balance_units":"123456789012345","spendable_units":"141454332907427",
                "transactions":[],"history_available":true,"index_ready":true,
                "index_height":959513}"#,
        )
        .expect("a captured address response");
        assert_eq!(
            ceiling_from_fetch(&Ok(fresh)),
            Ok(Spendable::Known(141_454_332_907_427))
        );

        // A null overlay is a real answer meaning "cannot compute", and it
        // reaches the same refusal path as every other missing ceiling.
        let rebuilding = alphanumeric_gui::backend::parse_address_state(
            r#"{"balance_units":"0","spendable_units":null,"transactions":null,
                "history_available":false,"index_ready":false,"index_height":null}"#,
        )
        .expect("a rebuilding index is a healthy response");
        assert_eq!(
            ceiling_from_fetch(&Ok(rebuilding)),
            Ok(Spendable::Unavailable)
        );
        let payment = prepare(Spendable::Known(1_000_000_000), "1").expect("prepared");
        assert_eq!(
            recheck_spendable(&payment, Spendable::Unavailable),
            Err(Blocker::SpendableUnavailable)
        );

        // A fetch that did not answer is a refusal to sign. In particular the
        // retryable ones: "the node is busy" is not "you have enough".
        for error in [
            ApiError::Busy,
            ApiError::RateLimited,
            ApiError::Transport("connection refused".into()),
            ApiError::NodeFault("index poisoned".into()),
            ApiError::Malformed("unexpected address response".into()),
            ApiError::Rejected("nope".into()),
        ] {
            let outcome = ceiling_from_fetch(&Err(error.clone()));
            let Err(message) = outcome else {
                panic!("{error:?} must not yield a ceiling to sign against");
            };
            assert!(message.contains("Nothing was signed"), "{message}");
        }
    }

    // Which outcomes leave the form filled in is a safety decision, not a
    // convenience one, so it is decided from the verdict in one place rather
    // than by whichever button the view happened to render.
    #[test]
    fn only_an_outcome_that_admitted_nothing_leaves_the_form_filled_in() {
        // Nothing was admitted and the user is being asked to correct
        // something: making them retype a 40-character address by hand to do
        // that is its own way to send funds somewhere unrecoverable.
        assert!(keeps_the_form(&Verdict::Refused(
            "transaction rejected: below the relay floor".into()
        )));

        // Everything else clears. `Settled` because the payment happened;
        // `Unresolved` and `Ambiguous` because it MAY have, and neither asks
        // for a correction -- a pre-filled form there is a short path to a
        // genuine second payment.
        for verdict in [
            Verdict::Settled {
                settlement: Settlement::Accepted,
                tx_id: None,
                height: None,
            },
            Verdict::Settled {
                settlement: Settlement::AlreadyConfirmed,
                tx_id: None,
                height: Some(7),
            },
            Verdict::Unresolved("the node never answered".into()),
            Verdict::Ambiguous {
                message: "it may already be on the chain".into(),
                details: Vec::new(),
            },
        ] {
            assert!(
                !keeps_the_form(&verdict),
                "{verdict:?} must not leave a payment one click from going again"
            );
        }
    }

    // Zero is refused before the fee clamp is even consulted, so the reason a
    // user sees is the one they can act on.
    #[test]
    fn a_zero_amount_is_refused_as_zero() {
        assert_eq!(
            prepare(Spendable::Known(1_000_000_000), "0"),
            Err(Blocker::ZeroAmount)
        );
        assert_eq!(
            prepare(Spendable::Known(1_000_000_000), "0.00000000"),
            Err(Blocker::ZeroAmount)
        );
    }

    /// Only `Refused` may be painted as recoverable. The other two failures
    /// mean the payment may exist on the chain, and a colour that invites a
    /// retry is a colour that invites a second payment.
    ///
    /// This is the screen half of the routing this branch spent its review
    /// rounds on: getting the verdict right and then drawing all three
    /// failures the same colour would put the defect back.
    #[test]
    fn only_a_refusal_is_painted_as_recoverable() {
        let settled = verdict_colour(&Verdict::Settled {
            settlement: Settlement::Accepted,
            tx_id: None,
            height: None,
        });
        let refused = verdict_colour(&Verdict::Refused("no".into()));
        let unresolved = verdict_colour(&Verdict::Unresolved("unknown".into()));
        let ambiguous = verdict_colour(&Verdict::Ambiguous {
            message: "check first".into(),
            details: Vec::new(),
        });

        assert_eq!(settled, crate::theme::ACCENT);
        assert_eq!(refused, crate::theme::ADVISORY);
        assert_eq!(unresolved, crate::theme::DANGER);
        assert_eq!(ambiguous, crate::theme::DANGER);

        assert_ne!(refused, unresolved, "a refusal must not read as an unknown");
        assert_ne!(refused, ambiguous, "a refusal must not read as a conflict");
        assert_ne!(settled, refused);
    }

    /// The result card's frame has to come from the same source as its text,
    /// or a fixed ADVISORY border tells a story the verdict's own colour
    /// contradicts -- softer than the DANGER text it surrounds on the two
    /// verdicts that mean money may already have moved.
    #[test]
    fn the_result_card_border_follows_the_verdict_not_a_fixed_advisory() {
        let theme = crate::theme::alphanumeric_theme();
        let refused = verdict_colour(&Verdict::Refused("no".into()));
        let unresolved = verdict_colour(&Verdict::Unresolved("unknown".into()));

        let refused_border = theme::verdict_card(refused)(&theme).border.color;
        let unresolved_border = theme::verdict_card(unresolved)(&theme).border.color;

        assert_ne!(
            refused_border, unresolved_border,
            "the card's border must distinguish a refusal from an unresolved outcome, \
             just as the text inside it does"
        );
    }
}
