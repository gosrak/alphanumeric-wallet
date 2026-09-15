//! Application state and the message loop.
//!
//! Everything that can lose funds lives in `seed`, `tx`, `photo`, `keystore`,
//! `model`, `backend` and `storage` -- none of which import `iced`. This module
//! and the views under it are presentation only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::{Element, Task};
use zeroize::Zeroizing;

use alphanumeric_gui::backend;
use alphanumeric_gui::model;
use alphanumeric_gui::node;
use alphanumeric_gui::photo::PhotoSecret;
use alphanumeric_gui::seed::MasterSeed;
use alphanumeric_gui::settings;
use alphanumeric_gui::startup;
use alphanumeric_gui::storage;
use alphanumeric_gui::storage::WalletMetadata;

use crate::view::send::{Prepared, Verdict};

/// Default explorer API address. A convention for this deployment, not a
/// protocol default -- the node has no default port and binds only when
/// `ALPHANUMERIC_EXPLORER_API` is set.
pub const DEFAULT_NODE_URL: &str = "http://127.0.0.1:8095";

/// Set once by `main` before the application runs. A static rather than a
/// constructor argument because `iced::application` requires `App::new` to
/// take none.
pub static SCREENSHOT_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Which screen is in front of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// No wallet yet: create one or restore one.
    Setup,
    /// The wallet's own node is coming up: bootstrap download, then catch-up.
    Startup,
    /// Balances and addresses.
    Wallet,
    /// An address and its QR code.
    Receive,
    /// Composing a payment.
    Send,
    /// All addresses' transaction history, merged.
    History,
    /// What the miner is doing, and its controls.
    Mining,
    /// The node process this wallet drives (or the external one it points
    /// at): state, resource use, log tail.
    Node,
    /// Node source and connection settings, gathered under one tab.
    Settings,
}

impl Screen {
    /// File stem used by the `--screenshot` flag. Kept next to the enum so a
    /// new screen cannot be added without one.
    fn file_stem(self) -> &'static str {
        match self {
            Screen::Setup => "setup",
            Screen::Startup => "startup",
            Screen::Wallet => "wallet",
            Screen::Receive => "receive",
            Screen::Send => "send",
            Screen::History => "history",
            Screen::Mining => "mining",
            Screen::Node => "node",
            Screen::Settings => "settings",
        }
    }
}

/// Which node this session drives. `External` -- an address the user points
/// at a node they run themselves -- is what `node_url` has always meant.
/// `Owned` starts a node as a child process instead. Defaults to `Owned`;
/// `Message::ChooseNodeSource` is the only way a fresh install picks
/// `External`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSource {
    Owned,
    External,
}

impl NodeSource {
    /// The words the picker's buttons say, and the words F7's summary uses
    /// to name the choice in effect. One source so the summary can never
    /// name a choice the buttons do not offer.
    pub fn label(self) -> &'static str {
        match self {
            NodeSource::Owned => "Run its own node",
            NodeSource::External => "Point at a node I already run",
        }
    }
}

/// The four-character check that stands between seeing the seed and losing it.
///
/// The seed is hidden while this is up, so the only way to answer is from
/// wherever it was saved.
pub struct BackupQuiz {
    /// Character offsets into the encoded seed, sorted.
    pub positions: Vec<usize>,
    pub typed: Zeroizing<String>,
    pub mismatch: bool,
}

/// A master seed that has not been written to disk yet, on its way either from
/// a fresh `MasterSeed::random()` or from a restore input.
#[derive(Clone)]
pub enum PendingWallet {
    /// Just generated; the backup gate has already been passed.
    Fresh(MasterSeed),
    /// Recovered from a master-seed string or a photograph.
    Restored(MasterSeed),
}

impl PendingWallet {
    fn master(&self) -> &MasterSeed {
        match self {
            PendingWallet::Fresh(master) | PendingWallet::Restored(master) => master,
        }
    }

    fn is_restored(&self) -> bool {
        matches!(self, PendingWallet::Restored(_))
    }
}

/// A wallet file opened with a passphrase, kept together so the passphrase
/// USE writes can never drift from the one that actually unlocked
/// `file.envelope` (spec G plan ruling 5, controller ruling: Task 6 fix
/// round 1). No `Debug`: carries a passphrase.
pub struct OpenedFile {
    pub file: Arc<alphanumeric_gui::import::WalletFile>,
    pub passphrase: Zeroizing<String>,
}

/// One stage of the setup screen's flow. Every field that can hold a master
/// seed or a passphrase is `Zeroizing`, matching the hygiene the core modules
/// already hold themselves to.
pub enum SetupStage {
    /// No wallet file exists yet: create one or restore one.
    Choose,
    /// A wallet file exists on disk; its passphrase unlocks it.
    Unlock {
        passphrase: Zeroizing<String>,
        error: Option<String>,
        busy: bool,
    },
    /// A fresh seed was just generated and must be typed back before anything
    /// else can happen -- backup is a gate, not a suggestion (spec 3.4).
    ConfirmBackup {
        master: MasterSeed,
        shown: Zeroizing<String>,
        /// `None` while the seed is still on screen. Becomes `Some` when the
        /// user says they have saved it, and the seed is hidden from that
        /// moment on -- a quiz you can answer by reading the answer off the
        /// screen above it is not a quiz.
        quiz: Option<BackupQuiz>,
        /// Where the seed was written, if it was. Shown so the user can see it
        /// landed somewhere before they are asked to prove it.
        saved_to: Option<PathBuf>,
        save_error: Option<String>,
    },
    /// Restoring a wallet. The master-seed string and the photo are two
    /// separate inputs (spec 3.2) -- one field must never try to accept both.
    Restore {
        seed_input: Zeroizing<String>,
        seed_error: Option<String>,
        photo: Option<Arc<PhotoSecret>>,
        photo_busy: bool,
        photo_error: Option<String>,
    },
    /// Importing a wallet file (spec G §3.5): choose it, open it with its own
    /// passphrase, see what is in it, then use it. `preview`, once `Some`,
    /// carries the file together with the exact passphrase that opened it
    /// (`OpenedFile`) -- USE always installs THAT passphrase, never whatever
    /// `passphrase` (the text field) currently holds, so plan ruling 5 holds
    /// by construction rather than by timing (controller ruling: Task 6 fix
    /// round 1). Picking a different file, or any edit to the passphrase
    /// field, clears `preview` with it; `ImportPassphraseChanged` refuses the
    /// edit outright while `busy`, so nothing typed while an OPEN or a USE is
    /// in flight can ever reach the wallet that gets installed.
    ImportFile {
        path: Option<PathBuf>,
        passphrase: Zeroizing<String>,
        busy: bool,
        error: Option<String>,
        preview: Option<OpenedFile>,
    },
    /// A master seed is in hand (fresh or restored); choose a passphrase to
    /// encrypt it with before it touches disk.
    SetPassphrase {
        pending: PendingWallet,
        passphrase: Zeroizing<String>,
        confirm: Zeroizing<String>,
        busy: bool,
        error: Option<String>,
        /// Set only when a RESTORE's node check or discovery scan could not
        /// reach the node -- a first-class guidance state (spec 4.1), not an
        /// error string. `Fresh` wallets never set this: they save before
        /// ever asking the node, so connectivity failing there is not a
        /// reason to block anything.
        node_unreachable: Option<String>,
    },
}

/// What a completed setup flow (create, restore, or unlock) hands back: enough
/// to build a `WalletState`. `status` is `None` when the node could not be
/// reached -- that is not treated as a setup failure, since the wallet itself
/// is valid either way; the wallet screen shows the "no node" guidance instead.
#[derive(Clone)]
pub(crate) struct Ready {
    master: MasterSeed,
    next_index: u32,
    status: Option<backend::NodeStatus>,
    passphrase: Zeroizing<String>,
    /// The exact address `install_wallet` builds its client from -- carried
    /// through rather than re-read from `self.node_url`, so an edit made to
    /// the field while a restore's discovery scan is still running cannot
    /// desynchronise the node the scan ran against from the one the wallet
    /// screen ends up polling.
    node_url: String,
    /// Where the wallet this one replaced was set aside
    /// (`storage::replace_archiving`), if one was. `None` for an unlock, and
    /// for an install with no wallet file there before it.
    archived: Option<PathBuf>,
    /// The imported seeds this wallet's file already holds (spec H). Empty
    /// for a fresh create/restore -- there is nothing to import yet -- and
    /// read back from the file's metadata for an unlock or a file import.
    imported: Vec<Zeroizing<String>>,
}

/// Why a create/restore/unlock flow did not produce a ready wallet. Both
/// variants carry a plain, already-user-facing message -- never a seed or a
/// passphrase -- so `Debug` (for `.expect()` in tests) is safe here.
#[derive(Debug, Clone)]
pub(crate) enum SetupFailure {
    /// The node could not be reached (or a restore's discovery scan could not
    /// finish) -- a first-class guidance state (spec 4.1), not an error toast.
    /// Only ever produced on the restore path, which needs the node before it
    /// can know `next_index` and therefore before it will save anything.
    NodeUnreachable(String),
    /// Anything else: a bad passphrase, a corrupt file, a disk error.
    Other(String),
}

/// The real maximum a transaction can send (spec 4.2), or a reason there is
/// no figure to show. Kept as three states rather than `Option<i128>` because
/// two different things used to collapse into the same `None`: never having
/// fetched this row yet, and the node answering but being unable to compute
/// the spendable overlay (`spendable_units: null` -- a real, legitimate
/// response, not an error). A user cannot tell "this node cannot compute your
/// spendable balance right now" from "still loading" if both render the same
/// way, so each gets its own visible state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spendable {
    /// Never successfully fetched.
    Pending,
    /// The node answered, but could not compute this overlay.
    Unavailable,
    Known(i128),
}

/// Where an address's signing key comes from (spec H §4.2).
///
/// `Derived` is every address this wallet made itself: the key is
/// `master.child_seed(index)`, recomputed when needed, and a master-seed
/// restore brings it back. `Imported` is a key pasted from somewhere else
/// and stored in the wallet file; it has no index and no way to be
/// recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    Derived(u32),
    Imported(usize),
}

pub struct AddressEntry {
    pub source: AddressSource,
    pub address: String,
    /// Includes immature mining rewards and pending debits.
    pub balance_units: Option<i128>,
    pub spendable: Spendable,
    /// Set only for a NON-retryable failure -- a retryable one (503/429) is
    /// "later", not a failure, and must not be shown as one.
    pub error: Option<String>,
    pub loading: bool,
    /// The first page of this address's history from its last successful
    /// refresh (`AddressState::as_page`). F1's recent activity and F2's
    /// incoming list read it -- no request of their own.
    pub recent: Option<backend::AddressPage>,
}

impl AddressEntry {
    /// What the IDX column and the `[0]` tags show.
    pub fn label(&self) -> String {
        match self.source {
            AddressSource::Derived(index) => index.to_string(),
            AddressSource::Imported(_) => "IMP".to_string(),
        }
    }

    pub fn is_imported(&self) -> bool {
        matches!(self.source, AddressSource::Imported(_))
    }
}

/// What the send screen is doing.
pub enum SendStage {
    /// Filling in the form. Nothing is signed and nothing is outstanding.
    Compose,
    /// A payment has been prepared and is on screen for approval. Still
    /// unsigned: a `Prepared` is arithmetic, and holding one commits nothing.
    Confirm(Prepared),
    /// Approved, and the sender's spendable balance is being fetched fresh so
    /// the ceiling can be checked against an answer taken now rather than
    /// against whatever the ten-second poll last left in the address row.
    /// Still unsigned.
    Checking(Prepared),
    /// A signed body has been posted; the answer has not come back.
    Sending,
    /// The node answered, or failed to.
    Answered(Verdict),
}

/// The send screen's state.
///
/// Lives on `WalletState` rather than on `App` because a payment needs the
/// seed, the client and an address to exist at all, and because navigating
/// away from the screen must not be able to discard a payment whose fate is
/// unknown.
pub struct SendState {
    pub recipient: String,
    /// The amount as TYPED, in coins. It is quantised to integer units by
    /// `view::send::parse_coin_amount` and never held as a float.
    pub amount: String,
    pub fee: Option<backend::FeeEstimate>,
    pub fee_error: Option<String>,
    pub fee_in_flight: bool,
    /// The node's clock minus this computer's, in seconds, as of the last fee
    /// estimate. `None` when it could not be read.
    pub clock_offset: Option<i64>,
    pub stage: SendStage,
    /// A reason the confirmation could not proceed, shown on that screen.
    pub error: Option<String>,
    /// The EXACT body that was signed and posted, held so that a retry
    /// re-posts it byte for byte.
    ///
    /// This is the whole point of `/v2/`: the same key with the same signed
    /// transaction is a retry the node can recognise, while a rebuilt body
    /// carries a fresh key and a fresh timestamp and is a SECOND payment. It
    /// is cleared only when the node's answer is definitive.
    pub outstanding: Option<Arc<serde_json::Value>>,
}

impl SendState {
    fn new() -> Self {
        Self {
            recipient: String::new(),
            amount: String::new(),
            fee: None,
            fee_error: None,
            fee_in_flight: false,
            clock_offset: None,
            stage: SendStage::Compose,
            error: None,
            outstanding: None,
        }
    }

    /// Put the screen back to the form, keeping what was typed.
    ///
    /// Only for an outcome where nothing was admitted and the user is being
    /// asked to correct something -- see `view::send::keeps_the_form`.
    fn return_to_form(&mut self) {
        self.stage = SendStage::Compose;
        self.error = None;
        self.outstanding = None;
    }

    /// Put the screen back to an EMPTY form. Never called while a signed body
    /// is outstanding except by the user's own explicit discard.
    fn reset(&mut self) {
        self.recipient.clear();
        self.amount.clear();
        self.return_to_form();
    }
}

/// The seed-reveal gate on the receive screen.
///
/// A spendable key reaches the screen from `Revealed` and nowhere else, and the
/// only way into `Revealed` is a passphrase that opens the wallet file. No
/// `Debug`: the whole point of this type is that its contents never reach a log,
/// a panic message, or a formatted error.
/// What a successful reveal hands back. A struct rather than a tuple so the
/// seed stays `Zeroizing` while it travels through the message queue -- the
/// same reason `Ready` carries its passphrase wrapped.
#[derive(Clone)]
pub struct Revealed {
    pub address: String,
    pub seed_hex: Zeroizing<String>,
}

pub enum RevealState {
    /// Nothing asked for. Every screen change returns here.
    Idle,
    /// The warning is up and the passphrase field is live.
    Asking {
        passphrase: Zeroizing<String>,
        error: Option<String>,
        busy: bool,
    },
    /// The child seed for the address that was active when it was asked for.
    /// The address is kept beside it so a reveal cannot outlive the selection
    /// it belongs to and end up labelled with a different row.
    Revealed {
        address: String,
        seed_hex: Zeroizing<String>,
    },
}

/// A master seed on its way back from the blocking task that re-opened the
/// wallet file. No `Debug` (spec G §4.5); `Zeroizing` through the queue.
#[derive(Clone)]
pub struct MasterSeedText(pub Zeroizing<String>);

/// F7: the master-seed reveal gate (spec G §3.3). Lives on `WalletState`, so
/// closing the wallet drops it; `Message::Show`, `HistoryOpen` and
/// `ImportStart` return it to `Idle`.
pub enum MasterRevealState {
    Idle,
    /// `request` is this attempt's number from `App::master_reveal_requests`;
    /// only a `MasterRevealDone` carrying it is applied.
    Asking {
        passphrase: Zeroizing<String>,
        error: Option<String>,
        busy: bool,
        request: u64,
    },
    Revealed {
        seed: Zeroizing<String>,
    },
}

/// F1's IMPORT panel (spec H §3.2). The pasted seed lives here and nowhere
/// else, in `Zeroizing`, and goes back to `Idle` on any screen change.
pub enum ImportKeyState {
    Idle,
    Asking {
        seed: Zeroizing<String>,
        error: Option<String>,
    },
    /// CHECK derived an address from the seed. Nothing has been stored.
    Checked {
        seed: Zeroizing<String>,
        address: String,
    },
    /// The file is being re-sealed.
    Busy,
}

/// The confirm step for taking an imported key out (spec H §3.6). Removal
/// deletes the key from the wallet file, so the address has to be typed back
/// -- the same shape as the send confirmation.
pub struct RemoveKey {
    pub slot: usize,
    pub address: String,
    pub typed: String,
    pub error: Option<String>,
}

/// Import mode (spec G §3.4-3.5): the wallet that was open has been closed
/// and the setup screen is choosing its replacement. Only public facts about
/// the closed wallet are kept.
pub struct ImportContext {
    /// Index 0's address of the closed wallet, for the same-wallet check (§4.3).
    pub replaced_first_address: String,
}

const SAME_WALLET: &str = "This is the wallet that was already open. Nothing was changed.";

pub struct WalletState {
    pub master: MasterSeed,
    pub client: Arc<backend::Client>,
    pub wallet_path: PathBuf,
    /// Kept for the session so adding an address can re-seal the keystore
    /// without asking again. Zeroized on drop like every other secret here.
    pub passphrase: Zeroizing<String>,
    pub next_index: u32,
    /// The imported seeds, in payload slot order -- indexed by
    /// `AddressSource::Imported(slot)`.
    pub imported: Vec<Zeroizing<String>>,
    pub addresses: Vec<AddressEntry>,
    /// Index into `addresses`, not an address index -- the actively-polled row.
    pub active: usize,
    /// `None` means the node has never answered. The wallet screen shows the
    /// "no node reachable" guidance in that case rather than an empty balance.
    pub node_status: Option<backend::NodeStatus>,
    /// A non-retryable error from the most recent status check, shown as a
    /// small warning while `node_status` still holds the last good reading.
    pub node_status_error: Option<String>,
    /// The 10-second active-address-only poll, in flight.
    pub poll_in_flight: bool,
    /// A full refresh (status + every address), in flight.
    pub fetch_in_flight: bool,
    pub adding_address: bool,
    pub save_error: Option<String>,
    /// QR encoding of the active address, kept alongside it rather than
    /// rebuilt on every frame. `None` only if the address somehow does not
    /// fit a QR code -- 40 hex characters always does.
    pub qr: Option<iced::widget::qr_code::Data>,
    pub send: SendState,
    /// Receive: the seed-reveal gate. Never carried across a screen change.
    pub reveal: RevealState,
    /// The history screen's state. `None` if it has never been entered, or
    /// has been left.
    pub history: Option<HistoryState>,
    /// Settings: the master-seed reveal gate. Never carried across a screen change.
    pub master_reveal: MasterRevealState,
    /// Settings: an export's save dialog or copy is running.
    pub exporting: bool,
    /// Settings: where the last export went, or why it failed.
    pub export_result: Option<Result<PathBuf, String>>,
    /// Settings: the IMPORT WALLET confirm panel is up; the path is where the
    /// current file would be set aside, computed when the panel opened
    /// (planning ruling 4).
    pub import_confirm: Option<PathBuf>,
    /// F1's IMPORT AN ADDRESS panel. Never carried across a screen change.
    pub import_key: ImportKeyState,
    /// F1's REMOVE AN IMPORTED ADDRESS panel. Never carried across a screen
    /// change.
    pub removing_key: Option<RemoveKey>,
}

impl WalletState {
    /// The seed that signs for `source`, or `None` if the wallet no longer
    /// has it. The single place the two kinds of key differ.
    pub fn signing_seed(&self, source: AddressSource) -> Option<Zeroizing<[u8; 32]>> {
        match source {
            AddressSource::Derived(index) => Some(self.master.child_seed(index)),
            AddressSource::Imported(slot) => {
                let hex = self.imported.get(slot)?;
                alphanumeric_gui::seed::parse_imported_seed(hex).ok()
            }
        }
    }
}

/// State for the history screen. Dropped whole on leaving the screen.
pub struct HistoryState {
    pub merge: alphanumeric_gui::history::Merge,
    /// A page request is outstanding. A lock against sending two at once.
    pub in_flight: bool,
    /// Why the last page request failed, if it did. If set, the list is
    /// stopped where it is.
    pub error: Option<String>,
    /// This state's generation, from `App::history_generation` at the
    /// moment it was built. See that field's comment.
    generation: u64,
}

impl HistoryState {
    pub fn rows_len(&self) -> usize {
        self.merge.rows().len()
    }
}

pub struct App {
    pub screen: Screen,
    pub node_url: String,
    pub wallet_path: Option<PathBuf>,
    pub setup: SetupStage,
    pub wallet: Option<WalletState>,
    /// Which node this session drives.
    pub node_source: NodeSource,
    /// `Some` only when `node_source` is `Owned`. Its `Drop` is what stops the
    /// node (SIGTERM, then join the supervisor thread) -- dropping it IS how
    /// this app stops the node, so it is never dropped and immediately
    /// recreated in the same statement, and `stop()` is never called on it
    /// before letting it drop.
    supervisor: Option<node::Supervisor>,
    /// What the startup screen shows. Recomputed from `supervisor.state()`,
    /// `startup_status` and the log tail every time either changes.
    node_phase: startup::Phase,
    /// Only used before a wallet exists. Once `Screen::Wallet` is reached,
    /// `WalletState::node_status` is the one true reading -- this field
    /// exists only because `WalletState` is `None` until then, so the
    /// startup screen has nothing else to poll into.
    startup_status: Option<backend::NodeStatus>,
    startup_poll_in_flight: bool,
    /// Editable node-binary path, `Owned` only. Empty means unconfigured --
    /// see `configured_binary`, not an empty path.
    pub node_binary_input: String,
    /// Editable P2P port, `Owned` only, kept exactly as typed. Parsed only in
    /// `resolved_ports`, at the moment the node is actually started -- not on
    /// every keystroke, which would reject `"7"` and `"72"` on the way to
    /// typing `"7200"`.
    pub p2p_port_input: String,
    /// Editable explorer port, `Owned` only. Same typing rule as
    /// `p2p_port_input`.
    pub explorer_port_input: String,
    /// Editable stats port, `Owned` only. Same typing rule as
    /// `p2p_port_input`.
    pub stats_port_input: String,
    /// Where `--screenshot <dir>` writes to. `None` in every normal run.
    screenshot_dir: Option<std::path::PathBuf>,
    /// Set once a capture has landed, so the frame subscription stops asking
    /// for more.
    screenshot_taken: bool,
    /// Real `RedrawRequested` events seen since start, while a capture is
    /// still pending. Used to let a couple of frames pass before capturing
    /// -- see `Message::CaptureScreenshot`.
    screenshot_frames_seen: u32,
    /// What the wallet remembers between runs: node binary and ports, and
    /// the mining choices. Loaded once at start from `settings_path`.
    pub settings: settings::Settings,
    pub settings_path: Option<std::path::PathBuf>,
    /// The last failure to write the settings file, shown on F5 and F7.
    pub settings_error: Option<String>,
    /// The GPUs the node last reported (`gpu_devices`), so F5 can offer the
    /// per-GPU switches before anything mines.
    pub known_gpus: Vec<backend::GpuDevice>,
    /// The mining launch the running node was started with, to tell an
    /// edited setting apart from an applied one.
    pub launched_mining: Option<node::MiningLaunch>,
    /// F5: why the last START was refused, until the next edit.
    pub mining_error: Option<String>,
    /// F5: the thread count as typed, so a half-edited field is not lost.
    pub mining_threads_input: String,

    /// The console strip's last-read `/stats` and `/explorer/supply`.
    /// `None` until the first `ConsoleTick` answers. `node_stats` is kept
    /// (not cleared) ONLY when a refetch merely failed -- a transient error
    /// must not blank a value that was known a moment ago. It IS cleared the
    /// moment the process it described is known gone rather than merely
    /// unreachable: `ConsoleTick`'s `Owned` branch when the supervisor is not
    /// `Running`, its `External` branch (every tick, closing the race where
    /// an in-flight request from the old client lands after a source
    /// change), `Message::ChooseNodeSource`, and `ensure_owned_node` (a
    /// restart drops the old supervisor before this is cleared, the same
    /// moment `prev_cpu_sample` is). None of these are instantaneous with
    /// the process actually stopping -- a `ConsoleTick` has to run and
    /// observe it -- so a caller that must never show a dead or
    /// just-restarted process's PREVIOUS reading (`view::node`'s uptime line
    /// is the one that cares) still gates on `node_state()` itself rather
    /// than trusting this field alone.
    node_stats: Option<backend::NodeStats>,
    supply: Option<backend::Supply>,
    /// Locks against `ConsoleTick` overlapping itself -- the same reason
    /// `poll_in_flight` exists for the wallet's 10-second poll.
    stats_in_flight: bool,
    supply_in_flight: bool,
    /// When each was last ASKED for (not answered), so `ConsoleTick` can
    /// tell whether its 10s/60s cadences are due without a second
    /// subscription per cadence.
    last_stats_at: Option<std::time::Instant>,
    last_supply_at: Option<std::time::Instant>,
    /// Same cadence tracker, for the data directory's size -- kept SEPARATE
    /// from `last_stats_at` rather than reusing it, because the two are due
    /// on independent conditions: `/stats` only fires while `Running`, the
    /// disk walk runs whenever `Owned` regardless of `Running`. Sharing one
    /// timestamp would make the disk walk inherit `/stats`'s gate (freezing
    /// DISK while stopped, the defect this field exists to avoid) or make
    /// `/stats` inherit the disk walk's un-gated cadence (issuing a stats
    /// request that will only fail while the process is down).
    last_disk_at: Option<std::time::Instant>,
    /// The stats port the currently running node was actually started with,
    /// frozen at `ensure_owned_node` time -- the same treatment `node_url`
    /// already gets from the explorer port. `ConsoleTick` polls THIS, not a
    /// live reparse of `stats_port_input`: a port typed on F7 is captioned
    /// "takes effect at the next node start", so the poll must not retarget
    /// itself the moment someone types, only when a node actually starts
    /// with the new value.
    node_stats_url: String,
    /// `/explorer/status`, refreshed by `ConsoleTick` on the same 10s cadence
    /// as `/stats` -- but only on the screens `PollTick`'s own 10s poll does
    /// NOT already cover (Wallet/Receive/Send). Without this, Node/
    /// Settings show a `wallet.node_status` frozen at whatever the wallet
    /// screen last read, while the rest of the grid (fed by `/stats`) keeps
    /// ticking -- half the strip live, half dead, with no way to tell them
    /// apart.
    console_status_in_flight: bool,
    last_console_status_at: Option<std::time::Instant>,
    /// Which node the wallet's client points at, as a generation count.
    /// `App::retarget_client` is the only thing that moves it, and it is the
    /// only thing that replaces `wallet.client` -- so "the epoch changed"
    /// means exactly "the node being read changed".
    ///
    /// Every request that reads the node captures this at issue time and
    /// echoes it back in its answer (`PollTickFetched`, `RefreshAllFetched`,
    /// `HistoryStatusFetched`, `ConsoleStatusFetched`, `StatsFetched`). The
    /// client is cloned into the async task at issue time, not re-read when
    /// the answer lands, so an answer carrying an old epoch is a real reading
    /// of a node the wallet has since stopped reading -- a source switch, a
    /// restart, a retyped URL. Applying it would put back what the switch
    /// cleared. Such an answer is dropped; only the bookkeeping the request
    /// itself set (its in-flight flag, its rows' "Updating...") is unwound.
    node_epoch: u64,
    /// The previous CPU tick sample, for `proc::cpu_percent`'s two-sample
    /// requirement. `None` in `External` mode, and reset whenever the
    /// process is not `Running` -- a sample carried across a restart would
    /// read as a nonsense delta against a different process's counters.
    prev_cpu_sample: Option<alphanumeric_gui::proc::CpuSample>,
    /// Derived process metrics, read by `console_data` into the grid.
    /// `None` in `External` mode: there is no process here to measure.
    cpu_percent: Option<f32>,
    rss_kib: Option<u64>,
    disk_bytes: Option<u64>,
    /// The owned node's data directory -- where it runs, what DISK measures,
    /// and what F6/F7 name. Resolved once in `App::new` so all four agree,
    /// and so a test can point it at a scratch directory instead of walking
    /// the real `~/.alphanumeric-gui/node` of whoever runs the suite.
    data_dir: Option<PathBuf>,
    /// The node screen's log tail. Refreshed by `ConsoleTick` only while
    /// `Screen::Node` is actually on screen -- `Supervisor::log_tail` reads
    /// the log file line by line, and doing that on every redraw (the node
    /// screen's `view()` would otherwise call it directly) would re-read a
    /// multi-megabyte file every time the mouse moves. Kept, not cleared,
    /// once the screen is left -- the same "last known good" rule
    /// `node_status_error` and `node_stats` already follow.
    node_log_tail: Vec<String>,
    /// Catch-up speed for the startup screen (`startup::SyncRate`), fed by
    /// `NodeStatusFetched`. Cleared whenever a new owned node starts.
    sync_rate: alphanumeric_gui::startup::SyncRate,
    /// Zero point for `sync_rate`'s seconds.
    sync_origin: std::time::Instant,
    /// The last node log lines `NodeStatusFetched` already read for
    /// `startup::phase` -- the startup screen shows them without its own I/O.
    startup_log: Vec<String>,
    /// Unlock shows the node settings only when asked (plan ruling 6).
    pub(crate) setup_settings_open: bool,
    /// Where this session's import set the previous wallet aside (spec G
    /// §3.1), shown on F7 as PREVIOUS WALLET.
    pub last_archive: Option<PathBuf>,
    /// Some while an import started from F7 is choosing the replacement.
    pub importing: Option<ImportContext>,
    /// First run only: wallets `replace_archiving` set aside beside the
    /// wallet path, listed once at start (planning ruling 4) for the
    /// first-run hint (spec G §3.6).
    pub archives_found: Vec<PathBuf>,
    /// Bumped every time `HistoryOpen` builds a new `HistoryState`. Copied
    /// into that state's `generation` and echoed back in
    /// `Message::HistoryPageFetched`, so a response addressed to a
    /// `HistoryState` that has since been dropped and replaced (leave the
    /// screen, come back) can be told apart from one belonging to the
    /// current state and dropped instead of corrupting it. On `App`, not the
    /// wallet, so a wallet installed by an import never reuses a generation a
    /// closed wallet's request still carries (spec G §4.6).
    history_generation: u64,
    /// Numbers every master-reveal attempt so a late answer can be told from
    /// the current one -- App-wide, so a new wallet's numbers never repeat an
    /// old one's.
    master_reveal_requests: u64,
    /// The window's current width, in logical pixels. Initialised to
    /// `main.rs`'s `window_settings` starting size and kept current by
    /// `iced::window::resize_events()` (`subscription`, `Message::WindowResized`).
    /// `narrow()` is the only reader -- every two-panel screen (`kit::split`)
    /// goes through it rather than reading this field directly.
    window_width: f32,
}

/// How many real redraws to let pass before capturing. `window::frames()`
/// only fires on `RedrawRequested`, so the very first event already
/// corresponds to a frame that has been drawn into the renderer -- but this
/// leaves a margin rather than betting the flag's only purpose on that being
/// exactly one frame on every machine.
const SCREENSHOT_MIN_FRAMES: u32 = 2;

/// How many rows one "load more" tries to add. Separate from the page size --
/// with several addresses, filling one screen's worth can take several
/// pages.
const HISTORY_PAGE_ROWS: usize = 25;

/// The pause between two explorer reads issued back-to-back by this process.
///
/// The node's token bucket refills at 10/s, caps at 30, and is shared across
/// every explorer read endpoint -- so a burst of address GETs answers 429
/// (`explorer_address_handler`) whether they come from a refresh or from the history
/// screen. `Message::RefreshAll` sleeps this long between addresses, and the
/// history fetch does the same before each page: the merge invariant makes
/// every stream answer before the first row is drawn, so opening History on a
/// 30-address wallet is exactly the burst this spaces out. One constant, both
/// callers, so the two cannot drift apart.
const ADDRESS_READ_PACING: std::time::Duration = std::time::Duration::from_millis(150);

#[derive(Clone)]
pub enum Message {
    /// Move to another screen.
    Show(Screen),
    /// The explorer API address, editable throughout setup.
    NodeUrlChanged(String),

    /// Setup: whether the wallet brings its own node or points at one
    /// already running elsewhere. Asked once, on a fresh install.
    ChooseNodeSource(NodeSource),
    /// Setup: the node binary path, `Owned` only.
    NodeBinaryChanged(String),
    /// Setup: the P2P port, `Owned` only.
    P2pPortChanged(String),
    /// Setup: the explorer port, `Owned` only.
    ExplorerPortChanged(String),
    /// Setup: the stats port, `Owned` only.
    StatsPortChanged(String),

    /// Setup: choosing between create and restore.
    StartCreate,
    StartRestore,
    BackToChoose,

    /// Setup: the backup gate.
    BackupTypedChanged(String),
    ConfirmBackup,
    /// Backup: put the master seed on the clipboard.
    BackupCopy,
    /// Backup: write the master seed to a file the user picks.
    BackupSaveToFile,
    BackupSavedToFile(Result<Option<PathBuf>, String>),
    /// Backup: the user says it is saved. Hides the seed and raises the quiz.
    BackupSavedIt,
    /// Backup: put the seed back on screen. Clears the quiz, so the next
    /// `BackupSavedIt` draws fresh positions -- otherwise someone could read
    /// which characters are asked for, go back, and read them off the display.
    BackupShowAgain,

    /// Setup: restoring, with the two inputs kept apart.
    RestoreSeedInputChanged(String),
    ChoosePhoto,
    PhotoPrepared(Result<Option<Arc<PhotoSecret>>, String>),
    UseSeedInput,
    UsePhoto,

    /// Setup: choosing a passphrase for a fresh or restored wallet.
    NewPassphraseChanged(String),
    NewPassphraseConfirmChanged(String),
    ConfirmNewPassphrase,

    /// Setup: unlocking an existing wallet file.
    UnlockPassphraseChanged(String),
    Unlock,

    /// Startup: the 1-second poll of the wallet's own node while it comes up.
    NodeTick,
    NodeStatusFetched(Result<backend::NodeStatus, backend::ApiError>),
    /// F5: the address the rewards go to.
    MiningAddressPicked(String),
    /// F5: copy the address the node is mining to.
    CopyPayout(String),
    /// F5: GPU or CPU.
    MiningBackendPicked(settings::Backend),
    /// F5: the CPU thread count as typed; blank means the node's default.
    MiningThreadsChanged(String),
    /// F5: one GPU switched on (`true`) or off by its node index.
    MiningGpuToggled(u32, bool),
    /// F5: start mining with the settings as they stand -- saves them and
    /// restarts the owned node with the mining variables.
    MiningStart,
    /// F5: stop mining -- saves `enabled: false` and restarts without them.
    MiningStop,
    /// Startup: the node failed to come up (or was stopped) and the user
    /// asked to try again.
    RetryNode,
    /// The startup screen's "Node settings": go to setup with the node
    /// settings already open.
    OpenSetupSettings,
    /// Unlock's NODE SETTINGS toggle.
    ToggleSetupSettings,

    /// A create, restore, or unlock flow finished -- either the wallet is
    /// ready, or setup failed with a message to show inline.
    SetupReady(Result<Ready, SetupFailure>),

    /// Wallet: the 10-second poll of the node status and the active address.
    PollTick,
    /// The `u64` on this and the four other node answers is `App::node_epoch`
    /// at issue time -- see that field.
    PollTickFetched(
        u64,
        Result<backend::NodeStatus, backend::ApiError>,
        Result<backend::AddressState, backend::ApiError>,
    ),
    /// Wallet: status plus every address, sequentially -- shown on screen and
    /// on manual refresh, never on the short timer (spec 4.2).
    RefreshAll,
    RefreshAllFetched(
        u64,
        Result<backend::NodeStatus, backend::ApiError>,
        Vec<Result<backend::AddressState, backend::ApiError>>,
    ),
    SetActiveAddress(usize),
    AddAddress,
    AddressStoreSaved(Result<u32, String>),

    /// F1's IMPORT AN ADDRESS panel (spec H §3.2).
    ImportKeyStart,
    ImportKeySeedChanged(String),
    /// Derive the address the pasted seed gives, without storing anything.
    ImportKeyCheck,
    /// Store the checked seed and add its row.
    ImportKeyAdd,
    ImportKeyCancel,

    /// F1's REMOVE AN IMPORTED ADDRESS panel (spec H §3.6).
    RemoveKeyStart(usize),
    RemoveKeyTypedChanged(String),
    RemoveKeyConfirm,
    RemoveKeyCancel,

    /// Receive: copy the active address to the clipboard.
    CopyAddress,

    /// Send: copy a settled payment's transaction id to the clipboard (the
    /// RESULT panel's COPY). Read from the verdict on screen, not carried in
    /// the message, so only an id that is actually shown can be copied.
    CopySentTxId,

    /// Receive: the seed-reveal gate. `RevealSeedStart` only raises the warning
    /// and the passphrase field -- nothing is derived until `RevealSeedConfirm`,
    /// and nothing is shown unless the passphrase opens the wallet file.
    RevealSeedStart,
    RevealPassphraseChanged(String),
    RevealSeedConfirm,
    RevealSeedRevealed(Result<Revealed, String>),
    RevealSeedCancel,
    /// Receive: copy the revealed seed, then overwrite the clipboard.
    CopySeed,
    /// Settings: save a copy of the encrypted wallet file (spec G §3.2).
    ExportWalletFile,
    ExportWalletFileDone(Result<Option<PathBuf>, String>),
    /// Settings: the master-seed reveal gate (spec G §3.3).
    MasterRevealStart,
    MasterRevealPassphraseChanged(String),
    MasterRevealConfirm,
    /// The `u64` is the request number `MasterRevealConfirm` issued.
    MasterRevealDone(u64, Result<MasterSeedText, String>),
    MasterRevealHide,
    CopyMasterSeed,
    ClearClipboard,
    /// Settings: IMPORT WALLET -- raise the confirm panel (spec G §3.4).
    ImportStart,
    ImportCancelConfirm,
    /// Close the open wallet and choose its replacement.
    ImportContinue,
    /// Import mode: give up and go back to unlocking the wallet that was open.
    ImportAbort,

