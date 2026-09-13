//! Putting the keystore envelope on disk, and taking it off again.
//!
//! `keystore.rs` decides what the bytes are; this module decides where they go
//! and how they get there without a crash leaving a wallet in pieces.

use std::path::{Path, PathBuf};

use crate::keystore;
use crate::seed::MasterSeed;
use zeroize::Zeroizing;

/// The variables that name the user's home directory, first match wins.
/// Windows has no `HOME`: the profile directory is `USERPROFILE`, and it is
/// preferred there because a Git-Bash or MSYS shell sets a `HOME` of its
/// own that points into the shell's tree, not the user's.
#[cfg(windows)]
const HOME_VARS: &[&str] = &["USERPROFILE", "HOME"];
#[cfg(not(windows))]
const HOME_VARS: &[&str] = &["HOME"];

/// The user's home directory, or `None` if no variable names one.
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(|key| std::env::var_os(key))
}

/// `home_dir` over any lookup, so a test need not touch the process
/// environment (tests run in parallel threads; `set_var` races them).
pub fn home_dir_from<F>(lookup: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    HOME_VARS
        .iter()
        .find_map(|key| lookup(key))
        .map(PathBuf::from)
}

/// `~/.alphanumeric-gui/seed.enc`, or `None` if there is no home directory.
pub fn default_path() -> Option<PathBuf> {
    home_dir().map(|home| {
        let mut path = home;
        path.push(".alphanumeric-gui");
        path.push("seed.enc");
        path
    })
}

pub fn exists(path: &Path) -> bool {
    path.is_file()
}

/// A half-written keystore is a lost wallet, so the bytes go to a temporary in
/// the same directory and are renamed into place -- rename within a filesystem
/// is atomic, so a reader sees either the old file or the new one, never a torn
/// one. The temporary is CREATED at 0600 rather than written and then narrowed,
/// because a write-then-chmod leaves a window in which the whole envelope is
/// readable by anyone. Every failure path removes the temporary.
pub fn save(
    path: &Path,
    master: &MasterSeed,
    payload: &[u8],
    passphrase: &[u8],
) -> Result<(), String> {
    let envelope = keystore::seal(master, payload, passphrase)?;
    write_envelope(path, &envelope)
}

/// Put an already-sealed envelope at `path` the way `save` always has: a
/// temporary created at 0600 in the same directory, fsynced, renamed into
/// place. Split out so an imported wallet file can be written exactly as it
/// was read, without re-sealing it (spec G §4.4).
pub fn write_envelope(path: &Path, envelope: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "The wallet path has no directory.".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;

    let temporary = path.with_extension("enc.tmp");

    // Create the temporary ALREADY restricted rather than writing and then
    // narrowing it. A write-then-chmod leaves a window in which the whole
    // encrypted envelope sits under a listable name at whatever the umask gave
    // it -- typically group- and world-readable.
    let write_result = (|| -> std::io::Result<()> {
        // Remove any stale temporary first. `mode` applies only when open()
        // CREATES the file -- reopening a leftover from a crash (or from before
        // this was fixed) would truncate it at whatever permissions it already
        // had, which is the very window this is meant to close.
        let _ = std::fs::remove_file(&temporary);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        std::io::Write::write_all(&mut file, envelope)?;
        file.sync_all()
    })();

    // Every failure path removes the temporary. Leaving one behind means an
    // encrypted envelope lingering under a name nothing will ever clean up.
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Could not write {}: {e}", temporary.display()));
    }

    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("Could not replace {}: {e}", path.display())
    })
}

pub fn load(path: &Path, passphrase: &[u8]) -> Result<(MasterSeed, Zeroizing<Vec<u8>>), String> {
    let envelope =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    keystore::open(&envelope, passphrase)
}

