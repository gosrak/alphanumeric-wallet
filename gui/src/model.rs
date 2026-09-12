//! Types and conversions the screens need, with no `iced` in sight.
//!
//! Keeping this module GUI-free is what lets the wallet's arithmetic be tested
//! without a window.

use crate::seed::MasterSeed;
use crate::tx::{address_from_public_key, public_key_from_seed, units_to_amount_string};

/// Parse an amount the node sent as a JSON string.
///
/// `*_units` arrives as a string, so this is the wire format rather than a
/// preference. The node also publishes each amount as a decimal f64, which is
/// exact only below 2^53 units -- about 90 million coins. That is far off today,
/// but reading the integer costs nothing and never has to be revisited.
pub fn parse_units(text: &str) -> Result<i128, String> {
    let trimmed = text.trim();
    // Reject a sign character outright. Every amount the node sends -- balance,
    // spendable, fee, amount -- is non-negative, and a user typing an amount to
    // send has no business entering one either. i128::from_str would happily
    // accept "+5" and "-5", so a stray character from a proxy, a transport
    // glitch, or a keyboard would silently flip a value's meaning instead of
    // failing where someone would notice.
    if trimmed.starts_with('-') || trimmed.starts_with('+') {
        return Err("Expected a non-negative integer amount in atomic units.".to_string());
    }
    trimmed
        .parse::<i128>()
        .map_err(|_| "Expected an integer amount in atomic units.".to_string())
}

/// The address a bare 32-byte seed gives. `address_for_index` is this
/// function over a child seed; an imported key has no index and goes
/// straight here.
pub fn address_for_seed(seed: &[u8; 32]) -> String {
    address_from_public_key(&public_key_from_seed(seed))
}

/// The address for one derivation index: master -> child seed -> public key ->
/// address. No shortcut, and no caching that could go stale against the master.
pub fn address_for_index(master: &MasterSeed, index: u32) -> String {
    address_for_seed(&master.child_seed(index))
}

/// A balance for display: fixed 8 decimals with trailing zeros trimmed, so 1.5
/// coins reads as `1.5` and not `1.50000000`. The signed message never uses this
/// form -- `tx::units_to_amount_string` is the one the node verifies against.
pub fn format_coins(units: i128) -> String {
    let fixed = units_to_amount_string(units);
    let trimmed = fixed.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The node sends `*_units` as JSON STRINGS, so parsing a string is the wire
    // format rather than a choice. Reading them as integers also keeps the wallet
    // away from the 2^53-unit boundary (about 90 million coins) past which the
    // node's companion f64 fields stop being exact.
    #[test]
    fn units_parse_exactly_from_strings() {
        assert_eq!(parse_units("0").unwrap(), 0);
        assert_eq!(parse_units("123456789012345").unwrap(), 123_456_789_012_345);
        assert_eq!(parse_units("  20000  ").unwrap(), 20_000);
        // A value that an f64 round trip would corrupt.
        assert_eq!(
            parse_units("9007199254740993").unwrap(),
            9_007_199_254_740_993
        );
    }

    #[test]
    fn units_reject_anything_that_is_not_an_integer() {
        assert!(parse_units("").is_err());
        assert!(parse_units("1.5").is_err());
        assert!(parse_units("abc").is_err());
        assert!(parse_units("1e9").is_err());
    }

    // Every amount the node sends is non-negative, and so is every amount a user
    // can legitimately type. i128::from_str accepts both signs, so without this
    // a stray character would flip a balance's meaning rather than fail where
    // somebody would see it.
    #[test]
    fn units_reject_a_sign_character() {
        assert!(parse_units("-5").is_err());
        assert!(parse_units("+5").is_err());
        assert!(parse_units("-00042").is_err());
        assert!(parse_units(" -1 ").is_err());
    }

    // Deriving an address must go master -> child seed -> public key -> address,
    // with no shortcut. Pinned against the same rule tx.rs implements so a future
    // refactor of this helper cannot quietly change which address index 0 is.
    #[test]
    fn address_for_index_matches_the_long_way_round() {
        let master = MasterSeed::from_bytes([7u8; 32]);
        for index in [0u32, 1, 7] {
            let child = master.child_seed(index);
            let expected =
                crate::tx::address_from_public_key(&crate::tx::public_key_from_seed(&child));
            assert_eq!(address_for_index(&master, index), expected);
            assert_eq!(address_for_index(&master, index).len(), 40);
        }
    }

    #[test]
    fn different_indices_give_different_addresses() {
        let master = MasterSeed::from_bytes([9u8; 32]);
        assert_ne!(address_for_index(&master, 0), address_for_index(&master, 1));
    }

    // Display trims trailing zeros so a balance reads as 1.5 rather than
    // 1.50000000, but never loses a significant digit.
    #[test]
    fn coins_display_trimmed_without_losing_precision() {
        assert_eq!(format_coins(150_000_000), "1.5");
        assert_eq!(format_coins(100_000_000), "1");
        assert_eq!(format_coins(1), "0.00000001");
        assert_eq!(format_coins(0), "0");
        assert_eq!(format_coins(123_456_789_012_345), "1234567.89012345");
    }

    // THE CROSS-IMPLEMENTATION VECTOR. The node asserts the same seed gives
    // the same address (`src/a9/wallet.rs`,
    // `the_address_for_a_fixed_seed_is_the_one_the_gui_derives`). An imported
    // key is only useful while the two agree.
    #[test]
    fn an_imported_seed_gives_the_address_the_node_gives() {
        assert_eq!(
            address_for_seed(&[0x11u8; 32]),
            "f811b100866449d60739eeb137ee004e76fb09d4"
        );
    }

    // The derived path must go through the same function, or the two could
    // drift apart.
    #[test]
    fn a_derived_address_is_the_address_of_its_child_seed() {
        let master = MasterSeed::from_bytes([9u8; 32]);
        assert_eq!(
            address_for_index(&master, 2),
            address_for_seed(&master.child_seed(2))
        );
    }
}
