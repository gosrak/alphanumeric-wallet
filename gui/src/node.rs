//! The node the wallet brings along.
//!
//! This module deals with the process but knows nothing of `iced` -- a
//! discipline of the lib crate. Stage determination doesn't live here:
//! `startup.rs` answers that as a pure function.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Avoids the mining node's 7177. Naming the port explicitly means the node
/// fails on conflict instead of quietly leaking onto another one -- the
/// random port fallback in `src/a9/node.rs`'s `initialize_listener` only
/// applies to the branch where no address was given. Failing is the
/// behavior we want.
pub const DEFAULT_P2P_PORT: u16 = 7178;
/// Avoids the mining node's 8095.
pub const DEFAULT_EXPLORER_PORT: u16 = 8096;
/// Avoids the mining node's 8787.
pub const DEFAULT_STATS_PORT: u16 = 8097;
pub const LOG_FILE: &str = "node.log";
/// The instance lock the node creates in its own cwd (`main.rs`'s
/// `INSTANCE_LOCK_PATH`). The data directory belongs to the GUI, so this is
/// where we read the pid back when reclaiming an orphan.
pub const INSTANCE_LOCK: &str = ".alphanumeric.instance.lock";
/// The conventional name of the node binary.
/// `alphanumeric`, or `alphanumeric.exe` on Windows.
pub fn binary_file_name() -> String {
    format!("alphanumeric{}", std::env::consts::EXE_SUFFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfig {
    pub binary: PathBuf,
    pub data_dir: PathBuf,
    pub p2p_port: u16,
    pub explorer_port: u16,
    pub stats_port: u16,
}

impl NodeConfig {
    pub fn log_path(&self) -> PathBuf {
        self.data_dir.join(LOG_FILE)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_dir.join(INSTANCE_LOCK)
    }
}

/// Same convention as the keystore (`storage::default_path` is
/// `~/.alphanumeric-gui/seed.enc`).
pub fn default_data_dir() -> Option<PathBuf> {
    crate::storage::home_dir().map(|home| {
        let mut path = home;
        path.push(".alphanumeric-gui");
        path.push("node");
        path
    })
}

/// The configured path if there is one, otherwise `alphanumeric` next to the
/// GUI executable.
///
/// We do not search `PATH`. What's there is likely the mining node's
/// binary, possibly a different version with different build features --
/// the node the wallet starts must be one the wallet knows about.
///
/// `exe_dir` is a parameter for testing: `current_exe()` points at the test
/// executable. Callers pass `std::env::current_exe()?.parent()`.
pub fn locate_binary(configured: Option<&Path>, exe_dir: &Path) -> Result<PathBuf, String> {
    if let Some(path) = configured {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!(
                "The node binary configured at {} is not there.",
                path.display()
            ))
        };
    }
    let name = binary_file_name();
    let sibling = exe_dir.join(&name);
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(format!(
            "No node binary found. Looked for `{name}` next to the wallet, in {}. \
             Set the path in settings, or put the two side by side.",
            exe_dir.display()
        ))
    }
}

/// The whole environment given to the child. **It does not inherit the
/// parent's environment** -- an `ALPHANUMERIC_*` left in the user's shell
/// would drag the wallet's node into the mining node's configuration.
/// `inherited_env_keys` are passed separately by the caller (Task 2).
pub fn child_env(config: &NodeConfig) -> Vec<(String, String)> {
    vec![
        // Starts as a node with no wallet. With no `private.key` it
        // proceeds without a prompt (`main.rs`'s `async_main`, the
        // "Headless mode: no private.key found" branch).
        ("ALPHANUMERIC_HEADLESS".into(), "true".into()),
        (
            "ALPHANUMERIC_DB_PATH".into(),
            config.data_dir.join("blockchain.db").display().to_string(),
        ),
        ("ALPHANUMERIC_PORT".into(), config.p2p_port.to_string()),
        (
            "ALPHANUMERIC_EXPLORER_API".into(),
            config.explorer_port.to_string(),
        ),
        // Receives but does not advertise. The pull path is not a toggle
        // (`src/a9/node.rs`'s `block_relay_sync_enabled` is
        // unconditionally true).
        ("ALPHANUMERIC_DISABLE_PUBLIC_ANNOUNCE".into(), "true".into()),
        // The console reads peers, hashrate, and difficulty from here. It
        // binds 127.0.0.1 by default, and a bind failure doesn't kill the
        // node, just stats (`src/a9/node.rs`'s `start_stats_server` returns
        // `Ok(())` on a bind failure) -- turning it on costs little.
        ("ALPHANUMERIC_STATS_ENABLED".into(), "true".into()),
        (
            "ALPHANUMERIC_STATS_PORT".into(),
            config.stats_port.to_string(),
        ),
        ("NO_COLOR".into(), "1".into()),
    ]
}

/// The parent's variables the child gets back after `env_clear`: what the
/// platform's loader and runtime cannot do without, and nothing that could
/// carry a mining node's configuration. On Windows a process without
/// SYSTEMROOT commonly fails in DLL or Winsock initialisation, and the
/// profile directory stands in for HOME.
pub fn inherited_env_keys() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "PATH",
            "LANG",
            "SYSTEMROOT",
            "SYSTEMDRIVE",
            "USERPROFILE",
            "TEMP",
            "TMP",
        ]
    }
    #[cfg(not(windows))]
    {
        &["HOME", "PATH", "LANG"]
    }
}

pub fn explorer_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn stats_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Went down on its own when asked (SIGTERM; Ctrl-C on Windows). The
    /// node has removed the lock and released its ports.
    Graceful,
    /// Overran the grace period and was killed outright (SIGKILL;
    /// `TerminateProcess` on Windows). The node's `StartupLockGuard::drop`
    /// never ran, so the lock may still be sitting there -- the next start's
    /// `reclaim_orphan` cleans it up.
    Killed,
}