    /// Setup: start importing a wallet file (spec G §3.5).
    StartImportFile,
    /// Setup: choose the file to import.
    ImportPickFile,
    /// Setup: the file dialog's answer, or `None` if it was cancelled.
    ImportFilePicked(Option<PathBuf>),
    /// Setup: editing the passphrase of the file being imported.
    ImportPassphraseChanged(String),
    /// Setup: open the chosen file with the typed passphrase.
    ImportOpen,
    /// Setup: the opened file together with the passphrase that opened it,
    /// or why it did not open.
    ImportOpened(Result<(Arc<alphanumeric_gui::import::WalletFile>, Zeroizing<String>), String>),
    /// Setup: write the opened file in place of the wallet's own.
    ImportUse,

    /// History: enter the screen, building the merge from scratch.
    HistoryOpen,
    /// History: "load more".
    HistoryMore,
    /// History: one address's page has come back. The `u64` is the
    /// `HistoryState` generation the request was made under -- see
    /// `App::history_generation`. The `Option<Cursor>` is the cursor
    /// the page was ASKED for, echoed back so `Merge::accept` can drop
    /// anything at or above it; without it the merger cannot tell a correct
    /// page from one a node re-served.
    HistoryPageFetched(
        usize,
        u64,
        Option<backend::Cursor>,
        Result<backend::AddressPage, backend::ApiError>,
    ),
    /// History: the chain height, re-read on entering the screen. The
    /// staleness banner compares the index height against it; `ConsoleTick`
    /// also keeps this refreshed every 10s while the screen stays open (the
    /// same `wallet.node_status` its own `/explorer/status` poll now covers
    /// on Node/Settings/History alike), but there is no reason to
    /// make someone who just opened the screen wait out that cadence --
    /// without this immediate read the comparison would start from whatever
    /// the height was when some other screen last looked.
    HistoryStatusFetched(u64, Result<backend::NodeStatus, backend::ApiError>),

    /// Send: composing.
    SendRecipientChanged(String),
    SendAmountChanged(String),
    /// Send: read `/fee-estimate`, and the node's clock with it.
    SendFeeRefresh,
    SendFeeFetched(Result<(backend::FeeEstimate, Option<i64>), backend::ApiError>),
    /// Send: move a validated payment to the confirmation screen. Nothing is
    /// signed by this.
    SendReview,
    SendBackToCompose,
    /// Send: approve a prepared payment. Fetches the sender's spendable
    /// balance fresh; signs nothing by itself.
    SendConfirm,
    /// Send: that fetch came back. The ONLY message that produces a signature,
    /// and only after this answer clears the ceiling.
    SendSpendableChecked(Result<backend::AddressState, backend::ApiError>),
    /// Send: re-post the identical outstanding body -- never a fresh one.
    SendRetry,
    SendSubmitted(Result<String, backend::ApiError>),
    /// Send: discard the screen's state and start a new payment.
    SendClear,

    /// Developer flag only (`--screenshot <dir>`): capture the current screen
    /// to a PNG. Never reachable from any widget -- it is emitted by the
    /// `window::frames()` subscription the flag installs, so the normal run
    /// has no path to it. Landing a few times before the actual capture is
    /// dispatched is expected (see the handler); each landing is a real
    /// redraw, not a guess at one.
    CaptureScreenshot,
    /// The compositor handed back pixels. Carries the raw RGBA rather than a
    /// path so the write happens in one place.
    ScreenshotReady(iced::window::Screenshot),

    /// The console strip's tick. One message for all three of its cadences
    /// (proc metrics 2s, `/stats` + disk 10s, supply 60s) rather than three
    /// subscriptions -- three would wake on three unrelated phases and
    /// interleave their requests. `update` decides internally what is due.
    ConsoleTick,
    StatsFetched(u64, Result<backend::NodeStats, backend::ApiError>),
    SupplyFetched(Result<backend::Supply, backend::ApiError>),
    /// `ConsoleTick`'s own `/explorer/status` read -- the one that covers
    /// Node/Settings/History, where `PollTick` does not. See
    /// `App::console_status_in_flight`. The `u64` is `App::node_epoch` at
    /// issue time, like every other node answer.
    ConsoleStatusFetched(u64, Result<backend::NodeStatus, backend::ApiError>),

    /// The window was resized, to this new width in logical pixels
    /// (`iced::window::resize_events()`). Drives `App::narrow()`, which
    /// every two-panel screen reads via `kit::split` (R1).
    WindowResized(f32),

    /// F10, or the tab bar's own Quit button: ask the runtime to exit.
    /// Closing the window would do the same thing -- `Supervisor`'s `Drop`
    /// SIGTERMs the owned child either way -- this just gives the keyboard-
    /// only flow the reference wallet's bottom bar promises a way to do it
    /// without reaching for the mouse.
    Quit,
}

/// Serialise `next_index` and the imported list into the payload
/// `storage::save` seals -- `Zeroizing` because the buffer holds every
/// imported seed in plain hex, and a plain `Vec<u8>` here would leave that
/// copy sitting in freed memory once the caller is done with it.
fn keystore_payload(
    next_index: u32,
    imported: &[Zeroizing<String>],
) -> Result<Zeroizing<Vec<u8>>, String> {
    let imported = imported
        .iter()
        .map(|seed| storage::ImportedKey {
            seed: seed.to_string(),
            added: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        })
        .collect();
    serde_json::to_vec(&WalletMetadata {
        next_index,
        imported,
    })
    .map(Zeroizing::new)
    .map_err(|error| error.to_string())
}

