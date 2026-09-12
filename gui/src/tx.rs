//! Transaction construction and signing, implemented against `SIGNING_SPEC.md`
//! rather than against the node's code.

use ml_dsa::{Keypair, MlDsa87, Signature, Signer, SigningKey};
use sha2::{Digest, Sha256};

// ml-dsa gates ALL of its wiping behind an off-by-default feature: without it,
// SigningKey::drop and ExpandedSigningKey::drop are no-ops and every signature
// leaves several KB of full spending key in freed memory. This fails to compile
// if the feature is ever dropped.
const _: fn() = || {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    assert_zeroize_on_drop::<SigningKey<MlDsa87>>();
};

/// 1 coin = 100,000,000 atomic units.
pub const MONEY_SCALE: i128 = 100_000_000;

/// Render atomic units as the fixed 8-decimal string the signed message uses.
///
/// Built from integers, never from a float: formatting an f64 is where "1.5"
/// instead of "1.50000000" comes from, and the node re-derives this string
/// byte-for-byte before checking the signature.
///
/// Negative input is a caller error: this formats it as `-1.50000000` rather
/// than rejecting it, and the node will reject the transaction.
pub fn units_to_amount_string(units: i128) -> String {
    let negative = units < 0;
    let magnitude = units.unsigned_abs();
    let scale = MONEY_SCALE.unsigned_abs();
    let whole = magnitude / scale;
    let fraction = magnitude % scale;
    format!(
        "{}{}.{:08}",
        if negative { "-" } else { "" },
        whole,
        fraction
    )
}

/// The exact byte string the node verifies the signature over:
/// `{sender}:{recipient}:{amount}:{fee}:{timestamp}`.
///
/// Nothing else is included -- not the public key, not the sig_hash, not JSON.
/// Addresses go in verbatim; never change their case after building this.
pub fn signing_message(
    sender: &str,
    recipient: &str,
    amount_units: i128,
    fee_units: i128,
    timestamp: u64,
) -> String {
    format!(
        "{sender}:{recipient}:{}:{}:{timestamp}",
        units_to_amount_string(amount_units),
        units_to_amount_string(fee_units)
    )
}

/// Derive the 2592-byte encoded verifying key from a 32-byte seed.
///
/// The seed IS the secret key in ML-DSA-87; the public key is derived, which is
/// why 32 bytes back up a whole wallet.
pub fn public_key_from_seed(seed: &[u8; 32]) -> Vec<u8> {
    let signing_key = SigningKey::<MlDsa87>::from_seed(&(*seed).into());
    signing_key.verifying_key().encode().to_vec()
}

/// `hex(sha256(public_key)[..20])` -- exactly 40 lowercase hex characters.
pub fn address_from_public_key(public_key: &[u8]) -> String {
    hex::encode(&Sha256::digest(public_key)[..20])
}

/// Sign with pure ML-DSA-87 and an empty context, in the deterministic variant
/// SIGNING_SPEC's vector uses.
pub fn sign_message(seed: &[u8; 32], message: &[u8]) -> Vec<u8> {
    let signing_key = SigningKey::<MlDsa87>::from_seed(&(*seed).into());
    let signature: Signature<MlDsa87> = signing_key.sign(message);
    signature.encode().to_vec()
}

/// `sha256(signature_bytes)` -- the commitment the transaction carries as
/// `sig_hash`.
pub fn sig_hash(signature: &[u8]) -> String {
    hex::encode(Sha256::digest(signature))
}

/// The relay floor to assume when the node has not been asked yet.
///
/// Nodes refuse to mempool or relay a transaction below their relay floor.
/// Policy, not consensus -- so it is the NODE's number, and the node publishes
/// it on every `/explorer/fee-estimate` as `floor_fee_units`. `clamp_fee_units`
/// uses that answer whenever there is one; this constant is what it falls back
/// to before any estimate has been fetched, and is the value every released
/// node has advertised. A below-floor transaction never propagates, so treat
/// whichever of the two is in force as a hard minimum.
pub const RELAY_FLOOR_UNITS: i128 = 10_000; // 0.0001 coins

/// Whisper's per-amount slope. Mirrors the node's FEE_PERCENTAGE.
pub const FEE_PERCENTAGE: f64 = 0.000563063063;

