//! Master seed, child derivation, and the encodings that keep a master seed from
//! being mistaken for an address seed.

use std::fmt;

use rand::RngCore;
use zeroize::{Zeroize, Zeroizing};

/// Derives a child seed from the master. NEVER change this string: every wallet
/// derived under it becomes unreachable if it moves.
pub const CHILD_CONTEXT: &str = "alphanumeric wallet child seed v1";
/// Checksum inside the `a9m1` master-seed encoding. NEVER change.
pub const CHECKSUM_CONTEXT: &str = "alphanumeric master seed checksum v1";
/// On-screen fingerprint of a master secret. NEVER change.
pub const KEYID_CONTEXT: &str = "alphanumeric master secret fingerprint v1";

/// Prefix of the master-seed encoding. Deliberately contains `m`, which is not a
/// hex digit, so no 64-character hex address seed can ever begin with it.
pub const MASTER_PREFIX: &str = "a9m1";

const SEED_BYTES: usize = 32;
const CHECKSUM_BYTES: usize = 4;
/// prefix(4) + hex seed(64) + hex checksum(8)
const MASTER_ENCODED_LEN: usize = MASTER_PREFIX.len() + SEED_BYTES * 2 + CHECKSUM_BYTES * 2;

#[derive(Debug, PartialEq, Eq)]
pub enum SeedError {
    /// Input carries the master-seed prefix where an address seed was expected.
    IsAMasterSeed,
    /// Input has no master-seed prefix where a master seed was expected.
    NotAMasterSeed,
    /// Master-seed checksum does not match its payload.
    ChecksumMismatch,
    /// Wrong number of characters.
    BadLength { expected: usize, got: usize },
    /// Non-hexadecimal characters.
    NotHex,
}

// Written out rather than derived so no variant can ever carry the input. The
// input to these parsers IS a spendable key; an error string that quotes it puts
// the key in logs and on screen.
impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SeedError::IsAMasterSeed => write!(
                f,
                "That is a master seed ({MASTER_PREFIX}...), which is the root of a derivation \
                 tree rather than one address key. Importing it as a single address would leave \
                 the rest of the wallet behind."
            ),
            SeedError::NotAMasterSeed => write!(
                f,
                "That is not a master seed. A master seed starts with {MASTER_PREFIX} and is \
                 {MASTER_ENCODED_LEN} characters; a plain 64-character value is a single address \
                 seed and belongs in the address-seed field."
            ),
            SeedError::ChecksumMismatch => write!(
                f,
                "Master seed checksum does not match. The value is mistyped or truncated."
            ),
            SeedError::BadLength { expected, got } => {
                write!(f, "Expected {expected} characters, got {got}.")
            }
            SeedError::NotHex => write!(f, "Expected hexadecimal characters only (0-9, a-f)."),
        }
    }
}

impl std::error::Error for SeedError {}

/// Why a pasted address seed was refused. The input is never part of the
/// message: it is a spendable key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSeedError {
    /// An `a9m1...` master seed. Importing it as an address key would give
    /// one unrelated address while the rest of the wallet's funds stay where
    /// they are -- the node refuses this on its side too.
    MasterSeed,
    NotASeed,
}

impl std::fmt::Display for ImportSeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MasterSeed => f.write_str(
                "That is a master seed, not an address key. A master seed is the root of a \
                 derivation tree: importing it here would add one unrelated address. Export the \
                 individual address seed instead.",
            ),
            Self::NotASeed => f.write_str(
                "An address seed is 64 hexadecimal characters, the value the node's export-seed \
                 prints.",
            ),
        }
    }
}

impl std::error::Error for ImportSeedError {}

/// The root of a wallet's derivation tree.
///
/// Never used directly as an ML-DSA seed -- child 0 is derived like any other, so
/// "the master" and "address 0" cannot collide.
#[derive(Clone)]
pub struct MasterSeed(Zeroizing<[u8; SEED_BYTES]>);