/// Re-seal the wallet file for a new derived address: `next_index` moves
/// forward and every imported key already in the wallet is carried along
/// untouched. A free function, not `App::write_keys`, because `AddAddress`
/// dispatches this through `Task::perform` and cannot hold a borrow of
/// `self` across the `.await`.
async fn save_after_add_address(
    path: PathBuf,
    master: MasterSeed,
    passphrase: Zeroizing<String>,
    updated_next: u32,
    imported: Vec<Zeroizing<String>>,
) -> Result<u32, String> {
    let metadata = keystore_payload(updated_next, &imported)?;
    tokio::task::spawn_blocking(move || {
        storage::save(&path, &master, &metadata, passphrase.as_bytes())
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(updated_next)
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let wallet_path = storage::default_path();
        let setup = match &wallet_path {
            Some(path) if storage::exists(path) => SetupStage::Unlock {
                passphrase: Zeroizing::new(String::new()),
                error: None,
                busy: false,
            },
            _ => SetupStage::Choose,
        };
        // A revisit (a wallet file already exists) already chose a node
        // source once, so the owned node starts right away. A fresh install
        // (`SetupStage::Choose`) waits for Task 8's picker instead -- starting
        // here would pull the snapshot down before anyone was ever asked
        // (spec section 3).
        //
        // `!cfg!(test)` is load-bearing, not defensive dressing: roughly
        // thirty tests call `App::new()`, and the only reason none of them
        // fork a node today is that no file named `alphanumeric` happens to
        // sit next to the test binary in `target/debug/deps/`. The moment one
        // does, `cargo test` would run `reclaim_orphan` against the real
        // `~/.alphanumeric-gui/node/.alphanumeric.instance.lock` and SIGTERM
        // the wallet node the person running the tests is actually using.
        // Tests that need the startup screen set `screen`/`node_phase`
        // themselves.
        let start_node = !cfg!(test) && matches!(setup, SetupStage::Unlock { .. });
        let archives_found = match (&setup, &wallet_path) {
            (SetupStage::Choose, Some(path)) => storage::find_archives(path),
            _ => Vec::new(),
        };
        let mut app = Self {
            screen: Screen::Setup,
            node_url: DEFAULT_NODE_URL.to_string(),
            wallet_path,
            setup,
            wallet: None,
            node_source: NodeSource::Owned,
            supervisor: None,
            node_phase: startup::Phase::Starting {
                last_line: None,
                percent: None,
            },
            startup_status: None,
            startup_poll_in_flight: false,
            node_binary_input: String::new(),
            p2p_port_input: String::new(),
            explorer_port_input: String::new(),
            stats_port_input: String::new(),
            screenshot_dir: SCREENSHOT_DIR.get().cloned(),
            screenshot_taken: false,
            screenshot_frames_seen: 0,
            settings: settings::Settings::default(),
            // `None` under test for the same reason `start_node` is false
            // there: a test must neither read nor overwrite the settings of
            // the wallet the person running it actually uses. Tests that
            // exercise saving set a temporary path themselves.
            settings_path: if cfg!(test) {
                None
            } else {
                settings::default_path()
            },
            settings_error: None,
            known_gpus: Vec::new(),
            launched_mining: None,
            mining_error: None,
            mining_threads_input: String::new(),
            node_stats: None,
            supply: None,
            stats_in_flight: false,
            supply_in_flight: false,
            last_stats_at: None,
            last_supply_at: None,
            last_disk_at: None,
            node_stats_url: node::stats_url(node::DEFAULT_STATS_PORT),
            console_status_in_flight: false,
            last_console_status_at: None,
            node_epoch: 0,
            prev_cpu_sample: None,
            cpu_percent: None,
            rss_kib: None,
            disk_bytes: None,
            data_dir: node::default_data_dir(),
            node_log_tail: Vec::new(),
            sync_rate: Default::default(),
            sync_origin: std::time::Instant::now(),
            startup_log: Vec::new(),
            setup_settings_open: false,
            last_archive: None,
            importing: None,
            archives_found,
            history_generation: 0,
            master_reveal_requests: 0,
            window_width: 1000.0,
        };
        // Before the node starts: its binary, ports and mining come from here.
        let loaded = app
            .settings_path
            .as_deref()
            .map(settings::load)
            .unwrap_or_default();
        app.apply_loaded_settings(loaded);
        if start_node {
            app.ensure_owned_node();
            app.screen = Screen::Startup;
        }
        (app, Task::none())
    }

    /// Seeds the node inputs from the file and keeps the rest.
    pub fn apply_loaded_settings(&mut self, loaded: settings::Settings) {
        self.node_binary_input = loaded.node.binary.clone().unwrap_or_default();
        let port = |p: Option<u16>| p.map(|p| p.to_string()).unwrap_or_default();
        self.p2p_port_input = port(loaded.node.p2p_port);
        self.explorer_port_input = port(loaded.node.explorer_port);
        self.stats_port_input = port(loaded.node.stats_port);
        self.mining_threads_input = loaded
            .mining
            .cpu_threads
            .map(|n| n.to_string())
            .unwrap_or_default();
        self.settings = loaded;
    }

    /// An edit on F5 that the running node has not been started with.
    pub fn mining_dirty(&self) -> bool {
        self.mining_launch() != self.launched_mining
    }

    /// Every address of the wallet as an F5 drop-down entry: own and
    /// imported, labelled the way F1 labels them, shortened to one line.
    pub fn mining_address_choices(&self) -> Vec<PayoutChoice> {
        self.wallet
            .as_ref()
            .map(|w| {
                w.addresses
                    .iter()
                    .map(|e| PayoutChoice {
                        label: format!(
                            "[{}] {}",
                            e.label(),
                            crate::view::kit::short_address(&e.address)
                        ),
                        address: e.address.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The drop-down's selection: the saved payout address, if this wallet
    /// holds it. A saved address the wallet does not hold selects nothing,
    /// so START asks for a choice rather than mining to a stranger.
    pub fn mining_selected_choice(&self) -> Option<PayoutChoice> {
        let saved = self.settings.mining.address.as_deref()?;
        self.mining_address_choices()
            .into_iter()
            .find(|c| c.address == saved)
    }

    /// `[0]`, `[IMP]`: how the miner panel names a payout address this
    /// wallet holds. `None` for any other address.
    pub fn payout_label(&self, address: &str) -> Option<String> {
        self.wallet
            .as_ref()?
            .addresses
            .iter()
            .find(|e| e.address == address)
            .map(|e| format!("[{}]", e.label()))
    }

    /// START and STOP both end here: remember the choice, then restart the
    /// owned node so it picks the mining variables up (or drops them). A
    /// node the wallet merely points at cannot be told to mine.
    fn apply_mining(&mut self) -> Task<Message> {
        self.save_settings();
        if self.node_source != NodeSource::Owned {
            self.mining_error =
                Some("Mining needs the wallet's own node; this wallet points at another.".into());
            return Task::none();
        }
        self.update(Message::RetryNode)
    }

    /// Writes the settings file from the inputs as they stand (an empty
    /// input is "the default", stored as `None`). A failure is remembered
    /// in `settings_error`, never fatal.
    pub fn save_settings(&mut self) {
        let text = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let port = |s: &str| s.trim().parse::<u16>().ok();
        self.settings.node = settings::NodeSettings {
            binary: text(&self.node_binary_input),
            p2p_port: port(&self.p2p_port_input),
            explorer_port: port(&self.explorer_port_input),
            stats_port: port(&self.stats_port_input),
        };
        self.settings_error = match &self.settings_path {
            Some(path) => settings::save(path, &self.settings).err(),
            None => Some("No home directory to keep settings in.".to_string()),
        };
    }

    /// The mining the node is launched with: `None` unless mining is on and
    /// an address is chosen. The GPU list is sent only when some reported
    /// GPU is switched off; otherwise the node uses every usable card.
    pub fn mining_launch(&self) -> Option<node::MiningLaunch> {
        let m = &self.settings.mining;
        if !m.enabled {
            return None;
        }
        let address = m.address.clone()?;
        let gpu_devices = match m.backend {
            settings::Backend::Gpu if !m.disabled_gpus.is_empty() => {
                let kept: Vec<u32> = self
                    .known_gpus
                    .iter()
                    .map(|g| g.index)
                    .filter(|i| !m.disabled_gpus.contains(i))
                    .collect();
                (!kept.is_empty()).then_some(kept)
            }
            _ => None,
        };
        Some(node::MiningLaunch {
            address,
            backend: m.backend,
            cpu_threads: m.cpu_threads,
            gpu_devices,
        })
    }

    /// Points the wallet's client at `self.node_url`, and moves `node_epoch`
    /// so every answer still in flight from the previous client is
    /// recognised as describing a node the wallet no longer reads. The only
    /// place `wallet.client` is replaced -- keeping the two together is what
    /// makes the epoch mean anything.
    ///
    /// The epoch moves even when there is no wallet yet, or `node_url` does
    /// not parse (mid-edit): in both cases whatever was in flight was asked
    /// of something other than what the wallet will read next.
    fn retarget_client(&mut self) {
        self.node_epoch = self.node_epoch.wrapping_add(1);
        if let Some(wallet) = &mut self.wallet {
            if let Ok(client) = backend::Client::new(&self.node_url) {
                wallet.client = Arc::new(client);
            }
        }
    }

    /// Starts (or restarts) the wallet's owned node and switches to the
    /// Starts (or restarts) the wallet's owned node. Does NOT touch
    /// `self.screen` -- callers decide separately whether the user should be
    /// taken to the startup screen or left where they are (Major 2): a fresh
    /// install choosing "Run its own node" mid-setup should see the node warm
    /// up behind it, not get its screen taken over, while `App::new` on a
    /// revisit and `Message::RetryNode` both do want the takeover and set
    /// `screen = Screen::Startup` themselves right after calling this.
    ///
    /// Resets `node_url`, `startup_status` and `node_phase` together, because
    /// a retry that leaves any one of them stale either polls the node that
    /// just failed, or jumps ahead on a reading that belonged to it.
    fn ensure_owned_node(&mut self) {
        // What this start is asked to mine, recorded before the attempt so
        // F5 can tell an edit from what the node was given.
        self.launched_mining = self.mining_launch();
        // Dropped here, before the new one is built below, so an old and a
        // new child's lifetimes never overlap -- two processes racing for
        // the same port. `Message::RetryNode` also drops the supervisor
        // before calling this, which is now redundant with this line but
        // still correct: whichever runs, the drop happens before the new
        // supervisor is constructed.
        self.supervisor = None;
        self.node_source = NodeSource::Owned;
        let (p2p_port, explorer_port, stats_port) = self.resolved_ports();
        self.node_url = node::explorer_url(explorer_port);
        // Frozen here, at start time, the same way `node_url` is -- not
        // reparsed from `stats_port_input` on every tick. See the field's
        // own doc comment.
        self.node_stats_url = node::stats_url(stats_port);
        self.startup_status = None;
        self.sync_rate.clear();
        // A CPU sample taken from the OLD pid must never be compared
        // against one from the new child a restart just started -- that
        // would read as a real delta between two unrelated processes'
        // counters. `ConsoleTick` only clears this when it OBSERVES a
        // non-`Running` state, which a restart that reaches `Running`
        // again inside one 2s tick window would skip past.
        self.prev_cpu_sample = None;
        // The same argument applies to `node_stats`: the supervisor above
        // was just dropped (SIGTERM) and a new one is about to be built, so
        // whatever `/stats` last answered describes a process that is
        // either gone or about to be replaced. Left alone, a restart
        // (`Message::RetryNode`, or the wallet screen's own "APPLY AND
        // RESTART NODE") could show the OLD run's reading for up to one
        // `ConsoleTick` cadence after the new child starts, before that tick
        // corrects it -- the same one-tick gap `prev_cpu_sample`'s comment
        // above already accepts, made no worse, but no reason to leave this
        // one uncleared when it can be exact instead.
        self.node_stats = None;
        // `Message::NodeUrlChanged`'s own comment states the invariant this
        // must also uphold: otherwise every fetch keeps hitting the OLD
        // address no matter what `node_url` now says. Before this wave
        // `ensure_owned_node` (as `start_owned_node`) could only run before a
        // wallet existed, so there was no client to go stale. Now
        // `Message::RetryNode` is reachable from the wallet screen's Owned
        // banner -- a port changed there and applied via "APPLY AND RESTART
        // NODE" must retarget the client that screen already has, not
        // leave it pointed at the port that just failed.
        self.retarget_client();
        match build_node_config(
            self.configured_binary().as_deref(),
            self.data_dir.as_deref(),
            p2p_port,
            explorer_port,
            stats_port,
        ) {
            Ok(mut config) => {
                config.mining = self.mining_launch();
                self.launched_mining = config.mining.clone();
                // Called directly, never from a `Task::perform` -- the
                // supervisor spawns the child from a dedicated OS thread on
                // purpose, so `PR_SET_PDEATHSIG` fires on the death of the
                // thread that forked it, not on some unrelated tokio worker.
                self.supervisor = Some(node::Supervisor::start(config));
                self.node_phase = startup::Phase::Starting {
                    last_line: None,
                    percent: None,
                };
            }
            Err(message) => {
                self.supervisor = None;
                self.node_phase = startup::Phase::Failed { message };
            }
        }
    }

    /// The ports the node is started with. The input strings are kept exactly
    /// as typed (see `Message::P2pPortChanged`); parsing happens only here,
    /// at the moment they are needed, and an input that will not parse falls
    /// back to the default rather than blocking the node from starting.
    pub fn resolved_ports(&self) -> (u16, u16, u16) {
        (
            self.p2p_port_input
                .trim()
                .parse()
                .unwrap_or(node::DEFAULT_P2P_PORT),
            self.explorer_port_input
                .trim()
                .parse()
                .unwrap_or(node::DEFAULT_EXPLORER_PORT),
            self.stats_port_input
                .trim()
                .parse()
                .unwrap_or(node::DEFAULT_STATS_PORT),
        )
    }

    /// The configured node binary, if the user set one. `None` -- including a
    /// blank or whitespace-only field -- means "unconfigured", not "empty
    /// path": `node::locate_binary` takes that as license to look next to the
    /// wallet's own executable (Task 1).
    pub fn configured_binary(&self) -> Option<PathBuf> {
        let trimmed = self.node_binary_input.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }

    /// What to show for the node binary before it is actually located --
    /// the configured path if there is one, otherwise where an unconfigured
    /// one would be looked for. A guess for display only; `build_node_config`
    /// (via `node::locate_binary`) is what actually resolves it and can still
    /// fail if nothing is there.
    pub fn node_binary_display(&self) -> String {
        match self.configured_binary() {
            Some(path) => path.display().to_string(),
            None => {
                let name = node::binary_file_name();
                exe_dir()
                    .map(|dir| dir.join(&name).display().to_string())
                    .unwrap_or_else(|| format!("{name} (next to the wallet)"))
            }
        }
    }

    /// The owned node process's current state. `None` in `External` mode --
    /// there is no supervisor there -- and also in `Owned` mode when
    /// `ensure_owned_node`'s config step itself failed (no binary, no home
    /// directory) before a `Supervisor` was ever built; `view::node`
    /// distinguishes the two by `node_source`, since both read `None` here.
    pub fn node_state(&self) -> Option<node::NodeState> {
        self.supervisor.as_ref().map(node::Supervisor::state)
    }

    /// The reason `Owned` mode has no supervisor at all, when there is one --
    /// `ensure_owned_node` records that failure as `node_phase`, the same
    /// place the startup screen reads it from.
    pub fn node_start_error(&self) -> Option<&str> {
        match &self.node_phase {
            startup::Phase::Failed { message } => Some(message.as_str()),
            _ => None,
        }
    }

    /// The node screen's log tail, last refreshed by `ConsoleTick` while that
    /// screen was on top. A field read, not I/O -- see the field's own doc
    /// comment for why the read itself does not belong in `view()`.
    pub fn node_log_tail(&self) -> &[String] {
        &self.node_log_tail
    }

    /// The CPU gauge's share of the whole machine (the grid and F6 draw it).
    /// `None` in `External` mode, or whenever the process is not `Running`
    /// -- see `proc::cpu_percent`'s own two-sample requirement.
    pub fn cpu_share(&self) -> Option<f32> {
        self.cpu_percent
            .and_then(alphanumeric_gui::proc::core_share)
    }

    /// How far the memory gauge is drawn: resident memory against
    /// `proc::RSS_FULL_KIB`. The figure beside it is the real one.
    pub fn mem_share(&self) -> Option<f32> {
        alphanumeric_gui::proc::rss_share(self.rss_kib)
    }

    /// The owned process's resident memory. Same `None` rule as
    /// `cpu_share`.
    pub fn process_rss_kib(&self) -> Option<u64> {
        self.rss_kib
    }

    /// The owned data directory's size on disk. `None` in `External` mode --
    /// this wallet manages no data directory there.
    pub fn process_disk_bytes(&self) -> Option<u64> {
        self.disk_bytes
    }

    /// See the field. `None` only when there is no home directory.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// The last-read `/stats`, including `uptime_secs` -- only fetched in
    /// `Owned` mode while the supervisor reports `Running`
    /// (`Message::ConsoleTick`), kept (not cleared) on a failed refetch, and
    /// cleared as soon as `ConsoleTick` (or a source change, or a restart)
    /// observes the process is no longer the one it described -- see
    /// `node_stats`'s own field doc for the exact list. That clearing is not
    /// instantaneous with the process actually stopping, so callers that
    /// must not show a dead or just-restarted process's PREVIOUS reading
    /// still gate on `node_state()` being `Running` themselves
    /// (`view::node`'s `uptime_line` is the one that does).
    pub fn node_stats(&self) -> Option<&backend::NodeStats> {
        self.node_stats.as_ref()
    }

    /// Below `kit::NARROW_BELOW`, a two-panel screen stacks its panels
    /// (`kit::split`) instead of sitting them side by side -- at the 760 px
    /// minimum window width, a ~330 px panel wraps table cells mid-number
    /// (R1).
    pub fn narrow(&self) -> bool {
        self.window_width < crate::view::kit::NARROW_BELOW
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Show(screen) => {
                // Every tab needs a wallet to show something for -- there is
                // no address to send from and no node status to describe
                // before one exists. `Setup` and `Startup` are the two
                // screens a walletless session can still be on, so they are
                // the only ones exempt.
                if self.wallet.is_none() && !matches!(screen, Screen::Setup | Screen::Startup) {
                    return Task::none();
                }
                self.screen = screen;
                // A revealed seed never survives a screen change. Before the
                // per-screen arms, so their early returns cannot skip it.
                if let Some(wallet) = &mut self.wallet {
                    wallet.reveal = RevealState::Idle;
                    wallet.master_reveal = MasterRevealState::Idle;
                    wallet.export_result = None;
                    wallet.import_key = ImportKeyState::Idle;
                    wallet.removing_key = None;
                    // History too. Keeping it would show a stale list on the
                    // way back in, complete-looking and missing every block
                    // mined in between. `HistoryOpen` changes the screen
                    // itself rather than going through `Show`, so this
                    // unconditional drop cannot erase a history just opened.
                    wallet.history = None;
                }
                match screen {
                    // "Shown on screen" is one of the two times a non-active
                    // address gets polled (spec 4.2); the other is manual
                    // refresh, which sends the same message.
                    Screen::Wallet => return self.update(Message::RefreshAll),
                    // The fee recommendation is priced off the node's live
                    // mempool, so it is re-read on entry rather than carried
                    // over from whenever this screen was last open. Nothing
                    // about the screen's state is reset here: a payment whose
                    // outcome is unknown must survive navigating away.
                    Screen::Send => return self.update(Message::SendFeeRefresh),
                    // `wallet.history` was just unconditionally cleared above;
                    // `HistoryOpen` rebuilds it, so the reset stays in one
                    // place rather than duplicated here.
                    Screen::History => return self.update(Message::HistoryOpen),
                    _ => {}
                }
                Task::none()
            }
            Message::NodeUrlChanged(value) => {
                self.node_url = value;
                // If a wallet already exists -- most importantly, if the user
                // is correcting the address from the wallet screen's own "no
                // node reachable" banner -- rebuild its client immediately.
                // Otherwise every fetch would keep hitting the OLD address no
                // matter what the field now shows. `Client::new` is pure
                // string validation; an unparsable value mid-edit just leaves
                // the previous client in place until a valid one replaces it.
                self.retarget_client();
                Task::none()
            }

            Message::ChooseNodeSource(source) => {
                self.node_source = source;
                // A `/stats` or `/explorer/status` reading describes whatever
                // node answered it, and the node this wallet talks to is
                // about to change (or its own owned process is about to be
                // SIGTERMed). Keeping either would show PEERS, HASHRATE,
                // MEMPOOL, DIFFICULTY, BLOCK REWARD and AVG BLOCK still
                // asserting a measurement of a node that is no longer the one
                // in front of the user -- unlike a transient refetch failure,
                // this is not "possibly stale", it is known wrong.
                self.node_stats = None;
                if let Some(wallet) = &mut self.wallet {
                    wallet.node_status = None;
                    wallet.node_status_error = None;
                }
                // Requests already in flight were issued against the OLD
                // client and would put back what the clear above removed the
                // moment they land. Both arms below retarget the client
                // (`ensure_owned_node` for Owned, directly for External), and
                // `retarget_client` moves `node_epoch`, which is what makes
                // those answers recognisable as stale.
                match source {
                    NodeSource::Owned => {
                        // Deliberately does NOT take the screen (Major 2):
                        // the user is mid-setup, and the node can warm up
                        // behind them rather than yanking them to the
                        // startup screen for a choice they just made.
                        self.ensure_owned_node();
                    }
                    NodeSource::External => {
                        // Assigning `None` drops the supervisor, and its
                        // `Drop` is what SIGTERMs the child (see the field's
                        // doc comment) -- `stop()` is never called before
                        // letting it drop.
                        self.supervisor = None;
                        self.node_url = DEFAULT_NODE_URL.to_string();
                        self.node_phase = startup::Phase::Ready;
                        // Same invariant as `ensure_owned_node`: a wallet
                        // already loaded (reachable here since `Setup` now
                        // has a way back to `Wallet`, see `Message::Show`)
                        // must have its client retargeted too, or it keeps
                        // fetching through whatever address it had before.
                        self.retarget_client();
                    }
                }
                Task::none()
            }
            Message::NodeBinaryChanged(value) => {
                self.node_binary_input = value;
                Task::none()
            }
            Message::P2pPortChanged(value) => {
                self.p2p_port_input = value;
                Task::none()
            }
            Message::ExplorerPortChanged(value) => {
                self.explorer_port_input = value;
                Task::none()
            }
            Message::StatsPortChanged(value) => {
                self.stats_port_input = value;
                Task::none()
            }

            Message::StartCreate => {
                // A fresh install's `node_source` is already `Owned` by
                // default (the picker on `Choose` renders it as chosen), but
                // unless the user actually pressed the Owned button nothing
                // has started one yet. Left unguarded, `node_url` stays at
                // its default -- which on this machine is the mining node's
                // own port -- and the wallet screen that follows would
                // silently poll a node nobody chose (Major 2).
                if self.node_source == NodeSource::Owned && self.supervisor.is_none() {
                    self.ensure_owned_node();
                }
                let master = MasterSeed::random();
                let shown = master.encode();
                self.setup = SetupStage::ConfirmBackup {
                    master,
                    shown,
                    quiz: None,
                    saved_to: None,
                    save_error: None,
                };
                Task::none()
            }
            Message::StartRestore => {
                // Same reasoning as `StartCreate`: a restore that scans
                // addresses off a node the user never chose is worse than
                // silent, it is wrong. `ensure_owned_node` here means the
                // scan a few steps later in `ConfirmNewPassphrase` hits the
                // wallet's own node instead.
                if self.node_source == NodeSource::Owned && self.supervisor.is_none() {
                    self.ensure_owned_node();
                }
                self.setup = SetupStage::Restore {
                    seed_input: Zeroizing::new(String::new()),
                    seed_error: None,
                    photo: None,
                    photo_busy: false,
                    photo_error: None,
                };
                Task::none()
            }
            Message::BackToChoose => {
                // M6: the same defense in depth as `ImportAbort` -- a write
                // already dispatched cannot be un-dispatched, so BACK must
                // not throw the stage away while OPEN or USE is still
                // running underneath it.
                if self.setup_writing() {
                    return Task::none();
                }
                self.setup = SetupStage::Choose;
                Task::none()
            }

            Message::BackupTypedChanged(value) => {
                if let SetupStage::ConfirmBackup {
                    quiz: Some(quiz), ..
                } = &mut self.setup
                {
                    *quiz.typed = value;
                    quiz.mismatch = false;
                }
                Task::none()
            }

            Message::BackupCopy => {
                let SetupStage::ConfirmBackup { shown, quiz, .. } = &self.setup else {
                    return Task::none();
                };
                // Only while the seed is still on screen. Once the quiz is up
                // the seed is meant to be gone, and handing it back through the
                // clipboard would answer the quiz for them.
                if quiz.is_some() {
                    return Task::none();
                }
                iced::clipboard::write(shown.to_string())
            }
            Message::BackupSaveToFile => {
                let SetupStage::ConfirmBackup { shown, quiz, .. } = &mut self.setup else {
                    return Task::none();
                };
                if quiz.is_some() {
                    return Task::none();
                }
                let seed = shown.clone();
                Task::perform(
                    async move {
                        let chosen = rfd::AsyncFileDialog::new()
                            .set_title("Save your master seed")
                            .set_file_name("alphanumeric-master-seed.txt")
                            .save_file()
                            .await;
                        let Some(handle) = chosen else {
                            return Ok(None);
                        };
                        let path = handle.path().to_path_buf();
                        let write_path = path.clone();
                        tokio::task::spawn_blocking(move || {
                            alphanumeric_gui::storage::write_seed_backup(&write_path, &seed)
                        })
                        .await
                        .map_err(|error| format!("Save seed: {error}"))??;
                        Ok(Some(path))
                    },
                    Message::BackupSavedToFile,
                )
            }
            Message::BackupSavedToFile(result) => {
                if let SetupStage::ConfirmBackup {
                    saved_to,
                    save_error,
                    ..
                } = &mut self.setup
                {
                    match result {
                        Ok(Some(path)) => {
                            *saved_to = Some(path);
                            *save_error = None;
                        }
                        // The dialog was dismissed. Not an error, and not a save.
                        Ok(None) => {}
                        Err(message) => {
                            *save_error = Some(message);
                            *saved_to = None;
                        }
                    }
                }
                Task::none()
            }
            Message::BackupShowAgain => {
                if let SetupStage::ConfirmBackup { quiz, .. } = &mut self.setup {
                    *quiz = None;
                }
                Task::none()
            }
            Message::BackupSavedIt => {
                if let SetupStage::ConfirmBackup { shown, quiz, .. } = &mut self.setup {
                    if quiz.is_none() {
                        let mut rng = rand::thread_rng();
                        *quiz = Some(BackupQuiz {
                            positions: backup_quiz_positions(shown.chars().count(), &mut rng),
                            typed: Zeroizing::new(String::new()),
                            mismatch: false,
                        });
                    }
                }
                Task::none()
            }

            Message::ConfirmBackup => {
                let confirmed = match &mut self.setup {
                    SetupStage::ConfirmBackup {
                        shown,
                        quiz: Some(quiz),
                        ..
                    } => {
                        let ok = backup_quiz_passed(shown, &quiz.positions, &quiz.typed);
                        quiz.mismatch = !ok;
                        ok
                    }
                    // No quiz yet means the seed is still on screen: there is
                    // nothing to confirm, and the button that sends this is not
                    // offered in that state.
                    _ => false,
                };
                if confirmed {
                    if let SetupStage::ConfirmBackup { master, .. } = &self.setup {
                        let master = master.clone();
                        self.setup = SetupStage::SetPassphrase {
                            pending: PendingWallet::Fresh(master),
                            passphrase: Zeroizing::new(String::new()),
                            confirm: Zeroizing::new(String::new()),
                            busy: false,
                            error: None,
                            node_unreachable: None,
                        };
                    }
                }
                Task::none()
            }

            Message::RestoreSeedInputChanged(value) => {
                if let SetupStage::Restore {
                    seed_input,
                    seed_error,
                    ..
                } = &mut self.setup
                {
                    *seed_input = Zeroizing::new(value);
                    *seed_error = None;
                }
                Task::none()
            }
            Message::ChoosePhoto => {
                match &mut self.setup {
                    SetupStage::Restore {
                        photo_busy,
                        photo_error,
                        ..
                    } => {
                        if *photo_busy {
                            return Task::none();
                        }
                        *photo_busy = true;
                        *photo_error = None;
                    }
                    _ => return Task::none(),
                }
                Task::perform(
                    async move {
                        let selected = rfd::AsyncFileDialog::new()
                            .set_title("Choose a photo")
                            .add_filter(
                                "Photos",
                                &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff"],
                            )
                            .pick_file()
                            .await;
                        let Some(handle) = selected else {
                            return Ok(None);
                        };
                        let path = handle.path().to_path_buf();
                        tokio::task::spawn_blocking(move || {
                            alphanumeric_gui::photo::prepare_secret_photo(&path)
                        })
                        .await
                        .map_err(|error| format!("Prepare photo: {error}"))?
                        .map(Arc::new)
                        .map(Some)
                    },
                    Message::PhotoPrepared,
                )
            }
            Message::PhotoPrepared(result) => {
                if let SetupStage::Restore {
                    photo_busy,
                    photo,
                    photo_error,
                    ..
                } = &mut self.setup
                {
                    *photo_busy = false;
                    match result {
                        Ok(Some(secret)) => {
                            *photo = Some(secret);
                            *photo_error = None;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            *photo = None;
                            *photo_error = Some(error);
                        }
                    }
                }
                Task::none()
            }
            Message::UseSeedInput => {
                let decoded = match &self.setup {
                    SetupStage::Restore { seed_input, .. } => Some(MasterSeed::decode(seed_input)),
                    _ => None,
                };
                match decoded {
                    Some(Ok(master)) if self.is_the_replaced_wallet(&master) => {
                        if let SetupStage::Restore { seed_error, .. } = &mut self.setup {
                            *seed_error = Some(SAME_WALLET.into());
                        }
                    }
                    Some(Ok(master)) => {
                        self.setup = SetupStage::SetPassphrase {
                            pending: PendingWallet::Restored(master),
                            passphrase: Zeroizing::new(String::new()),
                            confirm: Zeroizing::new(String::new()),
                            busy: false,
                            error: None,
                            node_unreachable: None,
                        };
                    }
                    Some(Err(error)) => {
                        if let SetupStage::Restore { seed_error, .. } = &mut self.setup {
                            *seed_error = Some(error.to_string());
                        }
                    }
                    None => {}
                }
                Task::none()
            }
            Message::UsePhoto => {
                let master = match &self.setup {
                    SetupStage::Restore {
                        photo: Some(secret),
                        ..
                    } => Some(secret.master()),
                    _ => None,
                };
                if let Some(master) = master {
                    if self.is_the_replaced_wallet(&master) {
                        if let SetupStage::Restore { photo_error, .. } = &mut self.setup {
                            *photo_error = Some(SAME_WALLET.into());
                        }
                    } else {
                        self.setup = SetupStage::SetPassphrase {
                            pending: PendingWallet::Restored(master),
                            passphrase: Zeroizing::new(String::new()),
                            confirm: Zeroizing::new(String::new()),
                            busy: false,
                            error: None,
                            node_unreachable: None,
                        };
                    }
                }
                Task::none()
            }

            Message::NewPassphraseChanged(value) => {
                if let SetupStage::SetPassphrase {
                    passphrase, error, ..
                } = &mut self.setup
                {
                    *passphrase = Zeroizing::new(value);
                    *error = None;
                }
                Task::none()
            }
            Message::NewPassphraseConfirmChanged(value) => {
                if let SetupStage::SetPassphrase { confirm, error, .. } = &mut self.setup {
                    *confirm = Zeroizing::new(value);
                    *error = None;
                }
                Task::none()
            }
            Message::ConfirmNewPassphrase => {
                let Some(path) = self.wallet_path.clone() else {
                    if let SetupStage::SetPassphrase { error, .. } = &mut self.setup {
                        *error = Some("No home directory available to store a wallet.".into());
                    }
                    return Task::none();
                };
                // Validated HERE, synchronously, and the same string is carried
                // through to `install_wallet` rather than re-read from
                // `self.node_url` later. `Client::new` only parses the string --
                // it does no I/O -- so a value that passes here can never fail
                // to construct again downstream, which is what keeps a stage
                // from being left showing "Working..." forever.
                let node_url = self.node_url.clone();
                if let Err(message) = backend::Client::new(&node_url) {
                    if let SetupStage::SetPassphrase { error, .. } = &mut self.setup {
                        *error = Some(message);
                    }
                    return Task::none();
                }
                let (master, passphrase, is_restored) = match &mut self.setup {
                    SetupStage::SetPassphrase {
                        pending,
                        passphrase,
                        confirm,
                        busy,
                        error,
                        node_unreachable,
                    } => {
                        if passphrase.is_empty() {
                            *error = Some("Choose a passphrase.".into());
                            return Task::none();
                        }
                        if passphrase.as_str() != confirm.as_str() {
                            *error = Some("Passphrases do not match.".into());
                            return Task::none();
                        }
                        *error = None;
                        *node_unreachable = None;
                        *busy = true;
                        (
                            pending.master().clone(),
                            passphrase.clone(),
                            pending.is_restored(),
                        )
                    }
                    _ => return Task::none(),
                };

                if is_restored {
                    // Order matters here, and it is the opposite of the fresh
                    // path below: nothing is written to disk until discovery
                    // has actually run, because a restored wallet saved with a
                    // guessed `next_index` before we know the real one is how
                    // addresses quietly go missing. A fresh wallet, by
                    // contrast, has no backup other than what the user was just
                    // shown, so it is saved BEFORE the node is even asked --
                    // losing connectivity must never be able to strand the
                    // only copy of a brand new seed.
                    let stamp = archive_stamp();
                    Task::perform(
                        async move {
                            // The message is plain explanation only -- the
                            // view renders the launch command itself, always
                            // freshly derived from the CURRENT node address
                            // field, so an edit made after this failure and
                            // before Retry is what the next attempt actually
                            // uses rather than a stale string baked in here.
                            let client = backend::Client::new(&node_url).map_err(|_| {
                                SetupFailure::NodeUnreachable(
                                    "The configured node address could not be used.".into(),
                                )
                            })?;
                            let status = client.status().await.map_err(|_| {
                                SetupFailure::NodeUnreachable(
                                    "Could not reach the node to scan for used addresses.".into(),
                                )
                            })?;
                            // A scan with an unknown chain height cannot be told
                            // apart from "no addresses used" -- treating `None`
                            // as `0` would make a restored wallet's funded
                            // addresses simply not come back. Telling the user
                            // to wait and try again is the honest answer here.
                            let chain_height = status.height.ok_or_else(|| {
                                SetupFailure::NodeUnreachable(
                                    "The node is still coming up and has no chain height yet. \
                                     Try the restore again in a moment."
                                        .into(),
                                )
                            })?;
                            let next_index = discover_via_network(&client, &master, chain_height)
                                .await
                                .map_err(|error| {
                                    SetupFailure::NodeUnreachable(format!(
                                        "Could not finish scanning for used addresses: {error}"
                                    ))
                                })?;
                            // M8: through the one function that builds this
                            // payload, so a field added later cannot be added
                            // to only one of create/restore/write_keys.
                            let metadata =
                                keystore_payload(next_index, &[]).map_err(SetupFailure::Other)?;
                            let (save_master, save_path, save_pass) =
                                (master.clone(), path.clone(), passphrase.clone());
                            // Spec G §4.1: a wallet already at the path is set
                            // aside, never overwritten, and put back if the save
                            // fails.
                            let archived = tokio::task::spawn_blocking(move || {
                                storage::replace_archiving(&save_path, &stamp, |target| {
                                    storage::save(
                                        target,
                                        &save_master,
                                        &metadata,
                                        save_pass.as_bytes(),
                                    )
                                })
                            })
                            .await
                            .map_err(|error| SetupFailure::Other(error.to_string()))?
                            .map_err(SetupFailure::Other)?;
                            Ok(Ready {
                                master,
                                next_index,
                                status: Some(status),
                                passphrase,
                                node_url,
                                archived,
                                // A fresh restore: nothing has been imported yet.
                                imported: Vec::new(),
                            })
                        },
                        Message::SetupReady,
                    )
                } else {
                    let stamp = archive_stamp();
                    Task::perform(
                        async move {
                            // M8: same reasoning as the restore branch above.
                            let metadata = keystore_payload(1, &[]).map_err(SetupFailure::Other)?;
                            let (save_master, save_path, save_pass) =
                                (master.clone(), path.clone(), passphrase.clone());
                            // Spec G §4.1: a wallet already at the path is set
                            // aside, never overwritten, and put back if the save
                            // fails.
                            let archived = tokio::task::spawn_blocking(move || {
                                storage::replace_archiving(&save_path, &stamp, |target| {
                                    storage::save(
                                        target,
                                        &save_master,
                                        &metadata,
                                        save_pass.as_bytes(),
                                    )
                                })
                            })
                            .await
                            .map_err(|error| SetupFailure::Other(error.to_string()))?
                            .map_err(SetupFailure::Other)?;
                            let status = probe_node(&node_url).await;
                            Ok(Ready {
                                master,
                                next_index: 1,
                                status,
                                passphrase,
                                node_url,
                                archived,
                                // A fresh create: nothing has been imported yet.
                                imported: Vec::new(),
                            })
                        },
                        Message::SetupReady,
                    )
                }
            }

            Message::UnlockPassphraseChanged(value) => {
                if let SetupStage::Unlock {
                    passphrase, error, ..
                } = &mut self.setup
                {
                    *passphrase = Zeroizing::new(value);
                    *error = None;
                }
                Task::none()
            }
            Message::Unlock => {
                let Some(path) = self.wallet_path.clone() else {
                    if let SetupStage::Unlock { error, .. } = &mut self.setup {
                        *error = Some("No home directory available.".into());
                    }
                    return Task::none();
                };
                // Same reasoning as `ConfirmNewPassphrase`: validated now, and
                // the exact string is carried in `Ready` rather than re-read
                // from `self.node_url` after the fact.
                let node_url = self.node_url.clone();
                if let Err(message) = backend::Client::new(&node_url) {
                    if let SetupStage::Unlock { error, .. } = &mut self.setup {
                        *error = Some(message);
                    }
                    return Task::none();
                }
                let passphrase = match &mut self.setup {
                    SetupStage::Unlock {
                        passphrase,
                        busy,
                        error,
                    } => {
                        *error = None;
                        *busy = true;
                        passphrase.clone()
                    }
                    _ => return Task::none(),
                };
                Task::perform(
                    open_wallet_for_unlock(path, passphrase, node_url),
                    Message::SetupReady,
                )
            }

            Message::SetupReady(result) => {
                match result {
                    Ok(ready) => {
                        let Ready {
                            master,
                            next_index,
                            status,
                            passphrase,
                            node_url,
                            archived,
                            imported,
                        } = ready;
                        let installed = self.install_wallet(
                            master, next_index, status, passphrase, &node_url, imported,
                        );
                        match installed {
                            // "Shown on screen" (spec 4.2) includes the very
                            // first time the wallet screen appears -- without
                            // this every non-active address would sit at
                            // "..." until the user found the Refresh button.
                            Ok(()) => {
                                if archived.is_some() {
                                    self.last_archive = archived;
                                }
                                self.importing = None;
                                // M8: a finished file import leaves the
                                // master and the passphrase that opened it
                                // sitting in `preview` -- a second, unwiped
                                // copy of both once the wallet is installed
                                // and the real ones live on `WalletState`.
                                // Also drops the stale "SAVING..." a return
                                // to this stage would otherwise still show.
                                if let SetupStage::ImportFile {
                                    path,
                                    passphrase,
                                    busy,
                                    preview,
                                    ..
                                } = &mut self.setup
                                {
                                    *preview = None;
                                    *passphrase = Zeroizing::new(String::new());
                                    *busy = false;
                                    *path = None;
                                }
                                return self.update(Message::RefreshAll);
                            }
                            Err(message) => set_stage_error(&mut self.setup, message),
                        }
                    }
                    Err(SetupFailure::Other(message)) => set_stage_error(&mut self.setup, message),
                    Err(SetupFailure::NodeUnreachable(message)) => {
                        if let SetupStage::SetPassphrase {
                            busy,
                            node_unreachable,
                            ..
                        } = &mut self.setup
                        {
                            *busy = false;
                            *node_unreachable = Some(message);
                        }
                    }
                }
                Task::none()
            }

            Message::NodeTick => {
                if self.startup_poll_in_flight {
                    return Task::none();
                }
                self.startup_poll_in_flight = true;
                let node_url = self.node_url.clone();
                Task::perform(poll_node_status(node_url), Message::NodeStatusFetched)
            }

            Message::NodeStatusFetched(result) => {
                self.startup_poll_in_flight = false;
                if let Ok(status) = result {
                    note_gpus(&mut self.known_gpus, &status);
                    if let Some(height) = status.height {
                        self.sync_rate
                            .push(self.sync_origin.elapsed().as_secs_f64(), height);
                    }
                    self.startup_status = Some(status);
                }
                // A node with no owned supervisor has no process state we can
                // observe -- `phase()` reads `None` as "judge from the status
                // alone", the same path a `Running` node takes.
                let supervisor_state = self.supervisor.as_ref().map(|s| s.state());
                let tail = self
                    .supervisor
                    .as_ref()
                    .map(|s| s.log_tail(20))
                    .unwrap_or_default();
                self.startup_log = tail.iter().rev().take(3).rev().cloned().collect();
                self.node_phase = startup::phase(
                    supervisor_state.as_ref(),
                    self.startup_status.as_ref(),
                    &tail,
                );
                // Once Ready, never go back -- a new block or two must not
                // bounce the screen. Falling behind again belongs in the
                // wallet screen's banner, not in taking the screen away.
                //
                // Routed through `Message::Show` rather than assigning
                // `self.screen` directly, so this takeover is subject to the
                // same "a revealed seed never survives a screen change" reset
                // as any other navigation -- otherwise a seed revealed before
                // the node died would still be on screen once the restarted
                // node comes back up.
                if matches!(self.node_phase, startup::Phase::Ready)
                    && matches!(self.screen, Screen::Startup)
                {
                    let target = if self.wallet.is_some() {
                        Screen::Wallet
                    } else {
                        Screen::Setup
                    };
                    return self.update(Message::Show(target));
                }
                Task::none()
            }

            Message::CopyPayout(address) => iced::clipboard::write(address),
            Message::MiningAddressPicked(address) => {
                self.settings.mining.address = Some(address);
                self.mining_error = None;
                Task::none()
            }
            Message::MiningBackendPicked(backend) => {
                self.settings.mining.backend = backend;
                self.mining_error = None;
                Task::none()
            }
            Message::MiningThreadsChanged(text) => {
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                self.settings.mining.cpu_threads =
                    digits.parse::<u32>().ok().map(|n| n.clamp(1, 1024));
                self.mining_threads_input = digits;
                self.mining_error = None;
                Task::none()
            }
            Message::MiningGpuToggled(index, on) => {
                let list = &mut self.settings.mining.disabled_gpus;
                if on {
                    list.retain(|i| *i != index);
                } else if !list.contains(&index) {
                    list.push(index);
                    list.sort_unstable();
                }
                self.mining_error = None;
                Task::none()
            }
            Message::MiningStart => {
                if self.mining_selected_choice().is_none() {
                    self.mining_error = Some("Pick the address the rewards go to first.".into());
                    return Task::none();
                }
                self.settings.mining.enabled = true;
                self.apply_mining()
            }
            Message::MiningStop => {
                self.settings.mining.enabled = false;
                self.apply_mining()
            }
            Message::RetryNode => {
                // Dropping the old supervisor here (rather than calling
                // `stop()` on it) is what actually stops the node -- see
                // `node::Supervisor`'s `Drop`. Assigning `None` drops the old
                // value before `ensure_owned_node` builds a new one, so the
                // two processes' lifetimes never overlap in this function.
                // This is also the button behind "apply these settings" in
                // `node_settings` and the owned-node restart on the wallet
                // screen's banner -- both want the same takeover to the
                // startup screen so the user watches the new node come up.
                self.save_settings();
                self.supervisor = None;
                self.ensure_owned_node();
                // Through `Message::Show`, not a direct assignment, so this
                // takeover clears a revealed seed the same as any other
                // screen change (see the comment on `Message::Show`).
                self.update(Message::Show(Screen::Startup))
            }
            Message::OpenSetupSettings => {
                self.setup_settings_open = true;
                self.update(Message::Show(Screen::Setup))
            }
            Message::ToggleSetupSettings => {
                self.setup_settings_open = !self.setup_settings_open;
                Task::none()
            }

            Message::PollTick => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Overlapping requests are exactly how the token bucket in
                // front of `/explorer/address` returns 503 (spec 4.2), so a
                // tick that lands while one is still outstanding is dropped.
                if wallet.poll_in_flight || wallet.fetch_in_flight {
                    return Task::none();
                }
                wallet.poll_in_flight = true;
                let client = wallet.client.clone();
                let active_address = wallet.addresses[wallet.active].address.clone();
                let epoch = self.node_epoch;
                Task::perform(
                    async move {
                        let status = client.status().await;
                        let address = client.address(&active_address).await;
                        (status, address)
                    },
                    move |(status, address)| Message::PollTickFetched(epoch, status, address),
                )
            }
            Message::PollTickFetched(epoch, status, address_result) => {
                let stale = epoch != self.node_epoch;
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.poll_in_flight = false;
                // Asked of a node the wallet no longer reads -- see
                // `App::node_epoch`. The next tick asks the current one.
                if stale {
                    return Task::none();
                }
                apply_status(wallet, &mut self.known_gpus, status);
                let active = wallet.active;
                if let Some(entry) = wallet.addresses.get_mut(active) {
                    // The active row really is retried in ~10s by the next
                    // tick, so "Updating..." staying on for it is honest.
                    apply_address_result(entry, address_result, true);
                }
                Task::none()
            }

            Message::RefreshAll => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if wallet.fetch_in_flight {
                    return Task::none();
                }
                wallet.fetch_in_flight = true;
                for entry in &mut wallet.addresses {
                    entry.loading = true;
                }
                let client = wallet.client.clone();
                let addresses: Vec<String> = wallet
                    .addresses
                    .iter()
                    .map(|entry| entry.address.clone())
                    .collect();
                let epoch = self.node_epoch;
                Task::perform(
                    async move {
                        let status = client.status().await;
                        let mut results = Vec::with_capacity(addresses.len());
                        for address in &addresses {
                            results.push(client.address(address).await);
                            // A short pause between addresses -- spec 4.2 asks
                            // for a discovery scan to space its requests out,
                            // and a full refresh hits the same token bucket.
                            tokio::time::sleep(ADDRESS_READ_PACING).await;
                        }
                        (status, results)
                    },
                    move |(status, results)| Message::RefreshAllFetched(epoch, status, results),
                )
            }
            Message::RefreshAllFetched(epoch, status, results) => {
                let stale = epoch != self.node_epoch;
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.fetch_in_flight = false;
                if stale {
                    // This request set every row to "Updating..." when it
                    // was issued. Its answer describes a node the wallet no
                    // longer reads (`App::node_epoch`), so it is dropped --
                    // but the rows must not go on promising it.
                    for entry in &mut wallet.addresses {
                        entry.loading = false;
                    }
                    return Task::none();
                }
                apply_status(wallet, &mut self.known_gpus, status);
                for (entry, result) in wallet.addresses.iter_mut().zip(results) {
                    // No follow-up is scheduled for any of these rows until
                    // the next screen entry or manual refresh, so a retryable
                    // result here must NOT leave "Updating..." showing --
                    // that would describe a retry that is not coming.
                    apply_address_result(entry, result, false);
                }
                Task::none()
            }

            Message::SetActiveAddress(position) => {
                if let Some(wallet) = &mut self.wallet {
                    if wallet.addresses.get(position).is_some() && position != wallet.active {
                        // The previous active row's "Updating..." (if set)
                        // was honest only because PollTick was about to
                        // retry it in ~10s. Once it stops being active,
                        // nothing retries it -- leaving the flag on would be
                        // exactly the lie `scheduled_retry` exists to
                        // prevent, reached through a different door.
                        if let Some(previous) = wallet.addresses.get_mut(wallet.active) {
                            previous.loading = false;
                        }
                        wallet.active = position;
                        if let Some(entry) = wallet.addresses.get(position) {
                            wallet.qr = iced::widget::qr_code::Data::new(&entry.address).ok();
                        }
                        // A seed revealed for the old row must not stay on
                        // screen under the new row's address and QR (NEXT
                        // ADDRESS on Receive). A check still running is
                        // dropped with it: `RevealSeedRevealed` only lands
                        // on `Asking { busy: true }`, and only for the
                        // address that is active. Pressing the row that is
                        // already active changes nothing, so it is left
                        // alone -- this branch is not reached.
                        wallet.reveal = RevealState::Idle;
                    }
                }
                Task::none()
            }
            Message::CopySentTxId => {
                let Some(wallet) = &self.wallet else {
                    return Task::none();
                };
                let SendStage::Answered(verdict) = &wallet.send.stage else {
                    return Task::none();
                };
                match crate::view::send::settled_tx_id(verdict) {
                    Some(id) => iced::clipboard::write(id.to_string()),
                    None => Task::none(),
                }
            }
            Message::CopyAddress => {
                let Some(wallet) = &self.wallet else {
                    return Task::none();
                };
                let Some(entry) = wallet.addresses.get(wallet.active) else {
                    return Task::none();
                };
                iced::clipboard::write(entry.address.clone())
            }

            Message::RevealSeedStart => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.reveal = RevealState::Asking {
                    passphrase: Zeroizing::new(String::new()),
                    error: None,
                    busy: false,
                };
                Task::none()
            }
            Message::RevealPassphraseChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    if let RevealState::Asking {
                        passphrase, error, ..
                    } = &mut wallet.reveal
                    {
                        *passphrase = Zeroizing::new(value);
                        *error = None;
                    }
                }
                Task::none()
            }
            Message::RevealSeedConfirm => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Read before the borrow below, and carried into the task, so
                // the seed that comes back is labelled with the row it was
                // asked for even if the selection moves while Argon2id runs.
                let Some(entry) = wallet.addresses.get(wallet.active) else {
                    return Task::none();
                };
                let address = entry.address.clone();
                let source = entry.source;
                let path = wallet.wallet_path.clone();
                let passphrase = match &mut wallet.reveal {
                    RevealState::Asking {
                        passphrase,
                        error,
                        busy,
                    } => {
                        if passphrase.is_empty() {
                            *error = Some("Enter your passphrase.".into());
                            return Task::none();
                        }
                        *error = None;
                        *busy = true;
                        passphrase.clone()
                    }
                    _ => return Task::none(),
                };
                Task::perform(
                    async move {
                        // Argon2id at the keystore's cost is tens of
                        // milliseconds of solid CPU. On the UI thread that is a
                        // visible stall, so it goes where `Unlock` puts it.
                        tokio::task::spawn_blocking(move || {
                            let seed_hex = match source {
                                AddressSource::Derived(index) => storage::reveal_child_seed_hex(
                                    &path,
                                    passphrase.as_bytes(),
                                    index,
                                ),
                                AddressSource::Imported(slot) => storage::reveal_imported_seed_hex(
                                    &path,
                                    passphrase.as_bytes(),
                                    slot,
                                ),
                            }?;
                            Ok(Revealed { address, seed_hex })
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    },
                    Message::RevealSeedRevealed,
                )
            }
            Message::RevealSeedRevealed(result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                match result {
                    Ok(revealed) => {
                        // Only if the request is still outstanding. Argon2id
                        // takes long enough to press Cancel during, and Cancel
                        // (like any screen change) sets `Idle` -- without this
                        // guard the derivation would land afterwards and put a
                        // spendable key on screen that the user had just
                        // dismissed.
                        //
                        // And only for the address on screen. Busy alone is
                        // not enough: NEXT ADDRESS during the check, then
                        // asking again for the new row, makes the state
                        // `Asking { busy: true }` once more, and the old
                        // row's derivation would answer the new request.
                        let for_active = wallet
                            .addresses
                            .get(wallet.active)
                            .is_some_and(|entry| entry.address == revealed.address);
                        if for_active
                            && matches!(wallet.reveal, RevealState::Asking { busy: true, .. })
                        {
                            wallet.reveal = RevealState::Revealed {
                                address: revealed.address,
                                seed_hex: revealed.seed_hex,
                            };
                        }
                    }
                    Err(message) => {
                        if let RevealState::Asking {
                            passphrase,
                            error,
                            busy,
                        } = &mut wallet.reveal
                        {
                            *busy = false;
                            *error = Some(message);
                            // Drop the attempt that failed. Leaving it in the
                            // field invites a second Confirm on the same wrong
                            // input, and it is a secret sitting in a widget.
                            *passphrase = Zeroizing::new(String::new());
                        }
                    }
                }
                Task::none()
            }
            Message::RevealSeedCancel => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.reveal = RevealState::Idle;
                }
                Task::none()
            }
            Message::CopySeed => {
                let Some(wallet) = &self.wallet else {
                    return Task::none();
                };
                let RevealState::Revealed { seed_hex, .. } = &wallet.reveal else {
                    return Task::none();
                };
                iced::clipboard::write(seed_hex.to_string())
            }
            Message::ExportWalletFile => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if wallet.exporting {
                    return Task::none();
                }
                wallet.exporting = true;
                wallet.export_result = None;
                let source = wallet.wallet_path.clone();
                let suggested = chrono::Local::now()
                    .format("alphanumeric-wallet-%Y-%m-%d.enc")
                    .to_string();
                Task::perform(
                    async move {
                        // I2: the rfd save dialog is not modal, so a user can
                        // press EXPORT, leave it open, finish an import, and
                        // only then press Save. Reading the envelope now --
                        // off the UI thread, before the dialog even opens --
                        // means the bytes written are the wallet as it was
                        // when EXPORT was pressed, not whatever wallet is
                        // open by the time Save is actually clicked.
                        let read_source = source.clone();
                        let envelope = tokio::task::spawn_blocking(move || {
                            std::fs::read(&read_source).map_err(|e| {
                                format!("Could not read {}: {e}", read_source.display())
                            })
                        })
                        .await
                        .map_err(|error| format!("Export: {error}"))??;
                        let chosen = rfd::AsyncFileDialog::new()
                            .set_title("Save a copy of the wallet file")
                            .set_file_name(&suggested)
                            .save_file()
                            .await;
                        let Some(handle) = chosen else {
                            return Ok(None);
                        };
                        let dest = handle.path().to_path_buf();
                        let write_dest = dest.clone();
                        tokio::task::spawn_blocking(move || {
                            storage::export_bytes(&source, &envelope, &write_dest)
                        })
                        .await
                        .map_err(|error| format!("Export: {error}"))??;
                        Ok(Some(dest))
                    },
                    Message::ExportWalletFileDone,
                )
            }
            Message::ExportWalletFileDone(result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Only the wallet that asked. A wallet installed since (an
                // import) never set this flag, so an old answer is dropped.
                if !wallet.exporting {
                    return Task::none();
                }
                wallet.exporting = false;
                wallet.export_result = match result {
                    Ok(Some(path)) => Some(Ok(path)),
                    Ok(None) => None,
                    Err(message) => Some(Err(message)),
                };
                Task::none()
            }
            Message::MasterRevealStart => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.master_reveal = MasterRevealState::Asking {
                    passphrase: Zeroizing::new(String::new()),
                    error: None,
                    busy: false,
                    request: 0,
                };
                Task::none()
            }
            Message::MasterRevealPassphraseChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    if let MasterRevealState::Asking {
                        passphrase, error, ..
                    } = &mut wallet.master_reveal
                    {
                        *passphrase = Zeroizing::new(value);
                        *error = None;
                    }
                }
                Task::none()
            }
            Message::MasterRevealConfirm => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let path = wallet.wallet_path.clone();
                self.master_reveal_requests = self.master_reveal_requests.wrapping_add(1);
                let number = self.master_reveal_requests;
                let passphrase = match &mut wallet.master_reveal {
                    MasterRevealState::Asking {
                        passphrase,
                        error,
                        busy,
                        request,
                    } => {
                        if *busy {
                            return Task::none();
                        }
                        if passphrase.is_empty() {
                            *error = Some("Enter your passphrase.".into());
                            return Task::none();
                        }
                        *error = None;
                        *busy = true;
                        *request = number;
                        passphrase.clone()
                    }
                    _ => return Task::none(),
                };
                Task::perform(
                    async move {
                        // Argon2id: off the UI thread, as `Unlock` does.
                        tokio::task::spawn_blocking(move || {
                            storage::reveal_master_seed(&path, passphrase.as_bytes())
                                .map(MasterSeedText)
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    },
                    move |result| Message::MasterRevealDone(number, result),
                )
            }
            Message::MasterRevealDone(number, result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Only the attempt still waiting. Cancel, a screen change or
                // a second attempt all leave an earlier answer with nowhere
                // to go -- a refusal must not blank the field someone is
                // typing in again, and a seed must not appear after HIDE.
                let waiting = matches!(
                    &wallet.master_reveal,
                    MasterRevealState::Asking { busy: true, request, .. } if *request == number
                );
                if !waiting {
                    return Task::none();
                }
                match result {
                    Ok(MasterSeedText(seed)) => {
                        wallet.master_reveal = MasterRevealState::Revealed { seed };
                    }
                    Err(message) => {
                        if let MasterRevealState::Asking {
                            passphrase,
                            error,
                            busy,
                            ..
                        } = &mut wallet.master_reveal
                        {
                            *busy = false;
                            *error = Some(message);
                            *passphrase = Zeroizing::new(String::new());
                        }
                    }
                }
                Task::none()
            }
            Message::MasterRevealHide => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.master_reveal = MasterRevealState::Idle;
                }
                Task::none()
            }
            Message::CopyMasterSeed => {
                let Some(wallet) = &self.wallet else {
                    return Task::none();
                };
                let MasterRevealState::Revealed { seed } = &wallet.master_reveal else {
                    return Task::none();
                };
                iced::clipboard::write(seed.to_string())
            }
            Message::ClearClipboard => iced::clipboard::write(String::new()),
            Message::ImportStart => {
                if self.import_blocker().is_some() {
                    return Task::none();
                }
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.master_reveal = MasterRevealState::Idle;
                wallet.import_confirm = Some(storage::archive_path(
                    &wallet.wallet_path,
                    &archive_stamp(),
                    |candidate| candidate.exists(),
                ));
                Task::none()
            }
            Message::ImportCancelConfirm => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.import_confirm = None;
                }
                Task::none()
            }
            Message::ImportContinue => {
                if self.import_blocker().is_some() {
                    return Task::none();
                }
                let Some(wallet) = &self.wallet else {
                    return Task::none();
                };
                if wallet.import_confirm.is_none() {
                    return Task::none();
                }
                let replaced_first_address = wallet
                    .addresses
                    .first()
                    .map(|entry| entry.address.clone())
                    .unwrap_or_default();
                // Drops the master, the session passphrase, any revealed seed
                // and every cache with the wallet -- the secrets are Zeroizing.
                self.wallet = None;
                // Spec G §4.6: every PollTick/RefreshAll answer still in
                // flight was asked for the wallet just closed. Moving the
                // epoch makes them stale, so they cannot land on the one
                // installed next (`App::node_epoch`).
                self.node_epoch = self.node_epoch.wrapping_add(1);
                self.importing = Some(ImportContext {
                    replaced_first_address,
                });
                self.setup = SetupStage::Choose;
                self.screen = Screen::Setup;
                Task::none()
            }
            Message::ImportAbort => {
                if self.setup_writing() {
                    return Task::none();
                }
                if self.importing.take().is_none() {
                    return Task::none();
                }
                // Nothing was written yet (the old file moves only when the
                // new one is saved), so the file is still there to unlock.
                self.setup = match &self.wallet_path {
                    Some(path) if storage::exists(path) => SetupStage::Unlock {
                        passphrase: Zeroizing::new(String::new()),
                        error: None,
                        busy: false,
                    },
                    _ => {
                        // M7: a rollback that also failed leaves no wallet
                        // file behind, only its archive -- the first-run
                        // screen this falls back to must be able to name it,
                        // exactly as a fresh `App::new()` would.
                        self.archives_found = self
                            .wallet_path
                            .as_deref()
                            .map(storage::find_archives)
                            .unwrap_or_default();
                        SetupStage::Choose
                    }
                };
                self.screen = Screen::Setup;
                Task::none()
            }
            Message::StartImportFile => {
                // As `StartCreate`/`StartRestore`: on a first run nothing
                // has started the wallet's own node yet.
                if self.node_source == NodeSource::Owned && self.supervisor.is_none() {
                    self.ensure_owned_node();
                }
                self.setup = SetupStage::ImportFile {
                    path: None,
                    passphrase: Zeroizing::new(String::new()),
                    busy: false,
                    error: None,
                    preview: None,
                };
                Task::none()
            }
            Message::ImportPickFile => {
                if !matches!(self.setup, SetupStage::ImportFile { busy: false, .. }) {
                    return Task::none();
                }
                Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose a wallet file")
                            .pick_file()
                            .await
                            .map(|handle| handle.path().to_path_buf())
                    },
                    Message::ImportFilePicked,
                )
            }
            Message::ImportFilePicked(picked) => {
                if let (
                    Some(picked),
                    SetupStage::ImportFile {
                        path,
                        preview,
                        error,
                        busy: false,
                        ..
                    },
                ) = (picked, &mut self.setup)
                {
                    *path = Some(picked);
                    *preview = None;
                    *error = None;
                }
                Task::none()
            }
            Message::ImportPassphraseChanged(value) => {
                // Controller ruling, Task 6 fix round 1: refused outright
                // while busy, so an edit made mid-OPEN or mid-USE can never
                // land after the fact -- `preview` and the passphrase it
                // carries stay exactly what was opened.
                if let SetupStage::ImportFile {
                    passphrase,
                    preview,
                    error,
                    busy: false,
                    ..
                } = &mut self.setup
                {
                    *passphrase = Zeroizing::new(value);
                    *preview = None;
                    *error = None;
                }
                Task::none()
            }
            Message::ImportOpen => {
                let (path, passphrase) = match &mut self.setup {
                    SetupStage::ImportFile {
                        path: Some(path),
                        passphrase,
                        busy,
                        error,
                        ..
                    } => {
                        if *busy {
                            return Task::none();
                        }
                        if passphrase.is_empty() {
                            *error = Some("Enter the passphrase of that file.".into());
                            return Task::none();
                        }
                        *busy = true;
                        *error = None;
                        (path.clone(), passphrase.clone())
                    }
                    _ => return Task::none(),
                };
                Task::perform(
                    async move {
                        // Cloned before the move below: this is the
                        // passphrase that actually opens `path`, carried back
                        // to the stage so USE can never use anything else
                        // (controller ruling, Task 6 fix round 1).
                        let opened_with = passphrase.clone();
                        // Argon2id again: off the UI thread.
                        tokio::task::spawn_blocking(move || {
                            alphanumeric_gui::import::open_wallet_file(&path, passphrase.as_bytes())
                                .map(Arc::new)
                        })
                        .await
                        .map_err(|error| error.to_string())?
                        .map(|file| (file, opened_with))
                    },
                    Message::ImportOpened,
                )
            }
            Message::ImportOpened(result) => {
                let same = matches!(&result, Ok((file, _)) if self
                    .importing
                    .as_ref()
                    .is_some_and(|context| context.replaced_first_address == file.first_address));
                if let SetupStage::ImportFile {
                    busy,
                    error,
                    preview,
                    passphrase,
                    ..
                } = &mut self.setup
                {
                    // Only the open still waiting: a stage left and re-entered
                    // (BACK, then WALLET FILE again) never asked.
                    if !*busy {
                        return Task::none();
                    }
                    *busy = false;
                    match result {
                        Ok(_) if same => {
                            *preview = None;
                            *error = Some(SAME_WALLET.into());
                        }
                        Ok((file, opened_with)) => {
                            *preview = Some(OpenedFile {
                                file,
                                passphrase: opened_with,
                            });
                            *error = None;
                        }
                        Err(message) => {
                            *preview = None;
                            *error = Some(message);
                            // A failed secret does not stay in the widget.
                            *passphrase = Zeroizing::new(String::new());
                        }
                    }
                }
                Task::none()
            }
            Message::ImportUse => {
                let Some(wallet_path) = self.wallet_path.clone() else {
                    set_stage_error(
                        &mut self.setup,
                        "No home directory available to store a wallet.".into(),
                    );
                    return Task::none();
                };
                // Validated now and carried in `Ready`, as `Unlock` does.
                let node_url = self.node_url.clone();
                if let Err(message) = backend::Client::new(&node_url) {
                    set_stage_error(&mut self.setup, message);
                    return Task::none();
                }
                // The passphrase that opened the file, from `preview` --
                // never `passphrase` (the text field): controller ruling,
                // Task 6 fix round 1.
                let (file, passphrase) = match &mut self.setup {
                    SetupStage::ImportFile {
                        preview: Some(opened),
                        busy,
                        error,
                        ..
                    } => {
                        if *busy {
                            return Task::none();
                        }
                        *busy = true;
                        *error = None;
                        (Arc::clone(&opened.file), opened.passphrase.clone())
                    }
                    _ => return Task::none(),
                };
                Task::perform(
                    use_imported_file(wallet_path, archive_stamp(), file, passphrase, node_url),
                    Message::SetupReady,
                )
            }
            Message::HistoryOpen => {
                // Bumped, not reset -- a stale response from the state this
                // replaces must never match the new one's generation.
                self.history_generation = self.history_generation.wrapping_add(1);
                let generation = self.history_generation;
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let addresses: Vec<String> = wallet
                    .addresses
                    .iter()
                    .map(|entry| entry.address.clone())
                    .collect();
                wallet.history = Some(HistoryState {
                    merge: alphanumeric_gui::history::Merge::new(addresses),
                    in_flight: false,
                    error: None,
                    generation,
                });
                // Duplicated from `Message::Show`, which is where a revealed
                // seed is normally cleared ("A revealed seed never survives a
                // screen change"). This arm sets `self.screen` itself instead
                // of going through `Show`, so it would otherwise be the one
                // screen change that carries the `Zeroizing<String>` across.
                // No path reaches here from Receive today; the duplication is
                // what keeps that from becoming a silent leak if one is added.
                wallet.reveal = RevealState::Idle;
                wallet.master_reveal = MasterRevealState::Idle;
                wallet.import_key = ImportKeyState::Idle;
                wallet.removing_key = None;
                let client = Arc::clone(&wallet.client);
                // The staleness banner compares the index height against
                // `node_status.height`. `ConsoleTick` refreshes that value
                // every 10s while this screen stays open, but that cadence
                // is no reason to make the person who just opened it wait --
                // without this immediate re-read the comparison would start
                // against whatever the height was when some other screen
                // last looked, and a list missing every block since then
                // presents itself as complete -- the exact under-reporting
                // spec 6.2 exists to prevent.
                let epoch = self.node_epoch;
                let status = Task::perform(async move { client.status().await }, move |result| {
                    Message::HistoryStatusFetched(epoch, result)
                });
                // The `wallet` borrow ends here. It cannot overlap with the
                // `self` use below or this would not compile.
                self.screen = Screen::History;
                Task::batch([status, self.update(Message::HistoryMore)])
            }
            Message::HistoryStatusFetched(epoch, status) => {
                // Not the history generation: this writes the same field the
                // poll writes, and a late answer from the SAME node is still
                // a real chain height. From a different node it is not --
                // see `App::node_epoch`.
                if epoch != self.node_epoch {
                    return Task::none();
                }
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                apply_status(wallet, &mut self.known_gpus, status);
                Task::none()
            }
            Message::HistoryMore => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Everything that touches `history` finishes first. Holding
                // that borrow while touching `wallet.client` or
                // `wallet.addresses` would be a borrow conflict.
                let (index, address, before, generation) = {
                    let Some(history) = &mut wallet.history else {
                        return Task::none();
                    };
                    if history.in_flight {
                        return Task::none();
                    }
                    history.error = None;
                    // The goal is one screen's worth more than what's out now.
                    let want = history.merge.rows().len() + HISTORY_PAGE_ROWS;
                    match history.merge.advance(want) {
                        alphanumeric_gui::history::Need::Page {
                            index,
                            address,
                            before,
                        } => {
                            history.in_flight = true;
                            // The address comes from the merge's own stream,
                            // not from `wallet.addresses[index]`. Those are
                            // two sources of truth for one thing:
                            // `Message::AddressStoreSaved` can push a new
                            // address while this screen is open, and re-
                            // resolving by index would then read a list the
                            // merge has never seen.
                            (index, address, before, history.generation)
                        }
                        alphanumeric_gui::history::Need::Idle => return Task::none(),
                    }
                };
                let client = Arc::clone(&wallet.client);
                Task::perform(
                    async move {
                        // Paced against the same shared token bucket as
                        // `Message::RefreshAll` -- see `ADDRESS_READ_PACING`.
                        // Before the GET, not after: sleeping afterwards would
                        // hold the page back from the screen for no benefit.
                        tokio::time::sleep(ADDRESS_READ_PACING).await;
                        client
                            .address_page(&address, alphanumeric_gui::history::PAGE_LIMIT, before)
                            .await
                    },
                    move |result| Message::HistoryPageFetched(index, generation, before, result),
                )
            }
            Message::HistoryPageFetched(index, generation, before, result) => {
                // Wrapped in a block so the `self.wallet` borrow ends before
                // `self.update` is called. Calling it while the borrow is
                // still held would not compile.
                let keep_going = {
                    let Some(wallet) = &mut self.wallet else {
                        return Task::none();
                    };
                    let Some(history) = &mut wallet.history else {
                        return Task::none();
                    };
                    if history.generation != generation {
                        // This answers a request from a `HistoryState` that
                        // no longer exists -- the screen was left and
                        // reopened while it was in flight. The current
                        // state's own request for this same `index` may
                        // still be outstanding, so this is dropped whole:
                        // no `accept` (it would double-feed that stream's
                        // buffer and could collapse two distinct entries
                        // into a false `Internal` transfer), no `in_flight`
                        // change (it is not this response's lock to clear),
                        // no recursion.
                        return Task::none();
                    }
                    history.in_flight = false;
                    match result {
                        Ok(page) => {
                            // A page with entries but no `next` cursor already
                            // means `finished`; the case being caught here is
                            // narrower: nothing was added to the stream at
                            // all, yet the node still claims a next page
                            // exists. Without this check `advance` sees the
                            // same unfinished, unstalled, empty-buffered
                            // stream next time and asks for the identical page
                            // again -- an unbounded loop of GETs against a
                            // node that `node_url` makes user-editable, so
                            // this cannot be dismissed as "this node would
                            // never do that".
                            //
                            // Judged on what `accept` KEPT, not on what
                            // arrived: it drops every entry at or above the
                            // cursor the page was asked for, so a node that
                            // re-serves an earlier page sends a full-looking
                            // page that adds nothing. Counting the wire's
                            // entries here would call that progress and hand
                            // the loop straight back.
                            let has_next = page.next.is_some();
                            let kept = history.merge.accept(index, before, page);
                            if kept == 0 && has_next {
                                history.error = Some(
                                    "The node said more history was available but returned no new entries.".to_string(),
                                );
                                false
                            } else {
                                true
                            }
                        }
                        Err(error) => {
                            // A failed address is not skipped. Skipping would
                            // break the merge's ordering invariant and the
                            // list would silently go out of order.
                            history.error = Some(error.to_string());
                            false
                        }
                    }
                };
                if keep_going {
                    // One page is rarely enough. Sequential because the
                    // node's handler answers 429 (`explorer_address_handler`).
                    self.update(Message::HistoryMore)
                } else {
                    Task::none()
                }
            }
            Message::AddAddress => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if wallet.adding_address {
                    return Task::none();
                }
                wallet.adding_address = true;
                wallet.save_error = None;
                let new_index = wallet.next_index;
                let updated_next = new_index.saturating_add(1);
                let master = wallet.master.clone();
                let path = wallet.wallet_path.clone();
                let passphrase = wallet.passphrase.clone();
                let imported = wallet.imported.clone();
                Task::perform(
                    save_after_add_address(path, master, passphrase, updated_next, imported),
                    Message::AddressStoreSaved,
                )
            }
            Message::AddressStoreSaved(result) => {
                // A block so the `self.wallet` borrow ends before
                // `self.update` (the `HistoryPageFetched` pattern).
                let added = {
                    let Some(wallet) = &mut self.wallet else {
                        return Task::none();
                    };
                    wallet.adding_address = false;
                    match result {
                        Ok(updated_next) => {
                            let new_index = wallet.next_index;
                            wallet.next_index = updated_next;
                            let address = model::address_for_index(&wallet.master, new_index);
                            wallet.addresses.push(AddressEntry {
                                source: AddressSource::Derived(new_index),
                                address,
                                balance_units: None,
                                spendable: Spendable::Pending,
                                error: None,
                                loading: false,
                                recent: None,
                            });
                            true
                        }
                        Err(message) => {
                            wallet.save_error = Some(message);
                            false
                        }
                    }
                };
                // The new row has no first page yet, and the recent-rows
                // merger rightly will not order the others without it, so
                // F1/F2 would sit on "Waiting..."/PARTIAL until the user
                // pressed REFRESH. Fetch now. If a full refresh is already
                // running it bails (`fetch_in_flight`), and that refresh
                // does not cover the new row -- it stays unfetched until the
                // next REFRESH or screen entry, as before.
                if added {
                    self.update(Message::RefreshAll)
                } else {
                    Task::none()
                }
            }

            Message::ImportKeyStart => {
                if self.key_change_blocker().is_some() {
                    return Task::none();
                }
                if let Some(wallet) = &mut self.wallet {
                    wallet.import_key = ImportKeyState::Asking {
                        seed: Zeroizing::new(String::new()),
                        error: None,
                    };
                }
                Task::none()
            }
            Message::ImportKeySeedChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.import_key = ImportKeyState::Asking {
                        seed: Zeroizing::new(value),
                        error: None,
                    };
                }
                Task::none()
            }
            Message::ImportKeyCheck => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let ImportKeyState::Asking { seed, .. } = &wallet.import_key else {
                    return Task::none();
                };
                let seed = seed.clone();
                let parsed = match alphanumeric_gui::seed::parse_imported_seed(&seed) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        wallet.import_key = ImportKeyState::Asking {
                            seed,
                            error: Some(error.to_string()),
                        };
                        return Task::none();
                    }
                };
                let address = alphanumeric_gui::model::address_for_seed(&parsed);
                if wallet
                    .addresses
                    .iter()
                    .any(|entry| entry.address == address)
                {
                    wallet.import_key = ImportKeyState::Asking {
                        seed,
                        error: Some("That address is already in this wallet.".into()),
                    };
                    return Task::none();
                }
                wallet.import_key = ImportKeyState::Checked { seed, address };
                Task::none()
            }
            Message::ImportKeyAdd => {
                // I3: blocked must say why (spec 4.4), not silently do
                // nothing -- the panel is reachable with `+ ADD` gated only
                // on `!wallet.adding_address`, so a press while blocked would
                // otherwise look like it never registered.
                if let Some(reason) = self.key_change_blocker() {
                    if let Some(wallet) = &mut self.wallet {
                        let kept_seed = match &wallet.import_key {
                            ImportKeyState::Checked { seed, .. }
                            | ImportKeyState::Asking { seed, .. } => Some(seed.clone()),
                            ImportKeyState::Idle | ImportKeyState::Busy => None,
                        };
                        if let Some(seed) = kept_seed {
                            wallet.import_key = ImportKeyState::Asking {
                                seed,
                                error: Some(reason.into()),
                            };
                        }
                    }
                    return Task::none();
                }
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let ImportKeyState::Checked { seed, address } = &wallet.import_key else {
                    return Task::none();
                };
                let (seed, address) = (seed.clone(), address.clone());
                let slot = wallet.imported.len();
                wallet.imported.push(seed);
                wallet.addresses.push(AddressEntry {
                    source: AddressSource::Imported(slot),
                    address,
                    balance_units: None,
                    spendable: Spendable::Pending,
                    error: None,
                    loading: false,
                    recent: None,
                });
                wallet.import_key = ImportKeyState::Busy;
                // Written synchronously: the file is a few hundred bytes and
                // the list on screen must never claim a key the file lacks.
                match self.write_keys() {
                    Ok(()) => {
                        if let Some(wallet) = &mut self.wallet {
                            wallet.import_key = ImportKeyState::Idle;
                        }
                        // The new row has no figures yet.
                        self.update(Message::RefreshAll)
                    }
                    Err(message) => {
                        if let Some(wallet) = &mut self.wallet {
                            // Put the wallet back the way it was: the file is
                            // the record, and it does not have this key.
                            wallet.imported.pop();
                            wallet.addresses.pop();
                            wallet.import_key = ImportKeyState::Asking {
                                seed: Zeroizing::new(String::new()),
                                error: Some(message),
                            };
                        }
                        Task::none()
                    }
                }
            }
            Message::ImportKeyCancel => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.import_key = ImportKeyState::Idle;
                }
                Task::none()
            }

            Message::RemoveKeyStart(slot) => {
                if self.key_change_blocker().is_some() {
                    return Task::none();
                }
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let Some(entry) = wallet
                    .addresses
                    .iter()
                    .find(|entry| entry.source == AddressSource::Imported(slot))
                else {
                    return Task::none();
                };
                wallet.removing_key = Some(RemoveKey {
                    slot,
                    address: entry.address.clone(),
                    typed: String::new(),
                    error: None,
                });
                Task::none()
            }
            Message::RemoveKeyTypedChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    if let Some(removing) = &mut wallet.removing_key {
                        removing.typed = value;
                        removing.error = None;
                    }
                }
                Task::none()
            }
            Message::RemoveKeyCancel => {
                if let Some(wallet) = &mut self.wallet {
                    wallet.removing_key = None;
                }
                Task::none()
            }
            Message::RemoveKeyConfirm => {
                // I3: same as `ImportKeyAdd` -- blocked must say why, not
                // silently do nothing.
                if let Some(reason) = self.key_change_blocker() {
                    if let Some(wallet) = &mut self.wallet {
                        if let Some(removing) = &mut wallet.removing_key {
                            removing.error = Some(reason.into());
                        }
                    }
                    return Task::none();
                }
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let Some(removing) = &mut wallet.removing_key else {
                    return Task::none();
                };
                if removing.typed.trim() != removing.address {
                    removing.error = Some("That is not this address.".into());
                    return Task::none();
                }
                let slot = removing.slot;
                // M6: `slot` is only ever set by `RemoveKeyStart` from a row
                // that exists, but nothing stops a future desync from
                // reaching here with a slot the list no longer has -- refuse
                // rather than let `Vec::remove` panic.
                if slot >= wallet.imported.len() {
                    return Task::none();
                }
                // Kept for the rollback below: the file is the record.
                let seed = wallet.imported.remove(slot);
                let address = removing.address.clone();
                // I2: by SLOT, not by address -- a derived row can carry the
                // same address an imported one does (the master later derives
                // what an import already holds), and deleting by address
                // would take that row down too.
                wallet
                    .addresses
                    .retain(|entry| entry.source != AddressSource::Imported(slot));
                // Ruling 4: every later slot moved down by one.
                for entry in &mut wallet.addresses {
                    if let AddressSource::Imported(position) = &mut entry.source {
                        if *position > slot {
                            *position -= 1;
                        }
                    }
                }
                wallet.active = wallet.active.min(wallet.addresses.len().saturating_sub(1));
                // C1: `wallet.qr` is drawn by F2 with no re-derivation of its
                // own -- it must be rebuilt for whatever `active` now points
                // at, exactly the way `SetActiveAddress` builds it, or the
                // just-deleted address's QR stays on screen next to a
                // different address's text and COPY button.
                wallet.qr = wallet
                    .addresses
                    .get(wallet.active)
                    .and_then(|entry| iced::widget::qr_code::Data::new(&entry.address).ok());
                // Same invariant `SetActiveAddress` keeps: nothing on screen
                // may outlive the row it belongs to.
                wallet.reveal = RevealState::Idle;
                match self.write_keys() {
                    Ok(()) => {
                        if let Some(wallet) = &mut self.wallet {
                            wallet.removing_key = None;
                        }
                        Task::none()
                    }
                    Err(message) => {
                        // Nothing was written, so the wallet goes back to
                        // what the file still says.
                        if let Some(wallet) = &mut self.wallet {
                            wallet.imported.insert(slot, seed);
                            for entry in &mut wallet.addresses {
                                if let AddressSource::Imported(position) = &mut entry.source {
                                    if *position >= slot {
                                        *position += 1;
                                    }
                                }
                            }
                            wallet.addresses.push(AddressEntry {
                                source: AddressSource::Imported(slot),
                                address,
                                balance_units: None,
                                spendable: Spendable::Pending,
                                error: None,
                                loading: false,
                                recent: None,
                            });
                            // Symmetric with the success path above: the row
                            // just put back may not be where `active` (still
                            // clamped from before the rollback) now points,
                            // so `qr` has to be rebuilt for it too rather than
                            // left describing whatever was active mid-removal.
                            wallet.qr = wallet.addresses.get(wallet.active).and_then(|entry| {
                                iced::widget::qr_code::Data::new(&entry.address).ok()
                            });
                            wallet.reveal = RevealState::Idle;
                            if let Some(removing) = &mut wallet.removing_key {
                                removing.error = Some(message);
                            }
                        }
                        Task::none()
                    }
                }
            }

            Message::SendRecipientChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    // Only while composing. An edit arriving in any other
                    // stage would change what is on screen out from under a
                    // payment that has already been approved or signed.
                    if matches!(wallet.send.stage, SendStage::Compose) {
                        wallet.send.recipient = value;
                        wallet.send.error = None;
                    }
                }
                Task::none()
            }
            Message::SendAmountChanged(value) => {
                if let Some(wallet) = &mut self.wallet {
                    if matches!(wallet.send.stage, SendStage::Compose) {
                        wallet.send.amount = value;
                        wallet.send.error = None;
                    }
                }
                Task::none()
            }
            Message::SendFeeRefresh => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if wallet.send.fee_in_flight {
                    return Task::none();
                }
                wallet.send.fee_in_flight = true;
                let client = wallet.client.clone();
                Task::perform(
                    async move { client.fee_estimate_with_clock().await },
                    Message::SendFeeFetched,
                )
            }
            Message::SendFeeFetched(result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                wallet.send.fee_in_flight = false;
                match result {
                    Ok((estimate, clock_offset)) => {
                        wallet.send.fee = Some(estimate);
                        wallet.send.clock_offset = clock_offset;
                        wallet.send.fee_error = None;
                    }
                    // The previous estimate is kept rather than cleared: it
                    // is still the last thing the node actually said, and the
                    // error beside it says how old that is. The clock reading
                    // IS cleared -- an offset from a response that did not
                    // arrive is not a reading.
                    Err(error) => {
                        wallet.send.fee_error = Some(error.to_string());
                        wallet.send.clock_offset = None;
                    }
                }
                Task::none()
            }
            Message::SendReview => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if !matches!(wallet.send.stage, SendStage::Compose) {
                    return Task::none();
                }
                let Some(entry) = wallet.addresses.get(wallet.active) else {
                    return Task::none();
                };
                match crate::view::send::prepare_payment(
                    &entry.address,
                    entry.source,
                    entry.spendable,
                    &wallet.send.recipient,
                    &wallet.send.amount,
                    wallet.send.fee.as_ref(),
                ) {
                    Ok(prepared) => {
                        wallet.send.error = None;
                        wallet.send.stage = SendStage::Confirm(prepared);
                    }
                    // The button that sends this message is only offered when
                    // the payment already validates, so this arm exists to
                    // make a race impossible rather than to be seen.
                    Err(blocker) => wallet.send.error = Some(blocker.explain()),
                }
                Task::none()
            }
            Message::SendBackToCompose => {
                if let Some(wallet) = &mut self.wallet {
                    if matches!(wallet.send.stage, SendStage::Confirm(_)) {
                        wallet.send.stage = SendStage::Compose;
                        wallet.send.error = None;
                    }
                }
                Task::none()
            }
            Message::SendConfirm => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                // Never sign a second payment while one is unaccounted for.
                if wallet.send.outstanding.is_some() {
                    return Task::none();
                }
                let SendStage::Confirm(prepared) = &wallet.send.stage else {
                    return Task::none();
                };
                let prepared = prepared.clone();
                // The confirmation may have been on screen for minutes, and
                // the ten-second poll refreshes only the ACTIVE address --
                // which need not be this payment's sender. So the ceiling is
                // fetched fresh, for this address, right now. Requirement 1 is
                // "block above spendable BEFORE signing", and a check that
                // reads a stale row is not that check.
                let sender = prepared.sender.clone();
                let client = wallet.client.clone();
                wallet.send.error = None;
                wallet.send.stage = SendStage::Checking(prepared);
                Task::perform(
                    async move { client.address(&sender).await },
                    Message::SendSpendableChecked,
                )
            }
            Message::SendSpendableChecked(result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let SendStage::Checking(prepared) = &wallet.send.stage else {
                    return Task::none();
                };
                let prepared = prepared.clone();
                if wallet.send.outstanding.is_some() {
                    return Task::none();
                }
                // A fetch that failed is a refusal to sign, never a licence to
                // fall back on the cached number: "the node could not be
                // asked" and "the node said you have this much" are different
                // facts, and only one of them is a ceiling.
                let spendable = match crate::view::send::ceiling_from_fetch(&result) {
                    Ok(spendable) => spendable,
                    Err(message) => {
                        wallet.send.error = Some(message);
                        wallet.send.stage = SendStage::Compose;
                        return Task::none();
                    }
                };
                // The answer is authoritative and freshly taken, so the row it
                // describes is brought up to date with it too -- otherwise the
                // wallet screen would go on showing an older figure than the
                // one a payment was just judged against.
                if let Some(entry) = wallet
                    .addresses
                    .iter_mut()
                    .find(|entry| entry.source == prepared.sender_source)
                {
                    apply_address_result(entry, result, false);
                }
                // Back to the form rather than staying on a confirmation that
                // is now known to be wrong: what the user approved no longer
                // fits, so the approval itself has to be taken back. An
                // `Unavailable` answer lands here as well, in the same refusal
                // path as every other reason the ceiling is not a number.
                if let Err(blocker) = crate::view::send::recheck_spendable(&prepared, spendable) {
                    wallet.send.error = Some(blocker.explain());
                    wallet.send.stage = SendStage::Compose;
                    return Task::none();
                }
                let Some(timestamp) = backend::local_unix_now() else {
                    wallet.send.error = Some(
                        "This computer's clock is set before 1970, so no timestamp can be \
                         signed."
                            .into(),
                    );
                    wallet.send.stage = SendStage::Compose;
                    return Task::none();
                };
                let body = {
                    // The child seed exists only inside this block, and
                    // zeroizes when it ends. It is never moved into the async
                    // task that follows.
                    let Some(seed) = wallet.signing_seed(prepared.sender_source) else {
                        wallet.send.error =
                            Some("This wallet no longer has the key for that address.".into());
                        wallet.send.stage = SendStage::Compose;
                        return Task::none();
                    };
                    // Controller ruling: an import or a removal between
                    // confirming this payment and this instant can have moved
                    // what `prepared.sender_source` resolves to -- a removed
                    // slot's number is reused by the next one down (Ruling
                    // 4). Signing only after the seed's own address still
                    // matches what was confirmed is what stops the wallet
                    // from signing for one address with another address's
                    // key.
                    if alphanumeric_gui::model::address_for_seed(&seed) != prepared.sender {
                        wallet.send.error = Some(
                            "The key for that address changed while this payment was being \
                             prepared. Start it again."
                                .into(),
                        );
                        wallet.send.stage = SendStage::Compose;
                        return Task::none();
                    }
                    crate::view::send::build_submission(
                        &seed,
                        &prepared.sender,
                        &prepared.recipient,
                        prepared.amount_units,
                        prepared.fee_units,
                        timestamp,
                    )
                };
                wallet.send.error = None;
                wallet.send.outstanding = Some(Arc::new(body));
                wallet.send.stage = SendStage::Sending;
                submit_outstanding(wallet)
            }
            Message::SendRetry => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                if wallet.send.outstanding.is_none()
                    || matches!(wallet.send.stage, SendStage::Sending)
                {
                    return Task::none();
                }
                wallet.send.stage = SendStage::Sending;
                submit_outstanding(wallet)
            }
            Message::SendSubmitted(result) => {
                let Some(wallet) = &mut self.wallet else {
                    return Task::none();
                };
                let verdict = crate::view::send::verdict(&result);
                match &verdict {
                    // Definitive either way: the node has the payment, or it
                    // never admitted it. Nothing is left to re-post.
                    Verdict::Settled { .. } | Verdict::Refused(_) => {
                        wallet.send.outstanding = None;
                    }
                    // Held, so the retry button re-posts these exact bytes.
                    // An `Ambiguous` outcome keeps them too: the user is told
                    // to check before anything else happens, and discarding
                    // the record of what was sent is not how that gets easier.
                    Verdict::Unresolved(_) | Verdict::Ambiguous { .. } => {}
                }
                wallet.send.stage = SendStage::Answered(verdict);
                Task::none()
            }
            Message::SendClear => {
                if let Some(wallet) = &mut self.wallet {
                    // Not while a request is actually in flight: its answer is
                    // about to arrive and would land on a cleared screen.
                    if matches!(
                        wallet.send.stage,
                        SendStage::Sending | SendStage::Checking(_)
                    ) {
                        return Task::none();
                    }
                    // Whether the typed fields survive is decided here, from
                    // the outcome, rather than by which button was pressed --
                    // a view that got this wrong would be leaving a payment
                    // that may already have gone through pre-filled and one
                    // click from going again.
                    let keep = match &wallet.send.stage {
                        SendStage::Answered(verdict) => crate::view::send::keeps_the_form(verdict),
                        _ => false,
                    };
                    if keep {
                        wallet.send.return_to_form();
                    } else {
                        wallet.send.reset();
                    }
                }
                Task::none()
            }

            Message::CaptureScreenshot => {
                // A wall-clock timer fired regardless of whether a frame had
                // actually been drawn, so on a slow start it could -- and did
                // -- reuse whatever the window was cleared to. `frames()`
                // only fires on `RedrawRequested`, and iced_winit broadcasts
                // that event to subscriptions only after `interface.draw()`
                // has already filled the renderer for that same frame
                // (`iced_winit-0.14.0/src/lib.rs`, the `present_span` block
                // follows `draw_span`) -- so every landing here already
                // corresponds to a real paint. `SCREENSHOT_MIN_FRAMES` is a
                // margin on top of that guarantee, not a substitute for it.
                self.screenshot_frames_seen += 1;
                if self.screenshot_frames_seen < SCREENSHOT_MIN_FRAMES {
                    return Task::none();
                }
                // `latest()` is `Task<Option<Id>>` and `Task<Option<T>>::and_then`
                // drops the `None` case for us (`iced_runtime-0.14.0/src/task.rs:360`),
                // so no window means no screenshot and no error either -- which is
                // right for a developer aid.
                iced::window::latest()
                    .and_then(iced::window::screenshot)
                    .map(Message::ScreenshotReady)
            }
            Message::ScreenshotReady(shot) => {
                if let Some(dir) = &self.screenshot_dir {
                    let path = dir.join(format!("{}.png", self.screen.file_stem()));
                    // A failed capture must not take the app down: this is a
                    // developer aid, and the screen behind it is still correct
                    // whether or not the PNG lands.
                    if let Err(error) = write_png(&path, &shot) {
                        eprintln!("screenshot {}: {error}", path.display());
                    }
                }
                self.screenshot_taken = true;
                Task::none()
            }

            Message::ConsoleTick => {
                if self.wallet.is_none() {
                    return Task::none();
                }
                let now = std::time::Instant::now();
                let mut tasks = Vec::new();

                // Process metrics: cheap and local, so every tick (2s).
                // `None` in `External` mode and whenever the owned process
                // is not actually `Running` -- there is no `/proc/<pid>` to
                // read yet (or any more).
                let running_pid = if self.node_source == NodeSource::Owned {
                    match self.supervisor.as_ref().map(|s| s.state()) {
                        Some(node::NodeState::Running { pid }) => Some(pid),
                        _ => None,
                    }
                } else {
                    None
                };
                match running_pid {
                    Some(pid) => {
                        if let Some(sample) = alphanumeric_gui::proc::read_cpu_sample(pid) {
                            self.cpu_percent = self.prev_cpu_sample.and_then(|prev| {
                                alphanumeric_gui::proc::cpu_percent(&prev, &sample)
                            });
                            self.prev_cpu_sample = Some(sample);
                        }
                        self.rss_kib = alphanumeric_gui::proc::read_rss_kib(pid);
                    }
                    None => {
                        self.cpu_percent = None;
                        self.rss_kib = None;
                        self.prev_cpu_sample = None;
                    }
                }

                // The node screen's log tail: real file I/O, so only read
                // while that screen is actually the one on top -- every
                // other tick (every other screen with a console strip) does
                // not pay for it.
                if self.node_source == NodeSource::Owned && self.screen == Screen::Node {
                    self.node_log_tail = self
                        .supervisor
                        .as_ref()
                        .map(|s| s.log_tail(20))
                        .unwrap_or_default();
                }

                // `/stats` (an HTTP read on the child's own loopback port --
                // Task 4): real work, so every 10s rather than every 2, and
                // only in `Owned` mode while the supervisor reports
                // `Running` -- `External` has no such server to ask, and a
                // reading taken from a process that is not `Running` would
                // describe one that either never existed or no longer does
                // (see the `None` arm below).
                let stats_due = self
                    .last_stats_at
                    .is_none_or(|at| now.duration_since(at) >= std::time::Duration::from_secs(10));
                if self.node_source == NodeSource::Owned {
                    match running_pid {
                        Some(_) => {
                            if stats_due && !self.stats_in_flight {
                                self.stats_in_flight = true;
                                self.last_stats_at = Some(now);
                                if let Some(wallet) = &self.wallet {
                                    let client = wallet.client.clone();
                                    // The port the running node was actually
                                    // started with, frozen at start time --
                                    // not a live reparse of
                                    // `stats_port_input`. See the field's own
                                    // doc comment.
                                    let base = self.node_stats_url.clone();
                                    let epoch = self.node_epoch;
                                    tasks.push(Task::perform(
                                        async move { client.stats(&base).await },
                                        move |result| Message::StatsFetched(epoch, result),
                                    ));
                                }
                            }
                        }
                        None => {
                            // Not `Running` -- known gone, not merely
                            // unreachable for a moment. A kept `/stats`
                            // reading here would assert PEERS, HASHRATE,
                            // MEMPOOL, DIFFICULTY, BLOCK REWARD and AVG BLOCK
                            // are still live measurements of a process that
                            // no longer exists; every one of those cells
                            // already knows how to show an honest `—`.
                            self.node_stats = None;
                            // The strip above them is the same lie, aimed at
                            // SYNCED, HEIGHT, VERSION and the MINING chip:
                            // `wallet.node_status` is fed by `apply_status`,
                            // which keeps the last good value on any
                            // NON-retryable error -- and connection-refused
                            // (what every subsequent poll gets once the
                            // process is gone) classifies as exactly that
                            // (`ApiError::Transport::is_retryable` ==
                            // false), so without this it would otherwise
                            // stay on screen forever. Keeping the last good
                            // value on a failed refetch is right when the
                            // failure is merely transient; it is wrong here,
                            // where the tick already knows why every later
                            // poll will fail the same way.
                            if let Some(wallet) = &mut self.wallet {
                                wallet.node_status = None;
                                wallet.node_status_error = None;
                            }
                        }
                    }
                } else {
                    // A `/stats` request issued a moment ago, while this
                    // wallet was still `Owned`, can land as `StatsFetched(Ok(..))`
                    // AFTER the switch to `External` -- that handler
                    // unconditionally overwrites `node_stats`, and nothing in
                    // `External` mode ever fetches `/stats` again to correct
                    // it. Clearing it here, not just in `ChooseNodeSource`,
                    // closes that race instead of leaving it to reopen the
                    // exact defect this wave fixes with roughly
                    // request-latency-over-10s odds.
                    self.node_stats = None;
                }

                // The data directory's size (a recursive walk): also real
                // work, so also throttled to a 10s cadence -- but its OWN,
                // not gated on `running_pid` the way `/stats` is above. The
                // directory exists, and is just as measurable, whether the
                // process using it is up, still starting, or has already
                // exited -- unlike `/stats`, this is not a reading taken
                // FROM the process, so there is no version of it that
                // describes a process that is not there. Gating this on
                // `Running` (as an earlier pass of this fix accidentally
                // did, by computing it inside the `Some(_)` arm above) would
                // make DISK read `—` before the node's first `Running` tick
                // and freeze the moment it exits, for a number that was
                // never wrong or unavailable in either case.
                if self.node_source == NodeSource::Owned {
                    let disk_due = self.last_disk_at.is_none_or(|at| {
                        now.duration_since(at) >= std::time::Duration::from_secs(10)
                    });
                    if disk_due {
                        self.last_disk_at = Some(now);
                        self.disk_bytes = self
                            .data_dir
                            .as_deref()
                            .and_then(alphanumeric_gui::proc::dir_size_bytes);
                    }
                } else {
                    self.disk_bytes = None;
                }

                // `/explorer/status`, on the same 10s cadence as `/stats` --
                // but only on the screens `PollTick`'s own 10s poll does NOT
                // already cover (Wallet/Receive/Send). Skipping it there
                // avoids double-polling; everywhere else (Node,
                // Settings, History) this is the only thing keeping
                // `wallet.node_status` -- and so STATUS, HASHRATE, BLOCKS
                // THIS SESSION and PAYOUT -- from freezing at whatever the
                // wallet screen last read while the rest of the grid (fed by
                // `/stats`) keeps ticking.
                let status_polled_elsewhere =
                    matches!(self.screen, Screen::Wallet | Screen::Receive | Screen::Send);
                if !status_polled_elsewhere {
                    let status_due = self.last_console_status_at.is_none_or(|at| {
                        now.duration_since(at) >= std::time::Duration::from_secs(10)
                    });
                    if status_due && !self.console_status_in_flight {
                        self.console_status_in_flight = true;
                        self.last_console_status_at = Some(now);
                        if let Some(wallet) = &self.wallet {
                            let client = wallet.client.clone();
                            // Carried through and echoed back so an answer
                            // that lands after `ChooseNodeSource` has since
                            // bumped this can be told apart from a current
                            // one -- see `node_epoch`'s own doc.
                            let epoch = self.node_epoch;
                            tasks.push(Task::perform(
                                async move { client.status().await },
                                move |result| Message::ConsoleStatusFetched(epoch, result),
                            ));
                        }
                    }
                }

                // Supply barely moves -- 60s, and unlike `/stats` it is read
                // through the wallet's own explorer client either way, so it
                // is not gated on `node_source`.
                let supply_due = self
                    .last_supply_at
                    .is_none_or(|at| now.duration_since(at) >= std::time::Duration::from_secs(60));
                if supply_due && !self.supply_in_flight {
                    self.supply_in_flight = true;
                    self.last_supply_at = Some(now);
                    if let Some(wallet) = &self.wallet {
                        let client = wallet.client.clone();
                        tasks.push(Task::perform(
                            async move { client.supply().await },
                            Message::SupplyFetched,
                        ));
                    }
                }

                Task::batch(tasks)
            }
            Message::StatsFetched(epoch, result) => {
                self.stats_in_flight = false;
                // From the process a restart or a source switch just
                // replaced -- see `App::node_epoch`.
                if epoch != self.node_epoch {
                    return Task::none();
                }
                // A failed refetch keeps the last known reading rather than
                // blanking it -- a transient miss on a 10-second poll is not
                // news, and every cell already reads `—` for "never fetched".
                if let Ok(stats) = result {
                    self.node_stats = Some(stats);
                }
                Task::none()
            }
            Message::SupplyFetched(result) => {
                self.supply_in_flight = false;
                if let Ok(supply) = result {
                    self.supply = Some(supply);
                }
                Task::none()
            }
            Message::ConsoleStatusFetched(epoch, result) => {
                self.console_status_in_flight = false;
                // Asked of a node the wallet no longer reads -- see
                // `App::node_epoch`.
                if epoch != self.node_epoch {
                    return Task::none();
                }
                if let Some(wallet) = &mut self.wallet {
                    apply_status(wallet, &mut self.known_gpus, result);
                }
                Task::none()
            }
            Message::WindowResized(width) => {
                self.window_width = width;
                Task::none()
            }
            Message::Quit => iced::exit(),
        }
    }

    /// Why the imported-key list cannot be changed right now (spec H §4.4).
    /// Both the add and the remove re-seal the wallet file, and a payment
    /// being signed or an address save already running must not race that.
    pub fn key_change_blocker(&self) -> Option<&'static str> {
        let wallet = self.wallet.as_ref()?;
        if wallet.send.outstanding.is_some()
            || matches!(
                wallet.send.stage,
                SendStage::Confirm(_) | SendStage::Checking(_) | SendStage::Sending
            )
        {
            return Some("A payment is still waiting for an answer. Resolve it on F3 first.");
        }
        if wallet.adding_address {
            return Some("An address is still being added. Wait for it to finish.");
        }
        None
    }

    /// Re-seal the wallet file from what the wallet now holds: the derived
    /// count and the imported seeds. The only writer of the imported list.
    fn write_keys(&mut self) -> Result<(), String> {
        let Some(wallet) = &self.wallet else {
            return Err("No wallet is open.".into());
        };
        let metadata = keystore_payload(wallet.next_index, &wallet.imported)?;
        storage::save(
            &wallet.wallet_path,
            &wallet.master,
            &metadata,
            wallet.passphrase.as_bytes(),
        )
    }

    /// Why IMPORT WALLET cannot run now, or `None` if it can (spec G §4.2 +
    /// planning ruling 2). Closing the wallet would drop a signed payment
    /// held for an identical retry, or let an address save still running
    /// re-seal the OLD wallet over the imported one.
    pub fn import_blocker(&self) -> Option<&'static str> {
        let wallet = self.wallet.as_ref()?;
        // I2: the rfd save dialog is not modal. Left open through an import,
        // a Save pressed afterward would write wallet B's bytes under the
        // name wallet A's EXPORT suggested.
        if wallet.exporting {
            return Some(
                "A wallet file export is still open. Finish or cancel it before importing \
                 another wallet.",
            );
        }
        if wallet.send.outstanding.is_some()
            || matches!(
                wallet.send.stage,
                SendStage::Checking(_) | SendStage::Sending
            )
        {
            return Some(
                "A payment is still waiting for an answer. Resolve it on F3 before importing \
                 another wallet.",
            );
        }
        if wallet.adding_address {
            return Some(
                "An address is still being added. Wait for it to finish before importing \
                 another wallet.",
            );
        }
        None
    }

    /// Whether `master` is the wallet an import just closed (spec G §4.3).
    fn is_the_replaced_wallet(&self, master: &MasterSeed) -> bool {
        self.importing.as_ref().is_some_and(|context| {
            model::address_for_index(master, 0) == context.replaced_first_address
        })
    }

    /// Whether a setup job that ends in a write to the wallet file is running
    /// right now, or might. Once such a job has been dispatched, cancel
    /// cannot be honoured: `SetupReady` installs whatever it comes back with
    /// regardless of `importing`, so refusing the write after the fact is not
    /// an option -- the only honest move left is to let it finish (controller
    /// ruling, Task 5 fix round 1). `ImportFile` shares one `busy` flag
    /// between OPEN (a read) and USE (the write): there is no way to tell
    /// them apart from here, so both are covered.
    pub(crate) fn setup_writing(&self) -> bool {
        matches!(
            self.setup,
            SetupStage::SetPassphrase { busy: true, .. }
                | SetupStage::ImportFile { busy: true, .. }
        )
    }

    /// Build the wallet screen's state from a completed setup flow and switch
    /// to it. Reached from create, restore, and unlock alike.
    /// Builds `WalletState` from a completed setup flow and switches to it.
    ///
    /// Returns `Err` rather than silently doing nothing on the two ways this
    /// can fail, so the caller can put the active stage back into a state the
    /// user can act on instead of leaving it showing "Working..." forever.
    fn install_wallet(
        &mut self,
        master: MasterSeed,
        next_index: u32,
        status: Option<backend::NodeStatus>,
        passphrase: Zeroizing<String>,
        node_url: &str,
        imported: Vec<Zeroizing<String>>,
    ) -> Result<(), String> {
        let Some(path) = self.wallet_path.clone() else {
            return Err("No home directory available to store a wallet.".into());
        };
        // `Ready::node_url` was already validated with `Client::new` before
        // the async work that produced it was even dispatched, and
        // `Client::new` is pure string parsing with no I/O -- so this can only
        // fail if that invariant is broken elsewhere, not from anything the
        // user did here.
        let client = backend::Client::new(node_url)?;
        let next_index = next_index.max(1);
        let mut addresses: Vec<AddressEntry> = (0..next_index)
            .map(|index| AddressEntry {
                source: AddressSource::Derived(index),
                address: model::address_for_index(&master, index),
                balance_units: None,
                spendable: Spendable::Pending,
                error: None,
                loading: false,
                recent: None,
            })
            .collect();
        // Fix round 1: a reopened wallet must show exactly what it showed
        // before it closed -- every imported row, not just the derived ones.
        // A stored seed that no longer parses is skipped rather than panicking
        // or aborting the whole open; the raw seed stays in `imported`
        // untouched either way, so a later save cannot lose it either.
        for (slot, seed_hex) in imported.iter().enumerate() {
            let Ok(parsed) = alphanumeric_gui::seed::parse_imported_seed(seed_hex) else {
                continue;
            };
            addresses.push(AddressEntry {
                source: AddressSource::Imported(slot),
                address: model::address_for_seed(&parsed),
                balance_units: None,
                spendable: Spendable::Pending,
                error: None,
                loading: false,
                recent: None,
            });
        }
        // Reuses the index-0 address already derived above, rather than
        // deriving it a second time -- an ML-DSA-87 keygen is not free, and
        // the string it produces is already sitting in `addresses[0]`.
        let qr = addresses
            .first()
            .and_then(|entry| iced::widget::qr_code::Data::new(&entry.address).ok());
        self.wallet = Some(WalletState {
            wallet_path: path,
            master,
            client: Arc::new(client),
            passphrase,
            next_index,
            imported,
            addresses,
            active: 0,
            node_status: status,
            node_status_error: None,
            poll_in_flight: false,
            fetch_in_flight: false,
            adding_address: false,
            save_error: None,
            qr,
            send: SendState::new(),
            reveal: RevealState::Idle,
            history: None,
            master_reveal: MasterRevealState::Idle,
            exporting: false,
            export_result: None,
            import_confirm: None,
            import_key: ImportKeyState::Idle,
            removing_key: None,
        });
        self.screen = Screen::Wallet;
        Ok(())
    }

    /// Whether this screen shows the tab strip and console. `view()` and
    /// `subscription()` must agree on the **same** answer -- keeping the
    /// same match in two places means a new full-screen view can get fixed
    /// in only one of them, leaving F-keys alive on a screen that
    /// shouldn't have them.
    fn tabs_visible(&self) -> bool {
        !matches!(self.screen, Screen::Setup | Screen::Startup)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let inner = match self.screen {
            Screen::Setup => crate::view::setup::view(self),
            Screen::Startup => crate::view::startup::startup_view(
                &self.node_phase,
                &self.sync_rate,
                &self.startup_log,
            ),
            Screen::Wallet => crate::view::wallet::view(self),
            Screen::Receive => crate::view::receive::view(self),
            Screen::Send => crate::view::send::view(self),
            Screen::History => crate::view::history::view(self),
            Screen::Mining => crate::view::mining::view(self),
            Screen::Node => crate::view::node::view(self),
            Screen::Settings => crate::view::settings::view(self),
        };
        // No wallet means no tabs and no console either: nothing to send
        // yet, no node to watch.
        if !self.tabs_visible() {
            return inner;
        }
        let data = self.console_data();
        iced::widget::column![
            iced::widget::container(crate::view::console::header_bar(&data))
                .padding(iced::Padding::ZERO.top(8).left(8).right(8)),
            iced::widget::container(crate::view::console::meter_grid(&data)).padding([6, 8]),
            iced::widget::container(inner).height(iced::Length::Fill),
            iced::widget::container(crate::view::tabs::command_bar(self.screen)).padding(8),
        ]
        .into()
    }

    /// Assembles the console strip's data from wherever each piece actually
    /// lives: node status and `maturing_units` off the wallet's addresses,
    /// `/stats` and supply off this tick's last reads, process metrics off
    /// the last `ConsoleTick`.
    fn console_data(&self) -> crate::view::console::ConsoleData<'_> {
        let status = self.wallet.as_ref().and_then(|w| w.node_status.as_ref());
        crate::view::console::ConsoleData {
            status,
            stats: self.node_stats.as_ref(),
            supply_units: self.supply.as_ref().map(|s| s.supply_units),
            maturing_units: self.maturing_units(),
            cpu_share: self.cpu_share(),
            mem_share: self.mem_share(),
            rss_kib: self.rss_kib,
            disk_bytes: self.disk_bytes,
            mining: mining_from_status(status),
            external: self.node_source == NodeSource::External,
        }
    }

    /// Total balance not yet spendable, across every address. `None` if any
    /// address's balance or spendable overlay is not known yet -- a partial
    /// sum would silently be a partial fact wearing a whole one's label.
    ///
    /// `pub`: `view::mining` shows the same MATURING figure the console grid
    /// does.
    pub fn maturing_units(&self) -> Option<i128> {
        let wallet = self.wallet.as_ref()?;
        let pairs: Option<Vec<(i128, Option<i128>)>> = wallet
            .addresses
            .iter()
            .map(|entry| {
                entry.balance_units.map(|balance| {
                    let spendable = match entry.spendable {
                        Spendable::Known(units) => Some(units),
                        Spendable::Pending | Spendable::Unavailable => None,
                    };
                    (balance, spendable)
                })
            })
            .collect();
        backend::maturing_units(&pairs?)
    }

    /// The newest `want` rows across every address, from pages already
    /// fetched. `complete: false` while any address has not answered.
    pub fn recent_activity(&self, want: usize) -> alphanumeric_gui::activity::Recent {
        let Some(wallet) = &self.wallet else {
            return alphanumeric_gui::activity::Recent {
                rows: Vec::new(),
                complete: true,
                exhaustive: true,
            };
        };
        let addresses: Vec<String> = wallet.addresses.iter().map(|e| e.address.clone()).collect();
        let pages: Vec<Option<backend::AddressPage>> =
            wallet.addresses.iter().map(|e| e.recent.clone()).collect();
        alphanumeric_gui::activity::recent_rows(&addresses, &pages, want)
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        // Only the active address is polled on a timer (spec 4.2); everything
        // else is refreshed on entry or by hand. Skipping the tick while a
        // request is already outstanding is what keeps this from overlapping
        // with itself into the very 503s it exists to avoid.
        // Send is on this list because the spendable figure it refuses to
        // sign above is only as good as its last poll: a send screen left
        // open against a frozen balance would be checking a payment against a
        // ceiling from whenever the user last visited the wallet.
        let polling = matches!(self.screen, Screen::Wallet | Screen::Receive | Screen::Send)
            && self
                .wallet
                .as_ref()
                .is_some_and(|wallet| !wallet.poll_in_flight);
        let poll = if polling {
            iced::time::every(std::time::Duration::from_secs(10)).map(|_| Message::PollTick)
        } else {
            iced::Subscription::none()
        };

        // The startup screen's 1-second poll. Gated off once `node_phase` is
        // `Failed` -- that state is terminal until `Message::RetryNode` (the
        // supervisor thread has already returned), so polling into it would
        // keep hitting a node that is not there and never surface the "could
        // not find the binary" message the user needs to see.
        let node_tick = if matches!(self.screen, Screen::Startup)
            && !self.startup_poll_in_flight
            && !matches!(self.node_phase, startup::Phase::Failed { .. })
        {
            iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::NodeTick)
        } else {
            iced::Subscription::none()
        };

        // Developer flag only. `window::frames()` fires on every real
        // `RedrawRequested`, which is what makes it safe to capture from at
        // all -- a wall-clock timer has no idea whether a frame has been
        // drawn yet, and on a slow start it fired before one had been. It
        // keeps firing at the refresh rate until `screenshot_taken` ends the
        // subscription below, so landing more than `SCREENSHOT_MIN_FRAMES`
        // times before that happens is expected, not a bug -- the write is
        // idempotent (same path, same screen), so the extra landings are
        // harmless.
        let capture = if self.screenshot_dir.is_some() && !self.screenshot_taken {
            iced::window::frames().map(|_| Message::CaptureScreenshot)
        } else {
            iced::Subscription::none()
        };

        // The tab bar (and so its F-key shortcuts) exists only on the same
        // screens `view` draws it on -- `tabs_visible()` is the one place
        // that decides which those are, shared with `view` rather than
        // matched again here. Without this gate, pressing F1 while
        // `Startup` is still showing (reachable with a wallet already
        // installed, e.g. mid-`RetryNode`) would leave the boot screen
        // before `NodeStatusFetched`'s own auto-navigate ever fires, taking
        // the user off a screen that is supposed to stay put until the node
        // is ready.
        let tabs_visible = self.tabs_visible();

        // Only keys a widget hasn't consumed reach here
        // (iced_futures/src/keyboard.rs). So even mid-text-entry, the input
        // field eats the characters and only F-keys arrive here.
        let keys = if tabs_visible {
            iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { ref key, .. } => {
                    if crate::view::tabs::is_quit_shortcut(key) {
                        Some(Message::Quit)
                    } else {
                        crate::view::tabs::shortcut(key).map(Message::Show)
                    }
                }
                _ => None,
            })
        } else {
            iced::Subscription::none()
        };

        // The console strip only exists once a wallet does AND the tab bar
        // is actually on screen -- Startup with a wallet already installed
        // (the `RetryNode` case above) has neither, and ticking there would
        // poll a node still coming up for a strip nobody sees.
        let console_tick = if self.wallet.is_some() && tabs_visible {
            iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::ConsoleTick)
        } else {
            iced::Subscription::none()
        };

        // Tracks `window_width` for `narrow()` (R1). Unconditional -- every
        // screen with a two-panel layout needs it, and there is no cost to
        // keeping it current on the others too.
        let resize =
            iced::window::resize_events().map(|(_id, size)| Message::WindowResized(size.width));

        iced::Subscription::batch([poll, node_tick, capture, keys, console_tick, resize])
    }
}