/// The child seed for one address, as 64 lowercase hex characters, gated on
/// re-entering the passphrase.
///
/// Reads the file rather than using the master already in memory: the gate is
/// the point, and `keystore::reveal_child_seed_hex` is where it lives.
pub fn reveal_child_seed_hex(
    path: &Path,
    passphrase: &[u8],
    index: u32,
) -> Result<Zeroizing<String>, String> {
    let envelope =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    keystore::reveal_child_seed_hex(&envelope, passphrase, index)
}

/// The stored seed of one imported address, as 64 lowercase hex characters,
/// gated on re-entering the passphrase (spec H §4.3). Reads the file, like
/// `reveal_child_seed_hex`: the gate is the point.
pub fn reveal_imported_seed_hex(
    path: &Path,
    passphrase: &[u8],
    slot: usize,
) -> Result<Zeroizing<String>, String> {
    let envelope =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    keystore::reveal_imported_seed_hex(&envelope, passphrase, slot)
}

/// Write the master seed to a file the user chose, at 0600.
///
/// This is a plaintext spendable secret on disk by the user's explicit request:
/// the backup screen offers it because a 76-character seed is not something a
/// person transcribes by hand, and a password manager or a printed page is how
/// such a thing is actually kept. The permissions are set at CREATE time rather
/// than after writing, for the reason `save` gives -- a write-then-chmod leaves
/// a window in which the whole secret sits under a listable name at whatever
/// the umask allowed.
///
/// Unlike `save` this does NOT write through a temporary: the user picked this
/// exact path in a file dialog, and a rename from a sibling temp would be a
/// surprise if that path is a mount point or a symlink they meant.
pub fn write_seed_backup(path: &Path, seed: &str) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    // A trailing newline so `cat` and editors show it as a line rather than
    // running the shell prompt onto the end of the seed.
    std::io::Write::write_all(&mut file, seed.as_bytes())
        .and_then(|()| std::io::Write::write_all(&mut file, b"\n"))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    std::io::Write::flush(&mut file).map_err(|e| format!("Could not flush {}: {e}", path.display()))
}

/// One key that came from somewhere other than this wallet's master seed
/// (spec H). `seed` is the 64-hex address seed the node's `export-seed`
/// prints; `added` is an ISO-8601 UTC timestamp, for display only.
///
/// No `Debug`: this holds a spendable key. `Drop` wipes the string, because
/// serde hands us a plain `String` and a plain `String`'s bytes would
/// otherwise stay in freed memory.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ImportedKey {
    pub seed: String,
    pub added: String,
}

impl Drop for ImportedKey {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.seed.zeroize();
    }
}

/// Wallet metadata sealed beside the master seed inside the keystore
/// envelope. `next_index` counts DERIVED addresses only; an imported key
/// never takes a derivation index.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct WalletMetadata {
    pub next_index: u32,
    /// Absent in every file written before sub-project H.
    #[serde(default)]
    pub imported: Vec<ImportedKey>,
}

/// Where the wallet at `wallet_path` goes when another replaces it:
/// `<stem>-<stamp>.enc` beside it, or `-2`, `-3`, ... after that if the name
/// is `taken`. Pure -- the caller says what is taken -- so the naming can be
/// tested without a filesystem.
pub fn archive_path(wallet_path: &Path, stamp: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = wallet_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = wallet_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("seed");
    let first = dir.join(format!("{stem}-{stamp}.enc"));
    if !taken(&first) {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem}-{stamp}-{n}.enc")))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range always reaches a free name")
}

