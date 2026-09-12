//! The on-disk envelope for a master seed.
//!
//! Deliberately NOT the node's legacy wallet envelope. That format has no version
//! byte and encodes its KDF parameters only implicitly, through library defaults
//! -- which is the fragility the node's `argon2 = "=0.5.3"` pin exists to contain.
//! Same primitives, explicit header, authenticated.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::seed::MasterSeed;

pub const MAGIC: &[u8; 4] = b"A9GW";
pub const VERSION: u8 = 1;

const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
/// magic(4) + version(1) + m_cost(4) + t_cost(4) + p_cost(4) + salt(16) + nonce(12)
const HEADER_BYTES: usize = 4 + 1 + 4 + 4 + 4 + SALT_BYTES + NONCE_BYTES;
const SEED_BYTES: usize = 32;

// OWASP's Argon2id baseline. Written into the header rather than assumed, so
// raising them later still opens today's files.
const M_COST: u32 = 19_456;
const T_COST: u32 = 2;
const P_COST: u32 = 1;

// Ceilings on what we will honour from a FILE's header. argon2 0.5.3 enforces
// only lower bounds -- MAX_M_COST is u32::MAX -- and `open` derives the key
// BEFORE the AEAD can authenticate anything, so a hostile file claiming
// m_cost = u32::MAX would have us attempt a ~4 TiB allocation from nothing more
// than the user opening it. Rust aborts the process on allocation failure; that
// is not a panic anyone can catch. Binding the header as AAD stops a rewrite
// DOWNWARD, which is what makes a brute force cheap; nothing but these ceilings
// stops a rewrite upward. Generous enough that costs can be raised for years
// without a format change.
const MAX_HEADER_M_COST: u32 = 1_048_576; // 1 GiB, in KiB units
const MAX_HEADER_T_COST: u32 = 16;
const MAX_HEADER_P_COST: u32 = 16;

fn derive_key(
    passphrase: &[u8],
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(m_cost, t_cost, p_cost, Some(32))
        .map_err(|e| format!("Invalid key-derivation parameters: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase, salt, &mut *key)
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(key)
}

/// Encrypt `master || payload` under `passphrase`.
///
/// The whole header is the AEAD's associated data, so the KDF cost parameters it
/// advertises cannot be rewritten without failing decryption.
pub fn seal(master: &MasterSeed, payload: &[u8], passphrase: &[u8]) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_BYTES];
    let mut nonce_bytes = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let mut header = Vec::with_capacity(HEADER_BYTES);
    header.extend_from_slice(MAGIC);
    header.push(VERSION);
    header.extend_from_slice(&M_COST.to_le_bytes());
    header.extend_from_slice(&T_COST.to_le_bytes());
    header.extend_from_slice(&P_COST.to_le_bytes());
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce_bytes);

    let key = derive_key(passphrase, &salt, M_COST, T_COST, P_COST)?;
    let cipher =
        Aes256Gcm::new_from_slice(key.as_slice()).map_err(|e| format!("Cipher setup: {e}"))?;

    let mut plaintext = Zeroizing::new(Vec::with_capacity(SEED_BYTES + payload.len()));
    plaintext.extend_from_slice(master.expose_bytes().as_slice());
    plaintext.extend_from_slice(payload);

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext.as_slice(),
                aad: &header,
            },
        )
        .map_err(|e| format!("Encryption failed: {e}"))?;

    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an envelope, returning the master seed and the payload beside it.
