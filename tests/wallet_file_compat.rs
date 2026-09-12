use alphanumeric::a9::{mgmt::WalletKeyData, mldsa, wallet::Wallet};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const WALLET_FILE_V1: &str = include_str!("fixtures/wallet-file-v1.json");

#[test]
fn wallet_key_file_json_contract_is_byte_stable() {
    let mut unencrypted = WalletKeyData::new(
        "unencrypted_fixture".to_string(),
        "0000000000000000000000000000000000000000".to_string(),
        Some(Zeroizing::new(vec![1, 2, 3, 4])),
        false,
    );
    unencrypted.last_sync_timestamp = 1_234_567_890;

    let mut encrypted = WalletKeyData::new(
        "encrypted_fixture".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
        Some(Zeroizing::new(vec![9, 8, 7])),
        true,
    );
    encrypted.last_sync_timestamp = 1_234_567_891;

    let serialized = serde_json::to_string(&vec![unencrypted, encrypted])
        .expect("wallet fixture must serialize");
    assert_eq!(serialized, WALLET_FILE_V1.trim_end());

    let decoded: Vec<WalletKeyData> =
        serde_json::from_str(WALLET_FILE_V1).expect("legacy wallet fixture must deserialize");
    assert_eq!(
        serde_json::to_string(&decoded).expect("decoded wallet fixture must reserialize"),
        WALLET_FILE_V1.trim_end()
    );
}

#[tokio::test]
async fn unencrypted_wallet_key_material_restores_without_format_translation() {
    let secret_seed = vec![7u8; mldsa::SECRET_KEY_BYTES];
    let public_key =
        mldsa::public_key_from_secret(&secret_seed).expect("fixture seed must derive a public key");
    let mut combined = secret_seed;
    combined.extend_from_slice(&public_key);

    let digest = Sha256::digest(&public_key);
    let address = hex::encode(&digest[..20]);
    let wallet = Wallet::from_key_bytes(
        "fixture".to_string(),
        address.clone(),
        Zeroizing::new(combined),
        None,
        false,
    )
    .expect("current unencrypted key bytes must restore");

    assert_eq!(wallet.address, address);
    assert_eq!(
        wallet.get_public_key_hex().await.as_deref(),
        Some(hex::encode(public_key).as_str())
    );
}

#[tokio::test]
async fn encrypted_wallet_envelope_restores_without_reencoding() {
    const PASSPHRASE: &[u8] = b"wallet-compatibility-test";

    let original = Wallet::new(Some(PASSPHRASE)).expect("encrypted fixture wallet must be created");
    let encrypted = original
        .encrypted_private_key
        .clone()
        .expect("encrypted wallet must carry its envelope");
    let restored = Wallet::from_key_bytes(
        original.name.clone(),
        original.address.clone(),
        encrypted,
        Some(PASSPHRASE),
        true,
    )
    .expect("current encrypted envelope must restore");

    assert_eq!(restored.address, original.address);
    assert_eq!(
        restored.get_public_key_hex().await,
        original.get_public_key_hex().await
    );
}

// The seed bridge's contract: export 32 bytes, import them back, and you get the same address
// and the same stored key bytes. If this breaks, a backup does not restore.
#[tokio::test]
async fn seed_export_import_round_trip_preserves_address_and_key_bytes() {
    let seed = vec![42u8; mldsa::SECRET_KEY_BYTES];

    let original = Wallet::from_seed(&seed, None).expect("wallet from seed");
    let exported = original
        .export_seed_hex()
        .await
        .expect("an unlocked wallet exports its seed");
    assert_eq!(exported.len(), 64);

    let decoded = hex::decode(exported.as_str()).expect("export is hex");
    let restored = Wallet::from_seed(&decoded, None).expect("re-import");

    assert_eq!(restored.address, original.address);
    assert_eq!(
        original.encrypted_private_key.as_ref().map(|k| k.to_vec()),
        restored.encrypted_private_key.as_ref().map(|k| k.to_vec()),
        "the stored key bytes must be identical after a seed round trip"
    );

    // The comparison above is self-consistency: both wallets come from the same deterministic
    // from_seed on the same bytes, so a deterministic layout bug -- reversed concatenation, a
    // truncated public key, stray padding -- would be reproduced identically on both sides and
    // slip through. Check the stored bytes against an INDEPENDENT oracle so the test pins the
    // layout its name claims, not merely that from_seed agrees with itself.
    let expected_public =
        mldsa::public_key_from_secret(&seed).expect("the fixture seed derives a public key");
    let mut expected_combined = seed.clone();
    expected_combined.extend_from_slice(&expected_public);
    assert_eq!(
        original.encrypted_private_key.as_ref().map(|k| k.to_vec()),
        Some(expected_combined),
        "stored key material must be exactly seed || public key, in that order"
    );
}

// A seed-built wallet must still load through the existing from_key_bytes path. If the two
// paths diverge, an imported wallet silently fails to load on the next launch.
#[tokio::test]
async fn a_seed_imported_wallet_loads_through_the_existing_key_bytes_path() {
    let seed = vec![55u8; mldsa::SECRET_KEY_BYTES];
    let wallet = Wallet::from_seed(&seed, None).expect("wallet from seed");
    let stored = wallet.encrypted_private_key.clone().expect("key material");

    let reloaded = Wallet::from_key_bytes(
        "reloaded".to_string(),
        wallet.address.clone(),
        stored,
        None,
        false,
    )
    .expect("a seed-built wallet must load through the existing path");
    assert_eq!(reloaded.address, wallet.address);
}