pub struct NodeProcess {
    child: Child,
}

impl NodeProcess {
    pub fn spawn(config: &NodeConfig) -> Result<Self, String> {
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            format!(
                "Could not create the node directory {}: {e}",
                config.data_dir.display()
            )
        })?;

        let log_path = config.log_path();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Could not open {}: {e}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("Could not duplicate the log handle: {e}"))?;

        let mut command = std::process::Command::new(&config.binary);
        command
            .current_dir(&config.data_dir)
            // The child starts from a blank slate. An ALPHANUMERIC_* left
            // in the user's shell must not drag the wallet's node into the
            // mining node's configuration.
            .env_clear()
            // A file, not a pipe: with a pipe, the child would stall at
            // 64 KB while nobody was reading it.
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        for key in inherited_env_keys() {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for (key, value) in child_env(config) {
            command.env(key, value);
        }

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // The node dies with the GUI. This fires on the death of the
            // *thread* that forked, so callers must call this function
            // from a dedicated thread tied to the GUI's lifetime (Task 3).
            // Calling it from a tokio worker means the child gets
            // SIGTERMed mid-sync once that thread is reclaimed as idle.
            unsafe {
                command.pre_exec(|| {
                    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)
                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                });
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // A console of its own, with no window. `ask_to_stop` attaches
            // to it to deliver a Ctrl-C; a child of a windowless GUI has no
            // console at all otherwise, and a Ctrl-C has nowhere to go.
            command.creation_flags(win::CREATE_NO_WINDOW);
        }

        let child = command.spawn().map_err(|e| {
            format!(
                "Could not start the node at {}: {e}",
                config.binary.display()
            )
        })?;

        // The node dies with the GUI, as PR_SET_PDEATHSIG does above. Best
        // effort: a GUI that is itself inside a job that forbids nesting
        // still gets its node, just not the tie.
        #[cfg(windows)]
        if let Err(e) = win::kill_with_the_gui(&child) {
            eprintln!("The node will not be stopped if the wallet crashes: {e}");
        }

        Ok(Self { child })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        self.child
            .try_wait()
            .map_err(|e| format!("Could not check on the node: {e}"))
    }

    /// Asks the node to stop and waits `grace`. Kills it outright if it's
    /// still alive after that.
    pub fn stop(&mut self, grace: Duration) -> Result<StopOutcome, String> {
        if self.exited()?.is_some() {
            return Ok(StopOutcome::Graceful);
        }
        // An ask that could not even be delivered has no reason to wait
        // for: straight to the kill.
        let deadline = if ask_to_stop(self.child.id()) {
            Instant::now() + grace
        } else {
            Instant::now()
        };
        while Instant::now() < deadline {
            if self.exited()?.is_some() {
                return Ok(StopOutcome::Graceful);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(StopOutcome::Killed)
    }
}

/// Asks `pid` to shut down cleanly: SIGTERM, or on Windows a Ctrl-C. `true`
/// if the ask was delivered -- not that the process obeyed.
fn ask_to_stop(pid: u32) -> bool {
    #[cfg(unix)]
    {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .is_ok()
    }
    #[cfg(windows)]
    {
        win::ask_to_stop(pid)
    }
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(windows)]
    {
        win::is_alive(pid)
    }
}

/// Windows has no signals. What stands in for them, and for PDEATHSIG.
#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        ATTACH_PARENT_PROCESS, CTRL_C_EVENT,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    pub use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Closed on drop.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a handle this process opened and has not closed.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// `kill(pid, 0)`'s answer. A pid that cannot be opened is alive
    /// unless the system says there is no such process: `reclaim_orphan`
    /// deletes the lock of a pid that is not alive, and a node that merely
    /// refused us a handle must not lose its lock.
    pub fn is_alive(pid: u32) -> bool {
        // SAFETY: plain system calls on a pid; the handle is closed by
        // `Owned` on every path.
        unsafe {
            let handle = Owned(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
            if handle.0.is_null() {
                let error = std::io::Error::last_os_error();
                return error.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32);
            }
            let mut code = 0u32;
            GetExitCodeProcess(handle.0, &mut code) != 0 && code == STILL_ACTIVE as u32
        }
    }

    /// The GUI's own Ctrl-C handler: swallows the event. Installed once,
    /// never removed. The event `ask_to_stop` sends reaches the GUI too,
    /// and it arrives AFTER `GenerateConsoleCtrlEvent` returns, on a thread
    /// of its own -- an "ignore" switched on for the call and off again
    /// right after it was measured (under Wine) to leave the GUI dead: the
    /// event landed once the ignore was already gone. A handler routine
    /// is not inherited by the node the GUI starts, unlike the
    /// `SetConsoleCtrlHandler(NULL, TRUE)` flag, so the node keeps its own
    /// Ctrl-C handling.
    unsafe extern "system" fn swallow_ctrl_c(ctrl_type: u32) -> windows_sys::core::BOOL {
        (ctrl_type == CTRL_C_EVENT) as windows_sys::core::BOOL
    }

    /// A Ctrl-C to `pid`, which must own a console (`CREATE_NO_WINDOW`
    /// gives it one): the GUI attaches to that console, sends the event to
    /// everything on it -- the node, and the GUI itself, which swallows it
    /// -- and detaches. `false` if the console could not be attached to,
    /// or the event not sent.
    pub fn ask_to_stop(pid: u32) -> bool {
        // SAFETY: console attachment is process-global state, undone before
        // returning; the handler is a plain function with the documented
        // signature, and stays installed for the process's lifetime.
        static SWALLOW: std::sync::Once = std::sync::Once::new();
        unsafe {
            SWALLOW.call_once(|| {
                SetConsoleCtrlHandler(Some(swallow_ctrl_c), 1);
            });
            FreeConsole();
            if AttachConsole(pid) == 0 {
                return false;
            }
            let sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0;
            FreeConsole();
            // Back to the console the wallet was started from, if there is
            // one (a terminal, or `cargo test`), so its output keeps going
            // there. Started from Explorer there is none, and this fails
            // without effect.
            AttachConsole(ATTACH_PARENT_PROCESS);
            sent
        }
    }

    /// Puts `child` in a job that kills its members when its last handle
    /// closes -- and the last handle closes when this process ends,
    /// however it ends. The job handle is leaked on purpose: closing it
    /// earlier would be the kill.
    pub fn kill_with_the_gui(child: &Child) -> Result<(), String> {
        // SAFETY: the limit struct is plain data passed by pointer with its
        // size; the child handle is `std`'s own, still open while `child`
        // lives.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(format!(
                    "could not create a job object: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!(
                    "could not set the job's kill-on-close limit: {error}"
                ));
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("could not assign the node to the job: {error}"));
            }
        }
        Ok(())
    }
}