impl fmt::Debug for MasterSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MasterSeed").field(&"<redacted>").finish()
    }
}

impl MasterSeed {
    pub fn from_bytes(bytes: [u8; SEED_BYTES]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn random() -> Self {
        let mut bytes = Zeroizing::new([0u8; SEED_BYTES]);
        rand::rngs::OsRng.fill_bytes(&mut *bytes);
        Self(bytes)
    }

    /// `BLAKE3::derive_key(CHILD_CONTEXT, master || u32_le(index))`.
    ///
    /// The index is fixed-width so `1` cannot be encoded two ways into two
    /// different wallets.
    pub fn child_seed(&self, index: u32) -> Zeroizing<[u8; SEED_BYTES]> {
        let mut hasher = blake3::Hasher::new_derive_key(CHILD_CONTEXT);
        hasher.update(self.0.as_slice());
        hasher.update(&index.to_le_bytes());
        let derived = *hasher.finalize().as_bytes();
        hasher.zeroize();
        Zeroizing::new(derived)
    }

    fn checksum(&self) -> [u8; CHECKSUM_BYTES] {
        let mut hasher = blake3::Hasher::new_derive_key(CHECKSUM_CONTEXT);
        hasher.update(self.0.as_slice());
        let digest = hasher.finalize();
        let mut out = [0u8; CHECKSUM_BYTES];
        out.copy_from_slice(&digest.as_bytes()[..CHECKSUM_BYTES]);
        hasher.zeroize();
        out
    }

    pub fn encode(&self) -> Zeroizing<String> {
        // Pre-sized, and built with push_str rather than format!. A format! buffer
        // has no literal pieces to estimate from, so it starts empty and GROWS --
        // and the intermediate allocation holding the whole hex master is orphaned
        // by the realloc, outside anything Zeroizing can reach. keystore.rs sizes
        // its plaintext buffer up front for exactly this reason.
        let body = Zeroizing::new(hex::encode(self.0.as_slice()));
        let mut encoded = Zeroizing::new(String::with_capacity(MASTER_ENCODED_LEN));
        encoded.push_str(MASTER_PREFIX);
        encoded.push_str(body.as_str());
        encoded.push_str(&hex::encode(self.checksum()));
        encoded
    }

    pub fn decode(text: &str) -> Result<Self, SeedError> {
        let trimmed = text.trim();

        // Compare the prefix in place: lowercasing the input would heap-allocate a
        // copy of a spendable value that nothing wipes.
        let prefix = MASTER_PREFIX.as_bytes();
        if trimmed.len() < prefix.len()
            || !trimmed.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            return Err(SeedError::NotAMasterSeed);
        }
        if trimmed.len() != MASTER_ENCODED_LEN {
            return Err(SeedError::BadLength {
                expected: MASTER_ENCODED_LEN,
                got: trimmed.len(),
            });
        }

        let body = &trimmed[prefix.len()..];

        // hex::decode collects through a Result whose iterator reports a lower size
        // bound of 0, so the Vec it builds grows 8 -> 16 -> 32 -> 64 -- and the
        // cap-32 intermediate holds the entire 32-byte master. decode_to_slice
        // writes straight into a pre-sized, already-wrapped buffer instead. The
        // length was already checked above, so a length mismatch here cannot
        // happen; only a non-hex character can produce an error.
        let mut decoded: Zeroizing<[u8; SEED_BYTES + CHECKSUM_BYTES]> =
            Zeroizing::new([0u8; SEED_BYTES + CHECKSUM_BYTES]);
        hex::decode_to_slice(body, &mut *decoded).map_err(|_| SeedError::NotHex)?;

        // Zeroizing: this array holds the full master seed, and it is populated
        // BEFORE the checksum is known to be good -- so on the mismatch path it
        // would otherwise sit on the stack after the function returns. Constructed
        // once and moved into the candidate: a bare [u8; 32] read out of a
        // Zeroizing by value is Copy, and dereferencing into one (as
        // `Self::from_bytes(*seed)` would) leaves a copy nothing wipes.
        let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
        seed.copy_from_slice(&decoded[..SEED_BYTES]);
        let candidate = Self(seed);

        if candidate.checksum() != decoded[SEED_BYTES..] {
            return Err(SeedError::ChecksumMismatch);
        }
        Ok(candidate)
    }