/// Put a DIFFERENT wallet at `wallet_path` without losing the one there
/// (spec G §4.1). The file already there, if any, is renamed to
/// `archive_path` -- never overwritten, never deleted -- and only then does
/// `write` put the new one in place. If `write` fails the archive is renamed
/// back, so a failed install leaves the directory as it was. Returns where
/// the old wallet went.
///
/// For installing another wallet only (create, restore, import). Adding an
/// address re-seals the SAME wallet and goes through `save` directly.
pub fn replace_archiving(
    wallet_path: &Path,
    stamp: &str,
    write: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Option<PathBuf>, String> {
    // `is_file()` swallows every stat error -- permission denied, a bad
    // mount, anything -- into the same `false` as "nothing here to archive",
    // which would let `write` silently overwrite a wallet it never actually
    // confirmed was gone (M9). `symlink_metadata` is checked explicitly so
    // only a real "not found" is treated as no wallet; every other error
    // fails closed. It is `symlink_metadata` rather than `metadata` so a
    // symlink AT `wallet_path` is archived too -- its target does not
    // matter, the name is what `write` is about to reuse.
    let archived = match std::fs::symlink_metadata(wallet_path) {
        Ok(_) => {
            let archive = archive_path(wallet_path, stamp, |candidate| candidate.exists());
            std::fs::rename(wallet_path, &archive).map_err(|e| {
                format!(
                    "Could not set the current wallet aside as {}: {e}. Nothing was changed.",
                    archive.display()
                )
            })?;
            Some(archive)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "Could not check {}: {e}. Nothing was changed.",
                wallet_path.display()
            ));
        }
    };
    if let Err(error) = write(wallet_path) {
        if let Some(archive) = &archived {
            if let Err(back) = std::fs::rename(archive, wallet_path) {
                return Err(rollback_failure(error, wallet_path, archive, back));
            }
        }
        return Err(error);
    }
    Ok(archived)
}

/// Spec §4.1 step 4: a failed rollback names both paths, so the user is not
/// left guessing where their wallet actually ended up. `error` almost always
/// ends its own sentence already; a period is appended only when it does
/// not, so the message never reads "...disk full.. The previous...".
fn rollback_failure(
    error: String,
    wallet_path: &Path,
    archive: &Path,
    back: std::io::Error,
) -> String {
    let sep = if error.ends_with('.') { "" } else { "." };
    format!(
        "{error}{sep} The previous wallet could not be moved back to {} ({back}); \
         it is safe at {}.",
        wallet_path.display(),
        archive.display()
    )
}

/// Every archive `replace_archiving` could have left beside `wallet_path`
/// (`<stem>-*.enc`), sorted by name -- which is by stamp.
pub fn find_archives(wallet_path: &Path) -> Vec<PathBuf> {
    let Some(dir) = wallet_path.parent() else {
        return Vec::new();
    };
    let stem = wallet_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("seed");
    let prefix = format!("{stem}-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".enc"))
        })
        .collect();
    found.sort();
    found
}

/// Put `envelope` at `dest` (spec G §3.2, §4.7), exactly as `export_copy`
/// always has: created at 0600, narrowed to 0600 before writing if `dest`
/// already existed, not through a temporary -- the user picked this exact
/// path in a file dialog (see `write_seed_backup`).
///
/// Split out from `export_copy` so `Message::ExportWalletFile` can read the
/// wallet file off the UI thread BEFORE the save dialog opens: the rfd
/// dialog is not modal, so a user can press EXPORT, leave it open, finish an
/// import, and only then press Save. Reading here, once, up front, means the
/// bytes written are the wallet as it was when EXPORT was pressed, not
/// whatever wallet happens to be open by the time Save is actually clicked
/// (I2).
pub fn export_bytes(wallet_path: &Path, envelope: &[u8], dest: &Path) -> Result<(), String> {
    if same_file(wallet_path, dest) {
        return Err("That is the wallet file itself. Choose another place for the copy.".into());
    }
    if same_parent(wallet_path, dest) {
        return Err("Choose a place outside the wallet's own folder for the copy.".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        // Both refusals above judge the path as written. A symbolic link
        // there can point anywhere -- at an archive inside the wallet
        // folder, say -- and following it would truncate that file, or a
        // dangling one would create a file where it points. The last path
        // component is never followed; the directories above it still are.
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut file = options.open(dest).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(nix::libc::ELOOP) {
            return format!(
                "{} is a symbolic link. Choose a plain file name for the copy.",
                dest.display()
            );
        }
        format!("Could not write {}: {e}", dest.display())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Could not restrict {}: {e}", dest.display()))?;
    }
    std::io::Write::write_all(&mut file, envelope)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("Could not write {}: {e}", dest.display()))
}