pub fn open(
    envelope: &[u8],
    passphrase: &[u8],
) -> Result<(MasterSeed, Zeroizing<Vec<u8>>), String> {
    if envelope.len() < HEADER_BYTES + SEED_BYTES {
        return Err("Not a wallet file: too short.".into());
    }
    if envelope[..4] != MAGIC[..] {
        return Err("Not a wallet file.".into());
    }
    if envelope[4] != VERSION {
        return Err(format!(
            "Unsupported wallet file version {} (this build reads version {VERSION}).",
            envelope[4]
        ));
    }

    let read_u32 = |offset: usize| -> u32 {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&envelope[offset..offset + 4]);
        u32::from_le_bytes(buf)
    };
    let m_cost = read_u32(5);
    let t_cost = read_u32(9);
    let p_cost = read_u32(13);

    if m_cost > MAX_HEADER_M_COST || t_cost > MAX_HEADER_T_COST || p_cost > MAX_HEADER_P_COST {
        // Refuse before deriving. See the ceilings' comment: this runs ahead of
        // any authentication, so the numbers here are still an attacker's.
        return Err(
            "This wallet file asks for more work than any real one needs. It is corrupt or hostile."
                .into(),
        );
    }

    let salt = &envelope[17..17 + SALT_BYTES];
    let nonce_bytes = &envelope[17 + SALT_BYTES..HEADER_BYTES];
    let header = &envelope[..HEADER_BYTES];
    let ciphertext = &envelope[HEADER_BYTES..];

    let key = derive_key(passphrase, salt, m_cost, t_cost, p_cost)?;
    let cipher =
        Aes256Gcm::new_from_slice(key.as_slice()).map_err(|e| format!("Cipher setup: {e}"))?;

    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ciphertext,
                    aad: header,
                },
            )
            .map_err(|_| {
                "Could not open the wallet file: wrong passphrase, or the file was modified."
                    .to_string()
            })?,
    );

    if plaintext.len() < SEED_BYTES {
        return Err("Wallet file is corrupt.".into());
    }
    // Zeroizing, for the same reason seed.rs's decode does it: this array holds
    // the recovered master, and a bare [u8; 32] is Copy -- from_bytes(*seed)
    // would copy out and leave the original resident on the stack after the
    // return. from_zeroizing takes ownership of the wrapper instead.
    let mut seed = Zeroizing::new([0u8; SEED_BYTES]);
    seed.copy_from_slice(&plaintext[..SEED_BYTES]);
    Ok((
        MasterSeed::from_zeroizing(seed),
        Zeroizing::new(plaintext[SEED_BYTES..].to_vec()),
    ))
}

/// The child seed for one derivation index, as 64 lowercase hex characters.
///
/// The passphrase is checked by opening the FILE again rather than by comparing
/// it to the one `WalletState` holds for the session. A comparison would be
/// string equality between two secrets, and all it could prove is that the user
/// typed the same thing twice -- not that they can open the wallet. Re-opening
/// also costs a full Argon2id derivation, which is the point: guessing at this
/// prompt is exactly as expensive as guessing at the unlock prompt.
///
/// The node's `export-seed` re-asks for the passphrase even when the wallet is
/// already unlocked, for the reason its own comment gives -- the passphrase
/// entered at startup proves nothing about who is sitting at the machine now.
/// This is that same gate.
pub fn reveal_child_seed_hex(
    envelope: &[u8],
    passphrase: &[u8],
    index: u32,
) -> Result<Zeroizing<String>, String> {
    let (master, _payload) = open(envelope, passphrase)?;
    // Wrapped before anything else can see it. `hex::encode` returns a bare
    // String holding a spendable key; binding it to a name first would leave
    // that String reachable and unwiped if a later `?` returned early.
    Ok(Zeroizing::new(hex::encode(
        master.child_seed(index).as_slice(),
    )))
}

/// The master seed in its `a9m1...` form (`MasterSeed::encode`), gated on
/// the passphrase opening `envelope` (spec G §4.5).
pub fn reveal_master_seed(envelope: &[u8], passphrase: &[u8]) -> Result<Zeroizing<String>, String> {
    let (master, _payload) = open(envelope, passphrase)?;
    Ok(master.encode())
}