    /// The raw master bytes. Crate-internal: only the keystore needs them, and it
    /// writes them straight into a buffer it encrypts and wipes.
    pub(crate) fn expose_bytes(&self) -> Zeroizing<[u8; SEED_BYTES]> {
        self.0.clone()
    }

    /// Take ownership of already-wrapped bytes, so a caller that built a
    /// `Zeroizing` does not have to copy out of it to construct a master.
    pub(crate) fn from_zeroizing(bytes: Zeroizing<[u8; SEED_BYTES]>) -> Self {
        Self(bytes)
    }

    /// Short fingerprint for display: lets someone confirm they loaded the right
    /// photo or seed without revealing any of it.
    pub fn key_id(&self) -> String {
        let mut hasher = blake3::Hasher::new_derive_key(KEYID_CONTEXT);
        hasher.update(self.0.as_slice());
        let encoded = hex::encode(&hasher.finalize().as_bytes()[..8]);
        hasher.zeroize();
        format!(
            "{}\u{b7}{}\u{b7}{}\u{b7}{}",
            &encoded[..4],
            &encoded[4..8],
            &encoded[8..12],
            &encoded[12..]
        )
    }
}

/// Parse a single address seed -- the plain 64-character hex form the node's
/// `import-seed` accepts and the GUI's per-address export produces.
///
/// A child seed IS such a value and parses here; that is the designed reverse
/// path. Only the master-seed form is refused, because treating a derivation-tree
/// root as one address would strand every other address in the wallet.
pub fn parse_address_seed_hex(text: &str) -> Result<Zeroizing<[u8; SEED_BYTES]>, SeedError> {
    let trimmed = text.trim();

    let prefix = MASTER_PREFIX.as_bytes();
    if trimmed.len() >= prefix.len()
        && trimmed.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        return Err(SeedError::IsAMasterSeed);
    }
    if trimmed.len() != SEED_BYTES * 2 {
        return Err(SeedError::BadLength {
            expected: SEED_BYTES * 2,
            got: trimmed.len(),
        });
    }

    // See MasterSeed::decode: hex::decode's Vec grows 8 -> 16 -> 32 -> 64, and the
    // cap-32 intermediate holds the entire 32-byte seed. decode_to_slice writes
    // directly into a pre-sized, already-wrapped buffer instead. Length was
    // already checked above, so only a non-hex character can error here.
    let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
    hex::decode_to_slice(trimmed, &mut *seed).map_err(|_| SeedError::NotHex)?;
    Ok(seed)
}

/// The 64-hex address seed a node's `export-seed` prints, as 32 bytes.
/// Surrounding whitespace is trimmed; nothing else is accepted.
pub fn parse_imported_seed(text: &str) -> Result<Zeroizing<[u8; SEED_BYTES]>, ImportSeedError> {
    // M4: `parse_address_seed_hex` already refuses the `a9m1` prefix
    // case-insensitively (`SeedError::IsAMasterSeed`, via `eq_ignore_ascii_case`
    // above) -- the duplicate check that used to live here made that variant
    // unreachable through this function. Map its error instead of re-checking.
    parse_address_seed_hex(text.trim()).map_err(|error| match error {
        SeedError::IsAMasterSeed => ImportSeedError::MasterSeed,
        _ => ImportSeedError::NotASeed,
    })
}