/// Whisper's constant offset. Mirrors the node's WHISPER_MIN_AMOUNT.
pub const WHISPER_MIN_UNITS: i128 = 10_000; // 0.0001 coins

/// Highest fee that is NOT read as a whisper for this amount.
///
/// Classification is a pure fee-band test with no flag and no marker: a fee is
/// read as a message whenever `fee - amount * FEE_PERCENTAGE` lands in
/// [0.0001, 0.01]. Mirrors the node's `max_non_whisper_fee_units` so an ordinary
/// payment is not displayed as a four-letter code.
pub fn max_non_whisper_fee_units(amount_units: i128) -> i128 {
    let amount = amount_units as f64 / MONEY_SCALE as f64;
    // Saturating: a float-to-i128 cast saturates rather than wrapping, so an
    // absurd amount lands on `i128::MAX` and the plain `+ WHISPER_MIN_UNITS`
    // that used to follow would overflow it. This runs on a render path.
    let band_start = ((amount * FEE_PERCENTAGE) * MONEY_SCALE as f64).round() as i128;
    band_start
        .saturating_add(WHISPER_MIN_UNITS)
        .saturating_sub(1)
}

/// The most this wallet will sign as a fee, whatever the node recommends.
///
/// The node's own CLI refuses any wallet fee above this
/// (`WALLET_FEE_SAFETY_LIMIT_UNITS`, `src/a9/blockchain.rs`), for the reason it
/// gives there: neither a typo nor a manipulated fee recommendation should be
/// able to burn a meaningful balance in one transaction. This wallet had no
/// equivalent, and the whisper band is no substitute -- its ceiling is
/// PROPORTIONAL to the amount, so on a large send it permits a fee of hundreds
/// of coins. The fee is shown before signing, so this is not the only guard,
/// but the node's own limit is the right absolute bound and this applies it too.
///
/// Deliberately NOT read from the wire, unlike the relay floor beside it -- the
/// node publishes this number as `explicit_cap_fee_units` on the same response.
/// The floor is a policy this wallet has to follow to get a transaction
/// relayed; this is a bound on what a node may talk this wallet into signing,
/// and a bound taken from the thing it guards against is not a bound. If the
/// network's limit ever moves, this constant moves with it in a release.
pub const FEE_SAFETY_LIMIT_UNITS: i128 = 1_000_000; // 0.01 coins

/// Bring a requested fee inside `[floor, min(whisper band start, safety limit))`.
///
/// `floor_units` is the node's own `floor_fee_units`, as published on
/// `/explorer/fee-estimate`; `None` means no estimate has been fetched and
/// falls back to `RELAY_FLOOR_UNITS`. Consulting the node's answer rather than
/// the compiled-in constant is the whole point of carrying it: the two agree
/// today, and a relay-policy change on the node would otherwise never reach
/// this wallet -- it would keep signing below-floor fees that never propagate.
///
/// Returns `None` when no such fee exists: the band start does not move with
/// the amount until the amount is large enough, so every amount below
/// `smallest_sendable_units(floor)` refuses -- 889 units (0.00000889 coins) at
/// the default floor, and higher if the node raises it. That is correct -- for
/// dust, any fee at or above the floor is already inside the band -- but a
/// caller that reads this as "only a zero amount" and unwraps will panic on a
/// real payment of a few hundred units. Refusing beats shipping a fee that
/// either never propagates or renders the payment as a message.
///
/// `FEE_SAFETY_LIMIT_UNITS` caps the ceiling in ABSOLUTE terms. The whisper
/// band's ceiling is proportional to the amount, so it bounds nothing on a
/// large send; the node's own wallet has refused a fee above this limit all
/// along and this one now does too.
pub fn clamp_fee_units(
    amount_units: i128,
    requested_fee_units: i128,
    floor_units: Option<i128>,
) -> Option<i128> {
    let floor = floor_units.unwrap_or(RELAY_FLOOR_UNITS);
    let ceiling = max_non_whisper_fee_units(amount_units).min(FEE_SAFETY_LIMIT_UNITS);
    if ceiling < floor {
        return None;
    }
    Some(requested_fee_units.clamp(floor, ceiling))
}