/// Writes a captured screenshot as a PNG.
///
/// `Screenshot::rgba` is always RGBA8 in sRGB (`iced_core::window::screenshot`),
/// so the buffer goes to the encoder unchanged -- no colour-space conversion
/// belongs here.
fn write_png(
    path: &std::path::Path,
    shot: &iced::window::Screenshot,
) -> Result<(), Box<dyn std::error::Error>> {
    let buffer = image::RgbaImage::from_raw(shot.size.width, shot.size.height, shot.rgba.to_vec())
        .ok_or("screenshot buffer does not match its reported size")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    buffer.save(path)?;
    Ok(())
}

/// The console strip's mining tuple, from a node status if there is one.
///
/// The node sends `mining` unconditionally whenever it answers at all
/// (`src/a9/node.rs`'s `mining_status_json`) -- only the other five mining fields are omitted
/// while idle. So "not mining" is a known fact (`Some((false, ..))`), not an
/// unknown one; `None` is reserved for "there is no status response at all".
/// Collapsing those two into one `None` would read a node that just told us
/// it is idle as a node we have not heard from.
fn mining_from_status(
    status: Option<&backend::NodeStatus>,
) -> Option<(bool, Option<f64>, Option<String>)> {
    status.map(|s| {
        (
            s.mining.unwrap_or(false),
            s.mining_hps,
            s.mining_backend.clone(),
        )
    })
}