/// Pinned derivation vector: child seed 0 of master `07 * 32`. Regenerating this
/// means the derivation rule moved, which makes every existing wallet
/// unreachable. Never "fix" this constant to match new behaviour.
#[cfg(test)]
const CHILD_ZERO_VECTOR: &str = "aa18294312fafeca77539b89c178e6c091c62bb37f8eaa50b5902d192dfcb2fd";

#[cfg(test)]
mod tests {
    use super::*;

    // Derivation is a consensus rule in everything but name: if these bytes ever
    // change, every wallet derived under the old rule becomes unreachable. Pinned
    // against a fixed master so a context-string typo cannot pass review.
    #[test]
    fn child_derivation_is_pinned() {
        let master = MasterSeed::from_bytes([7u8; 32]);

        let child0 = master.child_seed(0);
        let child1 = master.child_seed(1);

        // A master must never be usable as a child: if index 0 equalled the master,
        // "the master" and "address 0" would collide.
        assert_ne!(child0.as_slice(), &[7u8; 32]);
        assert_ne!(child0.as_slice(), child1.as_slice());

        // Fixed vector. Regenerate ONLY if the context string legitimately changes,
        // which it must not.
        assert_eq!(
            hex::encode(child0.as_slice()),
            CHILD_ZERO_VECTOR,
            "child seed 0 for master 07*32 changed -- this breaks existing wallets"
        );
    }

    // Index width is load-bearing: with a variable-width encoding, index 1 and a
    // differently-encoded 1 would derive different wallets.
    #[test]
    fn child_index_is_fixed_width_little_endian() {
        let master = MasterSeed::from_bytes([9u8; 32]);
        // Index 0 as well as 1: 0 pins that the index is hashed in rather than
        // short-circuited, which the self-generated vector cannot tell us.
        for index in [0u32, 1] {
            let mut direct = blake3::Hasher::new_derive_key(CHILD_CONTEXT);
            direct.update(&[9u8; 32]);
            direct.update(&index.to_le_bytes());
            assert_eq!(
                master.child_seed(index).as_slice(),
                direct.finalize().as_bytes(),
                "child derivation must be BLAKE3::derive_key(CHILD_CONTEXT, master || u32_le(index))"
            );
        }
    }

    #[test]
    fn master_encoding_round_trips_and_is_76_characters() {
        let master = MasterSeed::from_bytes([3u8; 32]);
        let encoded = master.encode();

        assert_eq!(encoded.len(), 76);
        assert!(encoded.starts_with("a9m1"));

        let decoded = MasterSeed::decode(&encoded).expect("our own encoding must decode");
        assert_eq!(
            decoded.child_seed(0).as_slice(),
            master.child_seed(0).as_slice()
        );
    }

    // A corrupted master seed must be refused, not silently turned into a
    // different wallet the user will believe is theirs.
    #[test]
    fn master_decoding_rejects_a_bad_checksum() {
        let master = MasterSeed::from_bytes([5u8; 32]);
        let mut broken = master.encode().to_string();
        // Flip the last checksum character.
        let last = broken.pop().expect("non-empty");
        broken.push(if last == '0' { '1' } else { '0' });

        assert!(matches!(
            MasterSeed::decode(&broken),
            Err(SeedError::ChecksumMismatch)
        ));
    }

    // Spec 3.2, the fund-loss trap. hex64 means "one address seed" to the node
    // and "root of a derivation tree" here. Each parser accepts exactly one form.
    #[test]
    fn master_decoder_rejects_a_plain_address_seed() {
        let address_seed = "07".repeat(32);
        assert!(matches!(
            MasterSeed::decode(&address_seed),
            Err(SeedError::NotAMasterSeed)
        ));
    }

    #[test]
    fn address_seed_parser_rejects_a_master_seed() {
        let master = MasterSeed::from_bytes([11u8; 32]).encode();
        assert!(matches!(
            parse_address_seed_hex(&master),
            Err(SeedError::IsAMasterSeed)
        ));
    }