/// Cleans up a node the GUI left behind when it died.
///
/// `PR_SET_PDEATHSIG` is Linux-only and best-effort -- it misses a parent
/// that dies between the fork and the prctl call. The data directory
/// belongs to the GUI, so the lock's pid is ours, and we stop it if it's
/// alive. The node itself only cleans up a **dead** pid.
///
/// Returns the pid that was reclaimed, or `None` if there was nothing to
/// clean up.
pub fn reclaim_orphan(lock_path: &Path, grace: Duration) -> Result<Option<u32>, String> {
    let Ok(raw) = std::fs::read_to_string(lock_path) else {
        return Ok(None);
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        // A hand-edited or half-written lock. This must not block startup.
        let _ = std::fs::remove_file(lock_path);
        return Ok(None);
    };
    if !is_alive(pid) {
        let _ = std::fs::remove_file(lock_path);
        return Ok(None);
    }
    let _ = ask_to_stop(pid);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            let _ = std::fs::remove_file(lock_path);
            return Ok(Some(pid));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "A node from an earlier run (pid {pid}) is still holding {}. \
         Stop it, then start the wallet again.",
        lock_path.display()
    ))
}

/// The last `max_lines` lines of the log. A missing file is an empty list,
/// not an error -- a moment where the node hasn't printed a single line yet
/// is normal.
///
/// Only reads a window at the end of the file. F6 is a screen left open, so
/// this gets called every 2 seconds, and the log is opened with
/// `.append(true)` so it never rotates -- reading from the start would make
/// the cost grow without bound as the log grows.
pub fn tail_log(log_path: &Path, max_lines: usize) -> Vec<String> {
    tail_log_window(log_path, max_lines, TAIL_WINDOW_BYTES)
}

/// Plenty for 20 lines. If it's not enough, `tail_log_window` doubles it.
const TAIL_WINDOW_BYTES: u64 = 64 * 1024;