/// One imported key's seed, gated the way `reveal_child_seed_hex` is. The
/// slot is a position in the payload's `imported` list.
pub fn reveal_imported_seed_hex(
    envelope: &[u8],
    passphrase: &[u8],
    slot: usize,
) -> Result<Zeroizing<String>, String> {
    let (_master, payload) = open(envelope, passphrase)?;
    let metadata: crate::storage::WalletMetadata =
        serde_json::from_slice(&payload).map_err(|_| "Wallet metadata is corrupt.".to_string())?;
    let key = metadata
        .imported
        .get(slot)
        .ok_or_else(|| "That imported address is no longer in this wallet.".to_string())?;
    Ok(Zeroizing::new(key.seed.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::MasterSeed;

    #[test]
    fn round_trips_the_master_and_its_payload() {
        let master = MasterSeed::from_bytes([31u8; 32]);
        let payload = br#"{"next_index":3}"#;
        let sealed = seal(&master, payload, b"open sesame").expect("seal");

        let (opened, opened_payload) = open(&sealed, b"open sesame").expect("open");
        assert_eq!(
            opened.child_seed(0).as_slice(),
            master.child_seed(0).as_slice()
        );
        assert_eq!(opened_payload.as_slice(), &payload[..]);
    }

    #[test]
    fn a_wrong_passphrase_fails() {
        let sealed = seal(&MasterSeed::from_bytes([1u8; 32]), b"{}", b"right").expect("seal");
        assert!(open(&sealed, b"wrong").is_err());
    }

    // A rewritten KDF cost must not decrypt -- but the AAD is NOT what rejects
    // it, and this test does not prove the AAD binding. The cost fields feed
    // `derive_key`, so a changed m_cost derives a DIFFERENT key and the AEAD tag
    // fails for that reason alone; this test passes unchanged with `aad: &[]`.
    // The same is true of every other field in today's header: magic and version
    // are checked explicitly in `open`, the three costs and the salt feed
    // `derive_key`, and the nonce feeds the AEAD. So no mutation of the CURRENT
    // header is detectable only through the associated data.
    //
    // The AAD still earns its place, because that is a property of today's
    // header rather than of the format: the first header field added that feeds
    // neither the KDF nor the AEAD would be unauthenticated without it. That
    // binding is pinned directly by `the_header_is_bound_as_associated_data`.
    #[test]
    fn a_rewritten_kdf_cost_fails_to_open() {
        let sealed = seal(&MasterSeed::from_bytes([2u8; 32]), b"{}", b"pass").expect("seal");

        let mut tampered = sealed.clone();
        // m_cost is the u32 immediately after magic(4) + version(1).
        tampered[5] ^= 0x01;
        assert!(
            open(&tampered, b"pass").is_err(),
            "a rewritten KDF cost derives a different key, so the tag must fail"
        );
    }

    // The claim the test above cannot make. Re-encrypt the identical plaintext
    // under the identical key, salt and nonce, changing NOTHING except the
    // associated data, and splice it behind the unmodified header. The key is
    // the same, the header is the same, the plaintext is the same -- so the only
    // thing that can reject the result is the AAD `seal` bound into the tag.
    #[test]
    fn the_header_is_bound_as_associated_data() {
        const SEED: [u8; 32] = [5u8; 32];
        const PAYLOAD: &[u8] = b"{}";

        let sealed = seal(&MasterSeed::from_bytes(SEED), PAYLOAD, b"pass").expect("seal");
        let header = sealed[..HEADER_BYTES].to_vec();
        let salt = &header[17..17 + SALT_BYTES];
        let nonce_bytes = &header[17 + SALT_BYTES..HEADER_BYTES];

        let key = derive_key(b"pass", salt, M_COST, T_COST, P_COST).expect("derive");
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).expect("cipher");

        let mut plaintext = Vec::new();
        plaintext.extend_from_slice(&SEED);
        plaintext.extend_from_slice(PAYLOAD);

        let reseal = |aad: &[u8]| {
            let ciphertext = cipher
                .encrypt(
                    Nonce::from_slice(nonce_bytes),
                    Payload {
                        msg: plaintext.as_slice(),
                        aad,
                    },
                )
                .expect("encrypt");
            let mut envelope = header.clone();
            envelope.extend_from_slice(&ciphertext);
            envelope
        };

        assert!(
            open(&reseal(&[]), b"pass").is_err(),
            "an envelope whose tag does not cover the header must not open"
        );
        // The control: identical in every way except that the AAD is the header,
        // so the refusal above is the binding and not some other difference.
        assert!(
            open(&reseal(&header), b"pass").is_ok(),
            "the same bytes bound to the header must open"
        );
    }

    #[test]
    fn the_envelope_is_self_describing() {
        let sealed = seal(&MasterSeed::from_bytes([3u8; 32]), b"{}", b"pass").expect("seal");
        assert_eq!(&sealed[..4], &MAGIC[..]);
        assert_eq!(sealed[4], VERSION);
    }

    #[test]
    fn a_foreign_or_truncated_envelope_is_refused() {
        assert!(open(b"", b"pass").is_err());
        assert!(open(b"NOPE", b"pass").is_err());

        let sealed = seal(&MasterSeed::from_bytes([4u8; 32]), b"{}", b"pass").expect("seal");
        assert!(open(&sealed[..sealed.len() - 1], b"pass").is_err());

        let mut wrong_version = sealed.clone();
        wrong_version[4] = 0xff;
        assert!(open(&wrong_version, b"pass").is_err());
    }

    // The ciphertext must not be a function of the passphrase alone: a fresh salt
    // and nonce per seal mean two identical wallets do not produce identical files.
    #[test]
    fn each_seal_uses_fresh_salt_and_nonce() {
        let master = MasterSeed::from_bytes([5u8; 32]);
        let first = seal(&master, b"{}", b"pass").expect("seal");
        let second = seal(&master, b"{}", b"pass").expect("seal");
        assert_ne!(first, second);
    }

    // A hostile file must be refused, not obeyed. These cost fields are read
    // before the AEAD can authenticate anything, so without a ceiling the only
    // thing standing between a user and a 4 TiB allocation is the file's own
    // honesty. The test asserts an error rather than measuring time or memory:
    // if the ceiling were removed this would not fail, it would hang or abort,
    // which is itself the signal.
    #[test]
    fn an_absurd_work_factor_is_refused_not_attempted() {
        let sealed = seal(&MasterSeed::from_bytes([6u8; 32]), b"{}", b"pass").expect("seal");

        for (offset, label) in [(5usize, "m_cost"), (9, "t_cost"), (13, "p_cost")] {
            let mut hostile = sealed.clone();
            hostile[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(
                open(&hostile, b"pass").is_err(),
                "{label} = u32::MAX must be refused before any key derivation"
            );
        }
    }

    // The magic check has its own branch, and every other refusal test trips a
    // different one first -- length, or AEAD failure. Flip a magic byte on an
    // otherwise valid envelope so that branch is actually exercised.
    #[test]
    fn a_wrong_magic_on_a_full_length_envelope_is_refused() {
        let mut sealed = seal(&MasterSeed::from_bytes([7u8; 32]), b"{}", b"pass").expect("seal");
        sealed[0] ^= 0x01;
        assert!(open(&sealed, b"pass").is_err());
    }

    #[test]
    fn reveal_child_seed_hex_is_the_master_derivation_for_that_index() {
        let master = MasterSeed::from_bytes([7u8; 32]);
        let sealed = seal(&master, b"{}", b"pass").expect("seal");

        let revealed = reveal_child_seed_hex(&sealed, b"pass", 3).expect("reveal");
        assert_eq!(
            revealed.as_str(),
            hex::encode(master.child_seed(3).as_slice())
        );
    }

    // Two indices must not hand back the same key. Without this, an off-by-one
    // in the caller would export a working seed for the wrong address and
    // nothing would look wrong until the funds were in the wrong place.
    #[test]
    fn reveal_child_seed_hex_separates_indices() {
        let sealed = seal(&MasterSeed::from_bytes([7u8; 32]), b"{}", b"pass").expect("seal");
        let zero = reveal_child_seed_hex(&sealed, b"pass", 0).expect("reveal");
        let one = reveal_child_seed_hex(&sealed, b"pass", 1).expect("reveal");
        assert_ne!(zero.as_str(), one.as_str());
    }

    #[test]
    fn reveal_child_seed_hex_refuses_a_wrong_passphrase() {
        let sealed = seal(&MasterSeed::from_bytes([7u8; 32]), b"{}", b"right").expect("seal");
        assert!(reveal_child_seed_hex(&sealed, b"wrong", 0).is_err());
    }

    // The node's `import-seed` takes exactly 64 hexadecimal characters and
    // rejects anything else without echoing what it got (mgmt.rs
    // parse_address_seed_hex). An export that is not this shape is not
    // importable, and the error the user would see would not say why.
    #[test]
    fn reveal_child_seed_hex_is_what_the_nodes_import_seed_accepts() {
        let sealed = seal(&MasterSeed::from_bytes([7u8; 32]), b"{}", b"pass").expect("seal");
        let revealed = reveal_child_seed_hex(&sealed, b"pass", 0).expect("reveal");
        assert_eq!(revealed.len(), 64);
        assert!(revealed
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }
}