    // The permitted direction, pinned so nobody "fixes" it later: a child seed IS
    // a valid address seed, and feeding one to the node's import-seed is the
    // designed reverse path (spec 3.2). Child seeds are plain hex64 and cannot be
    // distinguished from any other seed -- that is intended, not a gap.
    #[test]
    fn a_child_seed_parses_as_an_address_seed() {
        let master = MasterSeed::from_bytes([13u8; 32]);
        let child = master.child_seed(4);
        let as_text = hex::encode(child.as_slice());

        let parsed =
            parse_address_seed_hex(&as_text).expect("a child seed is a valid address seed");
        assert_eq!(parsed.as_slice(), child.as_slice());
    }

    #[test]
    fn address_seed_parser_rejects_malformed_input() {
        assert!(parse_address_seed_hex("").is_err());
        assert!(parse_address_seed_hex(&"07".repeat(31)).is_err());
        assert!(parse_address_seed_hex(&"07".repeat(33)).is_err());
        assert!(parse_address_seed_hex(&"zz".repeat(32)).is_err());
    }

    // Every rejection path must stay silent about the input: the input IS a
    // spendable key. Checks 16-character windows, because a partial echo leaks
    // just as badly as a whole one.
    #[test]
    fn errors_never_echo_key_material() {
        let master = MasterSeed::from_bytes([17u8; 32]).encode().to_string();
        let cases: Vec<String> = vec![
            master.clone(),
            "07".repeat(31),
            "07".repeat(33),
            "zz".repeat(32),
        ];

        for secret in cases {
            let messages = vec![
                MasterSeed::decode(&secret).err().map(|e| e.to_string()),
                parse_address_seed_hex(&secret).err().map(|e| e.to_string()),
            ];
            for message in messages.into_iter().flatten() {
                let leaked = secret.as_bytes().windows(16).any(|window| {
                    std::str::from_utf8(window)
                        .map(|fragment| message.contains(fragment))
                        .unwrap_or(false)
                });
                assert!(!leaked, "an error echoed key material: {message}");
            }
        }
    }

    // The fingerprint lets someone confirm they loaded the right photo without
    // exposing anything. It must not be derivable back into the secret, and it
    // must differ between masters.
    #[test]
    fn key_id_is_stable_grouped_and_master_specific() {
        let a = MasterSeed::from_bytes([1u8; 32]);
        let b = MasterSeed::from_bytes([2u8; 32]);

        assert_eq!(a.key_id(), a.key_id());
        assert_ne!(a.key_id(), b.key_id());
        // chars(), not len(): the separator is U+00B7, two bytes in UTF-8, so a
        // byte-length assertion here would be checking 22 while reading as 19.
        assert_eq!(a.key_id().chars().count(), 19); // 16 hex chars + 3 separators
        assert_eq!(a.key_id().matches('\u{b7}').count(), 3);
    }

    // CHILD_CONTEXT has a pinned vector; these two strings had nothing. If
    // CHECKSUM_CONTEXT drifts, every a9m1 backup a user wrote down starts
    // decoding as "mistyped or truncated" -- they are told their only backup is
    // corrupt. If KEYID_CONTEXT drifts, the fingerprint they compare against a
    // paper record stops matching and reads as the wrong wallet. A round-trip
    // test cannot catch either, because any self-consistent rule satisfies it.
    // These literals were computed independently, not taken from this code.
    #[test]
    fn checksum_and_fingerprint_contexts_are_pinned() {
        assert_eq!(
            MasterSeed::from_bytes([3u8; 32]).encode().as_str(),
            "a9m10303030303030303030303030303030303030303030303030303030303030303af1adb5b",
            "the a9m1 encoding changed -- existing written-down backups would stop decoding"
        );
        assert_eq!(
            MasterSeed::from_bytes([1u8; 32]).key_id(),
            "1383\u{b7}f935\u{b7}9480\u{b7}3812",
            "the fingerprint changed -- it would no longer match a user's paper record"
        );
    }