fn tail_log_window(log_path: &Path, max_lines: usize, first_window: u64) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};

    if max_lines == 0 {
        return Vec::new();
    }
    let Ok(mut file) = std::fs::File::open(log_path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut window = first_window.max(1);
    loop {
        let start = len.saturating_sub(window);
        let mut buf = Vec::new();
        // Only up to `len` -- anything the node appends while we're
        // reading is the next tick's business.
        if file.seek(SeekFrom::Start(start)).is_err()
            || (&mut file).take(len - start).read_to_end(&mut buf).is_err()
        {
            return Vec::new();
        }
        // A single non-UTF-8 byte must not hide every line after it.
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        // A window starting mid-file almost always starts mid-line. That
        // first fragment is not a line. (If the window happened to start
        // right at a line boundary, this throws away one whole line, and
        // the shortfall below widens the window to make up for it.)
        if start > 0 && !lines.is_empty() {
            lines.remove(0);
        }
        if lines.len() >= max_lines || start == 0 {
            let skip = lines.len().saturating_sub(max_lines);
            return lines[skip..].iter().map(|l| (*l).to_string()).collect();
        }
        window = window.saturating_mul(2);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeState {
    Starting,
    Running {
        pid: u32,
    },
    /// The child ended on its own. A port conflict arrives in this shape --
    /// the node fails outright if it can't claim the port it was given.
    Exited {
        code: Option<i32>,
    },
    Failed {
        message: String,
    },
    /// We stopped it. This is not recycled into `Exited` -- that means "the
    /// child ended on its own", a different event from being asked to go
    /// down.
    Stopped,
}

enum Command {
    Stop,
}

/// The dedicated thread that owns the node. `PR_SET_PDEATHSIG` fires on the
/// death of the thread that forked, so this thread never ends while the
/// `Supervisor` is alive.
pub struct Supervisor {
    tx: mpsc::Sender<Command>,
    state: Arc<Mutex<NodeState>>,
    log_path: PathBuf,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Time for the node to respond to SIGTERM. Measured at 0.5 seconds.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Time to wait when stopping someone else's orphan.
const RECLAIM_GRACE: Duration = Duration::from_secs(10);

impl Supervisor {
    pub fn start(config: NodeConfig) -> Self {
        let state = Arc::new(Mutex::new(NodeState::Starting));
        let log_path = config.log_path();
        let (tx, rx) = mpsc::channel();
        let thread_state = Arc::clone(&state);
        // `.ok()` used to swallow a spawn failure here, which left `handle`
        // `None`, `state` stuck at `Starting`, and nothing that would ever
        // move it -- a screen with no exit (this composes with the startup
        // screen's own one-way-door defect). Record the failure as
        // `Failed` instead, the same state a child that could not start at
        // all reports.
        let handle = match std::thread::Builder::new()
            .name("node-supervisor".into())
            .spawn(move || supervise(config, rx, thread_state))
        {
            Ok(handle) => Some(handle),
            Err(error) => {
                set_state(
                    &state,
                    NodeState::Failed {
                        message: format!("Could not start the node supervisor thread: {error}"),
                    },
                );
                None
            }
        };
        Self {
            tx,
            state,
            log_path,
            handle,
        }
    }

    /// A `Supervisor` that never spawns anything, fixed at whatever
    /// `NodeState` is given.
    ///
    /// For tests only -- this crate's suite deliberately never spawns a real
    /// process (see `app.rs`'s `unspawnable_binary_path`), and `ConsoleTick`
    /// gating a `/stats` fetch on `NodeState::Running` cannot otherwise be
    /// exercised without one. `pub` and `#[doc(hidden)]` rather than
    /// `#[cfg(test)]`: `alphanumeric_gui` is a library, and the binary's own
    /// `#[cfg(test)]` build links it WITHOUT `cfg(test)` set, so a
    /// `cfg(test)`-gated item here would be invisible from `app::tests`.
    /// `Drop` is safe on the result: `stop()` sends into a channel whose
    /// receiver is already gone, which `mpsc::Sender::send` simply reports as
    /// an `Err` and this type already discards; `handle` is `None`, so there
    /// is nothing to join.
    #[doc(hidden)]
    pub fn for_tests_with_state(state: NodeState) -> Self {
        let (tx, _rx) = mpsc::channel();
        Self {
            tx,
            state: Arc::new(Mutex::new(state)),
            log_path: PathBuf::new(),
            handle: None,
        }
    }

    pub fn state(&self) -> NodeState {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn log_tail(&self, max_lines: usize) -> Vec<String> {
        tail_log(&self.log_path, max_lines)
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn set_state(slot: &Arc<Mutex<NodeState>>, next: NodeState) {
    match slot.lock() {
        Ok(mut guard) => *guard = next,
        Err(poisoned) => *poisoned.into_inner() = next,
    }
}

fn supervise(config: NodeConfig, rx: mpsc::Receiver<Command>, state: Arc<Mutex<NodeState>>) {
    // A previous run may have left a node behind. Reclaim it first --
    // otherwise the new node collides with the lock and the ports, and
    // fails.
    if let Err(message) = reclaim_orphan(&config.lock_path(), RECLAIM_GRACE) {
        set_state(&state, NodeState::Failed { message });
        return;
    }

    let mut node = match NodeProcess::spawn(&config) {
        Ok(node) => node,
        Err(message) => {
            set_state(&state, NodeState::Failed { message });
            return;
        }
    };
    set_state(&state, NodeState::Running { pid: node.pid() });

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                let _ = node.stop(STOP_GRACE);
                set_state(&state, NodeState::Stopped);
                return;
            }
            Err(RecvTimeoutError::Timeout) => match node.exited() {
                Ok(Some(status)) => {
                    set_state(
                        &state,
                        NodeState::Exited {
                            code: status.code(),
                        },
                    );
                    return;
                }
                Ok(None) => {}
                Err(message) => {
                    set_state(&state, NodeState::Failed { message });
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // On Windows the node next to the wallet is `alphanumeric.exe`; a
    // lookup for the bare name never finds it.
    #[test]
    fn the_node_binary_carries_the_platform_exe_suffix() {
        assert_eq!(
            binary_file_name(),
            format!("alphanumeric{}", std::env::consts::EXE_SUFFIX)
        );
    }

    // Same convention as the keystore, and the same home lookup: a
    // Windows profile directory counts.
    #[test]
    fn the_default_data_dir_hangs_off_the_home_directory() {
        assert_eq!(
            default_data_dir(),
            crate::storage::home_dir().map(|home| home.join(".alphanumeric-gui").join("node"))
        );
    }

    // The child starts from a cleared environment. What it gets back is
    // what the platform's loader and runtime cannot do without: on Windows
    // a process without SYSTEMROOT commonly fails in DLL or Winsock
    // initialisation, and nothing else there stands in for HOME.
    #[test]
    fn the_inherited_variables_are_what_the_platform_needs() {
        let keys = inherited_env_keys();
        assert!(keys.contains(&"PATH"));
        if cfg!(windows) {
            for key in ["SYSTEMROOT", "SYSTEMDRIVE", "USERPROFILE", "TEMP", "TMP"] {
                assert!(keys.contains(&key), "{key} missing from {keys:?}");
            }
        } else {
            assert!(keys.contains(&"HOME"));
            assert!(!keys.contains(&"SYSTEMROOT"));
        }
    }

    #[test]
    fn a_configured_binary_that_exists_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let configured = dir.path().join("my-node");
        std::fs::write(&configured, b"").expect("write");
        let exe_dir = dir.path().join("elsewhere");
        assert_eq!(
            locate_binary(Some(&configured), &exe_dir).expect("found"),
            configured
        );
    }

    #[test]
    fn a_configured_binary_that_is_missing_is_an_error_naming_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let configured = dir.path().join("absent");
        let message = locate_binary(Some(&configured), dir.path()).expect_err("absent");
        assert!(
            message.contains(&configured.display().to_string()),
            "the error must name the path that was configured: {message}"
        );
    }

    #[test]
    fn without_a_configured_path_the_sibling_of_the_gui_is_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sibling = dir.path().join(binary_file_name());
        std::fs::write(&sibling, b"").expect("write");
        assert_eq!(locate_binary(None, dir.path()).expect("found"), sibling);
    }

    // The most common shape of deployment mistake. If the error doesn't say
    // where it looked, the user can't fix it.
    #[test]
    fn when_nothing_is_found_the_error_names_where_it_looked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let message = locate_binary(None, dir.path()).expect_err("nothing there");
        assert!(
            message.contains(&dir.path().display().to_string()),
            "the error must name the directory searched: {message}"
        );
    }

    fn sample_config() -> NodeConfig {
        NodeConfig {
            binary: PathBuf::from("/opt/alphanumeric"),
            data_dir: PathBuf::from("/home/someone/.alphanumeric-gui/node"),
            p2p_port: 7178,
            explorer_port: 8096,
            stats_port: 8097,
        }
    }

    #[test]
    fn the_child_environment_is_exactly_the_spec_list() {
        let env = child_env(&sample_config());
        let mut keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "ALPHANUMERIC_DB_PATH",
                "ALPHANUMERIC_DISABLE_PUBLIC_ANNOUNCE",
                "ALPHANUMERIC_EXPLORER_API",
                "ALPHANUMERIC_HEADLESS",
                "ALPHANUMERIC_PORT",
                "ALPHANUMERIC_STATS_ENABLED",
                "ALPHANUMERIC_STATS_PORT",
                "NO_COLOR",
            ]
        );
    }

    // The mining wallet key lives in the mining node's cwd, not this one.
    // Turning on mining would break the "the wallet holds the key, the node
    // doesn't" premise (spec §12).
    #[test]
    fn the_child_is_never_told_to_mine() {
        let env = child_env(&sample_config());
        assert!(
            !env.iter().any(|(k, _)| k.starts_with("ALPHANUMERIC_MINE")),
            "no mining variable may be handed to the wallet's node"
        );
    }

    #[test]
    fn the_database_lives_under_the_data_directory() {
        let config = sample_config();
        let env = child_env(&config);
        let db = env
            .iter()
            .find(|(k, _)| k == "ALPHANUMERIC_DB_PATH")
            .map(|(_, v)| v.clone())
            .expect("DB_PATH present");
        assert_eq!(
            db,
            config.data_dir.join("blockchain.db").display().to_string()
        );
    }

    // Given a bare number, the node binds 127.0.0.1 (`src/a9/node.rs`'s
    // `start_explorer_server`). The wallet node's explorer never faces
    // outward.
    #[test]
    fn the_explorer_port_is_sent_as_a_bare_number() {
        let env = child_env(&sample_config());
        assert!(env
            .iter()
            .any(|(k, v)| k == "ALPHANUMERIC_EXPLORER_API" && v == "8096"));
    }

    #[test]
    fn the_explorer_url_is_loopback() {
        assert_eq!(explorer_url(8096), "http://127.0.0.1:8096");
    }

    /// D turned stats off to avoid colliding with the mining node's 8787.
    /// The console reads peers and hashrate from here, so now it's turned
    /// on with its own port.
    #[test]
    fn the_child_runs_its_own_stats_server() {
        let env = child_env(&sample_config());
        assert!(env
            .iter()
            .any(|(k, v)| k == "ALPHANUMERIC_STATS_ENABLED" && v == "true"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "ALPHANUMERIC_STATS_PORT" && v == "8097"));
    }

    /// The ports the mining node holds. Targets the constants directly --
    /// just scanning sample_config's literals would still pass even if a
    /// constant regressed to 8787.
    #[test]
    fn no_default_port_collides_with_the_running_miner() {
        for (name, port) in [
            ("p2p", DEFAULT_P2P_PORT),
            ("explorer", DEFAULT_EXPLORER_PORT),
            ("stats", DEFAULT_STATS_PORT),
        ] {
            assert!(
                !matches!(port, 7177 | 8095 | 8787),
                "the default {name} port {port} is one the running miner holds"
            );
        }
    }

    /// Never uses the mining node's ports. Scans the environment, not the
    /// constants.
    #[test]
    fn the_child_environment_never_holds_miner_ports() {
        let env = child_env(&sample_config());
        for (_, v) in &env {
            assert_ne!(v, "7177", "P2P port of the running miner");
            assert_ne!(v, "8095", "explorer port of the running miner");
            assert_ne!(v, "8787", "stats port of the running miner");
        }
    }

    #[test]
    fn the_stats_url_is_loopback() {
        assert_eq!(stats_url(8097), "http://127.0.0.1:8097");
    }

    #[cfg(unix)]
    #[test]
    fn the_log_and_lock_sit_in_the_data_directory() {
        let config = sample_config();
        assert_eq!(config.log_path(), config.data_dir.join("node.log"));
        assert_eq!(
            config.lock_path(),
            config.data_dir.join(".alphanumeric.instance.lock")
        );
    }

    use std::time::Duration;

    #[cfg(unix)]
    /// A fake to stand in for the node. The `trap` flag makes one that
    /// honors SIGTERM and one that ignores it -- the latter actually
    /// exercises the SIGKILL fallback path.
    fn fake_node(dir: &Path, honors_sigterm: bool) -> PathBuf {
        let path = dir.join("fake-node");
        let body = if honors_sigterm {
            "#!/bin/sh\ntrap 'echo bye; exit 0' TERM\necho hello\nwhile true; do sleep 0.05; done\n"
        } else {
            "#!/bin/sh\ntrap '' TERM\necho hello\nwhile true; do sleep 0.05; done\n"
        };
        std::fs::write(&path, body).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    #[cfg(unix)]
    fn fake_config(dir: &Path, honors_sigterm: bool) -> NodeConfig {
        NodeConfig {
            binary: fake_node(dir, honors_sigterm),
            data_dir: dir.to_path_buf(),
            p2p_port: 7178,
            explorer_port: 8096,
            stats_port: 8097,
        }
    }

    /// Serializes every test that writes a script and then forks to exec it.
    /// `NodeProcess::spawn` uses `pre_exec` (for the PDEATHSIG hook), so it
    /// always really forks rather than using `posix_spawn`. If a fork from
    /// one test lands in the microsecond window while another test's
    /// `fs::write` still has its script open for writing (between that
    /// write's `open` and its `close`), the forked child inherits a copy of
    /// that write fd -- it is `O_CLOEXEC`-flagged, but the flag only closes
    /// it at *that child's own* exec, not before. If the writer's own exec
    /// of the same file lands inside that sub-window, the kernel still sees
    /// an open writer on the inode and returns `ETXTBSY`. Production never
    /// has this contention (one supervisor thread per node), so the gate
    /// lives here in tests, not in `NodeProcess::spawn`.
    static SPAWN_GATE: Mutex<()> = Mutex::new(());

    fn spawn_gate() -> std::sync::MutexGuard<'static, ()> {
        SPAWN_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Builds a config (which may write a script) and starts a supervisor
    /// for it, both under `SPAWN_GATE`, releasing the gate only once the
    /// background thread has actually finished its `NodeProcess::spawn`
    /// call -- i.e. once state has left `Starting`, successfully or not.
    /// That is the whole window in which this test's own exec could collide
    /// with another test's fork.
    fn start_supervised(build_config: impl FnOnce() -> NodeConfig) -> Supervisor {
        let _gate = spawn_gate();
        let supervisor = Supervisor::start(build_config());
        wait_for(
            || !matches!(supervisor.state(), NodeState::Starting),
            "the supervisor to finish spawning",
        );
        supervisor
    }

    #[cfg(unix)]
    /// `trap ... TERM` runs before the fake prints `hello` (see `fake_node`
    /// above), so a line in the log is a deterministic proof the trap is
    /// installed. Measured: sending SIGTERM right after `spawn()` with no
    /// wait loses the race 20/20 -- the freshly exec'd shell hasn't read its
    /// first line yet, so the *default* disposition kills it, and a test
    /// meant to exercise the SIGKILL fallback (or the honoring-SIGTERM path)
    /// passes through the wrong arm instead. This is the same class of bug
    /// as F-D: the test assumed a state the fake hadn't reached yet.
    fn wait_for_the_trap_to_be_installed(config: &NodeConfig) {
        for _ in 0..100 {
            if !tail_log(&config.log_path(), 1).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    #[test]
    fn spawning_creates_the_data_directory_and_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("does").join("not").join("exist");
        std::fs::create_dir_all(&nested).expect("mkdir for the fake binary");
        let (mut node, config) = {
            let _gate = spawn_gate();
            let mut config = fake_config(&nested, true);
            config.data_dir = nested.join("node");
            let node = NodeProcess::spawn(&config).expect("spawn");
            (node, config)
        };
        // Gives the child time to print a line.
        for _ in 0..100 {
            if !tail_log(&config.log_path(), 10).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(config.data_dir.is_dir(), "the data directory is created");
        assert_eq!(tail_log(&config.log_path(), 10), vec!["hello".to_string()]);
        node.stop(Duration::from_secs(2)).expect("stop");
    }

    #[test]
    fn a_missing_binary_fails_at_spawn_rather_than_later() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = NodeConfig {
            binary: dir.path().join("not-here"),
            data_dir: dir.path().join("node"),
            p2p_port: 7178,
            explorer_port: 8096,
            stats_port: 8097,
        };
        let result = {
            let _gate = spawn_gate();
            NodeProcess::spawn(&config)
        };
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_stops_it_without_a_kill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut node, config) = {
            let _gate = spawn_gate();
            let config = fake_config(dir.path(), true);
            let node = NodeProcess::spawn(&config).expect("spawn");
            (node, config)
        };
        wait_for_the_trap_to_be_installed(&config);
        assert_eq!(
            node.stop(Duration::from_secs(3)).expect("stop"),
            StopOutcome::Graceful
        );
    }

    /// SIGKILL skips the node's `StartupLockGuard::drop`, leaving the lock
    /// and ports behind. So it's used only as a last resort, and this
    /// checks that last resort actually exists.
    #[cfg(unix)]
    #[test]
    fn a_child_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut node, config) = {
            let _gate = spawn_gate();
            let config = fake_config(dir.path(), false);
            let node = NodeProcess::spawn(&config).expect("spawn");
            (node, config)
        };
        wait_for_the_trap_to_be_installed(&config);
        assert_eq!(
            node.stop(Duration::from_millis(300)).expect("stop"),
            StopOutcome::Killed
        );
    }

    #[cfg(unix)]
    #[test]
    fn exited_is_none_while_it_runs_and_some_after_it_stops() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut node = {
            let _gate = spawn_gate();
            let config = fake_config(dir.path(), true);
            NodeProcess::spawn(&config).expect("spawn")
        };
        assert!(node.exited().expect("poll").is_none());
        node.stop(Duration::from_secs(3)).expect("stop");
        assert!(node.exited().expect("poll").is_some());
    }

    #[test]
    fn there_is_nothing_to_reclaim_without_a_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join(INSTANCE_LOCK);
        assert_eq!(reclaim_orphan(&lock, Duration::from_secs(1)), Ok(None));
    }

    /// The node's own `is_process_alive` only cleans up a dead pid
    /// (`main.rs`). A live orphan is ours to reclaim.
    ///
    /// **Must not use our own child:** a child that got SIGTERM stays a
    /// zombie until someone calls `wait()`, and a zombie keeps getting
    /// caught by `kill(pid, 0)` -- `is_alive` would stay true forever, and
    /// this test would burn through the grace period and fail. Ending `sh`
    /// immediately orphans its grandchild, which a subreaper picks up, so
    /// it actually goes away. That's also the real situation
    /// `reclaim_orphan` deals with.
    #[cfg(unix)]
    #[test]
    fn a_live_orphan_named_by_the_lock_is_stopped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join(INSTANCE_LOCK);

        let out = {
            let _gate = spawn_gate();
            std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 300 >/dev/null 2>&1 & echo $!")
                .output()
                .expect("spawn an orphan")
        };
        let pid: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("the shell prints the orphan's pid");
        assert!(is_alive(pid), "the orphan is running before we reclaim it");

        // Writes the pid into the lock, the way the node does.
        std::fs::write(&lock, pid.to_string()).expect("write lock");

        assert_eq!(reclaim_orphan(&lock, Duration::from_secs(5)), Ok(Some(pid)));
        assert!(
            !lock.exists(),
            "the reclaimed lock is removed so the next start is clean"
        );
        assert!(!is_alive(pid), "the orphan is gone");
    }

    #[test]
    fn a_stale_lock_naming_a_dead_pid_is_just_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join(INSTANCE_LOCK);
        // pid 1 is init, not ours. Uses a very large pid that will never be alive.
        std::fs::write(&lock, "4194304").expect("write lock");
        assert_eq!(reclaim_orphan(&lock, Duration::from_secs(1)), Ok(None));
        assert!(!lock.exists());
    }

    #[test]
    fn a_junk_lock_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join(INSTANCE_LOCK);
        std::fs::write(&lock, "not a pid").expect("write lock");
        assert_eq!(reclaim_orphan(&lock, Duration::from_secs(1)), Ok(None));
    }

    #[test]
    fn the_log_tail_returns_the_last_lines_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        std::fs::write(&log, "one\ntwo\nthree\nfour\n").expect("write");
        assert_eq!(
            tail_log(&log, 2),
            vec!["three".to_string(), "four".to_string()]
        );
    }

    /// F6 is a screen left open, and the log is opened with `.append(true)`
    /// so it never rotates. Reading the whole file every 2 seconds just to
    /// get 20 lines would make the cost grow without bound as the log
    /// grows. Even in a file much larger than the window, the last lines
    /// must still come out in order.
    #[test]
    fn the_log_tail_of_a_file_far_larger_than_the_window_is_still_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        let body: String = (0..5_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&log, body).expect("write");
        assert_eq!(
            tail_log_window(&log, 3, 16),
            vec![
                "line 4997".to_string(),
                "line 4998".to_string(),
                "line 4999".to_string()
            ]
        );
    }

    /// A window that starts mid-file almost always starts mid-line. That
    /// fragment is not a line -- something like `ine 4997` must never show
    /// up in the log. If a line is longer than the window, the window
    /// widens until it yields a whole line.
    #[test]
    fn the_log_tail_never_returns_the_fragment_a_window_starts_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        let long = "x".repeat(100);
        std::fs::write(&log, format!("{long}a\n{long}b\n{long}c\n")).expect("write");
        assert_eq!(
            tail_log_window(&log, 2, 8),
            vec![format!("{long}b"), format!("{long}c")]
        );
    }

    /// It used to be `lines().map_while(Result::ok)`, so reading stopped at
    /// a single non-UTF-8 byte -- every line after it was gone for good.
    #[test]
    fn the_log_tail_survives_a_line_that_is_not_utf8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        std::fs::write(&log, b"before\n\xff\xfe\nafter\n").expect("write");
        assert_eq!(tail_log(&log, 1), vec!["after".to_string()]);
    }

    /// A final line with no trailing newline is still a line -- it's the
    /// one the node is currently writing.
    #[test]
    fn the_log_tail_includes_an_unterminated_last_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        std::fs::write(&log, "one\ntwo").expect("write");
        assert_eq!(
            tail_log(&log, 5),
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn the_log_tail_of_a_missing_file_is_empty_rather_than_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(tail_log(&dir.path().join("absent.log"), 5).is_empty());
    }

    // The Windows side of `is_alive`, `ask_to_stop` and `kill_with_the_gui`.
    // These run only on a Windows `cargo test`; on Linux they are not even
    // compiled.
    #[cfg(windows)]
    mod windows {
        use super::super::*;
        use std::os::windows::process::CommandExt;

        #[test]
        fn the_gui_itself_is_alive() {
            assert!(is_alive(std::process::id()));
        }

        #[test]
        fn an_exited_child_is_not_alive() {
            let mut child = std::process::Command::new("cmd")
                .args(["/c", "exit 0"])
                .creation_flags(win::CREATE_NO_WINDOW)
                .spawn()
                .expect("spawn cmd");
            let pid = child.id();
            child.wait().expect("wait");
            assert!(!is_alive(pid));
        }

        // What `NodeProcess::stop` and `reclaim_orphan` rely on: a child
        // started the way `spawn` starts the node (a hidden console of its
        // own) receives a Ctrl-C from the GUI, which has no console. ping
        // has no handler, so the default one ends it.
        #[test]
        fn a_ctrl_c_reaches_a_child_with_a_hidden_console() {
            let mut child = std::process::Command::new("ping")
                .args(["-n", "300", "127.0.0.1"])
                .creation_flags(win::CREATE_NO_WINDOW)
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn ping");
            std::thread::sleep(Duration::from_millis(500));
            assert!(ask_to_stop(child.id()), "the Ctrl-C was delivered");
            super::wait_for(
                || child.try_wait().expect("try_wait").is_some(),
                "ping to end on Ctrl-C",
            );
        }
    }

    /// The real node, started the way the wallet starts it and asked to
    /// stop the way the wallet asks. Only with `ALPHANUMERIC_NODE_BINARY`
    /// naming a node binary for THIS target (a Windows `cargo test` wants
    /// `alphanumeric.exe`; under Wine, the same): the other tests stand in
    /// scripts for the node, and this is the one that checks the stand-in
    /// against the real thing -- that the platform's stop signal reaches
    /// it and that it takes its lock away on the way out. Skipped, with a
    /// note, when the variable is not set.
    ///
    /// Heavy: a fresh data directory means the node fetches and unpacks
    /// the chain snapshot first (about 200 MB down, 1.5 GB on disk, a few
    /// minutes), and only a node past that point has its signal handling
    /// in place -- asked earlier it dies like any process would, lock and
    /// all. Readiness is the stats server answering on its port.
    #[test]
    fn the_real_node_stops_gracefully_and_removes_its_lock() {
        let Some(binary) = std::env::var_os("ALPHANUMERIC_NODE_BINARY") else {
            eprintln!("ALPHANUMERIC_NODE_BINARY not set; the real-node stop test is skipped");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let config = NodeConfig {
            binary: PathBuf::from(binary),
            data_dir: dir.path().join("node"),
            p2p_port: 17378,
            explorer_port: 18296,
            stats_port: 18297,
        };
        let mut node = NodeProcess::spawn(&config).expect("the node starts");
        let lock = config.lock_path();
        let started = Instant::now();
        let stats = std::net::SocketAddr::from(([127, 0, 0, 1], config.stats_port));
        let up =
            |addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok();
        while !up(stats) {
            assert!(
                node.exited().expect("try_wait").is_none(),
                "the node exited before its stats server came up:\n{}",
                std::fs::read_to_string(config.log_path()).unwrap_or_default()
            );
            assert!(
                started.elapsed() < Duration::from_secs(15 * 60),
                "no stats server after 15 min:\n{}",
                std::fs::read_to_string(config.log_path()).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        assert!(lock.exists(), "a running node holds its lock");
        let outcome = node.stop(Duration::from_secs(40)).expect("stop");
        assert_eq!(
            outcome,
            StopOutcome::Graceful,
            "log:\n{}",
            std::fs::read_to_string(config.log_path()).unwrap_or_default()
        );
        assert!(!lock.exists(), "a graceful exit removes the lock");
    }

    fn wait_for(mut done: impl FnMut() -> bool, what: &str) {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}");
    }

    #[cfg(unix)]
    #[test]
    fn the_supervisor_reaches_running_and_reports_the_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| fake_config(dir.path(), true));
        wait_for(
            || matches!(supervisor.state(), NodeState::Running { .. }),
            "the node to be running",
        );
        match supervisor.state() {
            NodeState::Running { pid } => assert!(is_alive(pid)),
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn a_binary_that_is_not_there_lands_in_failed_with_a_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| NodeConfig {
            binary: dir.path().join("absent"),
            data_dir: dir.path().join("node"),
            p2p_port: 7178,
            explorer_port: 8096,
            stats_port: 8097,
        });
        wait_for(
            || matches!(supervisor.state(), NodeState::Failed { .. }),
            "the failure to surface",
        );
    }

    /// A port conflict arrives through this path: the node fails outright
    /// if it can't claim the port it was given. If the supervisor fails to
    /// surface that as `Exited`, the GUI waits forever.
    #[test]
    fn a_child_that_dies_on_its_own_is_noticed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| {
            // A script the platform runs directly: a shell script, or on
            // Windows a batch file, which `Command` hands to cmd.exe.
            #[cfg(unix)]
            let binary = {
                use std::os::unix::fs::PermissionsExt;
                let binary = dir.path().join("quitter");
                std::fs::write(&binary, "#!/bin/sh\nexit 3\n").expect("write");
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
                binary
            };
            #[cfg(windows)]
            let binary = {
                let binary = dir.path().join("quitter.bat");
                std::fs::write(&binary, "@exit 3\r\n").expect("write");
                binary
            };
            NodeConfig {
                binary,
                data_dir: dir.path().join("node"),
                p2p_port: 7178,
                explorer_port: 8096,
                stats_port: 8097,
            }
        });
        wait_for(
            || matches!(supervisor.state(), NodeState::Exited { .. }),
            "the exit to be noticed",
        );
        assert_eq!(supervisor.state(), NodeState::Exited { code: Some(3) });
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_supervisor_stops_the_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| fake_config(dir.path(), true));
        wait_for(
            || matches!(supervisor.state(), NodeState::Running { .. }),
            "the node to be running",
        );
        let pid = match supervisor.state() {
            NodeState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };
        drop(supervisor);
        wait_for(|| !is_alive(pid), "the node to be gone after drop");
    }

    /// If `state()` still reads `Running` after `stop()`, a polling loop
    /// built on top of it spins forever waiting for a dead node. Checked
    /// without dropping the `Supervisor` -- the Drop path is already
    /// covered by `dropping_the_supervisor_stops_the_node`, and this defect
    /// only shows up when `stop()` is called without a Drop.
    #[cfg(unix)]
    #[test]
    fn an_explicitly_stopped_supervisor_stops_reporting_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| fake_config(dir.path(), true));
        wait_for(
            || matches!(supervisor.state(), NodeState::Running { .. }),
            "the node to be running",
        );
        supervisor.stop();
        wait_for(
            || !matches!(supervisor.state(), NodeState::Running { .. }),
            "state() to leave Running after an explicit stop",
        );
        assert_eq!(supervisor.state(), NodeState::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn the_supervisor_surfaces_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| fake_config(dir.path(), true));
        wait_for(
            || supervisor.log_tail(5).contains(&"hello".to_string()),
            "the node's first line",
        );
    }
}