/// The smallest amount that can carry any fee at all, under `floor_units`.
///
/// A fee has to be at or above the floor and strictly below the whisper band,
/// whose first unit is `max_non_whisper_fee_units(amount) + 1`. That ceiling
/// rises with the amount, so below some threshold no fee satisfies both and
/// `clamp_fee_units` refuses. The threshold moves with the floor, and the floor
/// belongs to the node -- which is why this is a function and not the constant
/// it used to be. A message that names a fixed 0.00000889 is a message telling
/// someone to send an amount that is still dust on a node that raised its
/// floor.
///
/// At the fallback floor the answer is 889 units, comfortably above the node's
/// own 564-unit minimum transaction amount -- so this, not that minimum, is the
/// binding constraint of the two.
pub fn smallest_sendable_units(floor_units: i128) -> i128 {
    // `max_non_whisper_fee_units(a)` is `round(a * FEE_PERCENTAGE) +
    // WHISPER_MIN_UNITS - 1`, so clearing the floor means the rounded product
    // must reach `needed`.
    // Saturating throughout: `floor_units` arrives from the node, and nothing on
    // this path is worth a panic in a debug build or a wrapped, nonsense figure
    // in a release one -- it renders an error message.
    let needed = floor_units
        .saturating_sub(WHISPER_MIN_UNITS)
        .saturating_add(1);
    if needed <= 0 {
        // The band already starts above the floor at any amount, so the
        // smallest amount that can be sent at all is one unit.
        return 1;
    }
    // `round(x) >= needed` iff `x >= needed - 0.5`.
    let mut candidate = (((needed as f64) - 0.5) / FEE_PERCENTAGE).ceil() as i128;
    candidate = candidate.max(1);
    // The closed form multiplies the units directly;
    // `max_non_whisper_fee_units` scales through coins and back. Correct the
    // last unit or two against the real function rather than trusting the two
    // float paths to land identically -- bounded, because this is a number for
    // an error message and a hostile floor must not spin here.
    for _ in 0..4 {
        if candidate > 1 && max_non_whisper_fee_units(candidate.saturating_sub(1)) >= floor_units {
            candidate = candidate.saturating_sub(1);
        } else {
            break;
        }
    }
    for _ in 0..4 {
        if max_non_whisper_fee_units(candidate) < floor_units {
            candidate = candidate.saturating_add(1);
        } else {
            break;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    // The one place SIGNING_SPEC calls out as easiest to get wrong: amounts in the
    // signed message are ALWAYS 8 fractional digits. A node re-derives this string
    // byte-for-byte, so "1.5" instead of "1.50000000" is a rejected transaction.
    #[test]
    fn amounts_always_carry_eight_fractional_digits() {
        assert_eq!(units_to_amount_string(150_000_000), "1.50000000");
        assert_eq!(units_to_amount_string(100_000), "0.00100000");
        assert_eq!(units_to_amount_string(1), "0.00000001");
        assert_eq!(units_to_amount_string(0), "0.00000000");
        assert_eq!(units_to_amount_string(100_000_000), "1.00000000");
        assert_eq!(units_to_amount_string(1_234_567_890_123), "12345.67890123");
    }

    // The exact string from SIGNING_SPEC's test vector. Nothing else is included --
    // not the public key, not the sig_hash, not JSON.
    #[test]
    fn signing_message_matches_the_spec_vector() {
        let message = signing_message(
            "9e1e860361994891b3165e611dc5aefcdd37dfbf",
            "84dab431b53e6522fe2e74914eec99f17758f4e3",
            150_000_000,
            100_000,
            1_783_600_000,
        );
        assert_eq!(
            message,
            "9e1e860361994891b3165e611dc5aefcdd37dfbf:84dab431b53e6522fe2e74914eec99f17758f4e3:1.50000000:0.00100000:1783600000"
        );
    }

    // Guards against a formatter that would render large values with separators or
    // scientific notation, both of which a node would reject.
    #[test]
    fn large_amounts_have_no_separators_or_exponent() {
        let rendered = units_to_amount_string(9_007_199_254_740_993);
        assert!(!rendered.contains(','));
        assert!(!rendered.contains('e'));
        assert_eq!(rendered, "90071992.54740993");
    }

    // SIGNING_SPEC's test vector, byte for byte. If all three hashes match, this
    // signer is network-compatible; if any drifts, transactions this GUI produces
    // will be rejected and the funds behind them are unspendable from here.
    #[test]
    fn signing_matches_the_spec_test_vector() {
        use sha2::{Digest, Sha256};

        let seed = [0x07u8; 32];

        let public_key = public_key_from_seed(&seed);
        assert_eq!(public_key.len(), 2592);
        assert_eq!(
            hex::encode(Sha256::digest(&public_key)),
            "9e1e860361994891b3165e611dc5aefcdd37dfbf5f247943daaeb57141fe7b6e",
            "public key drifted from the SIGNING_SPEC vector"
        );

        let sender = address_from_public_key(&public_key);
        assert_eq!(sender, "9e1e860361994891b3165e611dc5aefcdd37dfbf");
        assert_eq!(sender.len(), 40);

        let message = signing_message(
            &sender,
            "84dab431b53e6522fe2e74914eec99f17758f4e3",
            150_000_000,
            100_000,
            1_783_600_000,
        );
        let signature = sign_message(&seed, message.as_bytes());
        assert_eq!(signature.len(), 4627);
        assert_eq!(
            sig_hash(&signature),
            "e3b8ce82ea7c02c008d89049aff56fa86f34d3320bae82ccf000279c74144339",
            "signature drifted from the SIGNING_SPEC vector"
        );
    }

    // The address is derived, so a caller cannot pass an arbitrary sender: a node
    // re-derives it from pub_key and rejects a mismatch.
    #[test]
    fn address_is_the_first_twenty_bytes_of_the_public_key_digest() {
        use sha2::{Digest, Sha256};

        let public_key = public_key_from_seed(&[0x11u8; 32]);
        let digest = Sha256::digest(&public_key);
        assert_eq!(
            address_from_public_key(&public_key),
            hex::encode(&digest[..20])
        );
    }

    // Deterministic signing: the spec's vector uses it, so two runs over the same
    // message must produce identical bytes or the vector above is meaningless.
    #[test]
    fn signing_is_deterministic() {
        let seed = [0x21u8; 32];
        let first = sign_message(&seed, b"same message");
        let second = sign_message(&seed, b"same message");
        assert_eq!(first, second);
    }

    // A fee at or above amount*FEE_PERCENTAGE + 0.0001 reads as a whisper: the
    // node decodes it into a four-letter code and the reference client DISPLAYS
    // the payment as a message. Staying below that boundary is what keeps an
    // ordinary payment looking like a payment.
    #[test]
    fn fees_stay_below_the_whisper_band() {
        // 1.5 coins. The band start is computed here INDEPENDENTLY from the
        // formula, not from max_non_whisper_fee_units -- deriving it from the
        // function under test would make every assertion below circular.
        let amount = 150_000_000;
        let independent_band_start = independent_band_start_units(amount);

        assert_eq!(
            max_non_whisper_fee_units(amount),
            independent_band_start - 1,
            "the maximum non-whisper fee must be one unit below the band start"
        );
        assert!(max_non_whisper_fee_units(amount) >= RELAY_FLOOR_UNITS);
    }

    // Below the relay floor a transaction does not propagate at all, so a clamp
    // must never produce one -- it should refuse instead of silently shipping a
    // fee that goes nowhere.
    //
    // Amounts above the dust region only: `if let Some` here would let an
    // implementation that returned None for EVERYTHING pass this test, which
    // is exactly how the refusal region stayed undocumented.
    #[test]
    fn clamping_never_returns_a_fee_below_the_relay_floor() {
        for amount in [1_000i128, 150_000_000, 100_000_000_000] {
            let fee = clamp_fee_units(amount, 1_000_000_000, None).unwrap_or_else(|| {
                panic!("amount {amount} is above dust and must have a valid fee")
            });
            assert!(
                fee >= RELAY_FLOOR_UNITS,
                "amount {amount} produced a fee below the relay floor"
            );
            assert!(
                fee < independent_band_start_units(amount),
                "amount {amount} produced a fee inside the whisper band"
            );
        }
    }

    // A requested fee already in range passes through unchanged -- the clamp is a
    // ceiling, not a policy that overrides the user.
    #[test]
    fn a_fee_already_in_range_is_left_alone() {
        let amount = 150_000_000;
        let requested = RELAY_FLOOR_UNITS + 1;
        assert!(requested < independent_band_start_units(amount));
        assert_eq!(clamp_fee_units(amount, requested, None), Some(requested));
    }

    // The refusal region is wider than "zero", and a caller that believes the old
    // doc would unwrap and panic on a real payment. Pin both edges of it. 889 is
    // the first amount with any valid fee; 888 itself is deliberately not pinned
    // because its scaled fee sits ~5.6e-11 from a rounding boundary.
    #[test]
    fn dust_amounts_have_no_valid_fee() {
        assert_eq!(clamp_fee_units(0, 50_000, None), None);
        assert_eq!(clamp_fee_units(1, 50_000, None), None);
        assert_eq!(clamp_fee_units(500, 50_000, None), None);
        assert_eq!(
            clamp_fee_units(889, 50_000, None),
            Some(RELAY_FLOOR_UNITS),
            "889 units is the first amount with a fee that is both above the floor and below the band"
        );
    }

    // The node publishes its relay floor with every estimate, and the clamp has
    // to use THAT rather than the compiled-in constant: they agree today, so a
    // clamp that ignored the node's answer would look correct right up to the
    // release that changed relay policy.
    #[test]
    fn the_clamp_follows_the_node_s_floor_rather_than_the_constant() {
        let amount = 150_000_000; // 1.5 coins, comfortably above dust.

        // A floor ABOVE the constant lifts a fee the constant would have
        // accepted unchanged.
        assert_eq!(
            clamp_fee_units(amount, RELAY_FLOOR_UNITS, Some(40_000)),
            Some(40_000),
            "a node that raised its floor must raise the fee this wallet signs"
        );
        // A floor BELOW the constant lets a smaller fee through, because the
        // node -- not this binary -- decides what it will relay.
        assert_eq!(
            clamp_fee_units(amount, 2_500, Some(2_000)),
            Some(2_500),
            "a node that lowered its floor must not be overruled by the fallback"
        );
        // And `None` is the pre-estimate fallback, unchanged.
        assert_eq!(
            clamp_fee_units(amount, 1, None),
            clamp_fee_units(amount, 1, Some(RELAY_FLOOR_UNITS))
        );

        // A floor high enough to swallow the whole band refuses outright rather
        // than clamping into it.
        assert_eq!(clamp_fee_units(amount, 50_000, Some(10_000_000)), None);
    }

    // The whisper band's ceiling is PROPORTIONAL to the amount, so it bounds
    // nothing in absolute terms: on a large enough send it would let a
    // manipulated recommendation put hundreds of coins into the fee. The node's
    // own CLI has always refused a wallet fee above 0.01 coins for exactly that
    // reason; this wallet had no equivalent.
    #[test]
    fn no_recommendation_can_push_the_fee_past_the_absolute_safety_limit() {
        // 10 million coins. The whisper band starts around 5,630 coins here, so
        // the band alone would permit an enormous fee.
        let amount = 1_000_000_000_000_000i128;
        assert!(
            max_non_whisper_fee_units(amount) > FEE_SAFETY_LIMIT_UNITS,
            "this test is pointless unless the band ceiling is the looser bound"
        );

        assert_eq!(
            clamp_fee_units(amount, 500_000_000_000, None),
            Some(FEE_SAFETY_LIMIT_UNITS),
            "a recommendation above the safety limit is clamped to it"
        );
        // Mirrors the node's own constant. If the node ever moves it, this
        // wallet has to move with it rather than quietly signing more.
        assert_eq!(FEE_SAFETY_LIMIT_UNITS, 1_000_000);
        // Below the limit nothing changes: the clamp is a ceiling, not a policy
        // that overrides a fee already in range.
        assert_eq!(clamp_fee_units(amount, 40_000, None), Some(40_000));
    }

    // `floor_units` comes off the wire, and the dust threshold it feeds is
    // computed on a RENDER path. A node returning an absurd floor must not
    // panic a debug build or wrap into a nonsense figure in a release one.
    #[test]
    fn an_absurd_floor_from_the_node_neither_panics_nor_wraps() {
        for floor in [0i128, 1, i128::MAX, i128::MAX - 1, i128::MAX / 2] {
            let threshold = smallest_sendable_units(floor);
            assert!(
                threshold >= 1,
                "floor {floor}: the threshold must stay a sendable amount"
            );
        }
        // The same for the clamp and the band, at the extremes of the amount.
        for amount in [0i128, 1, i128::MAX, i128::MAX / 2] {
            let _ = max_non_whisper_fee_units(amount);
            let _ = clamp_fee_units(amount, i128::MAX, Some(i128::MAX));
            let _ = clamp_fee_units(amount, 0, None);
        }
    }

    // The dust threshold is a function of the floor, so the number the send
    // screen quotes has to be too.
    #[test]
    fn the_dust_threshold_moves_with_the_floor() {
        // The historical constant, now derived.
        assert_eq!(smallest_sendable_units(RELAY_FLOOR_UNITS), 889);

        for floor in [
            WHISPER_MIN_UNITS,
            RELAY_FLOOR_UNITS,
            20_000,
            40_000,
            250_000,
            1_000_000,
        ] {
            let threshold = smallest_sendable_units(floor);
            assert!(
                clamp_fee_units(threshold, floor, Some(floor)).is_some(),
                "floor {floor}: {threshold} must be sendable"
            );
            if threshold > 1 {
                assert_eq!(
                    clamp_fee_units(threshold - 1, floor, Some(floor)),
                    None,
                    "floor {floor}: {} must still be dust",
                    threshold - 1
                );
            }
        }

        // A floor at or below the band's own start makes every positive amount
        // sendable, so the threshold bottoms out at one unit rather than going
        // negative.
        assert_eq!(smallest_sendable_units(1), 1);
        assert_eq!(smallest_sendable_units(0), 1);
    }

    /// The whisper band's first unit, computed straight from the published
    /// formula rather than from the function under test.
    ///
    /// This still shares the production formula's shape, so on its own it only
    /// proves the wiring. `fee_ceiling_matches_the_node` below is what pins
    /// fidelity to the node's actual classifier.
    fn independent_band_start_units(amount_units: i128) -> i128 {
        let coins = amount_units as f64 / MONEY_SCALE as f64;
        ((coins * FEE_PERCENTAGE) * MONEY_SCALE as f64).round() as i128 + WHISPER_MIN_UNITS
    }

    // Fidelity, not wiring. The node classifies a payment as a whisper with its
    // own arithmetic (src/a9/whisper.rs::max_non_whisper_fee_units), which
    // converts to units at a different point than this crate does: it adds the
    // 0.0001 offset in floating point BEFORE the unit conversion, where we add it
    // in integers after. The two can only be shown to agree by comparing against
    // values the NODE produces -- a test that re-derives the expectation from this
    // crate's own formula would pass even if the two had drifted apart.
    //
    // These five were computed from the node's formula, not from this code. A
    // mismatch here means a GUI payment could be displayed as a four-letter
    // message on a chain that read the fee differently.
    #[test]
    fn fee_ceiling_matches_the_node() {
        for (amount_units, expected) in [
            (0i128, 9_999i128),
            (1_000, 10_000),
            (150_000_000, 94_458),
            (1_000_000_000, 573_062),
            (1_000_000_000_000, 563_073_062),
        ] {
            assert_eq!(
                max_non_whisper_fee_units(amount_units),
                expected,
                "fee ceiling for {amount_units} units diverged from the node's classifier"
            );
        }
    }

    // A CROSS-IMPLEMENTATION VECTOR. The node has the identical assertion, over
    // the identical seed, in `src/a9/wallet.rs`. Change one derivation and this
    // literal stops matching on that side -- which is the only thing that would
    // catch it. Until now each side only round-tripped against itself, so the
    // two could drift apart while both test suites stayed green, and the symptom
    // would have been the node mining to an address the GUI cannot spend.
    //
    // Seed 0x11*32 -> ML-DSA-87 verifying key -> hex(sha256(pk)[..20]).
    #[test]
    fn the_address_for_a_fixed_seed_is_the_one_the_node_derives() {
        assert_eq!(
            address_from_public_key(&public_key_from_seed(&[0x11u8; 32])),
            "f811b100866449d60739eeb137ee004e76fb09d4"
        );
    }
}