    // random() is the constructor every real wallet is born from, and the only
    // one whose silent failure is catastrophic: an implementation returning zeros
    // would mint one identical wallet for every user of the application.
    #[test]
    fn random_masters_differ_and_are_not_zero() {
        let a = MasterSeed::random();
        let b = MasterSeed::random();
        assert_ne!(a.encode().as_str(), b.encode().as_str());
        assert_ne!(
            a.child_seed(0).as_slice(),
            MasterSeed::from_bytes([0u8; 32]).child_seed(0).as_slice()
        );
    }

    // The case-insensitive prefix is load-bearing for a backup written out in
    // block capitals. Nothing asserted it, so a later "tightening" to starts_with
    // would strand those backups with no test turning red.
    #[test]
    fn an_uppercase_master_seed_still_decodes() {
        let master = MasterSeed::from_bytes([29u8; 32]);
        let shouted = master.encode().to_uppercase();
        let decoded = MasterSeed::decode(&shouted).expect("an uppercase backup must still open");
        assert_eq!(
            decoded.child_seed(0).as_slice(),
            master.child_seed(0).as_slice()
        );
    }

    // A mistyped character in the SEED body is the transcription error that
    // actually happens; the checksum exists to catch it. Without this, the
    // checksum is only tested against damage to itself.
    #[test]
    fn master_decoding_rejects_a_corrupted_seed_body() {
        let master = MasterSeed::from_bytes([19u8; 32]);
        let mut broken: Vec<char> = master.encode().chars().collect();
        // Index 4 is the first character of the seed body, just past the prefix.
        broken[4] = if broken[4] == '0' { '1' } else { '0' };
        let broken: String = broken.into_iter().collect();

        assert!(matches!(
            MasterSeed::decode(&broken),
            Err(SeedError::ChecksumMismatch)
        ));
    }

    // Debug must never print key material -- a stray {:?} years from now is how
    // these leaks actually happen.
    #[test]
    fn debug_is_redacted() {
        let master = MasterSeed::from_bytes([23u8; 32]);
        let rendered = format!("{:?}", master);
        // Asserting the whole expected rendering, not just the absence of one
        // substring: a Debug impl that printed the seed as hex alongside the word
        // "redacted" would still pass a `contains("redacted")`-only check.
        assert_eq!(rendered, "MasterSeed(\"<redacted>\")");
    }

    // Spec H §5.2: a master seed pasted as an address key would make one
    // unrelated address while the rest of the funds stay where they are. The
    // node refuses the same thing on its side, for the same reason.
    #[test]
    fn a_master_seed_is_refused_with_its_own_words() {
        let master = MasterSeed::from_bytes([3u8; 32]).encode();
        let error = parse_imported_seed(&master).expect_err("refused");
        assert!(matches!(error, ImportSeedError::MasterSeed));
        let words = error.to_string();
        assert!(words.contains("master seed"), "{words}");
        // Never echo the input: it is a spendable key.
        assert!(!words.contains(master.as_str()));
    }

    #[test]
    fn an_address_seed_is_taken_and_anything_else_is_refused() {
        let good = "07".repeat(32);
        assert_eq!(
            parse_imported_seed(&good).expect("taken").as_slice(),
            [0x07u8; 32]
        );
        assert_eq!(
            parse_imported_seed(&format!("  {good}  "))
                .expect("surrounding space is trimmed")
                .as_slice(),
            [0x07u8; 32]
        );
        for bad in ["", &"07".repeat(31), &"07".repeat(33), &"zz".repeat(32)] {
            assert!(matches!(
                parse_imported_seed(bad),
                Err(ImportSeedError::NotASeed)
            ));
        }
        let words = ImportSeedError::NotASeed.to_string();
        assert!(words.contains("64"), "{words}");
    }
}