/// One entry of F5's payout drop-down. The drop-down prints `label`; the
/// selection carries `address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayoutChoice {
    pub label: String,
    pub address: String,
}

impl std::fmt::Display for PayoutChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// Keeps the node's GPU roster whenever a status carries one (an 8.0.1 node
/// sends none, and an empty list must not erase a roster already seen).
fn note_gpus(known: &mut Vec<backend::GpuDevice>, status: &backend::NodeStatus) {
    if !status.gpu_devices.is_empty() {
        *known = status.gpu_devices.clone();
    }
}

/// Applies a status fetch to wallet state. A retryable error is not a failure
/// (spec 4.2): the last known status stays on screen, unchanged.
fn apply_status(
    wallet: &mut WalletState,
    known_gpus: &mut Vec<backend::GpuDevice>,
    status: Result<backend::NodeStatus, backend::ApiError>,
) {
    match status {
        Ok(status) => {
            note_gpus(known_gpus, &status);
            wallet.node_status = Some(status);
            wallet.node_status_error = None;
        }
        Err(error) if error.is_retryable() => {}
        Err(error) => wallet.node_status_error = Some(error.to_string()),
    }
}

/// Applies an address fetch to one entry.
///
/// A retryable error is never shown as a failure (task 8), but `loading` is
/// only left set when something will actually retry this row soon --
/// otherwise "Updating..." would describe a retry that is not scheduled.
/// `scheduled_retry` is true only for the active address under `PollTick`,
/// which really does come back around in ~10 seconds; `RefreshAll` fetches
/// every other row with no such follow-up, so a retryable result there just
/// keeps the last known values with no special indicator until the next
/// explicit refresh.
fn apply_address_result(
    entry: &mut AddressEntry,
    result: Result<backend::AddressState, backend::ApiError>,
    scheduled_retry: bool,
) {
    match result {
        Ok(state) => {
            entry.recent = Some(state.as_page());
            entry.balance_units = Some(state.balance_units);
            entry.spendable = match state.spendable_units {
                Some(units) => Spendable::Known(units),
                None => Spendable::Unavailable,
            };
            entry.error = None;
            entry.loading = false;
        }
        Err(error) if error.is_retryable() => entry.loading = scheduled_retry,
        Err(error) => {
            entry.error = Some(error.to_string());
            entry.loading = false;
        }
    }
}

/// Clears `busy` and sets `error` on whichever setup stage is active, if it is
/// one that has those fields. Used so a failure can never leave a stage
/// silently stuck showing "Working...".
fn set_stage_error(setup: &mut SetupStage, message: String) {
    match setup {
        SetupStage::SetPassphrase { busy, error, .. } => {
            *busy = false;
            *error = Some(message);
        }
        SetupStage::Unlock { busy, error, .. } => {
            *busy = false;
            *error = Some(message);
        }
        SetupStage::ImportFile { busy, error, .. } => {
            *busy = false;
            *error = Some(message);
        }
        _ => {}
    }
}

/// Post the body already held in `send.outstanding`, exactly as it is.
///
/// The one path to `/v2/submit-tx`, used by both the first attempt and every
/// retry, so a retry cannot accidentally become a rebuild: the idempotency key
/// and the signed transaction are whatever they were the first time, which is
/// the only reason the node can tell a retry from a second payment.
fn submit_outstanding(wallet: &WalletState) -> Task<Message> {
    let Some(body) = wallet.send.outstanding.clone() else {
        return Task::none();
    };
    let client = wallet.client.clone();
    Task::perform(
        async move { client.submit(&body).await },
        Message::SendSubmitted,
    )
}

/// Best-effort connectivity check. `None` means the node could not be reached
/// -- not treated as a setup failure, since the wallet itself is still valid;
/// the wallet screen shows the "no node reachable" guidance instead of a toast.
async fn probe_node(node_url: &str) -> Option<backend::NodeStatus> {
    backend::Client::new(node_url).ok()?.status().await.ok()
}

/// `Message::Unlock`'s task: read the wallet file at `path` and turn its
/// metadata -- derived count AND every imported seed (spec H fix round 1;
/// dropping the imported list here is how a reopened wallet used to have
/// `+ ADD` erase it) -- into a `Ready`.
async fn open_wallet_for_unlock(
    path: PathBuf,
    passphrase: Zeroizing<String>,
    node_url: String,
) -> Result<Ready, SetupFailure> {
    let (load_path, load_pass) = (path.clone(), passphrase.clone());
    let (master, payload) =
        tokio::task::spawn_blocking(move || storage::load(&load_path, load_pass.as_bytes()))
            .await
            .map_err(|error| SetupFailure::Other(error.to_string()))?
            .map_err(SetupFailure::Other)?;
    let mut metadata: WalletMetadata = serde_json::from_slice(&payload)
        .map_err(|error| SetupFailure::Other(format!("Wallet metadata is corrupt: {error}")))?;
    let next_index = metadata.next_index.max(1);
    // `mem::take`, not `.clone()`: `ImportedKey::drop` zeroizes `seed`, so a
    // clone would leave an unwrapped, un-zeroized copy of a spendable key.
    let imported: Vec<Zeroizing<String>> = metadata
        .imported
        .iter_mut()
        .map(|key| Zeroizing::new(std::mem::take(&mut key.seed)))
        .collect();
    let status = probe_node(&node_url).await;
    Ok(Ready {
        master,
        next_index,
        status,
        passphrase,
        node_url,
        archived: None,
        imported,
    })
}

/// Spec G §3.5/§4.1/§4.4: put an opened wallet file at the wallet path
/// exactly as it was read -- not re-sealed, so its passphrase and KDF
/// settings stay its own -- setting aside whatever wallet was there.
async fn use_imported_file(
    wallet_path: PathBuf,
    stamp: String,
    file: Arc<alphanumeric_gui::import::WalletFile>,
    passphrase: Zeroizing<String>,
    node_url: String,
) -> Result<Ready, SetupFailure> {
    let written = Arc::clone(&file);
    let archived = tokio::task::spawn_blocking(move || {
        storage::replace_archiving(&wallet_path, &stamp, |target| {
            storage::write_envelope(target, &written.envelope)
        })
    })
    .await
    .map_err(|error| SetupFailure::Other(error.to_string()))?
    .map_err(SetupFailure::Other)?;
    let status = probe_node(&node_url).await;
    Ok(Ready {
        master: file.master.clone(),
        next_index: file.next_index,
        status,
        passphrase,
        node_url,
        archived,
        // Sub-project G's file import must carry sub-project H's imported
        // keys too -- otherwise importing a wallet FILE silently drops every
        // key it holds.
        imported: file.imported.clone(),
    })
}

/// `storage::archive_path`'s stamp: local time, `YYYYMMDD-HHMMSS`, which sorts
/// the way it reads.
fn archive_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// The startup screen's 1-second poll. Unlike `probe_node`, the error is kept
/// rather than discarded -- `Message::NodeStatusFetched` needs to know the
/// difference between "not reachable yet" and "reachable but behind" only
/// through the phase it computes, and discarding the `Err` here would make
/// that computation see the exact same `None` in both cases.
async fn poll_node_status(node_url: String) -> Result<backend::NodeStatus, backend::ApiError> {
    let client = backend::Client::new(&node_url).map_err(backend::ApiError::Transport)?;
    client.status().await
}

/// The directory the running wallet executable lives in -- where an
/// unconfigured node binary is looked for, next to it. Shared by
/// `build_node_config` and `App::node_binary_display` so the two cannot name
/// two different places.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// Locates the wallet's own node binary and data directory. A free function,
/// not a method -- it needs no `&self`, and lives next to its only caller,
/// `App::ensure_owned_node`.
fn build_node_config(
    configured_binary: Option<&Path>,
    data_dir: Option<&Path>,
    p2p_port: u16,
    explorer_port: u16,
    stats_port: u16,
) -> Result<node::NodeConfig, String> {
    let dir =
        exe_dir().ok_or_else(|| "Could not determine the wallet's own directory.".to_string())?;
    let binary = node::locate_binary(configured_binary, &dir)?;
    let data_dir = data_dir
        .map(Path::to_path_buf)
        .ok_or_else(|| "No home directory available for the node's data.".to_string())?;
    Ok(node::NodeConfig {
        binary,
        data_dir,
        p2p_port,
        explorer_port,
        stats_port,
        mining: None,
    })
}

/// Extracts `host:port` from a configured node URL for the exact command line
/// that turns on the explorer API (spec 4.1). It is opt-in behind an
/// environment variable, and this is where most people get stuck on first run.
pub fn node_launch_command(node_url: &str) -> String {
    let trimmed = node_url.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    format!("ALPHANUMERIC_EXPLORER_API={without_scheme} ./alphanumeric")
}

/// True when the user has typed back exactly the master seed they were shown.
///
/// Trimmed and case-insensitive, matching what `MasterSeed::decode` accepts, so
/// a backup written out in block capitals confirms here and restores later.
/// How many characters the backup quiz asks for.
///
/// Four is enough that guessing is hopeless (16^4 over a hex alphabet) and few
/// enough that answering from a saved copy is a moment's work. The whole
/// 76-character string was the previous gate, and it was not answerable by hand
/// at all -- there was no copy button, the display was not selectable, and the
/// only way through was to transcribe 76 hex characters without a typo.
pub const BACKUP_QUIZ_LEN: usize = 4;

/// The `a9m1` prefix every master seed carries. Asking for a character inside
/// it would prove nothing, so the quiz never picks from there.
const BACKUP_QUIZ_SKIP_PREFIX: usize = 4;

/// Pick the character positions the quiz will ask for, given the seed's length.
///
/// Random per wallet: fixed positions would be learnable, and someone who
/// clicked through this screen once would know where to look without ever
/// saving anything. Returned sorted so the prompt reads left to right.
pub fn backup_quiz_positions(seed_len: usize, rng: &mut impl rand::Rng) -> Vec<usize> {
    let first = BACKUP_QUIZ_SKIP_PREFIX.min(seed_len);
    let mut available: Vec<usize> = (first..seed_len).collect();
    if available.len() <= BACKUP_QUIZ_LEN {
        return available;
    }
    let mut picked = Vec::with_capacity(BACKUP_QUIZ_LEN);
    for _ in 0..BACKUP_QUIZ_LEN {
        picked.push(available.remove(rng.gen_range(0..available.len())));
    }
    picked.sort_unstable();
    picked
}

/// Whether the typed characters match the seed at the asked-for positions.
///
/// Whitespace is the user's formatting, not their answer -- someone reading
/// four characters off a printout will space them out. Case is not an answer
/// either: the encoding is lowercase and `decode` accepts either, so a shouted
/// copy is the same copy.
pub fn backup_quiz_passed(shown: &str, positions: &[usize], typed: &str) -> bool {
    let answer: Vec<char> = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if answer.len() != positions.len() || positions.is_empty() {
        return false;
    }
    let seed: Vec<char> = shown.chars().collect();
    positions.iter().zip(answer).all(|(&at, given)| {
        seed.get(at)
            .is_some_and(|expected| expected.eq_ignore_ascii_case(&given))
    })
}

/// Consecutive unused indices that end an address-discovery scan (spec 3.5).
const DISCOVERY_GAP: u32 = 20;

/// The first index past everything that has been used.
///
/// Walks until it has seen `gap` consecutive unused indices, because a restore
/// from a photograph carries no record of how many addresses existed. Stopping
/// at the first unused one would silently drop every address after a gap.
pub fn discover_next_index(used: &dyn Fn(u32) -> bool, gap: u32) -> u32 {
    let mut index = 0u32;
    let mut last_used: Option<u32> = None;
    let mut run = 0u32;
    while run < gap {
        if used(index) {
            last_used = Some(index);
            run = 0;
        } else {
            run += 1;
        }
        index += 1;
    }
    last_used.map(|i| i + 1).unwrap_or(0)
}

/// The two errors that mean "try again", each with its own backoff (spec 4.2:
/// 503 "chain busy" gets exponential backoff, 429 "rate_limited" gets longer
/// still). Kept distinct from `is_retryable()` because the two need different
/// delays, not just the same "retry" verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryKind {
    /// 503: the chain lock was contended. Usually clears in well under a
    /// second, so backoff starts short.
    Busy,
    /// 429: the token bucket in front of `/explorer/address` is empty. It
    /// refills far slower than a lock is ever held, so backoff both starts
    /// and caps considerably higher than `Busy`'s.
    RateLimited,
}

impl RetryKind {
    fn of(error: &backend::ApiError) -> Option<Self> {
        match error {
            backend::ApiError::Busy => Some(RetryKind::Busy),
            backend::ApiError::RateLimited => Some(RetryKind::RateLimited),
            _ => None,
        }
    }
}

/// Attempts allowed before a retryable error is treated as a real failure.
///
/// Giving up is safe here: `discover_via_network` returns before
/// `storage::save` ever runs (see its caller), so a lock or a token bucket
/// that never clears cannot leave a wallet on disk with a truncated
/// `next_index` -- it only sends the user to the "no node reachable"
/// guidance screen, where Retry starts the whole scan over.
const RETRY_ATTEMPTS: u32 = 6;

/// The delay before retry attempt `attempt` (0-based), pure so the schedule
/// itself is testable without a live node.
///
/// Doubles each attempt from a per-kind starting point and is capped so
/// neither one sleep nor the total wait before `RETRY_ATTEMPTS` gives up grows
/// without bound. With `RETRY_ATTEMPTS = 6`: `Busy` sleeps 500ms, 1s, 2s, 4s,
/// then 8s twice (~23.5s total) before giving up; `RateLimited` sleeps 2s,
/// 4s, 8s, 16s, then 30s twice (~90s total). Long enough that a lock or a
/// bucket clearing on its own is given a real chance, short enough that a
/// restore that truly cannot proceed says so within about a minute and a
/// half rather than parking "Working..." indefinitely.
fn retry_delay(kind: RetryKind, attempt: u32) -> std::time::Duration {
    let (start_ms, cap_ms): (u64, u64) = match kind {
        RetryKind::Busy => (500, 8_000),
        RetryKind::RateLimited => (2_000, 30_000),
    };
    // `attempt.min(20)` keeps the shift itself from overflowing long before
    // the millisecond value would ever approach the cap.
    let scaled = start_ms.saturating_mul(1u64 << attempt.min(20));
    std::time::Duration::from_millis(scaled.min(cap_ms))
}

/// One index's authoritative usage, or a reason it cannot be determined yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeOutcome {
    /// The index answered, and can be trusted: `true` if the address has
    /// ever been used.
    Answered(bool),
    /// The index cannot answer authoritatively -- unbuilt, mid-rebuild, or
    /// behind the chain height this scan needs. NOT the same as "unused".
    NotReady,
}

/// Whether an address's state can be trusted, and if so, whether it has ever
/// been used. Pure, so the freshness rule itself is directly testable.
///
/// `history_available`/`index_ready` say the index has SOME metadata for this
/// address; `index_height` says how current it is. All three are required,
/// because the node's index write is documented fail-open
/// (blockchain.rs: a skipped write "only leaves the meta behind the tip,
/// which ensure_address_tx_index heals") -- a node can serve
/// `history_available: true` off a stale index for as long as it stays up, so
/// `history_available` alone cannot tell a truly-unused address apart from
/// one whose recent activity the index has not caught up to yet.
fn interpret_address_state(state: &backend::AddressState, chain_height: u64) -> ProbeOutcome {
    let authoritative = state.history_available
        && state.index_ready
        && state
            .index_height
            .is_some_and(|height| height >= chain_height);
    if !authoritative {
        return ProbeOutcome::NotReady;
    }
    let used = state.balance_units > 0
        || state.spendable_units.is_some_and(|units| units > 0)
        || state
            .transactions
            .as_ref()
            .is_some_and(|txs| !txs.is_empty());
    ProbeOutcome::Answered(used)
}

async fn probe_address(
    client: &backend::Client,
    address: &str,
    chain_height: u64,
) -> Result<ProbeOutcome, backend::ApiError> {
    let state = client.address(address).await?;
    Ok(interpret_address_state(&state, chain_height))
}

/// Why address discovery could not finish.
#[derive(Debug)]
enum ScanFailure {
    /// A real node/HTTP error: non-retryable, or retried past the cap.
    Api(backend::ApiError),
    /// The address index never answered authoritatively within the retry
    /// budget. Deliberately NOT retried the way `Busy`/`RateLimited` are:
    /// there is no way to know how far behind an indexer is, so guessing at
    /// a backoff for it would either give up too early or hang too long.
    /// Failing immediately and sending the user to Retry is the honest
    /// version of "the user retries" -- a restore that refuses to finish
    /// against a lagging node is correct, not broken.
    IndexNotReady,
}

impl std::fmt::Display for ScanFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanFailure::Api(error) => write!(f, "{error}"),
            ScanFailure::IndexNotReady => write!(
                f,
                "The node's address index has not caught up to the current chain height."
            ),
        }
    }
}

/// The gap-limited scan itself, over an injected `fetch` and `wait`.
///
/// Injected rather than calling `backend::Client` directly so the two
/// properties that protect funds here -- a retryable result retries the SAME
/// index and is never counted as unused, and the attempt cap terminates the
/// loop -- are testable without a live node or real delays, the way
/// `retry_delay` already is.
async fn scan_for_next_index<F, FFut, W, WFut>(
    gap: u32,
    fetch: F,
    wait: W,
) -> Result<u32, ScanFailure>
where
    F: Fn(u32) -> FFut,
    FFut: std::future::Future<Output = Result<ProbeOutcome, backend::ApiError>>,
    W: Fn(std::time::Duration) -> WFut,
    WFut: std::future::Future<Output = ()>,
{
    let mut used_flags: Vec<bool> = Vec::new();
    let mut index: u32 = 0;
    let mut run = 0u32;
    while run < gap {
        let mut attempt = 0u32;
        let used = loop {
            match fetch(index).await {
                Ok(ProbeOutcome::Answered(used)) => break used,
                // Fails closed immediately: see `ScanFailure::IndexNotReady`.
                Ok(ProbeOutcome::NotReady) => return Err(ScanFailure::IndexNotReady),
                Err(error) => {
                    let Some(kind) = RetryKind::of(&error) else {
                        return Err(ScanFailure::Api(error));
                    };
                    if attempt >= RETRY_ATTEMPTS {
                        return Err(ScanFailure::Api(error));
                    }
                    wait(retry_delay(kind, attempt)).await;
                    attempt += 1;
                }
            }
        };
        used_flags.push(used);
        if used {
            run = 0;
        } else {
            run += 1;
        }
        index = index.saturating_add(1);
        // A short pause between indices, so a restore does not trip the
        // per-process token bucket in front of `/explorer/address` (spec 4.2).
        wait(std::time::Duration::from_millis(200)).await;
    }
    let found = discover_next_index(
        &|i: u32| used_flags.get(i as usize).copied().unwrap_or(false),
        gap,
    );
    Ok(found.max(1))
}

