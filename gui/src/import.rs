//! Opening a wallet file someone chose to import (spec G §3.5, §4.4).
//!
//! Nothing here writes. The caller puts `WalletFile::envelope` in place with
//! `storage::replace_archiving` + `storage::write_envelope`, exactly as it
//! was read -- never re-sealed, so the file keeps its own passphrase and
//! key-derivation settings.

use std::io::Read;
use std::path::Path;

use zeroize::Zeroizing;

use crate::keystore;
use crate::model;
use crate::seed::MasterSeed;
use crate::storage::WalletMetadata;

/// A real envelope is a few hundred bytes. Anything this large is not one,
/// and reading it whole to find out would be the only cost of trying.
pub const MAX_WALLET_FILE_BYTES: u64 = 1 << 20;

/// A wallet file that opened with its own passphrase. No `Debug`: it holds
/// the master seed.
pub struct WalletFile {
    /// The bytes as read -- what gets written.
    pub envelope: Vec<u8>,
    pub master: MasterSeed,
    /// From the sealed metadata, at least 1.
    pub next_index: u32,
    /// Index 0's address, for the preview and the same-wallet check (§4.3).
    pub first_address: String,
    /// The file's imported seeds (spec H), in payload slot order. Taken out
    /// of the parsed `ImportedKey`s with `mem::take` rather than cloned: an
    /// `ImportedKey`'s `Drop` zeroizes its `seed`, and a `.clone()` here
    /// would leave that copy unwrapped and un-zeroized.
    pub imported: Vec<Zeroizing<String>>,
}

pub fn open_wallet_file(path: &Path, passphrase: &[u8]) -> Result<WalletFile, String> {
    // M3: a metadata length of 0 is not proof of anything -- a FIFO reports
    // it and then blocks `read` forever, and a character device like
    // `/dev/zero` reports it and then never ends. Both are refused outright
    // rather than trusted for their length.
    //
    // Opened non-blocking: for a FIFO a plain read-only open(2) itself waits
    // for a writer, before the metadata check below could ever run. With
    // O_NONBLOCK the open returns at once and `is_file` refuses it. On a
    // regular file the flag changes nothing.
    //
    // A stat first, which blocks on nothing: Windows refuses to open a
    // directory at all (access denied), and that would surface below as
    // "Could not read" rather than the plain statement of what it is. The
    // check after the open stays for whatever changed in between.
    let not_regular = |path: &Path| format!("{} is not a regular file.", path.display());
    if std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) {
        return Err(not_regular(path));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let is_regular = file
        .metadata()
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?
        .is_file();
    if !is_regular {
        return Err(not_regular(path));
    }
    // Bounded no matter how the file behaves: `take` caps the read at one
    // byte past the limit, so a regular file that is merely huge is refused
    // by length below, without ever reading the whole thing.
    let mut envelope = Vec::new();
    file.take(MAX_WALLET_FILE_BYTES + 1)
        .read_to_end(&mut envelope)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if envelope.len() as u64 > MAX_WALLET_FILE_BYTES {
        return Err(format!(
            "{} is too large to be a wallet file.",
            path.display()
        ));
    }
    let (master, payload) = keystore::open(&envelope, passphrase)?;
    let mut metadata: WalletMetadata = serde_json::from_slice(&payload)
        .map_err(|_| "The wallet file opened, but its address count is unreadable.".to_string())?;
    let next_index = metadata.next_index.max(1);
    // `mem::take`, not `.clone()`: `ImportedKey::drop` zeroizes `seed`, so a
    // clone would leave an unwrapped, un-zeroized copy of a spendable key.
    let imported: Vec<Zeroizing<String>> = metadata
        .imported
        .iter_mut()
        .map(|key| Zeroizing::new(std::mem::take(&mut key.seed)))
        .collect();
    let first_address = model::address_for_index(&master, 0);
    Ok(WalletFile {
        envelope,
        master,
        next_index,
        first_address,
        imported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;

    fn saved(dir: &Path, name: &str, seed: u8, payload: &[u8], pass: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        storage::save(&path, &MasterSeed::from_bytes([seed; 32]), payload, pass).expect("save");
        path
    }

    #[test]
    fn a_wallet_file_opens_with_its_own_passphrase_and_says_what_is_in_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved(dir.path(), "w.enc", 7, br#"{"next_index":3}"#, b"its own");
        let file = open_wallet_file(&path, b"its own").expect("opens");
        assert_eq!(file.next_index, 3);
        assert_eq!(
            file.first_address,
            model::address_for_index(&MasterSeed::from_bytes([7u8; 32]), 0)
        );
        assert_eq!(
            file.envelope,
            std::fs::read(&path).expect("read"),
            "bytes as read"
        );
    }

    #[test]
    fn a_wrong_passphrase_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved(dir.path(), "w.enc", 7, br#"{"next_index":3}"#, b"its own");
        assert!(open_wallet_file(&path, b"another").is_err());
    }

    #[test]
    fn something_that_is_not_a_wallet_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photo.jpg");
        std::fs::write(
            &path,
            b"\xff\xd8\xff\xe0 not an envelope at all, just bytes",
        )
        .expect("write");
        assert!(open_wallet_file(&path, b"x").is_err());
    }

    // M3: a directory has a metadata length of 0 like a FIFO or a character
    // device would, so this must be caught by the regular-file check, not by
    // a length that happens to look small.
    #[test]
    fn a_path_that_is_not_a_regular_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = open_wallet_file(dir.path(), b"x").err().expect("refused");
        assert!(error.contains("is not a regular file"), "{error}");
    }

    #[test]
    fn a_file_too_large_to_be_a_wallet_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.enc");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(MAX_WALLET_FILE_BYTES + 1).expect("grow");
        let error = open_wallet_file(&path, b"x").err().expect("refused");
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn a_wallet_file_without_an_address_count_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved(dir.path(), "w.enc", 7, b"{}", b"pass");
        assert!(open_wallet_file(&path, b"pass").is_err());
    }

    // Opening a named pipe for reading blocks in open(2) until a writer
    // appears -- before any metadata check can run. The import stage would
    // sit on "OPENING..." with CANCEL IMPORT disabled until a restart. It
    // must come back at once, refused.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fifo = dir.path().join("pipe.enc");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed");

        let (sent, received) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = sent.send(open_wallet_file(&path, b"x").err());
        });
        let error = received
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("open_wallet_file came back instead of blocking")
            .expect("a named pipe is refused");
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[test]
    fn an_address_count_of_zero_reads_as_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = saved(dir.path(), "w.enc", 7, br#"{"next_index":0}"#, b"pass");
        assert_eq!(
            open_wallet_file(&path, b"pass").expect("opens").next_index,
            1
        );
    }

    // Sub-project G's file import must not silently drop sub-project H's
    // imported keys: a file whose payload holds one carries it into
    // `WalletFile`.
    #[test]
    fn importing_a_wallet_file_keeps_its_imported_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let metadata = serde_json::to_vec(&WalletMetadata {
            next_index: 1,
            imported: vec![storage::ImportedKey {
                seed: "07".repeat(32),
                added: "2026-01-01T00:00:00Z".to_string(),
            }],
        })
        .expect("encode");
        let path = saved(dir.path(), "w.enc", 9, &metadata, b"pass");

        let file = open_wallet_file(&path, b"pass").expect("opens");

        assert_eq!(file.imported.len(), 1);
        assert_eq!(file.imported[0].to_string(), "07".repeat(32));
    }
}