/// A copy of the encrypted wallet file at a place the user chose (spec G
/// §3.2, §4.7). The bytes are copied as they are -- still sealed under the
/// wallet's passphrase. Reads the file fresh and delegates to `export_bytes`
/// for the same-file/same-directory refusals and the write itself.
pub fn export_copy(wallet_path: &Path, dest: &Path) -> Result<(), String> {
    let envelope = std::fs::read(wallet_path)
        .map_err(|e| format!("Could not read {}: {e}", wallet_path.display()))?;
    export_bytes(wallet_path, &envelope, dest)
}

/// Whether `dest` would land in the very directory `wallet_path` lives in
/// (M5): a user could pick a name inside the wallet's own folder -- for
/// example one of its own archive names -- and overwrite the only surviving
/// copy of a wallet an import already replaced.
fn same_parent(wallet_path: &Path, dest: &Path) -> bool {
    fn normalize(parent: Option<&Path>) -> PathBuf {
        match parent {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }
    let wallet_parent = normalize(wallet_path.parent());
    let dest_parent = normalize(dest.parent());
    match (
        std::fs::canonicalize(&wallet_parent),
        std::fs::canonicalize(&dest_parent),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `b` names the same file as `a`, which exists. `b` may not exist
/// yet: then its directory is resolved and its name joined back on.
fn same_file(a: &Path, b: &Path) -> bool {
    let Ok(a) = std::fs::canonicalize(a) else {
        return false;
    };
    let b = match std::fs::canonicalize(b) {
        Ok(b) => b,
        Err(_) => {
            let (Some(parent), Some(name)) = (b.parent(), b.file_name()) else {
                return false;
            };
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            match std::fs::canonicalize(parent) {
                Ok(parent) => parent.join(name),
                Err(_) => return false,
            }
        }
    };
    a == b
}

/// The master seed in its `a9m1...` form, gated on re-entering the
/// passphrase. Reads the file rather than using the master in memory, like
/// `reveal_child_seed_hex`: the gate is the point.
pub fn reveal_master_seed(path: &Path, passphrase: &[u8]) -> Result<Zeroizing<String>, String> {
    let envelope =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    keystore::reveal_master_seed(&envelope, passphrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Windows has no `HOME`; the profile directory is `USERPROFILE`. A
    // wallet that only knows `HOME` refuses to create itself there with "No
    // home directory available".
    #[test]
    fn the_home_directory_is_the_first_home_variable_that_is_set() {
        let only_home = |key: &str| (key == "HOME").then(|| std::ffi::OsString::from("/h"));
        assert_eq!(home_dir_from(only_home), Some(PathBuf::from("/h")));
        assert_eq!(home_dir_from(|_| None), None);
    }

    // A Git-Bash or MSYS shell leaves a `HOME` pointing into its own tree.
    // The wallet must not follow it: the profile directory is where the
    // user's files are, and where a wallet made without that shell went.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_profile_directory_wins_over_a_stray_home() {
        let both = |key: &str| match key {
            "USERPROFILE" => Some(std::ffi::OsString::from("C:\\Users\\me")),
            "HOME" => Some(std::ffi::OsString::from("C:\\msys64\\home\\me")),
            _ => None,
        };
        assert_eq!(home_dir_from(both), Some(PathBuf::from("C:\\Users\\me")));
    }

    #[test]
    fn the_default_wallet_path_hangs_off_the_home_directory() {
        assert_eq!(
            default_path(),
            home_dir().map(|home| home.join(".alphanumeric-gui").join("seed.enc"))
        );
    }
    use crate::seed::MasterSeed;

    #[test]
    fn a_saved_wallet_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let master = MasterSeed::from_bytes([13u8; 32]);

        save(&path, &master, br#"{"next_index":3}"#, b"pass").expect("save");
        assert!(exists(&path));

        let (opened, payload) = load(&path, b"pass").expect("load");
        assert_eq!(
            opened.child_seed(0).as_slice(),
            master.child_seed(0).as_slice()
        );
        assert_eq!(payload.as_slice(), &br#"{"next_index":3}"#[..]);
    }

    #[test]
    fn a_wrong_passphrase_does_not_open_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        save(&path, &MasterSeed::from_bytes([1u8; 32]), b"{}", b"right").expect("save");
        assert!(load(&path, b"wrong").is_err());
    }

    // A half-written keystore is a lost wallet. Write to a temporary and rename,
    // so the file at `path` is always either the old one or the new one.
    #[test]
    fn saving_over_an_existing_wallet_never_leaves_a_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let first = MasterSeed::from_bytes([2u8; 32]);
        let second = MasterSeed::from_bytes([3u8; 32]);

        save(&path, &first, b"{}", b"pass").expect("first save");
        save(&path, &second, b"{}", b"pass").expect("second save");

        let (opened, _) = load(&path, b"pass").expect("load");
        assert_eq!(
            opened.child_seed(0).as_slice(),
            second.child_seed(0).as_slice(),
            "the second save must have replaced the first completely"
        );
        // No temporary left behind.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "seed.enc")
            .collect();
        assert!(strays.is_empty(), "left temporary files behind: {strays:?}");
    }

    // The file holds a wallet. Group and other have no business reading it.
    #[cfg(unix)]
    #[test]
    fn the_wallet_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        save(&path, &MasterSeed::from_bytes([4u8; 32]), b"{}", b"pass").expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group/other bits set: {:o}", mode);
    }

    #[test]
    fn loading_a_missing_file_says_so_without_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(&dir.path().join("absent.enc"), b"pass").is_err());
        assert!(!exists(&dir.path().join("absent.enc")));
    }

    // A leftover temporary must not carry its old permissions into the new
    // wallet. `mode` applies only at creation, so without removing the stale file
    // first, truncating it would keep whatever mode it already had.
    #[cfg(unix)]
    #[test]
    fn a_stale_temporary_does_not_leak_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let stale = path.with_extension("enc.tmp");

        std::fs::write(&stale, b"left over from a crash").expect("stale temporary");
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644))
            .expect("widen the stale file");

        save(&path, &MasterSeed::from_bytes([6u8; 32]), b"{}", b"pass").expect("save");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "inherited the stale file's mode: {:o}",
            mode
        );
        assert!(
            !stale.exists(),
            "the temporary should be gone after a rename"
        );
    }

    #[test]
    fn a_saved_wallet_reveals_one_child_seed_only_to_the_right_passphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let master = MasterSeed::from_bytes([13u8; 32]);
        save(&path, &master, br#"{"next_index":3}"#, b"pass").expect("save");

        let revealed = reveal_child_seed_hex(&path, b"pass", 2).expect("reveal");
        assert_eq!(
            revealed.as_str(),
            hex::encode(master.child_seed(2).as_slice())
        );
        assert!(reveal_child_seed_hex(&path, b"not the passphrase", 2).is_err());
    }

    #[test]
    fn a_seed_backup_is_written_readable_only_by_its_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.txt");
        let master = MasterSeed::from_bytes([21u8; 32]);
        let encoded = master.encode();

        write_seed_backup(&path, &encoded).expect("write");

        let written = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(written.trim_end(), encoded.as_str());
        // The whole point of the file is that it is a spendable secret. A
        // group- or world-readable one on a shared box is the failure.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o077, 0, "mode was {:o}", mode);
        }
    }

    // Writing twice must not leave the tail of a longer previous seed behind.
    #[test]
    fn a_seed_backup_replaces_what_was_there() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.txt");
        std::fs::write(&path, "x".repeat(500)).expect("prefill");

        let encoded = MasterSeed::from_bytes([22u8; 32]).encode();
        write_seed_backup(&path, &encoded).expect("write");

        let written = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(written.trim_end(), encoded.as_str());
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    // Spec G §4.1: an archive name is never an existing file.
    #[test]
    fn an_archive_name_steps_past_names_already_taken() {
        let wallet = Path::new("/w/seed.enc");
        let stamp = "20260912-031500";
        assert_eq!(
            archive_path(wallet, stamp, |_| false),
            PathBuf::from("/w/seed-20260912-031500.enc")
        );
        let first = PathBuf::from("/w/seed-20260912-031500.enc");
        assert_eq!(
            archive_path(wallet, stamp, |p| p == first),
            PathBuf::from("/w/seed-20260912-031500-2.enc")
        );
        let second = PathBuf::from("/w/seed-20260912-031500-2.enc");
        assert_eq!(
            archive_path(wallet, stamp, |p| p == first || p == second),
            PathBuf::from("/w/seed-20260912-031500-3.enc")
        );
    }

    #[test]
    fn with_no_wallet_there_the_new_one_is_written_and_nothing_is_archived() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let archived = replace_archiving(&path, "20260912-031500", |target| {
            write_envelope(target, b"new")
        })
        .expect("replace");
        assert_eq!(archived, None);
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }

    #[test]
    fn the_wallet_already_there_is_moved_aside_whole_before_the_new_one_lands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        write_envelope(&path, b"old wallet").expect("old");
        let archived = replace_archiving(&path, "20260912-031500", |target| {
            write_envelope(target, b"new")
        })
        .expect("replace")
        .expect("an archive");
        assert_eq!(archived, dir.path().join("seed-20260912-031500.enc"));
        assert_eq!(std::fs::read(&archived).expect("archive"), b"old wallet");
        assert_eq!(std::fs::read(&path).expect("new"), b"new");
    }

    // A failed write must leave the directory exactly as it was.
    #[test]
    fn a_failed_write_puts_the_old_wallet_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        write_envelope(&path, b"old wallet").expect("old");
        let result = replace_archiving(&path, "20260912-031500", |_| Err("disk full".into()));
        assert_eq!(result, Err("disk full".to_string()));
        assert_eq!(std::fs::read(&path).expect("restored"), b"old wallet");
        assert!(find_archives(&path).is_empty(), "nothing left behind");
    }

    // M12: the rollback-failure message names both paths, and never doubles
    // a period `error` already ended with.
    #[test]
    fn a_rollback_failure_names_both_paths_without_doubling_the_period() {
        let wallet_path = Path::new("/w/seed.enc");
        let archive = Path::new("/w/seed-20260912-031500.enc");

        let message = rollback_failure(
            "disk full".to_string(),
            wallet_path,
            archive,
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert_eq!(
            message,
            "disk full. The previous wallet could not be moved back to /w/seed.enc (denied); \
             it is safe at /w/seed-20260912-031500.enc."
        );

        let message = rollback_failure(
            "disk full.".to_string(),
            wallet_path,
            archive,
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert!(
            !message.contains(".."),
            "must not double the period: {message}"
        );
    }

    #[test]
    fn an_existing_archive_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let earlier = dir.path().join("seed-20260912-031500.enc");
        write_envelope(&earlier, b"earlier archive").expect("earlier");
        write_envelope(&path, b"old wallet").expect("old");
        let archived = replace_archiving(&path, "20260912-031500", |target| {
            write_envelope(target, b"new")
        })
        .expect("replace")
        .expect("an archive");
        assert_eq!(archived, dir.path().join("seed-20260912-031500-2.enc"));
        assert_eq!(
            std::fs::read(&earlier).expect("earlier"),
            b"earlier archive"
        );
    }

    #[test]
    fn archives_are_found_beside_the_wallet_and_nothing_else_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        for name in [
            "seed.enc",
            "seed.enc.tmp",
            "other.enc",
            "seed-20260102-000000.enc",
            "seed-20260101-000000.enc",
        ] {
            std::fs::write(dir.path().join(name), b"x").expect("write");
        }
        assert_eq!(
            find_archives(&path),
            vec![
                dir.path().join("seed-20260101-000000.enc"),
                dir.path().join("seed-20260102-000000.enc"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_export_is_the_same_bytes_and_readable_by_the_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        save(
            &path,
            &MasterSeed::from_bytes([5u8; 32]),
            br#"{"next_index":2}"#,
            b"pass",
        )
        .expect("save");
        // Outside the wallet's own directory (M5): a destination beside the
        // wallet file is refused now, so this "the export just works" test
        // moves its target elsewhere to keep testing what it means to test.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out dir");
        let dest = out.join("backup.enc");
        // Pre-existing and world-readable: the export must narrow it, not keep it.
        std::fs::write(&dest, b"stale").expect("stale");
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        export_copy(&path, &dest).expect("export");

        assert_eq!(
            std::fs::read(&dest).expect("dest"),
            std::fs::read(&path).expect("src")
        );
        assert_eq!(mode(&dest), 0o600);
        let (_, payload) = load(&dest, b"pass").expect("the copy opens");
        assert_eq!(payload.as_slice(), &br#"{"next_index":2}"#[..]);
    }

    #[test]
    fn exporting_onto_the_wallet_file_itself_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        write_envelope(&path, b"wallet").expect("wallet");
        let same = dir.path().join(".").join("seed.enc");
        assert!(export_copy(&path, &same).is_err());
        assert_eq!(std::fs::read(&path).expect("untouched"), b"wallet");
    }

    // I2: `export_bytes` writes exactly the bytes it was given, not whatever
    // happens to be on disk at the moment it runs -- the whole point of
    // reading the envelope before the (non-modal) save dialog opens.
    #[cfg(unix)]
    #[test]
    fn export_bytes_writes_exactly_the_bytes_it_was_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_path = dir.path().join("seed.enc");
        write_envelope(&wallet_path, b"on disk right now").expect("wallet");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out dir");
        let dest = out.join("backup.enc");

        export_bytes(&wallet_path, b"envelope as of EXPORT", &dest).expect("export");

        assert_eq!(
            std::fs::read(&dest).expect("dest"),
            b"envelope as of EXPORT"
        );
        assert_eq!(mode(&dest), 0o600);
    }

    #[test]
    fn export_bytes_onto_the_wallet_file_itself_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_path = dir.path().join("seed.enc");
        write_envelope(&wallet_path, b"wallet").expect("wallet");

        assert!(export_bytes(&wallet_path, b"envelope", &wallet_path).is_err());
        assert_eq!(std::fs::read(&wallet_path).expect("untouched"), b"wallet");
    }

    // M5: a copy beside the wallet file could overwrite the only surviving
    // copy of a wallet an import already replaced -- for example one of the
    // wallet's own archive names.
    #[test]
    fn exporting_beside_the_wallet_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_path = dir.path().join("seed.enc");
        write_envelope(&wallet_path, b"wallet").expect("wallet");
        let dest = dir.path().join("seed-20260101-000000.enc");

        let error = export_copy(&wallet_path, &dest).expect_err("refused");
        assert!(error.contains("outside the wallet's own folder"), "{error}");
        assert!(!dest.exists(), "nothing was written");
        assert_eq!(std::fs::read(&wallet_path).expect("untouched"), b"wallet");
    }

    // A link outside the wallet folder can point at an archive inside it:
    // both refusals above look at the link's own path, and opening it would
    // then follow it and truncate the only copy of a replaced wallet. A
    // dangling link would create a file wherever it points. The final path
    // component is never followed.
    #[cfg(unix)]
    #[test]
    fn exporting_through_a_symbolic_link_is_refused_and_the_target_is_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_dir = dir.path().join("wallet");
        std::fs::create_dir(&wallet_dir).expect("wallet dir");
        let wallet_path = wallet_dir.join("seed.enc");
        write_envelope(&wallet_path, b"wallet").expect("wallet");
        let archive = wallet_dir.join("seed-20260101-000000.enc");
        write_envelope(&archive, b"the only copy of a replaced wallet").expect("archive");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");

        let link = out.join("backup.enc");
        std::os::unix::fs::symlink(&archive, &link).expect("symlink");
        let error = export_copy(&wallet_path, &link).expect_err("refused");
        assert!(error.contains("symbolic link"), "{error}");
        assert_eq!(
            std::fs::read(&archive).expect("archive"),
            b"the only copy of a replaced wallet"
        );

        let dangling = out.join("dangling.enc");
        let nowhere = wallet_dir.join("created-by-a-link.enc");
        std::os::unix::fs::symlink(&nowhere, &dangling).expect("symlink");
        assert!(export_copy(&wallet_path, &dangling).is_err());
        assert!(!nowhere.exists(), "nothing was created through the link");
    }

    #[test]
    fn exporting_into_a_subdirectory_still_works() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_path = dir.path().join("seed.enc");
        write_envelope(&wallet_path, b"wallet").expect("wallet");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let dest = sub.join("backup.enc");

        export_copy(&wallet_path, &dest).expect("export");

        assert_eq!(std::fs::read(&dest).expect("dest"), b"wallet");
    }

    #[test]
    fn the_master_seed_comes_back_only_for_the_right_passphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let master = MasterSeed::from_bytes([6u8; 32]);
        save(&path, &master, b"{}", b"right").expect("save");
        assert!(reveal_master_seed(&path, b"wrong").is_err());
        assert_eq!(
            reveal_master_seed(&path, b"right")
                .expect("reveal")
                .as_str(),
            master.encode().as_str()
        );
    }

    #[test]
    fn wallet_metadata_round_trips_its_json() {
        let bytes = serde_json::to_vec(&WalletMetadata {
            next_index: 4,
            imported: Vec::new(),
        })
        .expect("encode");
        // The exact wire shape is what older builds must keep parsing.
        assert_eq!(bytes, br#"{"next_index":4,"imported":[]}"#);
        let back: WalletMetadata = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back.next_index, 4);
        assert!(back.imported.is_empty());
    }

    // Spec H §4.1: an older wallet file has no `imported` field at all.
    #[test]
    fn a_payload_without_the_list_reads_as_an_empty_one() {
        let meta: WalletMetadata = serde_json::from_slice(br#"{"next_index":3}"#).expect("decode");
        assert_eq!(meta.next_index, 3);
        assert!(meta.imported.is_empty());
    }

    #[test]
    fn a_payload_with_the_list_round_trips() {
        let meta = WalletMetadata {
            next_index: 2,
            imported: vec![ImportedKey {
                seed: "11".repeat(32),
                added: "2026-09-12T05:33:50Z".to_string(),
            }],
        };
        let bytes = serde_json::to_vec(&meta).expect("encode");
        let back: WalletMetadata = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(back.next_index, 2);
        assert_eq!(back.imported.len(), 1);
        assert_eq!(back.imported[0].seed, "11".repeat(32));
        assert_eq!(back.imported[0].added, "2026-09-12T05:33:50Z");
    }

    // The gate is the same one `reveal_child_seed_hex` uses: the file is
    // re-opened with the passphrase the user just typed.
    #[test]
    fn an_imported_seed_comes_back_only_for_the_right_passphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let payload = serde_json::to_vec(&WalletMetadata {
            next_index: 1,
            imported: vec![ImportedKey {
                seed: "22".repeat(32),
                added: "2026-09-12T05:33:50Z".to_string(),
            }],
        })
        .expect("encode");
        save(&path, &MasterSeed::from_bytes([4u8; 32]), &payload, b"pass").expect("save");

        assert!(reveal_imported_seed_hex(&path, b"wrong", 0).is_err());
        assert_eq!(
            reveal_imported_seed_hex(&path, b"pass", 0)
                .expect("reveal")
                .as_str(),
            "22".repeat(32)
        );
        assert!(
            reveal_imported_seed_hex(&path, b"pass", 1).is_err(),
            "a slot that is not there is refused, not panicked on"
        );
    }
}