/// Scans the node for `next_index`, the real caller of [`discover_next_index`].
///
/// The result is clamped to at least 1: a wallet always has address 0 to show,
/// even when nothing has ever been used yet. `chain_height` is the height to
/// require each address's index to have caught up to before trusting its
/// answer -- the caller's own `/explorer/status` height, fetched once before
/// the scan starts.
async fn discover_via_network(
    client: &backend::Client,
    master: &MasterSeed,
    chain_height: u64,
) -> Result<u32, ScanFailure> {
    scan_for_next_index(
        DISCOVERY_GAP,
        |index| {
            let address = model::address_for_index(master, index);
            async move { probe_address(client, &address, chain_height).await }
        },
        tokio::time::sleep,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The launch the node gets: nothing unless mining is on and an address
    // is chosen; the GPU list only when some GPU was switched off.
    #[test]
    fn the_mining_launch_follows_the_settings_and_the_reported_gpus() {
        let (mut app, _) = App::new();
        app.settings.mining.enabled = true;
        app.settings.mining.address = Some("089b61914421754ca33e03b42c6dcd9c709c6cc1".into());
        app.settings.mining.disabled_gpus = vec![1];
        app.known_gpus = vec![
            backend::GpuDevice {
                index: 0,
                name: "A".into(),
            },
            backend::GpuDevice {
                index: 1,
                name: "B".into(),
            },
            backend::GpuDevice {
                index: 2,
                name: "C".into(),
            },
        ];
        let launch = app.mining_launch().expect("enabled with an address");
        assert_eq!(launch.gpu_devices, Some(vec![0, 2]));
        assert_eq!(launch.backend, settings::Backend::Gpu);
        app.settings.mining.disabled_gpus.clear();
        assert_eq!(app.mining_launch().unwrap().gpu_devices, None);
        app.settings.mining.backend = settings::Backend::Cpu;
        app.settings.mining.disabled_gpus = vec![1];
        assert_eq!(
            app.mining_launch().unwrap().gpu_devices,
            None,
            "CPU: no GPU list"
        );
        app.settings.mining.enabled = false;
        assert!(app.mining_launch().is_none());
        app.settings.mining.enabled = true;
        app.settings.mining.address = None;
        assert!(
            app.mining_launch().is_none(),
            "no address, nothing to mine to"
        );
    }

    // Node settings from the file land in the inputs the node is built from.
    #[test]
    fn node_settings_from_the_file_seed_the_inputs() {
        let (mut app, _) = App::new();
        let mut s = settings::Settings::default();
        s.node.p2p_port = Some(7180);
        s.node.binary = Some("/opt/node".into());
        app.apply_loaded_settings(s);
        assert_eq!(app.p2p_port_input, "7180");
        assert_eq!(app.explorer_port_input, "");
        assert_eq!(app.node_binary_input, "/opt/node");
        assert_eq!(app.resolved_ports().0, 7180);
    }

    // START remembers the choice, records the launch the node got, and
    // notices when an edit since then has not been applied.
    #[test]
    fn start_saves_the_choice_and_relaunches_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.settings_path = Some(dir.path().join("settings.json"));
        app.node_source = NodeSource::Owned;
        let address = address_at(&app, 0);
        let _ = app.update(Message::MiningAddressPicked(address.clone()));
        let _ = app.update(Message::MiningBackendPicked(settings::Backend::Cpu));
        let _ = app.update(Message::MiningThreadsChanged("3".into()));
        let _ = app.update(Message::MiningStart);
        assert!(app.settings.mining.enabled);
        assert_eq!(app.settings.mining.cpu_threads, Some(3));
        assert_eq!(
            app.settings.mining.address.as_deref(),
            Some(address.as_str())
        );
        let saved = settings::load(&dir.path().join("settings.json"));
        assert!(saved.mining.enabled);
        assert_eq!(saved.mining.backend, settings::Backend::Cpu);
        assert_eq!(
            app.launched_mining.as_ref().map(|l| l.backend),
            Some(settings::Backend::Cpu)
        );
        assert!(!app.mining_dirty());
        let _ = app.update(Message::MiningThreadsChanged("5".into()));
        assert!(app.mining_dirty(), "an edit not yet applied");
        let _ = app.update(Message::MiningStop);
        assert!(!app.settings.mining.enabled);
        assert!(app.launched_mining.is_none());
        assert!(
            !settings::load(&dir.path().join("settings.json"))
                .mining
                .enabled
        );
    }

    // A saved address this wallet does not hold (the wallet was replaced
    // since) is not mined to: START asks for a choice instead.
    #[test]
    fn start_refuses_a_saved_address_the_wallet_does_not_hold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.settings.mining.address = Some("ff".repeat(20));
        let _ = app.update(Message::MiningStart);
        assert!(!app.settings.mining.enabled);
        assert!(app.mining_error.is_some());
    }

    #[test]
    fn start_without_an_address_refuses_with_a_message() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::MiningStart);
        assert!(!app.settings.mining.enabled);
        assert!(app
            .mining_error
            .as_deref()
            .unwrap_or("")
            .contains("address"));
    }

    // Switching a GPU off is remembered by its index; on again removes it.
    #[test]
    fn a_gpu_switch_edits_the_disabled_list() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::MiningGpuToggled(1, false));
        let _ = app.update(Message::MiningGpuToggled(2, false));
        assert_eq!(app.settings.mining.disabled_gpus, vec![1, 2]);
        let _ = app.update(Message::MiningGpuToggled(1, true));
        assert_eq!(app.settings.mining.disabled_gpus, vec![2]);
    }

    // The drop-down offers every address of the wallet, each labelled the way
    // F1 labels it and shortened to fit one line.
    #[test]
    fn the_payout_choices_are_every_address_labelled_and_shortened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_with_two_addresses(dir.path());
        let choices = app.mining_address_choices();
        assert_eq!(choices.len(), 2);
        for (position, choice) in choices.iter().enumerate() {
            let address = address_at(&app, position);
            assert_eq!(choice.address, address);
            assert_eq!(
                choice.label,
                format!("[{position}] {}", crate::view::kit::short_address(&address))
            );
            assert_eq!(
                choice.to_string(),
                choice.label,
                "what the drop-down prints"
            );
        }
    }

    // The drop-down shows the saved address as selected, and nothing when
    // the saved address is not one of this wallet's (a restored or replaced
    // wallet must not appear to mine to an address it does not hold).
    #[test]
    fn the_selected_payout_is_the_saved_address_when_the_wallet_holds_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        assert_eq!(app.mining_selected_choice(), None);
        let second = address_at(&app, 1);
        let _ = app.update(Message::MiningAddressPicked(second.clone()));
        assert_eq!(
            app.mining_selected_choice().map(|c| c.address),
            Some(second)
        );
        app.settings.mining.address = Some("ff".repeat(20));
        assert_eq!(app.mining_selected_choice(), None);
    }

    // What the miner panel names the payout by: this wallet's label for one
    // of its own addresses, nothing for any other.
    #[test]
    fn the_payout_label_names_only_this_wallets_addresses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_with_two_addresses(dir.path());
        assert_eq!(
            app.payout_label(&address_at(&app, 1)).as_deref(),
            Some("[1]")
        );
        assert_eq!(app.payout_label(&"ff".repeat(20)), None);
    }

    // COPY on the miner panel copies the address the node is mining to.
    #[test]
    fn copy_payout_writes_the_address_it_is_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::CopyPayout(address_at(&app, 0)));
    }

    // What save_settings writes back: the inputs as they stand, empty = None.
    #[test]
    fn save_settings_records_the_inputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, _) = App::new();
        app.settings_path = Some(dir.path().join("settings.json"));
        app.p2p_port_input = "7181".into();
        app.node_binary_input = "  ".into();
        app.save_settings();
        assert!(app.settings_error.is_none());
        let saved = settings::load(&dir.path().join("settings.json"));
        assert_eq!(saved.node.p2p_port, Some(7181));
        assert_eq!(saved.node.binary, None);
    }

    // The hint names the file `node::locate_binary` will actually look
    // for. On Windows that is `alphanumeric.exe`; a hint without the suffix
    // sends the user to put a file there that is never found.
    #[test]
    fn the_unconfigured_binary_hint_names_the_platform_file() {
        let (mut app, _) = App::new();
        app.node_binary_input = String::new();
        let shown = app.node_binary_display();
        assert_eq!(
            Path::new(&shown).file_name().and_then(|name| name.to_str()),
            Some(node::binary_file_name().as_str()),
            "{shown}"
        );
    }
    use alphanumeric_gui::seed::MasterSeed;

    // M8: create and restore both went through `keystore_payload` by hand
    // (`serde_json::to_vec(&WalletMetadata { .. })`) before this fix routed
    // them through the one function `write_keys`/`save_after_add_address`
    // already used. Pinning what it produces for the exact arguments those
    // two call sites pass -- `next_index` and an empty imported list --
    // is the regression test for that dedup: if either literal (`1` for
    // create, `next_index` for restore) or the always-empty imported list
    // ever drifted, this fails alongside the on-disk format assertions
    // elsewhere in this file.
    #[test]
    fn keystore_payload_matches_a_by_hand_metadata_encoding() {
        for next_index in [1u32, 7] {
            let built = keystore_payload(next_index, &[]).expect("encodes");
            let by_hand = serde_json::to_vec(&WalletMetadata {
                next_index,
                imported: Vec::new(),
            })
            .expect("encodes");
            assert_eq!(*built, by_hand);
        }
    }

    /// A wallet installed against a throwaway path, so nothing here can reach
    /// the real `~/.alphanumeric-gui/seed.enc`.
    fn app_with_a_wallet(dir: &std::path::Path) -> App {
        let (mut app, _) = App::new();
        app.wallet_path = Some(dir.join("seed.enc"));
        // Never the real `~/.alphanumeric-gui/node`: every `ConsoleTick` in a
        // test would otherwise walk the data directory of whoever runs the
        // suite. Absent until a test creates it.
        app.data_dir = Some(dir.join("node"));
        app.install_wallet(
            MasterSeed::from_bytes([9u8; 32]),
            1,
            None,
            Zeroizing::new("pass".to_string()),
            DEFAULT_NODE_URL,
            Vec::new(),
        )
        .expect("install");
        app
    }

    /// `app_with_a_wallet` with two addresses, for everything that moves the
    /// selection.
    fn app_with_two_addresses(dir: &std::path::Path) -> App {
        let (mut app, _) = App::new();
        app.wallet_path = Some(dir.join("seed.enc"));
        app.data_dir = Some(dir.join("node"));
        app.install_wallet(
            MasterSeed::from_bytes([9u8; 32]),
            2,
            None,
            Zeroizing::new("pass".to_string()),
            DEFAULT_NODE_URL,
            Vec::new(),
        )
        .expect("install");
        app
    }

    fn address_at(app: &App, position: usize) -> String {
        app.wallet.as_ref().expect("wallet").addresses[position]
            .address
            .clone()
    }

    fn derived_clone_for_test(entry: &AddressEntry) -> AddressEntry {
        AddressEntry {
            source: entry.source,
            address: entry.address.clone(),
            balance_units: entry.balance_units,
            spendable: entry.spendable,
            error: entry.error.clone(),
            loading: entry.loading,
            recent: entry.recent.clone(),
        }
    }

    // Spec H §4.2: one function decides which key signs, and it is the only
    // place that knows the difference.
    #[test]
    fn an_imported_address_signs_with_its_stored_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let seed_hex = "07".repeat(32);
        let wallet = app.wallet.as_mut().expect("wallet");
        wallet.imported.push(Zeroizing::new(seed_hex.clone()));
        wallet.addresses.push(AddressEntry {
            source: AddressSource::Imported(0),
            address: alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]),
            balance_units: None,
            spendable: Spendable::Pending,
            error: None,
            loading: false,
            recent: None,
        });

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(
            wallet
                .signing_seed(AddressSource::Imported(0))
                .expect("a stored seed")
                .as_slice(),
            [0x07u8; 32]
        );
        assert_eq!(
            wallet
                .signing_seed(AddressSource::Derived(0))
                .expect("a derived seed")
                .as_slice(),
            wallet.master.child_seed(0).as_slice()
        );
        assert!(wallet.signing_seed(AddressSource::Imported(7)).is_none());
    }

    #[test]
    fn a_row_says_which_kind_it_is() {
        let derived = AddressEntry {
            source: AddressSource::Derived(2),
            address: "a".repeat(40),
            balance_units: None,
            spendable: Spendable::Pending,
            error: None,
            loading: false,
            recent: None,
        };
        let imported = AddressEntry {
            source: AddressSource::Imported(0),
            ..derived_clone_for_test(&derived)
        };
        assert_eq!(derived.label(), "2");
        assert!(!derived.is_imported());
        assert_eq!(imported.label(), "IMP");
        assert!(imported.is_imported());
    }

    // A new address arrives with no first page (`recent: None`), and the
    // merger rightly will not order the others without it: F1/F2 would
    // say "Waiting..."/PARTIAL until the user pressed REFRESH. Saving it
    // starts the refresh instead.
    #[test]
    fn an_added_address_is_fetched_at_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());

        let _ = app.update(Message::AddressStoreSaved(Ok(2)));

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.addresses.len(), 2);
        assert!(wallet.fetch_in_flight, "a full refresh is on its way");
        assert!(
            wallet.addresses[1].loading,
            "the new row says it is being read"
        );
    }

    fn checked_import(app: &mut App, seed_hex: &str) {
        let _ = app.update(Message::ImportKeyStart);
        let _ = app.update(Message::ImportKeySeedChanged(seed_hex.to_string()));
        let _ = app.update(Message::ImportKeyCheck);
    }

    // Spec H §3.2: CHECK derives the address and shows it. Nothing is stored.
    #[test]
    fn check_shows_the_address_the_seed_gives_and_stores_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        checked_import(&mut app, &"07".repeat(32));
        let wallet = app.wallet.as_ref().expect("wallet");
        match &wallet.import_key {
            ImportKeyState::Checked { address, .. } => assert_eq!(
                *address,
                alphanumeric_gui::model::address_for_seed(&[0x07u8; 32])
            ),
            _ => panic!("checked"),
        }
        assert!(wallet.imported.is_empty(), "CHECK stores nothing");
        assert_eq!(wallet.addresses.len(), 1);
    }

    #[test]
    fn a_master_seed_or_a_bad_seed_is_refused_without_being_echoed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let master = MasterSeed::from_bytes([3u8; 32]).encode();
        checked_import(&mut app, &master);
        match &app.wallet.as_ref().expect("wallet").import_key {
            ImportKeyState::Asking {
                error: Some(error), ..
            } => {
                assert!(error.contains("master seed"), "{error}");
                assert!(!error.contains(master.as_str()));
            }
            _ => panic!("still asking, with a reason"),
        }

        checked_import(&mut app, "not a seed");
        assert!(matches!(
            &app.wallet.as_ref().expect("wallet").import_key,
            ImportKeyState::Asking { error: Some(_), .. }
        ));
    }

    // Spec H §5.3: the address is already here, whichever kind it is.
    #[test]
    fn a_seed_for_an_address_already_in_the_wallet_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        // app_with_a_wallet's master is [9; 32]; its address 0 is derived.
        let child = MasterSeed::from_bytes([9u8; 32]).child_seed(0);
        checked_import(&mut app, &hex::encode(child.as_slice()));
        match &app.wallet.as_ref().expect("wallet").import_key {
            ImportKeyState::Asking {
                error: Some(error), ..
            } => {
                assert!(error.contains("already"), "{error}")
            }
            _ => panic!("refused with a reason"),
        }
    }

    // Important (I3): `ImportKeyAdd` returning `Task::none()` when blocked
    // said nothing -- the panel just sat there as if the press had not
    // registered. Spec 4.4 says both blocked AND the reason is shown.
    #[test]
    fn adding_while_a_payment_is_prepared_leaves_the_panel_open_with_the_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        checked_import(&mut app, &"07".repeat(32));
        assert!(matches!(
            app.wallet.as_ref().expect("wallet").import_key,
            ImportKeyState::Checked { .. }
        ));
        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Confirm(Prepared {
            sender: "a".repeat(40),
            sender_source: AddressSource::Derived(0),
            recipient: "b".repeat(40),
            amount_units: 1,
            fee_units: 0,
            total_units: 1,
        });

        let _ = app.update(Message::ImportKeyAdd);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(
            wallet.imported.is_empty(),
            "nothing was added while blocked"
        );
        match &wallet.import_key {
            ImportKeyState::Asking {
                seed,
                error: Some(error),
            } => {
                assert_eq!(seed.as_str(), "07".repeat(32), "the typed seed is kept");
                assert!(error.contains("payment"), "{error}");
            }
            _ => panic!("expected Asking with a reason after a blocked add"),
        }
    }

    // Same as above, for the removal side: a payment prepared between typing
    // the address back and pressing REMOVE must not silently do nothing.
    #[test]
    fn removing_while_a_payment_is_prepared_leaves_the_row_with_the_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let address = alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]);
        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged(address.clone()));

        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Confirm(Prepared {
            sender: "a".repeat(40),
            sender_source: AddressSource::Derived(0),
            recipient: "b".repeat(40),
            amount_units: 1,
            fee_units: 0,
            total_units: 1,
        });

        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(
            wallet.imported.len(),
            1,
            "nothing was removed while blocked"
        );
        let removing = wallet.removing_key.as_ref().expect("the row is still open");
        assert_eq!(removing.typed, address);
        let error = removing.error.as_ref().expect("a reason");
        assert!(error.contains("payment"), "{error}");
    }

    // The list and the file agree: the address shows up, and re-opening the
    // file finds the key.
    #[test]
    fn adding_stores_the_key_in_the_file_and_the_row_in_the_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        alphanumeric_gui::storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("wallet file");
        let mut app = app_with_a_wallet(dir.path());

        checked_import(&mut app, &"07".repeat(32));
        let _ = app.update(Message::ImportKeyAdd);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.imported.len(), 1);
        let row = wallet.addresses.last().expect("row");
        assert_eq!(row.source, AddressSource::Imported(0));
        assert!(row.is_imported());
        assert_eq!(
            row.address,
            alphanumeric_gui::model::address_for_seed(&[0x07u8; 32])
        );
        assert!(matches!(wallet.import_key, ImportKeyState::Idle));

        let (_, payload) = alphanumeric_gui::storage::load(&path, b"pass").expect("reopen");
        let meta: alphanumeric_gui::storage::WalletMetadata =
            serde_json::from_slice(&payload).expect("decode");
        assert_eq!(meta.next_index, 1, "an import takes no derivation index");
        assert_eq!(meta.imported.len(), 1);
        assert_eq!(meta.imported[0].seed, "07".repeat(32));
    }

    // Controller ruling 1: `AddAddress`'s save path must not discard the
    // imported list -- it used to hand-build an empty one. `AddAddress`
    // dispatches its save through `Task::perform`, so this awaits the same
    // free function directly rather than through `App::update`.
    #[tokio::test]
    async fn add_address_keeps_an_imported_key_already_in_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        alphanumeric_gui::storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("wallet file");
        let mut app = app_with_a_wallet(dir.path());
        checked_import(&mut app, &"07".repeat(32));
        let _ = app.update(Message::ImportKeyAdd);

        let wallet = app.wallet.as_ref().expect("wallet");
        let result = save_after_add_address(
            wallet.wallet_path.clone(),
            wallet.master.clone(),
            wallet.passphrase.clone(),
            wallet.next_index.saturating_add(1),
            wallet.imported.clone(),
        )
        .await;
        assert_eq!(result, Ok(2));

        let (_, payload) = alphanumeric_gui::storage::load(&path, b"pass").expect("reopen");
        let meta: alphanumeric_gui::storage::WalletMetadata =
            serde_json::from_slice(&payload).expect("decode");
        assert_eq!(meta.next_index, 2);
        assert_eq!(
            meta.imported.len(),
            1,
            "adding a derived address must not discard the imported key"
        );
        assert_eq!(meta.imported[0].seed, "07".repeat(32));
    }

    // Spec H §4.4: a key change while money or another save is in flight.
    #[test]
    fn a_key_change_waits_for_a_payment_or_an_address_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        assert_eq!(app.key_change_blocker(), None);

        app.wallet.as_mut().expect("wallet").send.outstanding =
            Some(Arc::new(serde_json::json!({})));
        assert!(app.key_change_blocker().is_some());
        let _ = app.update(Message::ImportKeyStart);
        assert!(matches!(
            app.wallet.as_ref().expect("wallet").import_key,
            ImportKeyState::Idle
        ));

        app.wallet.as_mut().expect("wallet").send.outstanding = None;
        app.wallet.as_mut().expect("wallet").adding_address = true;
        assert!(app.key_change_blocker().is_some());
    }

    fn with_one_imported(dir: &std::path::Path) -> App {
        let path = dir.join("seed.enc");
        alphanumeric_gui::storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("wallet file");
        let mut app = app_with_a_wallet(dir);
        let _ = app.update(Message::ImportKeyStart);
        let _ = app.update(Message::ImportKeySeedChanged("07".repeat(32)));
        let _ = app.update(Message::ImportKeyCheck);
        let _ = app.update(Message::ImportKeyAdd);
        app
    }

    // Spec H §3.6: the address has to be typed, exactly.
    #[test]
    fn removing_needs_the_address_typed_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let address = alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]);

        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged("something else".into()));
        let _ = app.update(Message::RemoveKeyConfirm);
        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.imported.len(), 1, "a wrong address removes nothing");
        assert!(wallet
            .removing_key
            .as_ref()
            .expect("still asking")
            .error
            .is_some());

        let _ = app.update(Message::RemoveKeyTypedChanged(address.clone()));
        let _ = app.update(Message::RemoveKeyConfirm);
        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(wallet.imported.is_empty());
        assert!(wallet
            .addresses
            .iter()
            .all(|entry| entry.address != address));
        assert!(wallet.removing_key.is_none());

        let (_, payload) =
            alphanumeric_gui::storage::load(&dir.path().join("seed.enc"), b"pass").expect("reopen");
        let meta: alphanumeric_gui::storage::WalletMetadata =
            serde_json::from_slice(&payload).expect("decode");
        assert!(
            meta.imported.is_empty(),
            "the key is gone from the file too"
        );
    }

    // Ruling 4: slots are positions, so what is left has to be renumbered.
    #[test]
    fn removing_the_first_of_two_renumbers_the_one_that_stays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let _ = app.update(Message::ImportKeyStart);
        let _ = app.update(Message::ImportKeySeedChanged("08".repeat(32)));
        let _ = app.update(Message::ImportKeyCheck);
        let _ = app.update(Message::ImportKeyAdd);
        let second = alphanumeric_gui::model::address_for_seed(&[0x08u8; 32]);

        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged(
            alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]),
        ));
        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.imported.len(), 1);
        let row = wallet
            .addresses
            .iter()
            .find(|entry| entry.address == second)
            .expect("the second address stays");
        assert_eq!(row.source, AddressSource::Imported(0), "renumbered");
        assert_eq!(
            wallet
                .signing_seed(AddressSource::Imported(0))
                .expect("seed")
                .as_slice(),
            [0x08u8; 32],
            "and its slot still points at its own key"
        );
    }

    // Critical: F2's Receive screen draws `wallet.qr` beside whatever address
    // `wallet.active` points at with no re-derivation of its own. A removal
    // that clamps `active` without rebuilding `qr` to match leaves the QR of
    // the just-deleted (and now unusable) address on screen next to a
    // different address's text and COPY button -- money sent there is gone.
    #[test]
    fn removing_the_active_row_moves_the_qr_and_the_selection_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        // addresses = [Derived(0), Imported(0)]; make the imported row active,
        // the way F1's row click or NEXT would.
        let imported_position = app
            .wallet
            .as_ref()
            .expect("wallet")
            .addresses
            .iter()
            .position(|entry| entry.is_imported())
            .expect("the imported row");
        let _ = app.update(Message::SetActiveAddress(imported_position));
        let removed_address = address_at(&app, imported_position);
        assert_eq!(
            app.wallet.as_ref().expect("wallet").active,
            imported_position,
            "the imported row is active before removal"
        );

        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged(removed_address.clone()));
        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(
            wallet.active < wallet.addresses.len(),
            "active must index a row that still exists"
        );
        let now_active_address = wallet.addresses[wallet.active].address.clone();
        assert_ne!(
            now_active_address, removed_address,
            "the removed address must not still be what's on screen"
        );
        let expected = iced::widget::qr_code::Data::new(&now_active_address)
            .expect("40 hex characters always encode");
        let qr = wallet.qr.as_ref().expect("a QR for the surviving row");
        // `qr_code::Data` has no `PartialEq`; its `Debug` is deterministic for
        // a freshly built value (same contents/width, and a brand new
        // `canvas::Cache` is always `Cache::Empty { previous: None }`), so
        // comparing the rendering is a faithful stand-in for equality here.
        assert_eq!(format!("{qr:?}"), format!("{expected:?}"));
        assert!(
            matches!(wallet.reveal, RevealState::Idle),
            "a reveal for the removed row must not survive under the new one"
        );

        // Selecting the row that is already active is a documented no-op
        // (SetActiveAddress's own guard): this demonstrates `active` and `qr`
        // are already in the state that message would produce, not that it
        // silently fixes a stale one.
        let active_before = wallet.active;
        let _ = app.update(Message::SetActiveAddress(active_before));
        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.active, active_before);
        let qr = wallet.qr.as_ref().expect("still a QR after the no-op");
        assert_eq!(format!("{qr:?}"), format!("{expected:?}"));
    }

    // Symmetric with the success path: the rollback re-inserts the removed
    // row and must recompute `active`/`qr`/`reveal` for wherever `active` ends
    // up afterward, not leave them describing the row mid-removal.
    #[test]
    fn removing_the_active_row_still_fixes_the_qr_when_the_save_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let imported_position = app
            .wallet
            .as_ref()
            .expect("wallet")
            .addresses
            .iter()
            .position(|entry| entry.is_imported())
            .expect("the imported row");
        let _ = app.update(Message::SetActiveAddress(imported_position));
        let removed_address = address_at(&app, imported_position);

        // Force `write_keys` to fail: make the wallet path's parent a plain
        // file, so `create_dir_all` on it cannot succeed.
        let blocker_file = dir.path().join("not-a-directory");
        std::fs::write(&blocker_file, b"in the way").expect("write blocker file");
        app.wallet.as_mut().expect("wallet").wallet_path = blocker_file.join("seed.enc");

        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged(removed_address.clone()));
        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.imported.len(), 1, "the save failed, nothing removed");
        assert!(wallet
            .removing_key
            .as_ref()
            .expect("still open, with a reason")
            .error
            .is_some());
        assert!(wallet.active < wallet.addresses.len());
        let now_active_address = wallet.addresses[wallet.active].address.clone();
        let expected = iced::widget::qr_code::Data::new(&now_active_address)
            .expect("40 hex characters always encode");
        let qr = wallet.qr.as_ref().expect("a QR for whatever is active now");
        assert_eq!(format!("{qr:?}"), format!("{expected:?}"));
        assert!(matches!(wallet.reveal, RevealState::Idle));
    }

    // Important (I2): the old `retain(|entry| entry.address != address)`
    // deleted every row with that address, so a derived row that happens to
    // carry the same address as the imported one being removed (reachable
    // once the master later derives the same key an import already holds)
    // would vanish too, though nothing asked to remove it.
    #[test]
    fn removing_an_imported_row_leaves_a_derived_row_with_the_same_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let imported_address = alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]);
        app.wallet
            .as_mut()
            .expect("wallet")
            .addresses
            .push(AddressEntry {
                source: AddressSource::Derived(3),
                address: imported_address.clone(),
                balance_units: None,
                spendable: Spendable::Pending,
                error: None,
                loading: false,
                recent: None,
            });

        let _ = app.update(Message::RemoveKeyStart(0));
        let _ = app.update(Message::RemoveKeyTypedChanged(imported_address.clone()));
        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(wallet.imported.is_empty(), "the imported key is gone");
        let survivors: Vec<_> = wallet
            .addresses
            .iter()
            .filter(|entry| entry.address == imported_address)
            .collect();
        assert_eq!(
            survivors.len(),
            1,
            "the derived row with the same address stays"
        );
        assert_eq!(survivors[0].source, AddressSource::Derived(3));
    }

    // Minor (M6): `slot` and `wallet.imported`/`wallet.addresses` are kept in
    // sync by every writer, but a desync anywhere else must not turn into an
    // out-of-bounds panic here.
    #[test]
    fn a_desynced_slot_does_not_panic_the_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let address = alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]);
        app.wallet.as_mut().expect("wallet").removing_key = Some(RemoveKey {
            slot: 5,
            address: address.clone(),
            typed: address.clone(),
            error: None,
        });

        let _ = app.update(Message::RemoveKeyConfirm);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(
            wallet.imported.len(),
            1,
            "the out-of-range slot changed nothing"
        );
    }

    #[test]
    fn removing_waits_for_a_payment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        app.wallet.as_mut().expect("wallet").send.outstanding =
            Some(Arc::new(serde_json::json!({})));
        let _ = app.update(Message::RemoveKeyStart(0));
        assert!(app.wallet.as_ref().expect("wallet").removing_key.is_none());
    }

    // Controller ruling: a payment already in `Confirm` freezes the same as
    // one in `Checking`/`Sending` -- its `sender_source` would otherwise
    // resolve to a different key out from under it once a slot moved.
    #[test]
    fn a_key_change_also_waits_for_a_payment_sitting_in_confirm() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Confirm(Prepared {
            sender: "a".repeat(40),
            sender_source: AddressSource::Imported(0),
            recipient: "b".repeat(40),
            amount_units: 1,
            fee_units: 0,
            total_units: 1,
        });
        assert!(app.key_change_blocker().is_some());

        let _ = app.update(Message::RemoveKeyStart(0));
        assert!(
            app.wallet.as_ref().expect("wallet").removing_key.is_none(),
            "a removal must not open while a payment is prepared"
        );
    }

    // Controller ruling: the seed the wallet is about to sign with must still
    // belong to the address the payment says it is paying from -- a removal
    // (or any other key change) between confirming and this check must not
    // let the wallet sign for one address with another address's key.
    #[test]
    fn signing_refuses_a_seed_that_no_longer_matches_the_prepared_sender() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let real_address = alphanumeric_gui::model::address_for_seed(&[0x07u8; 32]);
        assert_ne!(real_address, "b".repeat(40));
        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Checking(Prepared {
            // A stale sender the wallet no longer signs for -- as if the key
            // for `Imported(0)` changed after this payment was prepared.
            sender: "b".repeat(40),
            sender_source: AddressSource::Imported(0),
            recipient: "c".repeat(40),
            amount_units: 1,
            fee_units: 0,
            total_units: 1,
        });
        let state = backend::AddressState {
            balance_units: 1000,
            spendable_units: Some(1000),
            transactions: Some(Vec::new()),
            history_available: true,
            index_ready: true,
            index_height: Some(1),
            next: None,
        };

        let _ = app.update(Message::SendSpendableChecked(Ok(state)));

        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(matches!(wallet.send.stage, SendStage::Compose));
        // M10: `wallet.send.error.is_some()` alone would also pass if this
        // had gone through the DIFFERENT guard just above it in `app.rs` --
        // "This wallet no longer has the key for that address." -- which
        // fires when `signing_seed` returns `None` rather than when it
        // returns a seed whose address does not match. `Imported(0)` exists
        // here (`with_one_imported`), so only the address-mismatch guard can
        // be the one that ran; pinning its exact words is what stops this
        // test from silently passing through the wrong branch.
        let error = wallet.send.error.as_deref().expect("a reason");
        assert!(
            error.contains("changed while this payment was being prepared"),
            "{error}"
        );
        assert!(
            wallet.send.outstanding.is_none(),
            "nothing was signed or submitted"
        );
    }

    // Fix round 1 (Critical): open/unlock never populated `imported`, so a
    // reopened wallet showed no imported rows at all -- and the very next
    // `+ ADD` sealed an empty list over the file, destroying the key for
    // good. `open_wallet_for_unlock` is the exact function `Message::Unlock`
    // dispatches; this drives it directly against a real file, the way the
    // unlock path does, then installs from what it returns.
    #[tokio::test]
    async fn a_reopened_wallet_still_has_its_imported_addresses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let metadata = serde_json::to_vec(&WalletMetadata {
            next_index: 1,
            imported: vec![storage::ImportedKey {
                seed: "07".repeat(32),
                added: "2026-01-01T00:00:00Z".to_string(),
            }],
        })
        .expect("encode");
        alphanumeric_gui::storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            &metadata,
            b"pass",
        )
        .expect("wallet file");

        let ready = open_wallet_for_unlock(
            path.clone(),
            Zeroizing::new("pass".to_string()),
            "http://127.0.0.1:1".to_string(),
        )
        .await
        .expect("unlock");

        let (mut app, _) = App::new();
        app.wallet_path = Some(path);
        app.data_dir = Some(dir.path().join("node"));
        app.install_wallet(
            ready.master,
            ready.next_index,
            ready.status,
            ready.passphrase,
            &ready.node_url,
            ready.imported,
        )
        .expect("install");

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.imported.len(), 1);
        let row = wallet
            .addresses
            .iter()
            .find(|entry| entry.source == AddressSource::Imported(0))
            .expect("an imported row survived the reopen");
        assert_eq!(
            row.address,
            alphanumeric_gui::model::address_for_seed(&[0x07u8; 32])
        );
        assert_eq!(
            wallet
                .signing_seed(AddressSource::Imported(0))
                .expect("the stored seed")
                .as_slice(),
            [0x07u8; 32]
        );
    }

    // Fix round 1: `wallet.imported` and the number of `Imported` rows in
    // `wallet.addresses` are not the same count -- `install_wallet` keeps a
    // stored seed that no longer parses in `imported` (so a later save
    // cannot lose it) while giving it no row. The wallet panel's derived and
    // imported counts must come from the rows actually shown, the same
    // filters `wallet_rows`' caller uses, or this underflows (`addresses.len()
    // - imported.len()` when a bad seed makes `imported` the larger of the
    // two).
    #[test]
    fn the_wallet_panel_counts_rows_not_stored_seeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, _) = App::new();
        app.wallet_path = Some(dir.path().join("seed.enc"));
        app.data_dir = Some(dir.path().join("node"));
        app.install_wallet(
            MasterSeed::from_bytes([9u8; 32]),
            1,
            None,
            Zeroizing::new("pass".to_string()),
            "http://127.0.0.1:1",
            vec![
                Zeroizing::new("zz".repeat(32)),
                Zeroizing::new("07".repeat(32)),
            ],
        )
        .expect("install");

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(
            wallet.imported.len(),
            2,
            "both the bad and the good seed stay in `imported`"
        );
        // The same filters `settings::wallet_panel` feeds to `wallet_rows`.
        let derived = wallet.addresses.iter().filter(|e| !e.is_imported()).count();
        let imported = wallet.addresses.iter().filter(|e| e.is_imported()).count();
        assert_eq!(derived, 1, "the one derived address at next_index 1");
        assert_eq!(imported, 1, "only the seed that parsed got a row");
    }

    // The exact sequence that destroyed the key: open the file, then press
    // `+ ADD` -- run through the real save path each step uses, not a
    // hand-built stand-in for either.
    #[tokio::test]
    async fn adding_an_address_after_a_reopen_keeps_the_imported_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        let metadata = serde_json::to_vec(&WalletMetadata {
            next_index: 1,
            imported: vec![storage::ImportedKey {
                seed: "07".repeat(32),
                added: "2026-01-01T00:00:00Z".to_string(),
            }],
        })
        .expect("encode");
        alphanumeric_gui::storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            &metadata,
            b"pass",
        )
        .expect("wallet file");

        let ready = open_wallet_for_unlock(
            path.clone(),
            Zeroizing::new("pass".to_string()),
            "http://127.0.0.1:1".to_string(),
        )
        .await
        .expect("unlock");

        let (mut app, _) = App::new();
        app.wallet_path = Some(path.clone());
        app.data_dir = Some(dir.path().join("node"));
        app.install_wallet(
            ready.master,
            ready.next_index,
            ready.status,
            ready.passphrase,
            &ready.node_url,
            ready.imported,
        )
        .expect("install");

        let wallet = app.wallet.as_ref().expect("wallet");
        save_after_add_address(
            wallet.wallet_path.clone(),
            wallet.master.clone(),
            wallet.passphrase.clone(),
            wallet.next_index.saturating_add(1),
            wallet.imported.clone(),
        )
        .await
        .expect("save");

        let (_, payload) = alphanumeric_gui::storage::load(&path, b"pass").expect("reopen");
        let meta: alphanumeric_gui::storage::WalletMetadata =
            serde_json::from_slice(&payload).expect("decode");
        assert_eq!(meta.next_index, 2);
        assert_eq!(
            meta.imported.len(),
            1,
            "a reopened wallet's + ADD must not have erased the imported key"
        );
        assert_eq!(meta.imported[0].seed, "07".repeat(32));
    }

    // NEXT ADDRESS on Receive moves the address and QR to another row. A seed
    // left on screen would sit under that row's address and QR, one row
    // away from being pasted as the key for the wrong address.
    #[test]
    fn moving_to_another_address_hides_a_revealed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        let first = address_at(&app, 0);
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: first,
            seed_hex: Zeroizing::new("33".repeat(32)),
        };

        let _ = app.update(Message::SetActiveAddress(1));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Idle
        ));
    }

    // Pressing the row that is already active changes nothing on screen, so
    // it leaves the seed where it is: the seed still belongs to the address
    // it is shown under.
    #[test]
    fn selecting_the_active_address_again_leaves_the_seed_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        let first = address_at(&app, 0);
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: first,
            seed_hex: Zeroizing::new("33".repeat(32)),
        };

        let _ = app.update(Message::SetActiveAddress(0));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Revealed { .. }
        ));
    }

    // NEXT pressed while the passphrase check for [0] is still running: the
    // derivation that lands afterwards belongs to a request that no longer
    // exists.
    #[test]
    fn a_seed_still_being_derived_when_the_address_moves_never_lands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        let first = address_at(&app, 0);
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("pass".to_string()),
            error: None,
            busy: true,
        };

        let _ = app.update(Message::SetActiveAddress(1));
        let _ = app.update(Message::RevealSeedRevealed(Ok(Revealed {
            address: first,
            seed_hex: Zeroizing::new("44".repeat(32)),
        })));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Idle
        ));
    }

    // The same, but the user has already asked again for [1] by the time
    // [0]'s derivation lands, so the state is `Asking { busy: true }` once
    // more. Being busy is not enough: the seed that arrives must be for the
    // address on screen.
    #[test]
    fn a_seed_for_another_address_never_answers_a_newer_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        let first = address_at(&app, 0);
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("pass".to_string()),
            error: None,
            busy: true,
        };

        let _ = app.update(Message::SetActiveAddress(1));
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("pass".to_string()),
            error: None,
            busy: true,
        };
        let _ = app.update(Message::RevealSeedRevealed(Ok(Revealed {
            address: first,
            seed_hex: Zeroizing::new("44".repeat(32)),
        })));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Asking { busy: true, .. }
        ));
    }

    // The other side of the two tests above: the guard must still let the
    // answer for the address on screen through, or the gate never opens.
    #[test]
    fn a_seed_for_the_active_address_is_shown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_two_addresses(dir.path());
        let _ = app.update(Message::SetActiveAddress(1));
        let second = address_at(&app, 1);
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("pass".to_string()),
            error: None,
            busy: true,
        };

        let _ = app.update(Message::RevealSeedRevealed(Ok(Revealed {
            address: second.clone(),
            seed_hex: Zeroizing::new("44".repeat(32)),
        })));

        match &app.wallet.as_ref().expect("wallet").reveal {
            RevealState::Revealed { address, .. } => assert_eq!(address, &second),
            _ => panic!("the seed for the address on screen must be shown"),
        }
    }

    // Opening the history screen builds the merge from scratch. New blocks
    // attach at the top of the descending order, so rebuilding is correct,
    // not appending.
    #[test]
    fn opening_history_starts_a_fresh_merge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let wallet = app.wallet.as_ref().expect("wallet");
        let history = wallet.history.as_ref().expect("history started");
        assert!(history.rows_len() == 0);
        assert!(
            history.in_flight,
            "the first page's request must be in flight"
        );
        assert_eq!(app.screen, Screen::History);
    }

    // If a page fails, the list stops right there. Skipping the failed
    // address and continuing would break the merge invariant and leave the
    // list silently wrong.
    #[test]
    fn a_failed_page_stops_the_list_instead_of_skipping_the_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let generation = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history")
            .generation;
        let _ = app.update(Message::HistoryPageFetched(
            0,
            generation,
            None,
            Err(backend::ApiError::Transport("connection refused".into())),
        ));
        let history = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history");
        assert!(history.error.is_some());
        assert!(
            !history.in_flight,
            "does not automatically send the next request"
        );
        assert_eq!(history.rows_len(), 0);
    }

    // Leaving the history screen drops its state. Keeping it would show
    // the stale list first on re-entry, looking complete while missing
    // whatever blocks were mined in between.
    #[test]
    fn leaving_history_drops_the_merge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let _ = app.update(Message::Show(Screen::Wallet));
        assert!(app.wallet.as_ref().expect("wallet").history.is_none());
    }

    fn tx_entry(height: u64) -> backend::TxEntry {
        backend::TxEntry {
            amount_units: 5,
            fee_units: 1,
            counterparty: "MINING_REWARDS".into(),
            direction: "in".into(),
            height,
            position: 0,
            timestamp: height,
        }
    }

    /// The address fetch already carries the newest page of history; the
    /// wallet used to drop it and keep only the balance.
    #[test]
    fn a_refreshed_address_keeps_its_first_history_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let mut state = fresh_address_state(100);
        state.transactions = Some(vec![tx_entry(90)]);
        let entry = &mut app.wallet.as_mut().expect("wallet").addresses[0];
        apply_address_result(entry, Ok(state), false);
        let page = entry.recent.as_ref().expect("kept");
        assert_eq!(page.transactions.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn recent_activity_reads_only_what_was_already_fetched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        assert!(
            !app.recent_activity(8).complete,
            "an address never fetched cannot be ordered against"
        );
        let mut state = fresh_address_state(100);
        state.transactions = Some(vec![tx_entry(90), tx_entry(80)]);
        let entry = &mut app.wallet.as_mut().expect("wallet").addresses[0];
        apply_address_result(entry, Ok(state), false);
        let recent = app.recent_activity(8);
        assert!(recent.complete);
        let heights: Vec<u64> = recent.rows.iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![90, 80]);
    }

    fn history_tx(height: u64) -> backend::TxEntry {
        backend::TxEntry {
            amount_units: 1_000_000_000,
            fee_units: 50_000,
            counterparty: "someone-else".to_string(),
            direction: "in".to_string(),
            height,
            position: 0,
            timestamp: 1_788_000_000 + height,
        }
    }

    fn history_page(
        transactions: Vec<backend::TxEntry>,
        next: Option<backend::Cursor>,
    ) -> backend::AddressPage {
        backend::AddressPage {
            transactions: Some(transactions),
            next,
            history_available: true,
            index_ready: true,
            index_height: Some(1_000),
        }
    }

    // A response that belongs to a `HistoryState` already replaced by a fresh
    // one (left the screen and reopened it while a request was still in
    // flight) must not be accepted into the new state. Accepting it would
    // double-feed that stream's buffer -- worst case, collapsing two distinct
    // rows into one falsely reported as an internal transfer.
    #[test]
    fn a_stale_page_response_is_dropped_after_reopening_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let stale_generation = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history")
            .generation;
        let _ = app.update(Message::Show(Screen::Wallet));
        let _ = app.update(Message::HistoryOpen);

        let stale_page = history_page(vec![history_tx(100)], None);
        let _ = app.update(Message::HistoryPageFetched(
            0,
            stale_generation,
            None,
            Ok(stale_page),
        ));

        let history = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history");
        assert_eq!(
            history.rows_len(),
            0,
            "a stale response must not be accepted into the fresh merge"
        );
        assert!(
            history.in_flight,
            "the fresh state's own request is still outstanding"
        );
        assert!(
            history.error.is_none(),
            "a stale response is dropped, not surfaced as an error"
        );
    }

    // An empty page that still claims a next cursor exists would otherwise
    // make `advance` ask for the identical page forever: the stream stays
    // unfinished, unstalled, and empty, so nothing about its state changes
    // between calls.
    #[test]
    fn an_empty_page_that_claims_more_stops_instead_of_looping() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let generation = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history")
            .generation;

        let no_progress_page = history_page(
            vec![],
            Some(backend::Cursor {
                before_height: 100,
                before_pos: 0,
            }),
        );
        let _ = app.update(Message::HistoryPageFetched(
            0,
            generation,
            None,
            Ok(no_progress_page),
        ));

        let history = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history");
        assert!(
            history.error.is_some(),
            "an empty page that claims more must be surfaced, not retried silently"
        );
        assert!(
            !history.in_flight,
            "must not keep re-dispatching the same request"
        );
        assert_eq!(history.rows_len(), 0);
    }

    // A node that ignores `before` and re-serves a page it already sent
    // refills the stream with keys at or above rows already emitted. The
    // merger drops them, so nothing was added -- and that has to be read as
    // no progress, or the recursion asks for the identical page forever. The
    // page looks full on the wire, so counting its entries would call it
    // progress.
    #[test]
    fn a_re_served_page_is_no_progress_even_though_it_carries_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let generation = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history")
            .generation;

        let cursor = backend::Cursor {
            before_height: 100,
            before_pos: 0,
        };
        // Asked for everything below block 100; served block 120 and 100.
        let re_served = history_page(vec![history_tx(120), history_tx(100)], Some(cursor));
        let _ = app.update(Message::HistoryPageFetched(
            0,
            generation,
            Some(cursor),
            Ok(re_served),
        ));

        let history = app
            .wallet
            .as_ref()
            .expect("wallet")
            .history
            .as_ref()
            .expect("history");
        assert_eq!(
            history.rows_len(),
            0,
            "entries at or above the cursor must not reach the list"
        );
        assert!(
            history.error.is_some(),
            "a page that adds nothing while claiming more must be surfaced"
        );
        assert!(
            !history.in_flight,
            "must not keep re-dispatching the same request"
        );
    }

    // `HistoryOpen` sets `self.screen` itself rather than going through
    // `Message::Show`, which is where a revealed seed is normally cleared. It
    // has to clear it too, or it is the one screen change that carries the
    // seed across.
    #[test]
    fn opening_history_clears_a_revealed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: "f811b100866449d60739eeb137ee004e76fb09d4".to_string(),
            seed_hex: Zeroizing::new("22".repeat(32)),
        };

        let _ = app.update(Message::HistoryOpen);

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Idle
        ));
    }

    // `HistoryOpen` sets `self.screen` itself rather than going through
    // `Show` (see its own comment on the duplicated reveal resets); the
    // IMPORT panel must not survive that route either.
    #[test]
    fn opening_history_clears_the_import_panel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::ImportKeyStart);

        let _ = app.update(Message::HistoryOpen);

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").import_key,
            ImportKeyState::Idle
        ));
    }

    // The staleness banner compares the index height against
    // `node_status.height`. `ConsoleTick` keeps that refreshed every 10s
    // while History stays open, but `HistoryOpen` still re-reads it
    // immediately on entry so the comparison does not start from whatever
    // some other screen last left it at; this pins the arm that applies
    // that immediate answer.
    #[test]
    fn the_history_status_read_updates_the_chain_height() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let _ = app.update(Message::HistoryStatusFetched(
            app.node_epoch,
            Ok(backend::NodeStatus {
                height: Some(984_220),
                network_height: Some(984_220),
                blocks_behind: Some(0),
                index_ready: true,
                index_height: Some(984_220),
                version: "8.0.0".to_string(),
                finalized_height: None,
                mining: None,
                mining_address: None,
                mining_backend: None,
                mining_hps: None,
                mining_blocks: None,
                mining_payout_rotation: None,
                gpu_built: false,
                gpu_devices: Vec::new(),
                mining_hashes: None,
                mining_difficulty: None,
                mining_expected_block_secs: None,
                mining_threads: None,
                mining_devices: Vec::new(),
            }),
        ));
        assert_eq!(
            app.wallet
                .as_ref()
                .expect("wallet")
                .node_status
                .as_ref()
                .and_then(|status| status.height),
            Some(984_220)
        );
    }

    // A revealed seed must not be waiting on the screen when the user comes
    // back to it. Nothing else clears it -- `Show` is the only exit from the
    // receive screen, so if the reset is not there, the seed stays resident for
    // the rest of the session and reappears on the next visit.
    #[test]
    fn leaving_the_receive_screen_clears_a_revealed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: "f811b100866449d60739eeb137ee004e76fb09d4".to_string(),
            seed_hex: Zeroizing::new("11".repeat(32)),
        };

        let _ = app.update(Message::Show(Screen::Wallet));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Idle
        ));
    }

    // The gate opens on the passphrase, not on the button: pressing Reveal must
    // leave the state Asking, with nothing derived.
    #[test]
    fn asking_to_reveal_derives_nothing_by_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());

        let _ = app.update(Message::RevealSeedStart);

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Asking { .. }
        ));
    }

    // Spec H §4.3: the same gate, the stored seed.
    #[test]
    fn revealing_an_imported_address_asks_the_file_for_its_stored_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = with_one_imported(dir.path());
        let position = app
            .wallet
            .as_ref()
            .expect("wallet")
            .addresses
            .iter()
            .position(|entry| entry.is_imported())
            .expect("the imported row");
        let imported_address = address_at(&app, position);
        let wallet_path = app.wallet.as_ref().expect("wallet").wallet_path.clone();

        let _ = app.update(Message::SetActiveAddress(position));
        let _ = app.update(Message::RevealSeedStart);
        let _ = app.update(Message::RevealPassphraseChanged("pass".into()));
        let _ = app.update(Message::RevealSeedConfirm);
        assert!(
            matches!(
                app.wallet.as_ref().expect("wallet").reveal,
                RevealState::Asking { busy: true, .. }
            ),
            "the imported row asks the file too"
        );

        // M10: the assertion above only shows a task was dispatched, not
        // what it asked for or what would happen with its answer. Drive the
        // exact function `Message::RevealSeedConfirm` calls for an
        // `Imported` row (`storage::reveal_imported_seed_hex`) and feed its
        // real result back through `RevealSeedRevealed`, the way the async
        // task genuinely would.
        let seed_hex = storage::reveal_imported_seed_hex(&wallet_path, b"pass", 0)
            .expect("the file holds this slot's seed");
        assert_eq!(seed_hex.as_str(), "07".repeat(32));
        let _ = app.update(Message::RevealSeedRevealed(Ok(Revealed {
            address: imported_address.clone(),
            seed_hex,
        })));
        match &app.wallet.as_ref().expect("wallet").reveal {
            RevealState::Revealed { address, seed_hex } => {
                assert_eq!(*address, imported_address);
                assert_eq!(seed_hex.as_str(), "07".repeat(32));
            }
            _ => panic!("expected the imported row's own stored seed revealed"),
        }
    }

    // Argon2id runs long enough to press Cancel during it. The reveal that
    // comes back afterwards belongs to a request that no longer exists, and
    // showing it would put a spendable key on screen the user had dismissed.
    #[test]
    fn a_reveal_that_lands_after_cancel_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("pass".to_string()),
            error: None,
            busy: true,
        };

        let _ = app.update(Message::RevealSeedCancel);
        let _ = app.update(Message::RevealSeedRevealed(Ok(Revealed {
            address: "d8980f05402bf990c49a601f35bedf09ea77f189".to_string(),
            seed_hex: Zeroizing::new("11".repeat(32)),
        })));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").reveal,
            RevealState::Idle
        ));
    }

    // A refusal has to leave a state the user can act on: the reason visible,
    // the field empty, and the button live again. Leaving `busy` set would
    // strand the screen on "Checking..." with no way back except Cancel.
    #[test]
    fn a_refused_reveal_clears_the_attempt_and_says_why() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Asking {
            passphrase: Zeroizing::new("wrong".to_string()),
            error: None,
            busy: true,
        };

        let _ = app.update(Message::RevealSeedRevealed(Err("no good".to_string())));

        match &app.wallet.as_ref().expect("wallet").reveal {
            RevealState::Asking {
                passphrase,
                error,
                busy,
            } => {
                assert!(passphrase.is_empty());
                assert_eq!(error.as_deref(), Some("no good"));
                assert!(!busy);
            }
            _ => panic!("a refusal must stay on the passphrase prompt"),
        }
    }

    // The gate has to be answerable from a saved copy and not from memory of
    // having seen the screen. Four characters at positions the user cannot
    // predict is the smallest thing that is both.
    #[test]
    fn the_quiz_only_opens_on_the_right_characters() {
        let master = MasterSeed::from_bytes([5u8; 32]);
        let encoded = master.encode().to_string();
        let positions = vec![7usize, 20, 41, 61];
        let right: String = positions
            .iter()
            .map(|&at| encoded.chars().nth(at).expect("in range"))
            .collect();

        assert!(backup_quiz_passed(&encoded, &positions, &right));
        // Spacing is the user's formatting, not their answer.
        let spaced = right
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(backup_quiz_passed(&encoded, &positions, &spaced));
        // The encoding is lowercase and decode accepts either case.
        assert!(backup_quiz_passed(
            &encoded,
            &positions,
            &right.to_uppercase()
        ));

        assert!(!backup_quiz_passed(&encoded, &positions, ""));
        assert!(!backup_quiz_passed(&encoded, &positions, &right[..3]));
        // Right characters, wrong order -- someone reading their copy top to
        // bottom instead of at the positions asked for.
        let reversed: String = right.chars().rev().collect();
        if reversed != right {
            assert!(!backup_quiz_passed(&encoded, &positions, &reversed));
        }
    }

    // The prefix is the same on every wallet, so a character from it is not an
    // answer -- it is a constant. Asking for one would make the quiz weaker
    // exactly as often as it was picked.
    #[test]
    fn the_quiz_never_asks_for_a_character_from_the_constant_prefix() {
        let mut rng = rand::thread_rng();
        let len = MasterSeed::from_bytes([9u8; 32]).encode().len();
        for _ in 0..200 {
            let positions = backup_quiz_positions(len, &mut rng);
            assert_eq!(positions.len(), BACKUP_QUIZ_LEN);
            assert!(
                positions.iter().all(|&at| at >= 4 && at < len),
                "positions must sit inside the seed and past `a9m1`: {positions:?}"
            );
            assert!(
                positions.windows(2).all(|w| w[0] < w[1]),
                "sorted and distinct so the prompt reads left to right: {positions:?}"
            );
        }
    }

    // Fixed positions would be learnable: click through once and you know where
    // to look forever, without ever having saved anything.
    #[test]
    fn the_quiz_positions_are_not_the_same_every_time() {
        let mut rng = rand::thread_rng();
        let len = MasterSeed::from_bytes([9u8; 32]).encode().len();
        let first = backup_quiz_positions(len, &mut rng);
        assert!(
            (0..50).any(|_| backup_quiz_positions(len, &mut rng) != first),
            "50 draws all identical means the positions are not random"
        );
    }

    // Restoring from a photograph gives no record of how many addresses were
    // used, so the scan walks indices until it has seen a run of unused ones.
    // Stopping at the first unused address would lose every address after a gap.
    #[test]
    fn discovery_stops_after_a_full_gap_and_not_before() {
        // used[i] answers "does index i have history?"
        let used = |index: u32| matches!(index, 0 | 1 | 5);
        assert_eq!(discover_next_index(&used, 20), 6);

        let none_used = |_: u32| false;
        assert_eq!(discover_next_index(&none_used, 20), 0);

        // {0,1,5} -> 6 passes whether or not the `run = 0` reset on a used
        // index is actually there: 6 is also what a broken version (one that
        // never resets `run`) would return, since indices 2..5 are only four
        // unused in a row either way. {0,10,25} -> 26 does not: with the
        // reset, index 25's hit restarts the run and the scan walks the full
        // gap past it to 26; without the reset, the run started counting
        // unused indices back at index 1 and would have already hit the gap
        // limit around index 20, well before 25 is ever reached, giving 11
        // instead. This is the case that actually exercises the reset.
        let sparse = |index: u32| matches!(index, 0 | 10 | 25);
        assert_eq!(discover_next_index(&sparse, 20), 26);
    }

    // Spec 4.2: 503 (chain busy) backs off exponentially; 429 (rate limited)
    // starts and caps noticeably higher, since a token bucket refills far
    // slower than a lock is ever held. A flat delay for both -- or an
    // unbounded one -- is what this pins against regressing to.
    #[test]
    fn retry_delay_backs_off_exponentially_and_rate_limited_runs_longer() {
        use std::time::Duration;

        assert_eq!(retry_delay(RetryKind::Busy, 0), Duration::from_millis(500));
        assert_eq!(
            retry_delay(RetryKind::Busy, 1),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            retry_delay(RetryKind::Busy, 2),
            Duration::from_millis(2_000)
        );
        assert_eq!(
            retry_delay(RetryKind::Busy, 3),
            Duration::from_millis(4_000)
        );
        // Capped: further attempts do not keep doubling forever.
        assert_eq!(
            retry_delay(RetryKind::Busy, 4),
            Duration::from_millis(8_000)
        );
        assert_eq!(
            retry_delay(RetryKind::Busy, 10),
            Duration::from_millis(8_000)
        );

        assert_eq!(
            retry_delay(RetryKind::RateLimited, 0),
            Duration::from_millis(2_000)
        );
        assert_eq!(
            retry_delay(RetryKind::RateLimited, 1),
            Duration::from_millis(4_000)
        );
        assert_eq!(
            retry_delay(RetryKind::RateLimited, 4),
            Duration::from_millis(30_000)
        );
        assert_eq!(
            retry_delay(RetryKind::RateLimited, 10),
            Duration::from_millis(30_000)
        );

        // Every attempt, RateLimited waits longer than Busy does.
        for attempt in 0..RETRY_ATTEMPTS {
            assert!(
                retry_delay(RetryKind::RateLimited, attempt)
                    > retry_delay(RetryKind::Busy, attempt)
            );
        }
    }

    #[test]
    fn node_launch_command_uses_the_configured_address() {
        assert_eq!(
            node_launch_command(DEFAULT_NODE_URL),
            "ALPHANUMERIC_EXPLORER_API=127.0.0.1:8095 ./alphanumeric"
        );
        assert_eq!(
            node_launch_command("http://192.168.1.5:9000/"),
            "ALPHANUMERIC_EXPLORER_API=192.168.1.5:9000 ./alphanumeric"
        );
    }

    fn fresh_address_state(index_height: u64) -> backend::AddressState {
        backend::AddressState {
            balance_units: 0,
            spendable_units: Some(0),
            transactions: Some(Vec::new()),
            history_available: true,
            index_ready: true,
            index_height: Some(index_height),
            next: None,
        }
    }

    // The node's index write is fail-open (blockchain.rs): a node can serve
    // `history_available: true` off a stale index for as long as it stays up.
    // `history_available` alone is therefore not enough to trust a "no
    // history" answer -- `index_height` must be checked against the chain
    // height this scan actually needs.
    #[test]
    fn a_lagging_or_unbuilt_index_cannot_answer_authoritatively() {
        let fresh = fresh_address_state(100);
        assert_eq!(
            interpret_address_state(&fresh, 100),
            ProbeOutcome::Answered(false)
        );

        // Behind the height this scan needs: not authoritative, even though
        // history_available/index_ready both say true.
        assert_eq!(interpret_address_state(&fresh, 101), ProbeOutcome::NotReady);

        // Unbuilt or mid full-rebuild: history_available is false regardless
        // of anything else.
        let rebuilding = backend::AddressState {
            history_available: false,
            ..fresh_address_state(100)
        };
        assert_eq!(
            interpret_address_state(&rebuilding, 100),
            ProbeOutcome::NotReady
        );

        // No index metadata at all.
        let no_meta = backend::AddressState {
            index_height: None,
            ..fresh_address_state(100)
        };
        assert_eq!(
            interpret_address_state(&no_meta, 100),
            ProbeOutcome::NotReady
        );

        // `index_ready` is checked on its own, isolated from
        // `history_available`. The two are always equal on the wire today
        // (the node computes both from the same check), but that is a
        // property of today's node, not of the protocol -- this function is
        // the one that decides whether a restored wallet keeps its
        // addresses, and dropping this clause must not silently pass.
        let index_not_ready = backend::AddressState {
            index_ready: false,
            ..fresh_address_state(100)
        };
        assert_eq!(
            interpret_address_state(&index_not_ready, 100),
            ProbeOutcome::NotReady
        );
    }

    #[test]
    fn an_authoritative_answer_reads_balance_spendable_and_transactions() {
        let base = fresh_address_state(10);
        assert_eq!(
            interpret_address_state(&base, 10),
            ProbeOutcome::Answered(false)
        );

        let has_balance = backend::AddressState {
            balance_units: 1,
            ..fresh_address_state(10)
        };
        assert_eq!(
            interpret_address_state(&has_balance, 10),
            ProbeOutcome::Answered(true)
        );

        let has_spendable = backend::AddressState {
            spendable_units: Some(1),
            ..fresh_address_state(10)
        };
        assert_eq!(
            interpret_address_state(&has_spendable, 10),
            ProbeOutcome::Answered(true)
        );

        let has_history = backend::AddressState {
            transactions: Some(vec![backend::TxEntry {
                amount_units: 1,
                fee_units: 0,
                counterparty: "x".into(),
                direction: "in".into(),
                height: 1,
                position: 0,
                timestamp: 1,
            }]),
            ..fresh_address_state(10)
        };
        assert_eq!(
            interpret_address_state(&has_history, 10),
            ProbeOutcome::Answered(true)
        );
    }

    // The two properties that protect funds in `scan_for_next_index`: a
    // retryable result retries the SAME index and is never counted as unused,
    // and the attempt cap terminates the loop. Both asserted here rather than
    // by reading, with fetch and wait injected so this needs no live node and
    // no real delays.
    #[tokio::test]
    async fn a_retryable_error_retries_the_same_index_and_never_marks_it_unused() {
        use std::cell::RefCell;
        use std::collections::HashMap;

        let calls: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let script: RefCell<HashMap<u32, u32>> = RefCell::new(HashMap::new());
        let fetch = |index: u32| {
            calls.borrow_mut().push(index);
            let attempt = {
                let mut script = script.borrow_mut();
                let counter = script.entry(index).or_insert(0);
                let seen = *counter;
                *counter += 1;
                seen
            };
            async move {
                if index == 0 && attempt < 2 {
                    // Busy twice, then answers -- the SAME index each time.
                    Err(backend::ApiError::Busy)
                } else if index == 0 {
                    Ok(ProbeOutcome::Answered(true))
                } else {
                    Ok(ProbeOutcome::Answered(false))
                }
            }
        };

        let result = scan_for_next_index(3, fetch, |_| async {}).await;

        assert_eq!(result.expect("must resolve"), 1);
        // Index 0 was asked three times -- two failures and the eventual
        // success -- never skipped past or counted as unused in the meantime.
        assert_eq!(calls.borrow().iter().filter(|&&i| i == 0).count(), 3);
    }

    #[tokio::test]
    async fn the_retry_cap_terminates_when_a_node_stays_busy() {
        use std::cell::Cell;

        let calls = Cell::new(0u32);
        let fetch = |_: u32| {
            calls.set(calls.get() + 1);
            async { Err(backend::ApiError::Busy) }
        };

        let result = scan_for_next_index(20, fetch, |_| async {}).await;

        assert!(matches!(
            result,
            Err(ScanFailure::Api(backend::ApiError::Busy))
        ));
        // The first attempt plus RETRY_ATTEMPTS retries, then it gives up --
        // not fewer (which would give up too early) and not more (which
        // would never terminate).
        assert_eq!(calls.get(), RETRY_ATTEMPTS + 1);
    }

    // Finding 1's regression case: if an index-cannot-answer result were ever
    // folded into "unused" the way a plain zero balance is, a used index
    // sitting behind one the index has not caught up to would be silently
    // skipped and the scan would keep going past it. Instead it must abort
    // the whole scan rather than return a number.
    #[tokio::test]
    async fn index_not_ready_fails_the_scan_rather_than_being_read_as_unused() {
        let fetch = |index: u32| async move {
            match index {
                0 => Ok(ProbeOutcome::Answered(true)),
                1 => Ok(ProbeOutcome::NotReady),
                _ => Ok(ProbeOutcome::Answered(false)),
            }
        };

        let result = scan_for_next_index(3, fetch, |_| async {}).await;

        assert!(matches!(result, Err(ScanFailure::IndexNotReady)));
    }

    use alphanumeric_gui::backend::NodeStatus;
    use alphanumeric_gui::startup::Phase;

    fn behind_status(behind: u64) -> NodeStatus {
        NodeStatus {
            height: Some(100),
            network_height: Some(100 + behind),
            blocks_behind: Some(behind),
            index_ready: true,
            index_height: Some(100),
            version: "8.0.0".into(),
            finalized_height: None,
            mining: None,
            mining_address: None,
            mining_backend: None,
            mining_hps: None,
            mining_blocks: None,
            mining_payout_rotation: None,
            gpu_built: false,
            gpu_devices: Vec::new(),
            mining_hashes: None,
            mining_difficulty: None,
            mining_expected_block_secs: None,
            mining_threads: None,
            mining_devices: Vec::new(),
        }
    }

    /// Once on the wallet screen, a few new blocks behind must not send it
    /// back to the sync screen. **This actually drives `update`** -- a
    /// test that sets a field and reads that same field back checks
    /// nothing.
    #[test]
    fn once_on_the_wallet_screen_a_lagging_status_does_not_take_it_back() {
        let (mut app, _) = App::new();
        app.screen = Screen::Wallet;
        let _ = app.update(Message::NodeStatusFetched(Ok(behind_status(500))));
        assert_eq!(
            app.screen,
            Screen::Wallet,
            "falling behind belongs in a banner, not a screen change"
        );
        assert!(matches!(app.node_phase, Phase::CatchingUp { .. }));
    }

    /// The reverse must actually happen: catching up on the sync screen
    /// leaves it.
    #[test]
    fn the_startup_screen_hands_over_once_the_node_is_caught_up() {
        let (mut app, _) = App::new();
        app.screen = Screen::Startup;
        let _ = app.update(Message::NodeStatusFetched(Ok(behind_status(0))));
        assert_ne!(
            app.screen,
            Screen::Startup,
            "a caught-up node must not leave the user on the sync screen"
        );
    }

    /// The hand-over above assigns `self.screen` directly rather than going
    /// through `Message::Show`, which is where "a revealed seed never
    /// survives a screen change" is normally enforced. Pinned on its own,
    /// with no `RetryNode` in between, so this fails even if `RetryNode`'s
    /// own screen change (which also used to bypass `Show`) were fixed
    /// alone.
    #[test]
    fn the_startup_hand_over_clears_a_revealed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.screen = Screen::Startup;
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: "f811b100866449d60739eeb137ee004e76fb09d4".to_string(),
            seed_hex: Zeroizing::new("66".repeat(32)),
        };

        let _ = app.update(Message::NodeStatusFetched(Ok(behind_status(0))));

        assert_eq!(app.screen, Screen::Wallet);
        assert!(
            matches!(
                app.wallet.as_ref().expect("wallet").reveal,
                RevealState::Idle
            ),
            "the auto-navigate away from Startup must clear a revealed seed too"
        );
    }

    fn status_at(height: u64) -> NodeStatus {
        NodeStatus {
            height: Some(height),
            network_height: Some(height + 5_000),
            blocks_behind: Some(5_000),
            ..behind_status(0)
        }
    }

    /// The startup screen's speed comes from the heights `NodeTick` already
    /// reads. Faking the clock by moving the origin back is enough to span
    /// the 20 s the rate needs.
    #[test]
    fn startup_status_reads_feed_the_catch_up_speed() {
        let (mut app, _) = App::new();
        app.sync_origin = std::time::Instant::now() - std::time::Duration::from_secs(60);
        let _ = app.update(Message::NodeStatusFetched(Ok(status_at(1_000))));
        app.sync_origin -= std::time::Duration::from_secs(30);
        let _ = app.update(Message::NodeStatusFetched(Ok(status_at(1_300))));
        let rate = app.sync_rate.blocks_per_sec().expect("30 s of samples");
        assert!((rate - 10.0).abs() < 0.5, "{rate}");
    }

    /// Unlock hides the node settings behind a toggle (plan ruling 6); the
    /// startup screen's escape must land with them open, or a node that will
    /// not start is unfixable before unlocking.
    #[test]
    fn the_startup_escape_opens_the_node_settings() {
        let (mut app, _) = App::new();
        assert!(!app.setup_settings_open);
        let _ = app.update(Message::OpenSetupSettings);
        assert!(app.setup_settings_open);
        assert_eq!(app.screen, Screen::Setup);
        let _ = app.update(Message::ToggleSetupSettings);
        assert!(!app.setup_settings_open);
    }

    /// Whether the flag that keeps polls from overlapping actually clears.
    /// If it doesn't, the subscription dies forever and the screen
    /// freezes.
    #[test]
    fn a_finished_poll_clears_the_in_flight_flag() {
        let (mut app, _) = App::new();
        app.startup_poll_in_flight = true;
        let _ = app.update(Message::NodeStatusFetched(Ok(behind_status(0))));
        assert!(!app.startup_poll_in_flight);
    }

    /// A retry must not poll the node that just failed (its old `node_url`),
    /// and must not carry forward a status reading that belonged to it --
    /// either would let a dead node's stale "caught up" answer wave the
    /// screen through before the new process has said anything.
    #[test]
    fn retry_resets_the_polled_url_and_the_carried_status_together() {
        let (mut app, _) = App::new();
        app.node_url = "http://127.0.0.1:8095".to_string();
        app.startup_status = Some(behind_status(0));
        let _ = app.update(Message::RetryNode);
        assert_eq!(
            app.node_url,
            node::explorer_url(node::DEFAULT_EXPLORER_PORT),
            "a retry must poll the wallet's own node, not whatever it was left pointed at"
        );
        assert!(
            app.startup_status.is_none(),
            "a stale reading from the node that just failed must not survive the retry"
        );
    }

    #[test]
    fn the_default_node_source_is_the_wallets_own() {
        let (app, _) = App::new();
        assert_eq!(app.node_source, NodeSource::Owned);
    }

    /// Choosing an external node means no child process starts -- someone
    /// who wants to use another node's shouldn't be made to download
    /// 173 MB.
    #[test]
    fn choosing_an_external_node_stops_owning_one() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        assert_eq!(app.node_source, NodeSource::External);
        assert!(app.supervisor.is_none());
    }

    #[test]
    fn an_external_node_keeps_the_editable_url() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        assert_eq!(app.node_url, DEFAULT_NODE_URL);
    }

    /// Rejecting a half-typed value while typing would leave the user
    /// unable to fix the port.
    #[test]
    fn a_half_typed_port_is_kept_as_typed() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::P2pPortChanged("81".into()));
        assert_eq!(app.p2p_port_input, "81");
        let _ = app.update(Message::StatsPortChanged("809".into()));
        assert_eq!(app.stats_port_input, "809");
    }

    #[test]
    fn a_typed_port_is_what_the_node_is_started_with() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::P2pPortChanged("7200".into()));
        let _ = app.update(Message::ExplorerPortChanged("8200".into()));
        let _ = app.update(Message::StatsPortChanged("8297".into()));
        assert_eq!(app.resolved_ports(), (7200, 8200, 8297));
    }

    /// An empty field or garbage falls back to the default -- a fallback,
    /// not a rejection.
    #[test]
    fn an_unusable_port_falls_back_to_the_default() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::P2pPortChanged("".into()));
        let _ = app.update(Message::ExplorerPortChanged("not a port".into()));
        assert_eq!(
            app.resolved_ports(),
            (
                alphanumeric_gui::node::DEFAULT_P2P_PORT,
                alphanumeric_gui::node::DEFAULT_EXPLORER_PORT,
                alphanumeric_gui::node::DEFAULT_STATS_PORT
            )
        );
    }

    /// An empty binary path means "not configured", not "an empty path" --
    /// `locate_binary` must receive `None` to look next to the GUI (Task 1).
    #[test]
    fn an_empty_binary_path_means_look_next_to_the_wallet() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::NodeBinaryChanged("   ".into()));
        assert!(app.configured_binary().is_none());
    }

    /// A binary path that cannot possibly resolve, so `ensure_owned_node`'s
    /// call to `build_node_config` fails deterministically and no child
    /// process is ever spawned -- regardless of whether a file named
    /// `alphanumeric` happens to sit in `target/debug/deps/` on the machine
    /// running the suite.
    fn unspawnable_binary_path() -> String {
        "/nonexistent/this-path-must-not-exist/alphanumeric".to_string()
    }

    // Spec G §4.1/§3.1: where the replaced wallet went is kept for the
    // session so F7 can say so.
    #[test]
    fn installing_over_an_archived_wallet_remembers_where_it_went() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, _) = App::new();
        app.wallet_path = Some(dir.path().join("seed.enc"));
        app.data_dir = Some(dir.path().join("node"));
        let archive = dir.path().join("seed-20260912-031500.enc");

        let _ = app.update(Message::SetupReady(Ok(Ready {
            master: MasterSeed::from_bytes([9u8; 32]),
            next_index: 1,
            status: None,
            passphrase: Zeroizing::new("pass".to_string()),
            node_url: "http://127.0.0.1:1".to_string(),
            archived: Some(archive.clone()),
            imported: Vec::new(),
        })));

        assert!(app.wallet.is_some());
        assert_eq!(app.last_archive, Some(archive));
    }

    #[test]
    fn installing_with_nothing_archived_keeps_the_earlier_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, _) = App::new();
        app.wallet_path = Some(dir.path().join("seed.enc"));
        app.data_dir = Some(dir.path().join("node"));
        let earlier = dir.path().join("seed-20260101-000000.enc");
        app.last_archive = Some(earlier.clone());

        let _ = app.update(Message::SetupReady(Ok(Ready {
            master: MasterSeed::from_bytes([9u8; 32]),
            next_index: 1,
            status: None,
            passphrase: Zeroizing::new("pass".to_string()),
            node_url: "http://127.0.0.1:1".to_string(),
            archived: None,
            imported: Vec::new(),
        })));

        assert_eq!(app.last_archive, Some(earlier));
    }

    #[test]
    fn an_archive_stamp_is_a_sortable_local_time() {
        let stamp = archive_stamp();
        assert_eq!(stamp.len(), "20260912-031500".len(), "{stamp}");
        assert!(
            stamp.chars().enumerate().all(|(i, c)| if i == 8 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }),
            "{stamp}"
        );
    }

    /// Major 2: on a fresh install `node_source` defaults to `Owned` and the
    /// picker renders it as already chosen, but before this fix nothing had
    /// actually started a node -- `node_url` stayed at its default, which on
    /// this machine is the MINING node's port, not the wallet's own. A
    /// restore or a fresh create must point the scan at the wallet's own
    /// node instead.
    #[test]
    fn start_create_points_at_the_owned_node_instead_of_the_stale_default() {
        let (mut app, _) = App::new();
        app.node_binary_input = unspawnable_binary_path();
        assert_eq!(
            app.node_url, DEFAULT_NODE_URL,
            "sanity: nothing has started a node yet"
        );
        let _ = app.update(Message::StartCreate);
        assert_eq!(
            app.node_url,
            node::explorer_url(node::DEFAULT_EXPLORER_PORT),
            "a fresh Create must scan the wallet's own node, not whatever node_url defaulted to"
        );
        assert!(matches!(app.setup, SetupStage::ConfirmBackup { .. }));
    }

    #[test]
    fn start_restore_points_at_the_owned_node_instead_of_the_stale_default() {
        let (mut app, _) = App::new();
        app.node_binary_input = unspawnable_binary_path();
        let _ = app.update(Message::StartRestore);
        assert_eq!(
            app.node_url,
            node::explorer_url(node::DEFAULT_EXPLORER_PORT),
            "a fresh Restore must scan the wallet's own node, not whatever node_url defaulted to"
        );
        assert!(matches!(app.setup, SetupStage::Restore { .. }));
    }

    /// An External choice must still be respected by the same guard --
    /// `StartCreate`/`StartRestore` must not start an owned node behind a
    /// user who explicitly picked someone else's.
    #[test]
    fn choosing_external_then_restoring_does_not_start_an_owned_node() {
        let (mut app, _) = App::new();
        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let _ = app.update(Message::StartRestore);
        assert_eq!(app.node_url, DEFAULT_NODE_URL);
        assert!(app.supervisor.is_none());
    }

    /// Major 2: picking "Run its own node" mid-setup must warm the node up
    /// behind the user, not take the screen away from them -- they are
    /// answering a question on `Choose`, not asking to watch the node boot.
    #[test]
    fn choosing_owned_starts_the_node_without_taking_the_screen() {
        let (mut app, _) = App::new();
        app.node_binary_input = unspawnable_binary_path();
        assert_eq!(app.screen, Screen::Setup);
        let _ = app.update(Message::ChooseNodeSource(NodeSource::Owned));
        assert_eq!(
            app.screen,
            Screen::Setup,
            "the node should start behind the user, not take over the screen"
        );
        assert_eq!(
            app.node_url,
            node::explorer_url(node::DEFAULT_EXPLORER_PORT)
        );
    }

    /// The takeover to the startup screen must still happen from the places
    /// that ARE supposed to show it -- `RetryNode`, used by the startup
    /// screen's own "TRY AGAIN", by `node_settings`'s "APPLY AND RESTART
    /// NODE" button, and by the wallet screen's owned-node banner.
    #[test]
    fn retry_node_takes_the_screen_to_startup_from_the_wallet_screen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_binary_input = unspawnable_binary_path();
        app.screen = Screen::Wallet;
        let _ = app.update(Message::RetryNode);
        assert_eq!(app.screen, Screen::Startup);
    }

    /// `Message::NodeUrlChanged`'s own comment states the invariant: a
    /// changed `node_url` must rebuild the wallet's client, or every fetch
    /// keeps hitting the OLD address regardless of what the field now says.
    /// `RetryNode` (reachable from the wallet screen's Owned banner via
    /// `node_settings`'s "APPLY AND RESTART NODE" button, now that a
    /// loaded wallet can reach `RetryNode` at all) changes `node_url` too and
    /// must uphold the same invariant.
    #[test]
    fn retry_node_rebuilds_the_wallets_client_to_match_the_new_node_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_binary_input = unspawnable_binary_path();
        let _ = app.update(Message::ExplorerPortChanged("8200".into()));
        let _ = app.update(Message::RetryNode);
        let wallet = app.wallet.as_ref().expect("wallet still loaded");
        assert_eq!(
            wallet.client.endpoint(""),
            format!("{}/", node::explorer_url(8200)),
            "the wallet's client must follow node_url, not keep fetching through the old port"
        );
    }

    /// The same argument `prev_cpu_sample` already makes for a restart: the
    /// old supervisor is dropped (SIGTERM) before a new one is built, so a
    /// `/stats` reading taken from it must not survive as though it still
    /// described whatever comes up next.
    #[test]
    fn retry_node_clears_the_old_stats_reading() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_binary_input = unspawnable_binary_path();
        app.node_stats = Some(backend::NodeStats {
            peers: Some(5),
            hashrate_ths: None,
            difficulty: None,
            uptime_secs: None,
            mempool: None,
            avg_block_time_secs: None,
            block_reward: None,
        });

        let _ = app.update(Message::RetryNode);

        assert_eq!(
            app.node_stats, None,
            "a restart must not let the old run's reading outlive it"
        );
    }

    /// Same invariant, the other node-source switch that changes `node_url`.
    /// Before this wave `ChooseNodeSource` could only run with no wallet
    /// loaded yet, so a stale client was never observable; `Setup` is now
    /// reachable with a wallet loaded (via the "Back to wallet" escape and
    /// the startup screen's "Node settings" button), so this path can be hit
    /// too.
    #[test]
    fn choosing_external_rebuilds_the_wallets_client_when_a_wallet_is_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_url = node::explorer_url(8200);
        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let wallet = app.wallet.as_ref().expect("wallet still loaded");
        assert_eq!(
            wallet.client.endpoint(""),
            format!("{}/", DEFAULT_NODE_URL),
            "External resets node_url to the default -- the client must follow it there too"
        );
    }

    /// `Setup` is now reachable with a wallet already loaded (Wallet ->
    /// RetryNode -> Startup -> "Node settings" -> Setup); nothing on that
    /// path used to get back, so `Show(Screen::Wallet)` must still work from
    /// there.
    #[test]
    fn showing_the_wallet_screen_from_setup_returns_to_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.screen = Screen::Setup;
        let _ = app.update(Message::Show(Screen::Wallet));
        assert_eq!(app.screen, Screen::Wallet);
    }

    /// Reveal a seed, then drive the exact round trip a dead owned node
    /// forces: the wallet screen's banner restarts it (`RetryNode`), landing
    /// on the startup screen; once the new node reports Ready,
    /// `NodeStatusFetched`'s auto-navigate takes the screen back to Wallet.
    /// Both of those are screen changes outside `Message::Show`'s own
    /// arm, which is where "a revealed seed never survives a screen change"
    /// is normally enforced -- before this fix they assigned `self.screen`
    /// directly and skipped it, so the seed was still on screen at the end,
    /// never cancelled by the user.
    #[test]
    fn the_owned_node_restart_round_trip_clears_a_revealed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_binary_input = unspawnable_binary_path();
        // A history list open at the same time, for the same reason: it is
        // dropped by `Message::Show`'s unconditional clear too, and the task
        // names it as part of the same failure.
        let _ = app.update(Message::HistoryOpen);
        app.screen = Screen::Wallet;
        app.wallet.as_mut().expect("wallet").reveal = RevealState::Revealed {
            address: "f811b100866449d60739eeb137ee004e76fb09d4".to_string(),
            seed_hex: Zeroizing::new("55".repeat(32)),
        };

        // "APPLY AND RESTART NODE" on the wallet screen's Owned banner.
        let _ = app.update(Message::RetryNode);
        assert_eq!(app.screen, Screen::Startup);

        // The new node reports Ready; the startup screen auto-navigates away.
        let _ = app.update(Message::NodeStatusFetched(Ok(behind_status(0))));
        assert_eq!(app.screen, Screen::Wallet);

        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(
            matches!(wallet.reveal, RevealState::Idle),
            "a seed revealed before the restart must not survive the round trip back"
        );
        assert!(
            wallet.history.is_none(),
            "a history list open before the restart must not survive as a stale, \
             complete-looking list missing every block mined during it"
        );
    }

    /// Tab keys do nothing before a wallet exists. Sending the user to the
    /// "Send" screen with no wallet to send from would show an empty
    /// screen.
    #[test]
    fn tab_keys_do_nothing_before_a_wallet_exists() {
        let (mut app, _) = App::new();
        assert_eq!(app.screen, Screen::Setup);
        let _ = app.update(Message::Show(Screen::Send));
        assert_eq!(app.screen, Screen::Setup);
    }

    /// F-8: "not mining" is a known fact. If the node sent a status at
    /// all, the `mining` field is always present (`mining_status_json`),
    /// so it's `Some((false, ..))`, not `None`. `None` is reserved for "no
    /// status response at all".
    #[test]
    fn mining_from_status_distinguishes_no_status_from_not_mining() {
        assert_eq!(mining_from_status(None), None);

        let idle = behind_status(0);
        assert_eq!(mining_from_status(Some(&idle)), Some((false, None, None)));

        let mut active = behind_status(0);
        active.mining = Some(true);
        active.mining_hps = Some(2.5e9);
        active.mining_backend = Some("gpu".to_string());
        assert_eq!(
            mining_from_status(Some(&active)),
            Some((true, Some(2.5e9), Some("gpu".to_string())))
        );
    }

    /// `ConsoleTick` does not fire again while a request is already
    /// outstanding -- it bundles three cadences (2s/10s/60s) into one
    /// tick, so without this lock against overlap the next tick would
    /// fire again before the response comes back.
    #[test]
    fn a_console_tick_does_not_issue_a_second_stats_request_while_one_is_outstanding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        // `/stats` is only ever fetched for a process that is actually
        // `Running` -- `app_with_a_wallet` starts no real supervisor (this
        // suite never spawns a process), so a fake one fixed at `Running` is
        // what makes this tick due to fetch at all.
        app.supervisor = Some(node::Supervisor::for_tests_with_state(
            node::NodeState::Running { pid: 1 },
        ));

        let _ = app.update(Message::ConsoleTick);
        assert!(app.stats_in_flight, "the first tick must fetch stats");

        // Back-date the last request so the 10-second cadence is due again --
        // the only thing left to hold a second request back is the
        // in-flight lock itself, not the timer.
        let backdated = std::time::Instant::now() - std::time::Duration::from_secs(11);
        app.last_stats_at = Some(backdated);

        let _ = app.update(Message::ConsoleTick);
        assert!(
            app.stats_in_flight,
            "still outstanding -- must not have been cleared"
        );
        assert_eq!(
            app.last_stats_at,
            Some(backdated),
            "a tick that finds a request already in flight must not re-stamp \
             or re-issue it, even once the cadence is due again"
        );
    }

    /// Unlike `/stats`, the data directory's size is not a reading TAKEN
    /// FROM the process -- it is just as measurable whether the process
    /// using it is up, still starting, or has exited. Its own cadence must
    /// therefore advance regardless of `running_pid`, not only inside the
    /// `Some(_)` arm `/stats` itself is gated on.
    #[test]
    fn disk_size_is_recomputed_on_its_own_cadence_regardless_of_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        assert!(app.last_disk_at.is_none(), "sanity: never yet computed");

        let _ = app.update(Message::ConsoleTick);

        assert!(
            app.last_disk_at.is_some(),
            "no supervisor means nothing is Running, but the disk walk must \
             still have run -- it does not describe the process"
        );
    }

    /// The cadence test above proves the walk ran, not what it found. This
    /// one measures: DISK is the size of the wallet's data directory, it is
    /// NOT re-walked inside its 10 s cadence, and it IS once the cadence is
    /// due -- a file the node wrote in between shows up then and not before.
    #[test]
    fn disk_measures_the_data_directory_and_refreshes_only_when_due() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let data = dir.path().join("node");
        std::fs::create_dir_all(data.join("blockchain.db")).expect("mkdir");
        std::fs::write(
            data.join("blockchain.db").join("chain.redb"),
            vec![0u8; 3000],
        )
        .expect("chain");

        let _ = app.update(Message::ConsoleTick);
        assert_eq!(app.disk_bytes, Some(3000));

        std::fs::write(data.join("node.log"), vec![0u8; 1000]).expect("log");
        let _ = app.update(Message::ConsoleTick);
        assert_eq!(
            app.disk_bytes,
            Some(3000),
            "inside the cadence: not re-walked"
        );

        app.last_disk_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(11));
        let _ = app.update(Message::ConsoleTick);
        assert_eq!(
            app.disk_bytes,
            Some(4000),
            "cadence due: the new file is counted"
        );
    }

    /// A data directory that does not exist yet (the first start, before
    /// the snapshot lands) is "not known", never "0 B".
    #[test]
    fn disk_of_a_data_directory_not_yet_created_is_a_dash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());

        let _ = app.update(Message::ConsoleTick);

        assert!(app.last_disk_at.is_some(), "the walk ran");
        assert_eq!(app.disk_bytes, None);
    }

    /// `cpu_percent` counts 100 per busy core and the gauge fills at one of
    /// them: a share of the whole machine put every reading the node ever
    /// reaches in the bar's first pixel.
    #[test]
    fn the_cpu_gauge_fills_at_one_busy_core() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.cpu_percent = Some(70.5);
        let share = app.console_data().cpu_share.expect("known");
        assert!((share - 0.705).abs() < 0.001, "{share}");
        app.cpu_percent = None;
        assert_eq!(app.console_data().cpu_share, None, "unknown stays unknown");
    }

    /// The grid shows the real resident figure beside the bar, not a
    /// percentage of anything.
    #[test]
    fn the_memory_gauge_carries_the_real_figure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.rss_kib = Some(594 * 1024);
        let data = app.console_data();
        assert_eq!(data.rss_kib, Some(594 * 1024));
        let share = data.mem_share.expect("known");
        assert!((0.2..0.4).contains(&share), "{share}");
    }

    /// A `/stats` reading describes whatever process answered it. Once that
    /// process is not `Running` -- here, no supervisor at all, the same
    /// shape `app_with_a_wallet` already leaves things in -- keeping the old
    /// reading asserts a live measurement of nothing.
    #[test]
    fn a_console_tick_clears_stats_for_a_process_that_is_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_stats = Some(backend::NodeStats {
            peers: Some(5),
            hashrate_ths: Some(1.0),
            difficulty: Some(2.0),
            uptime_secs: Some(3),
            mempool: Some(4),
            avg_block_time_secs: Some(5.0),
            block_reward: Some(6.0),
        });

        let _ = app.update(Message::ConsoleTick);

        assert_eq!(
            app.node_stats, None,
            "no supervisor means nothing is Running -- the old reading must \
             not survive the tick that finds that out"
        );
    }

    /// The same reasoning, for the point where the node this wallet talks to
    /// actually changes: a reading taken from the old one must not outlive
    /// the switch, on either side of it.
    #[test]
    fn choosing_a_different_node_source_clears_the_old_readings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.node_stats = Some(backend::NodeStats {
            peers: Some(5),
            hashrate_ths: None,
            difficulty: None,
            uptime_secs: None,
            mempool: None,
            avg_block_time_secs: None,
            block_reward: None,
        });
        app.wallet.as_mut().expect("wallet").node_status = Some(behind_status(0));

        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));

        assert_eq!(
            app.node_stats, None,
            "PEERS/HASHRATE/MEMPOOL/DIFFICULTY/BLOCK REWARD/AVG BLOCK must not \
             keep asserting a measurement of the node just switched away from"
        );
        assert_eq!(
            app.wallet.as_ref().expect("wallet").node_status,
            None,
            "the wallet screen's own status reading is just as stale as the \
             console strip's"
        );
    }

    /// Node/Settings/History have no `PollTick` of their own --
    /// `ConsoleTick` is the only thing that can keep `wallet.node_status`
    /// (and so the header's SYNCED, HEIGHT and MINING chips) live there.
    #[test]
    fn console_tick_polls_status_on_a_screen_polltick_does_not_cover() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.screen = Screen::Node;

        let _ = app.update(Message::ConsoleTick);

        assert!(
            app.console_status_in_flight,
            "Node has no poll of its own for wallet.node_status -- without \
             this fetch it freezes at whatever the wallet screen last read"
        );
    }

    /// The wallet screen's own 10-second poll (`PollTick`) already keeps
    /// `wallet.node_status` current -- `ConsoleTick` must not fetch it a
    /// second time there.
    #[test]
    fn console_tick_does_not_double_poll_status_where_polltick_already_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.screen = Screen::Wallet;

        let _ = app.update(Message::ConsoleTick);

        assert!(
            !app.console_status_in_flight,
            "Wallet/Receive/Send are already covered by PollTick -- a second \
             fetch from ConsoleTick here would just double the request rate \
             against /explorer/status for nothing"
        );
    }

    /// `ConsoleStatusFetched` applies to the same place `PollTick` and
    /// `RefreshAll` already do, so every consumer of `wallet.node_status`
    /// sees one answer no matter which poll produced it.
    #[test]
    fn console_status_fetched_updates_the_wallet_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());

        let _ = app.update(Message::ConsoleStatusFetched(
            app.node_epoch,
            Ok(behind_status(3)),
        ));

        assert_eq!(
            app.wallet.as_ref().expect("wallet").node_status,
            Some(behind_status(3))
        );
    }

    /// `wallet.node_status` is fed by `apply_status`, which keeps the last
    /// good value on any NON-retryable error -- and connection-refused
    /// (what every poll gets once an Owned process has exited) classifies as
    /// exactly that. Left alone, SYNCED/HEIGHT/VERSION and the MINING chip
    /// would keep describing a process the same tick already knows is gone,
    /// directly above a Node screen reading `STATE Exited`.
    #[test]
    fn a_console_tick_clears_status_for_a_process_that_is_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").node_status = Some(behind_status(0));

        let _ = app.update(Message::ConsoleTick);

        assert_eq!(
            app.wallet.as_ref().expect("wallet").node_status,
            None,
            "no supervisor means nothing is Running -- the old status reading \
             must not survive the tick that finds that out, the same as \
             node_stats"
        );
    }

    /// A `/explorer/status` request issued against the OLD client can still
    /// answer after `ChooseNodeSource` has cleared `wallet.node_status` --
    /// applying it would repopulate exactly what the clear removed.
    #[test]
    fn a_stale_console_status_answer_from_before_a_source_change_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let epoch_before_switch = app.node_epoch;

        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let _ = app.update(Message::ConsoleStatusFetched(
            epoch_before_switch,
            Ok(behind_status(0)),
        ));

        assert_eq!(
            app.wallet.as_ref().expect("wallet").node_status,
            None,
            "an answer carrying the epoch from before the switch describes \
             the node just switched away from and must not be applied"
        );
    }

    /// `ConsoleStatusFetched` was the only answer that checked the epoch.
    /// `PollTick` issues its status read against the same cloned client, so
    /// an answer landing after a source change repopulated exactly what the
    /// change cleared -- on the three screens the user is most likely on.
    /// The in-flight flag still has to come down, or the next tick is
    /// refused forever.
    #[test]
    fn a_stale_poll_tick_answer_from_before_a_source_change_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let before = app.node_epoch;
        app.wallet.as_mut().expect("wallet").poll_in_flight = true;

        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let _ = app.update(Message::PollTickFetched(
            before,
            Ok(behind_status(0)),
            Err(backend::ApiError::Transport("old node".into())),
        ));

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(
            wallet.node_status, None,
            "the old node's reading must not return"
        );
        assert!(
            !wallet.poll_in_flight,
            "a dropped answer still ends the request"
        );
        assert_eq!(
            wallet.addresses[wallet.active].error, None,
            "the old node's failure is not news about the new one"
        );
    }

    /// Same for the full refresh -- and it set every row to "Updating..."
    /// when it was issued, so dropping its answer has to take that down too,
    /// or every row says a retry is coming that never will.
    #[test]
    fn a_stale_refresh_answer_is_dropped_and_its_rows_stop_loading() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let before = app.node_epoch;
        {
            let wallet = app.wallet.as_mut().expect("wallet");
            wallet.fetch_in_flight = true;
            for entry in &mut wallet.addresses {
                entry.loading = true;
            }
        }

        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let rows = app.wallet.as_ref().expect("wallet").addresses.len();
        let _ = app.update(Message::RefreshAllFetched(
            before,
            Ok(behind_status(0)),
            (0..rows)
                .map(|_| Err(backend::ApiError::Transport("old node".into())))
                .collect(),
        ));

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.node_status, None);
        assert!(!wallet.fetch_in_flight);
        assert!(wallet
            .addresses
            .iter()
            .all(|e| !e.loading && e.error.is_none()));
    }

    /// History's entry read, same shape.
    #[test]
    fn a_stale_history_status_answer_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let before = app.node_epoch;

        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let _ = app.update(Message::HistoryStatusFetched(before, Ok(behind_status(0))));

        assert_eq!(app.wallet.as_ref().expect("wallet").node_status, None);
    }

    /// A restart clears `node_stats` because the old process is gone. A
    /// `/stats` answer from that old process landing afterwards put its
    /// numbers back for up to one 10s cadence -- the Owned tick only clears
    /// on a non-`Running` state, and the new child is `Running`.
    #[test]
    fn a_stale_stats_answer_from_before_a_restart_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let before = app.node_epoch;
        app.stats_in_flight = true;

        app.retarget_client();
        let _ = app.update(Message::StatsFetched(
            before,
            Ok(backend::NodeStats {
                peers: Some(11),
                hashrate_ths: Some(27.8),
                difficulty: Some(464.0),
                uptime_secs: Some(1),
                mempool: None,
                avg_block_time_secs: None,
                block_reward: None,
            }),
        ));

        assert_eq!(app.node_stats, None);
        assert!(!app.stats_in_flight);
    }

    /// Every place that replaces the client moves the epoch -- that is the
    /// whole mechanism. Typing an external URL retargets on each keystroke.
    #[test]
    fn every_client_retarget_moves_the_epoch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());

        let e0 = app.node_epoch;
        let _ = app.update(Message::NodeUrlChanged("http://127.0.0.1:9000".into()));
        let e1 = app.node_epoch;
        let _ = app.update(Message::ChooseNodeSource(NodeSource::External));
        let e2 = app.node_epoch;

        assert_ne!(e0, e1, "NodeUrlChanged replaced the client");
        assert_ne!(e1, e2, "ChooseNodeSource replaced the client");
    }

    /// The explorer port is resolved once, at node-start time, and frozen
    /// into `node_url`. The stats port must get the same treatment: typing a
    /// new one under the "takes effect at the next node start" caption must
    /// not retarget the poll before a node has actually started with it.
    #[test]
    fn the_stats_poll_follows_the_port_the_node_was_started_with_not_the_live_field() {
        let (mut app, _) = App::new();
        app.stats_port_input = "9999".into();
        assert_eq!(
            app.node_stats_url,
            node::stats_url(node::DEFAULT_STATS_PORT),
            "typing alone must not move the poll target"
        );

        app.node_binary_input = unspawnable_binary_path();
        let _ = app.update(Message::ChooseNodeSource(NodeSource::Owned));

        assert_eq!(
            app.node_stats_url,
            node::stats_url(9999),
            "the port is frozen in at start time -- even when starting the \
             node itself then fails, the port it WOULD have used is what the \
             next poll targets, exactly as node_url already works"
        );
    }

    /// R1: `narrow()` flips at `kit::NARROW_BELOW` once a resize lands, not
    /// before -- the starting width (1000.0, `main.rs`'s `window_settings`)
    /// is above it.
    #[test]
    fn narrow_flips_at_the_threshold_after_a_window_resized_message() {
        let (mut app, _) = App::new();
        assert!(!app.narrow(), "the starting width is above the threshold");

        let _ = app.update(Message::WindowResized(crate::view::kit::NARROW_BELOW - 0.1));
        assert!(app.narrow());

        let _ = app.update(Message::WindowResized(crate::view::kit::NARROW_BELOW));
        assert!(!app.narrow(), "the threshold itself is not narrow");

        let _ = app.update(Message::WindowResized(1000.0));
        assert!(!app.narrow(), "widening back out un-narrows it");
    }

    fn asking_request(app: &App) -> Option<u64> {
        match &app.wallet.as_ref()?.master_reveal {
            MasterRevealState::Asking {
                busy: true,
                request,
                ..
            } => Some(*request),
            _ => None,
        }
    }

    // Spec G §3.3: the passphrase is checked by the task; only the request
    // still waiting takes its answer.
    #[test]
    fn a_master_seed_is_shown_only_to_the_request_still_waiting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::MasterRevealStart);
        let _ = app.update(Message::MasterRevealPassphraseChanged("pass".into()));
        let _ = app.update(Message::MasterRevealConfirm);
        let first = asking_request(&app).expect("asking");

        // Cancelled and asked again: the first answer belongs to nobody.
        let _ = app.update(Message::MasterRevealHide);
        let _ = app.update(Message::MasterRevealStart);
        let _ = app.update(Message::MasterRevealPassphraseChanged("pass".into()));
        let _ = app.update(Message::MasterRevealConfirm);
        let second = asking_request(&app).expect("asking again");
        assert_ne!(first, second);

        let _ = app.update(Message::MasterRevealDone(
            first,
            Err("Could not open the wallet file: wrong passphrase".into()),
        ));
        assert_eq!(
            asking_request(&app),
            Some(second),
            "a stale refusal changes nothing"
        );

        let _ = app.update(Message::MasterRevealDone(
            second,
            Ok(MasterSeedText(Zeroizing::new("a9m1seed".into()))),
        ));
        assert!(matches!(
            &app.wallet.as_ref().expect("wallet").master_reveal,
            MasterRevealState::Revealed { seed } if seed.as_str() == "a9m1seed"
        ));
    }

    #[test]
    fn a_refused_passphrase_empties_the_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::MasterRevealStart);
        let _ = app.update(Message::MasterRevealPassphraseChanged("wrong".into()));
        let _ = app.update(Message::MasterRevealConfirm);
        let request = asking_request(&app).expect("asking");
        let _ = app.update(Message::MasterRevealDone(
            request,
            Err("wrong passphrase".into()),
        ));
        match &app.wallet.as_ref().expect("wallet").master_reveal {
            MasterRevealState::Asking {
                passphrase,
                error,
                busy,
                ..
            } => {
                assert!(passphrase.is_empty());
                assert!(error.is_some());
                assert!(!busy);
            }
            _ => panic!("still asking"),
        }
    }

    // Spec G §4.5: a revealed master seed never survives a screen change.
    #[test]
    fn a_revealed_master_seed_is_gone_after_any_screen_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").master_reveal = MasterRevealState::Revealed {
            seed: Zeroizing::new("a9m1seed".into()),
        };
        let _ = app.update(Message::Show(Screen::Node));
        assert!(matches!(
            app.wallet.as_ref().expect("wallet").master_reveal,
            MasterRevealState::Idle
        ));

        app.wallet.as_mut().expect("wallet").master_reveal = MasterRevealState::Revealed {
            seed: Zeroizing::new("a9m1seed".into()),
        };
        let _ = app.update(Message::HistoryOpen);
        assert!(matches!(
            app.wallet.as_ref().expect("wallet").master_reveal,
            MasterRevealState::Idle
        ));
    }

    // M4: a success that lands after HIDE has nowhere to go -- the request
    // it answers is no longer the one anybody is waiting for.
    #[test]
    fn a_stale_seed_reveal_success_after_hide_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::MasterRevealStart);
        let _ = app.update(Message::MasterRevealPassphraseChanged("pass".into()));
        let _ = app.update(Message::MasterRevealConfirm);
        let request = asking_request(&app).expect("asking");

        let _ = app.update(Message::MasterRevealHide);
        let _ = app.update(Message::MasterRevealDone(
            request,
            Ok(MasterSeedText(Zeroizing::new("a9m1seed".into()))),
        ));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").master_reveal,
            MasterRevealState::Idle
        ));
    }

    // M4: sibling case -- a screen change, not HIDE, is what happened before
    // the answer landed.
    #[test]
    fn a_stale_seed_reveal_success_after_a_screen_change_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::MasterRevealStart);
        let _ = app.update(Message::MasterRevealPassphraseChanged("pass".into()));
        let _ = app.update(Message::MasterRevealConfirm);
        let request = asking_request(&app).expect("asking");

        let _ = app.update(Message::Show(Screen::Node));
        let _ = app.update(Message::MasterRevealDone(
            request,
            Ok(MasterSeedText(Zeroizing::new("a9m1seed".into()))),
        ));

        assert!(matches!(
            app.wallet.as_ref().expect("wallet").master_reveal,
            MasterRevealState::Idle
        ));
    }

    // An export answer is applied only while this wallet is waiting for one.
    #[test]
    fn an_export_answer_lands_only_while_an_export_is_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let saved = dir.path().join("backup.enc");

        let _ = app.update(Message::ExportWalletFileDone(Ok(Some(saved.clone()))));
        assert!(app.wallet.as_ref().expect("wallet").export_result.is_none());

        app.wallet.as_mut().expect("wallet").exporting = true;
        let _ = app.update(Message::ExportWalletFileDone(Ok(Some(saved.clone()))));
        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(!wallet.exporting);
        assert_eq!(wallet.export_result, Some(Ok(saved)));
    }

    #[test]
    fn a_cancelled_export_dialog_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").exporting = true;
        let _ = app.update(Message::ExportWalletFileDone(Ok(None)));
        let wallet = app.wallet.as_ref().expect("wallet");
        assert!(!wallet.exporting);
        assert!(wallet.export_result.is_none());
    }

    /// Closes the wallet for import. Also points the node binary at a path
    /// that cannot spawn: import-mode tests go on to `StartRestore` /
    /// `StartImportFile`, which call `ensure_owned_node()` (Global
    /// Constraints). `app_with_a_wallet` already gives a temporary data_dir.
    fn import_started(app: &mut App) {
        app.node_binary_input = unspawnable_binary_path();
        let _ = app.update(Message::ImportStart);
        let _ = app.update(Message::ImportContinue);
    }

    fn wallet_file(seed: u8, next_index: u32) -> Arc<alphanumeric_gui::import::WalletFile> {
        let master = MasterSeed::from_bytes([seed; 32]);
        Arc::new(alphanumeric_gui::import::WalletFile {
            envelope: vec![1, 2, 3],
            first_address: model::address_for_index(&master, 0),
            master,
            next_index,
            imported: Vec::new(),
        })
    }

    fn opening(app: &mut App, path: &Path) {
        let _ = app.update(Message::StartImportFile);
        let _ = app.update(Message::ImportFilePicked(Some(path.to_path_buf())));
        let _ = app.update(Message::ImportPassphraseChanged("its own".into()));
        let _ = app.update(Message::ImportOpen);
    }

    /// What `ImportOpen`'s task hands back on success: the file and the
    /// passphrase that opened it, side by side as `Message::ImportOpened`
    /// now requires.
    fn opened(
        seed: u8,
        next_index: u32,
        passphrase: &str,
    ) -> (Arc<alphanumeric_gui::import::WalletFile>, Zeroizing<String>) {
        (
            wallet_file(seed, next_index),
            Zeroizing::new(passphrase.to_string()),
        )
    }

    fn preview_of(app: &App) -> Option<u32> {
        match &app.setup {
            SetupStage::ImportFile { preview, .. } => {
                preview.as_ref().map(|opened| opened.file.next_index)
            }
            _ => None,
        }
    }

    #[test]
    fn an_opened_file_is_previewed_and_editing_the_passphrase_clears_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::ImportOpened(Ok(opened(7, 3, "its own"))));
        assert_eq!(preview_of(&app), Some(3));

        let _ = app.update(Message::ImportPassphraseChanged("edited".into()));
        assert_eq!(
            preview_of(&app),
            None,
            "USE must write what THIS passphrase opened"
        );
    }

    // Controller ruling, Task 6 fix round 1: plan ruling 5 must hold by
    // construction, not by timing. An edit made while OPEN is still running
    // must never reach the wallet USE installs.
    #[test]
    fn a_passphrase_edited_while_opening_never_reaches_the_installed_wallet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc")); // opened with "its own"

        // Still mid-OPEN (`opening` never runs the dispatched task): the edit
        // must be refused outright, not merely overwritten later.
        let _ = app.update(Message::ImportPassphraseChanged("edited".into()));
        match &app.setup {
            SetupStage::ImportFile { passphrase, .. } => {
                assert_eq!(
                    passphrase.as_str(),
                    "its own",
                    "an edit while busy must be refused"
                );
            }
            _ => panic!("still importing a file"),
        }

        // The open answers with the passphrase it actually used.
        let _ = app.update(Message::ImportOpened(Ok(opened(7, 3, "its own"))));
        match &app.setup {
            SetupStage::ImportFile {
                preview: Some(opened),
                ..
            } => {
                assert_eq!(opened.passphrase.as_str(), "its own");
            }
            _ => panic!("expected a preview"),
        }

        // M10: the test's name claims something about what ImportUse
        // actually SENDS, not merely what the stage stores -- prove USE
        // picks up the passphrase that opened the file, not any edit.
        let _ = app.update(Message::ImportUse);
        match &app.setup {
            SetupStage::ImportFile {
                busy: true,
                preview: Some(opened),
                ..
            } => {
                assert_eq!(opened.passphrase.as_str(), "its own");
            }
            _ => panic!("expected ImportFile {{ busy: true, preview: Some(_), .. }}"),
        }
    }

    #[test]
    fn importing_the_file_of_the_wallet_that_was_open_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::ImportOpened(Ok(opened(9, 1, "its own"))));
        match &app.setup {
            SetupStage::ImportFile {
                preview,
                error,
                busy,
                ..
            } => {
                assert!(preview.is_none());
                assert_eq!(error.as_deref(), Some(SAME_WALLET));
                assert!(!busy);
            }
            _ => panic!("still importing a file"),
        }
    }

    #[test]
    fn a_file_that_does_not_open_says_why_and_empties_the_passphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::ImportOpened(Err("wrong passphrase".into())));
        match &app.setup {
            SetupStage::ImportFile {
                preview,
                error,
                busy,
                passphrase,
                ..
            } => {
                assert!(preview.is_none());
                assert!(error.is_some());
                assert!(!busy);
                assert!(passphrase.is_empty());
            }
            _ => panic!("still importing a file"),
        }
    }

    #[test]
    fn an_answer_for_an_abandoned_open_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::BackToChoose);
        let _ = app.update(Message::StartImportFile);
        let _ = app.update(Message::ImportOpened(Ok(opened(7, 3, "its own"))));
        assert_eq!(preview_of(&app), None, "a fresh stage never asked");
    }

    #[test]
    fn the_first_run_screen_can_start_a_file_import() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, _) = App::new();
        app.node_binary_input = unspawnable_binary_path();
        app.data_dir = Some(dir.path().join("node"));
        app.wallet_path = Some(dir.path().join("seed.enc"));
        app.setup = SetupStage::Choose;
        let _ = app.update(Message::StartImportFile);
        assert!(matches!(app.setup, SetupStage::ImportFile { .. }));
    }

    // Spec G §4.1/§4.4, end to end on disk: the old file is set aside, the
    // chosen file lands byte for byte, and the wallet it describes comes back.
    #[tokio::test]
    async fn using_an_imported_file_sets_the_old_one_aside_and_writes_it_as_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wallet_path = dir.path().join("seed.enc");
        storage::save(
            &wallet_path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("old wallet");
        let old_bytes = std::fs::read(&wallet_path).expect("old");
        let chosen = dir.path().join("exported.enc");
        storage::save(
            &chosen,
            &MasterSeed::from_bytes([7u8; 32]),
            br#"{"next_index":3}"#,
            b"its own",
        )
        .expect("chosen");
        let file = Arc::new(
            alphanumeric_gui::import::open_wallet_file(&chosen, b"its own").expect("opens"),
        );

        let ready = use_imported_file(
            wallet_path.clone(),
            "20260912-031500".into(),
            Arc::clone(&file),
            Zeroizing::new("its own".into()),
            "http://127.0.0.1:1".into(),
        )
        .await
        .expect("imported");

        assert_eq!(ready.next_index, 3);
        let archive = ready.archived.clone().expect("archived");
        assert_eq!(std::fs::read(&archive).expect("archive"), old_bytes);
        assert_eq!(
            std::fs::read(&wallet_path).expect("new"),
            std::fs::read(&chosen).expect("chosen")
        );
        assert!(
            storage::load(&wallet_path, b"its own").is_ok(),
            "opens with its own passphrase"
        );

        // M10: the test's name claims the wallet is INSTALLED, not merely
        // written to disk -- prove it lands on a running `App`.
        let (mut app, _) = App::new();
        app.wallet_path = Some(wallet_path.clone());
        app.data_dir = Some(dir.path().join("node"));
        app.node_binary_input = unspawnable_binary_path();
        let _ = app.update(Message::SetupReady(Ok(ready)));

        let wallet = app.wallet.as_ref().expect("installed");
        assert_eq!(wallet.addresses.len(), 3);
        assert!(app.last_archive.is_some());
    }

    // Controller ruling, Task 6: `ImportFile { busy: true, .. }` is a write in
    // progress (USE) or an open in progress (OPEN) -- either way the same
    // "let it finish" reasoning as `SetPassphrase { busy: true, .. }` applies.
    #[test]
    fn cancel_is_refused_while_an_imported_file_is_being_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));

        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));

        let _ = app.update(Message::ImportAbort);

        assert!(
            app.importing.is_some(),
            "cancel must not be honoured while a file is being opened"
        );
        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));
    }

    // Sibling of the above: the same refusal, but for the USE-busy phase
    // rather than OPEN-busy.
    #[test]
    fn cancel_is_refused_while_the_opened_file_is_being_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::ImportOpened(Ok(opened(7, 3, "its own"))));
        let _ = app.update(Message::ImportUse);

        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));

        let _ = app.update(Message::ImportAbort);

        assert!(
            app.importing.is_some(),
            "cancel must not be honoured while a file is being saved"
        );
        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));
    }

    // I2: an export left open behind an rfd dialog must not let an import
    // close the wallet out from under it -- Save pressed afterward would
    // write the NEW wallet's bytes under the name EXPORT suggested for the
    // old one.
    #[test]
    fn import_is_refused_while_an_export_is_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").exporting = true;

        assert!(app.import_blocker().is_some());

        let _ = app.update(Message::ImportStart);
        assert!(
            app.wallet
                .as_ref()
                .expect("wallet")
                .import_confirm
                .is_none(),
            "a blocked import must not open the confirm panel"
        );
    }

    // M6: the same defense in depth as `ImportAbort` -- BACK must not throw
    // the stage away while OPEN or USE is still running underneath it.
    #[test]
    fn back_to_choose_is_refused_while_setup_is_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));

        let _ = app.update(Message::BackToChoose);

        assert!(matches!(
            app.setup,
            SetupStage::ImportFile { busy: true, .. }
        ));
    }

    // Spec G §4.2 + planning ruling 2.
    #[test]
    fn import_is_refused_while_a_payment_or_an_address_save_is_in_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        assert_eq!(app.import_blocker(), None);

        app.wallet.as_mut().expect("wallet").send.outstanding =
            Some(Arc::new(serde_json::json!({})));
        assert!(app.import_blocker().is_some());
        let _ = app.update(Message::ImportStart);
        assert!(app
            .wallet
            .as_ref()
            .expect("wallet")
            .import_confirm
            .is_none());

        app.wallet.as_mut().expect("wallet").send.outstanding = None;
        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Sending;
        assert!(app.import_blocker().is_some());

        app.wallet.as_mut().expect("wallet").send.stage = SendStage::Compose;
        app.wallet.as_mut().expect("wallet").adding_address = true;
        assert!(app.import_blocker().is_some());
        let _ = app.update(Message::ImportStart);
        let _ = app.update(Message::ImportContinue);
        assert!(
            app.wallet.is_some(),
            "a blocked import never closes the wallet"
        );
    }

    #[test]
    fn import_start_names_where_the_current_file_will_go_and_hides_the_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        app.wallet.as_mut().expect("wallet").master_reveal = MasterRevealState::Revealed {
            seed: Zeroizing::new("a9m1seed".into()),
        };
        let _ = app.update(Message::ImportStart);
        let wallet = app.wallet.as_ref().expect("wallet");
        let archive = wallet.import_confirm.as_ref().expect("confirming");
        assert_eq!(archive.parent(), Some(dir.path()));
        let name = archive.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(
            name.starts_with("seed-") && name.ends_with(".enc"),
            "{name}"
        );
        assert!(matches!(wallet.master_reveal, MasterRevealState::Idle));

        let _ = app.update(Message::ImportCancelConfirm);
        assert!(app
            .wallet
            .as_ref()
            .expect("wallet")
            .import_confirm
            .is_none());
    }

    #[test]
    fn continuing_closes_the_wallet_and_opens_the_import_screen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let first = app.wallet.as_ref().expect("wallet").addresses[0]
            .address
            .clone();
        let epoch = app.node_epoch;

        let _ = app.update(Message::ImportContinue);
        assert!(
            app.wallet.is_some(),
            "CONTINUE without the confirm panel does nothing"
        );

        import_started(&mut app);
        assert!(app.wallet.is_none());
        assert_eq!(app.screen, Screen::Setup);
        assert!(matches!(app.setup, SetupStage::Choose));
        assert_eq!(
            app.importing
                .as_ref()
                .map(|c| c.replaced_first_address.clone()),
            Some(first)
        );
        assert_ne!(app.node_epoch, epoch, "every answer in flight is now stale");
    }

    // Spec G §3.4: cancel before the new wallet is written changes nothing.
    #[test]
    fn cancelling_the_import_goes_back_to_unlocking_the_untouched_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("save");
        let before = std::fs::read(&path).expect("read");
        let mut app = app_with_a_wallet(dir.path());

        import_started(&mut app);
        let _ = app.update(Message::StartRestore);
        let _ = app.update(Message::ImportAbort);

        assert!(app.importing.is_none());
        assert!(matches!(app.setup, SetupStage::Unlock { .. }));
        assert_eq!(app.screen, Screen::Setup);
        assert_eq!(std::fs::read(&path).expect("read"), before);
        assert!(storage::find_archives(&path).is_empty());
    }

    // M7: a rollback that also failed leaves no wallet file behind, only its
    // archive -- the first-run screen `ImportAbort` falls back to must be
    // able to name it.
    #[test]
    fn import_abort_with_no_wallet_file_recomputes_the_archive_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.enc");
        storage::save(
            &path,
            &MasterSeed::from_bytes([9u8; 32]),
            br#"{"next_index":1}"#,
            b"pass",
        )
        .expect("save");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);

        // Simulate a rollback that also failed: the wallet file is gone and
        // only its archive remains.
        let archive = dir.path().join("seed-20260912-031500.enc");
        std::fs::rename(&path, &archive).expect("rename to archive");

        let _ = app.update(Message::ImportAbort);

        assert!(matches!(app.setup, SetupStage::Choose));
        assert_eq!(app.archives_found, vec![archive]);
    }

    // Controller ruling, Task 5 fix round 1: once the save is dispatched,
    // `SetupReady` installs it regardless of `importing` -- cancel can no
    // longer be honoured, so it must refuse instead of silently doing
    // nothing useful while the write finishes underneath it.
    #[test]
    fn cancel_is_refused_while_the_new_wallet_is_being_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        let _ = app.update(Message::StartRestore);
        let other = MasterSeed::from_bytes([7u8; 32]).encode();
        let _ = app.update(Message::RestoreSeedInputChanged(other.to_string()));
        let _ = app.update(Message::UseSeedInput);
        let _ = app.update(Message::NewPassphraseChanged("p".to_string()));
        let _ = app.update(Message::NewPassphraseConfirmChanged("p".to_string()));
        // The returned Task (the discovery scan) is never run -- `busy` is
        // set synchronously before it is dispatched, which is exactly the
        // window this guards.
        let _ = app.update(Message::ConfirmNewPassphrase);
        match &app.setup {
            SetupStage::SetPassphrase { busy, .. } => assert!(*busy, "should be saving"),
            _ => panic!("expected SetPassphrase {{ busy: true, .. }}"),
        }

        let _ = app.update(Message::ImportAbort);

        assert!(
            app.importing.is_some(),
            "cancel must not be honoured mid-save"
        );
        assert!(matches!(
            app.setup,
            SetupStage::SetPassphrase { busy: true, .. }
        ));
    }

    // Spec G §4.6: answers the closed wallet asked for never reach the next one.
    #[test]
    fn a_poll_answer_for_the_closed_wallet_is_not_applied_to_the_new_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let old_epoch = app.node_epoch;
        import_started(&mut app);
        app.install_wallet(
            MasterSeed::from_bytes([7u8; 32]),
            1,
            None,
            Zeroizing::new("other".to_string()),
            "http://127.0.0.1:1",
            Vec::new(),
        )
        .expect("install");

        let mut funded = fresh_address_state(100);
        funded.balance_units = 5_000_000_000;
        let _ = app.update(Message::PollTickFetched(
            old_epoch,
            Ok(behind_status(0)),
            Ok(funded),
        ));

        let wallet = app.wallet.as_ref().expect("wallet");
        assert_eq!(wallet.addresses[0].balance_units, None);
        assert_eq!(wallet.node_status, None);
    }

    #[test]
    fn a_history_page_for_the_closed_wallet_is_not_applied_to_the_new_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        let _ = app.update(Message::HistoryOpen);
        let old_generation = app
            .wallet
            .as_ref()
            .and_then(|w| w.history.as_ref())
            .expect("history")
            .generation;
        import_started(&mut app);
        app.install_wallet(
            MasterSeed::from_bytes([7u8; 32]),
            1,
            None,
            Zeroizing::new("other".to_string()),
            "http://127.0.0.1:1",
            Vec::new(),
        )
        .expect("install");
        let _ = app.update(Message::HistoryOpen);

        let _ = app.update(Message::HistoryPageFetched(
            0,
            old_generation,
            None,
            Err(backend::ApiError::Transport("old wallet's page".into())),
        ));

        let history = app
            .wallet
            .as_ref()
            .and_then(|w| w.history.as_ref())
            .expect("history");
        assert_ne!(history.generation, old_generation);
        assert!(
            history.error.is_none(),
            "the closed wallet's page did not land here"
        );
    }

    // Spec G §4.3.
    #[test]
    fn restoring_the_wallet_that_was_open_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        let _ = app.update(Message::StartRestore);
        let same = MasterSeed::from_bytes([9u8; 32]).encode();
        let _ = app.update(Message::RestoreSeedInputChanged(same.to_string()));
        let _ = app.update(Message::UseSeedInput);
        match &app.setup {
            SetupStage::Restore { seed_error, .. } => {
                assert_eq!(seed_error.as_deref(), Some(SAME_WALLET));
            }
            _ => panic!("still on the restore stage"),
        }

        let other = MasterSeed::from_bytes([7u8; 32]).encode();
        let _ = app.update(Message::RestoreSeedInputChanged(other.to_string()));
        let _ = app.update(Message::UseSeedInput);
        assert!(matches!(app.setup, SetupStage::SetPassphrase { .. }));
    }

    #[test]
    fn a_finished_import_leaves_import_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        let _ = app.update(Message::SetupReady(Ok(Ready {
            master: MasterSeed::from_bytes([7u8; 32]),
            next_index: 1,
            status: None,
            passphrase: Zeroizing::new("other".to_string()),
            node_url: "http://127.0.0.1:1".to_string(),
            archived: Some(dir.path().join("seed-20260912-031500.enc")),
            imported: Vec::new(),
        })));
        assert!(app.importing.is_none());
        assert!(app.wallet.is_some());
    }

    // M8: a finished file import must drop the second in-memory copy of the
    // master and the passphrase it holds in `preview` -- and the stale
    // "SAVING..." a return to this stage would otherwise still show.
    #[test]
    fn a_finished_file_import_drops_its_secrets_from_the_stage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = app_with_a_wallet(dir.path());
        import_started(&mut app);
        opening(&mut app, &dir.path().join("w.enc"));
        let _ = app.update(Message::ImportOpened(Ok(opened(7, 3, "its own"))));
        let _ = app.update(Message::ImportUse);
        assert!(matches!(
            app.setup,
            SetupStage::ImportFile {
                busy: true,
                preview: Some(_),
                ..
            }
        ));

        let _ = app.update(Message::SetupReady(Ok(Ready {
            master: MasterSeed::from_bytes([7u8; 32]),
            next_index: 3,
            status: None,
            passphrase: Zeroizing::new("its own".to_string()),
            node_url: "http://127.0.0.1:1".to_string(),
            archived: Some(dir.path().join("seed-20260912-031500.enc")),
            imported: Vec::new(),
        })));

        match &app.setup {
            SetupStage::ImportFile {
                preview,
                passphrase,
                busy,
                path,
                ..
            } => {
                assert!(
                    preview.is_none(),
                    "the second copy of the master must be gone"
                );
                assert!(passphrase.is_empty());
                assert!(!busy, "SAVING... must not linger");
                assert!(path.is_none());
            }
            _ => panic!("expected the ImportFile stage, cleared"),
        }
    }
}
