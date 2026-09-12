//! 지갑이 데리고 다니는 노드.
//!
//! 이 모듈은 프로세스를 다루지만 `iced` 는 모른다 -- lib 크레이트의 규율이다.
//! 단계 판정은 여기 있지 않다: `startup.rs` 가 순수 함수로 답한다.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 채굴 노드의 7177 을 피한다. 포트를 명시하면 노드는 충돌 시 조용히 다른
/// 포트로 새지 않고 실패한다 -- `src/a9/node.rs` `initialize_listener` 의 랜덤 포트 폴백은 주소를
/// 명시하지 않은 분기에만 있다. 실패하는 쪽이 우리가 원하는 거동이다.
pub const DEFAULT_P2P_PORT: u16 = 7178;
/// 채굴 노드의 8095 를 피한다.
pub const DEFAULT_EXPLORER_PORT: u16 = 8096;
/// 채굴 노드의 8787 을 피한다.
pub const DEFAULT_STATS_PORT: u16 = 8097;
pub const LOG_FILE: &str = "node.log";
/// 노드가 자기 cwd 에 만드는 인스턴스 락 (`main.rs` 의 `INSTANCE_LOCK_PATH`). 데이터 디렉터리가
/// GUI 소유이므로 고아를 걷어낼 때 여기서 pid 를 읽는다.
pub const INSTANCE_LOCK: &str = ".alphanumeric.instance.lock";
/// 노드 바이너리의 관례적 이름.
const BINARY_NAME: &str = "alphanumeric";

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

/// keystore 와 같은 규칙 (`storage::default_path` 가 `~/.alphanumeric-gui/seed.enc`).
pub fn default_data_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".alphanumeric-gui");
        path.push("node");
        path
    })
}

/// 설정된 경로가 있으면 그것, 없으면 GUI 실행 파일 옆의 `alphanumeric`.
///
/// `PATH` 는 뒤지지 않는다. 거기 있는 것은 채굴 노드의 바이너리일 가능성이
/// 높고 버전도 빌드 피처도 다를 수 있다 -- 지갑이 띄우는 노드는 지갑이 아는
/// 것이어야 한다.
///
/// `exe_dir` 를 인자로 받는 이유는 테스트다: `current_exe()` 는 테스트
/// 실행 파일을 가리킨다. 호출자가 `std::env::current_exe()?.parent()` 를 준다.
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
    let sibling = exe_dir.join(BINARY_NAME);
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(format!(
            "No node binary found. Looked for `{}` next to the wallet, in {}. \
             Set the path in settings, or put the two side by side.",
            BINARY_NAME,
            exe_dir.display()
        ))
    }
}

/// 자식에게 줄 환경 전부. **부모 환경은 상속하지 않는다** -- 사용자의 셸에
/// 있던 `ALPHANUMERIC_*` 가 지갑 노드를 채굴 노드 설정으로 끌고 간다.
/// `HOME`/`PATH`/`LANG` 은 호출자가 따로 넘긴다 (Task 2).
pub fn child_env(config: &NodeConfig) -> Vec<(String, String)> {
    vec![
        // 지갑 없는 노드로 뜬다. `private.key` 가 없으면 프롬프트 없이
        // 계속한다 (`main.rs` `async_main` 의 "Headless mode: no private.key
        // found" 분기).
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
        // 받아오되 광고하지 않는다. 당겨오는 경로는 토글이 아니다
        // (`src/a9/node.rs` 의 `block_relay_sync_enabled` 가 무조건 true).
        ("ALPHANUMERIC_DISABLE_PUBLIC_ANNOUNCE".into(), "true".into()),
        // 콘솔이 피어·해시레이트·난이도를 여기서 읽는다. 바인드는 기본이
        // 127.0.0.1 이고, 바인드에 실패해도 노드는 죽지 않고 stats 만 꺼진다
        // (`src/a9/node.rs` 의 `start_stats_server` 가 bind 실패에 `Ok(())`) -- 켜는 비용이 낮다.
        ("ALPHANUMERIC_STATS_ENABLED".into(), "true".into()),
        (
            "ALPHANUMERIC_STATS_PORT".into(),
            config.stats_port.to_string(),
        ),
        ("NO_COLOR".into(), "1".into()),
    ]
}

pub fn explorer_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn stats_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// SIGTERM 으로 스스로 내려갔다. 노드가 락을 지우고 포트를 반납한 상태다.
    Graceful,
    /// 유예를 넘겨 SIGKILL 했다. 노드의 `StartupLockGuard::drop` 이 돌지
    /// 않았으므로 락이 남아 있을 수 있다 -- 다음 기동의 `reclaim_orphan` 이
    /// 치운다.
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
            // 자식은 백지에서 시작한다. 사용자의 셸에 있던 ALPHANUMERIC_* 가
            // 지갑 노드를 채굴 노드 설정으로 끌고 가면 안 된다.
            .env_clear()
            // 파이프가 아니라 파일이다: 파이프였다면 아무도 읽지 않는 사이
            // 64 KB 에서 자식이 멎는다.
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        for key in ["HOME", "PATH", "LANG"] {
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
            // GUI 가 죽으면 노드도 죽는다. 이것이 발동하는 기준은 fork 한
            // *스레드*의 죽음이므로, 호출자는 GUI 수명에 묶인 전용 스레드에서
            // 이 함수를 불러야 한다 (Task 3). tokio 워커에서 부르면 그
            // 스레드가 유휴 회수될 때 자식이 동기화 중에 SIGTERM 을 맞는다.
            unsafe {
                command.pre_exec(|| {
                    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM)
                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                });
            }
        }

        let child = command.spawn().map_err(|e| {
            format!(
                "Could not start the node at {}: {e}",
                config.binary.display()
            )
        })?;
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

    /// SIGTERM 을 보내고 `grace` 만큼 기다린다. 그래도 살아 있으면 SIGKILL.
    pub fn stop(&mut self, grace: Duration) -> Result<StopOutcome, String> {
        if self.exited()?.is_some() {
            return Ok(StopOutcome::Graceful);
        }
        send_sigterm(self.child.id());
        let deadline = Instant::now() + grace;
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

fn send_sigterm(pid: u32) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// GUI 가 죽으면서 노드를 남겼을 때 그것을 걷어낸다.
///
/// `PR_SET_PDEATHSIG` 는 리눅스 한정이고 최선노력이다 -- fork 와 prctl 사이에
/// 부모가 죽으면 놓친다. 데이터 디렉터리가 GUI 소유이므로 락의 pid 는 우리
/// 것이고, 살아 있으면 세운다. 노드 자신은 **죽은** pid 만 치워준다.
///
/// 걷어낸 pid 를 돌려준다. 치울 것이 없었으면 `None`.
pub fn reclaim_orphan(lock_path: &Path, grace: Duration) -> Result<Option<u32>, String> {
    let Ok(raw) = std::fs::read_to_string(lock_path) else {
        return Ok(None);
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        // 사람이 손댔거나 반쯤 쓰인 락. 이것 때문에 기동을 막지 않는다.
        let _ = std::fs::remove_file(lock_path);
        return Ok(None);
    };
    if !is_alive(pid) {
        let _ = std::fs::remove_file(lock_path);
        return Ok(None);
    }
    send_sigterm(pid);
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

/// 로그의 마지막 `max_lines` 줄. 없는 파일은 빈 목록이지 오류가 아니다 --
/// 노드가 아직 한 줄도 찍지 않은 순간이 정상이다.
///
/// 파일 끝의 창만 읽는다. F6 는 열어 둔 채로 두는 화면이라 이게 2초마다
/// 불리고, 로그는 `.append(true)` 로 열려 회전하지 않는다 -- 처음부터 읽으면
/// 비용이 로그 크기를 따라 끝없이 자란다.
pub fn tail_log(log_path: &Path, max_lines: usize) -> Vec<String> {
    tail_log_window(log_path, max_lines, TAIL_WINDOW_BYTES)
}

/// 20줄에 넉넉하다. 모자라면 `tail_log_window` 가 두 배씩 넓힌다.
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
        // `len` 까지만 -- 읽는 사이에 노드가 덧붙인 것은 다음 틱의 몫이다.
        if file.seek(SeekFrom::Start(start)).is_err()
            || (&mut file).take(len - start).read_to_end(&mut buf).is_err()
        {
            return Vec::new();
        }
        // UTF-8 이 아닌 바이트 하나가 그 뒤의 줄을 전부 가리면 안 된다.
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        // 파일 중간에서 시작한 창은 거의 언제나 줄 중간에서 시작한다. 그
        // 첫 조각은 줄이 아니다. (창이 마침 줄 머리에서 시작했다면 온전한
        // 줄 하나를 버리는 셈인데, 그러면 모자란 만큼 아래에서 창을 넓힌다.)
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
    /// 자식이 스스로 끝났다. 포트 충돌이 이 모양으로 온다 -- 노드는 명시된
    /// 포트를 못 잡으면 실패로 끝낸다.
    Exited {
        code: Option<i32>,
    },
    Failed {
        message: String,
    },
    /// 우리가 세웠다. `Exited` 로 재활용하지 않는다 -- 그건 "자식이 스스로
    /// 끝났다"는 뜻이고, 요청받아 내려간 것과는 다른 사건이다.
    Stopped,
}

enum Command {
    Stop,
}

/// 노드를 소유하는 전용 스레드. `PR_SET_PDEATHSIG` 가 fork 한 스레드의
/// 죽음에 발동하므로, 이 스레드는 `Supervisor` 가 살아 있는 동안 절대 끝나지
/// 않는다.
pub struct Supervisor {
    tx: mpsc::Sender<Command>,
    state: Arc<Mutex<NodeState>>,
    log_path: PathBuf,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// 노드가 SIGTERM 에 응답할 시간. 실측은 0.5초였다.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// 남의 고아를 세울 때 기다리는 시간.
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
    // 지난 실행이 노드를 남겼을 수 있다. 먼저 걷어낸다 -- 안 그러면 새
    // 노드가 락과 포트에 부딪혀 실패한다.
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
        let sibling = dir.path().join("alphanumeric");
        std::fs::write(&sibling, b"").expect("write");
        assert_eq!(locate_binary(None, dir.path()).expect("found"), sibling);
    }

    // 배포 실수 중 가장 흔한 형태다. 어디를 찾았는지 말하지 않으면
    // 사용자가 고칠 수 없다.
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

    // 채굴 지갑 키는 채굴 노드의 cwd 에 있고 이 노드에는 없다. 채굴을 켜면
    // "지갑이 키를 갖고 노드는 갖지 않는다"는 뼈대가 깨진다 (스펙 §12).
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

    // 숫자만 주면 노드가 127.0.0.1 에 붙인다 (`src/a9/node.rs` 의 `start_explorer_server`). 지갑 노드의
    // 익스플로러는 절대 밖을 향하지 않는다.
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

    /// D 는 채굴 노드의 8787 과 부딪히지 않으려고 stats 를 껐다. 콘솔이
    /// 피어와 해시레이트를 여기서 읽으므로 이제 자기 포트로 켠다.
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

    /// 채굴 노드가 쥔 포트들. 상수를 직접 겨눈다 -- sample_config 의 리터럴을
    /// 훑는 것만으로는 상수가 8787 로 퇴행해도 통과한다.
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

    /// 채굴 노드의 포트를 절대 쓰지 않는다. 상수가 아니라 환경을 훑는다.
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

    /// 노드 대신 세울 가짜. `trap` 여부로 SIGTERM 을 받는 놈과 무시하는 놈을
    /// 만든다 -- 후자가 SIGKILL 폴백 경로를 실제로 태운다.
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
        // 자식이 한 줄 찍을 시간을 준다.
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

    /// SIGKILL 은 노드의 `StartupLockGuard::drop` 을 건너뛰어 락과 포트를
    /// 남긴다. 그래서 최후에만 쓰고, 최후가 실제로 있는지 확인한다.
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

    /// 노드 자신의 `is_process_alive` 는 죽은 pid 만 치워준다
    /// (`main.rs`). 살아 있는 고아는 우리가 걷어내야 한다.
    ///
    /// **우리 자식을 쓰면 안 된다:** SIGTERM 을 받은 자식은 누가 `wait()` 할
    /// 때까지 좀비로 남고, 좀비는 `kill(pid, 0)` 에 계속 잡힌다 -- `is_alive`
    /// 가 영원히 참이라 이 테스트가 유예를 다 쓰고 실패한다. `sh` 를 즉시
    /// 끝내 손자를 고아로 만들면 subreaper 가 거둬가므로 실제로 사라진다.
    /// 그것이 `reclaim_orphan` 이 상대하는 진짜 상황이기도 하다.
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

        // 노드가 하듯 락에 pid 를 적어둔다.
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
        // 1 번은 init 이라 우리 것이 아니다. 절대 죽지 않을 아주 큰 pid 를 쓴다.
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

    /// F6 는 열어 둔 채로 두는 화면이고 로그는 `.append(true)` 로 열려
    /// 회전하지 않는다. 20줄을 얻으려고 매 2초 파일 전체를 읽으면 비용이
    /// 로그 크기에 비례해 끝없이 자란다. 창 크기보다 훨씬 큰 파일에서도
    /// 마지막 줄들이 순서대로 나와야 한다.
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

    /// 창이 파일 중간에서 시작하면 거의 언제나 줄 중간에서 시작한다. 그
    /// 조각은 줄이 아니다 -- `ine 4997` 같은 것이 로그에 보여서는 안 된다.
    /// 한 줄이 창보다 길면 창을 넓혀서라도 온전한 줄을 낸다.
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

    /// 전에는 `lines().map_while(Result::ok)` 이라 UTF-8 이 아닌 바이트
    /// 하나에서 읽기가 멈췄다 -- 그 뒤의 줄은 영영 안 보였다.
    #[test]
    fn the_log_tail_survives_a_line_that_is_not_utf8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("node.log");
        std::fs::write(&log, b"before\n\xff\xfe\nafter\n").expect("write");
        assert_eq!(tail_log(&log, 1), vec!["after".to_string()]);
    }

    /// 마지막 줄에 줄바꿈이 없어도 줄이다 -- 노드가 쓰는 중인 줄이다.
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

    fn wait_for(mut done: impl FnMut() -> bool, what: &str) {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}");
    }

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

    /// 포트 충돌이 이 경로로 온다: 노드는 명시된 포트를 못 잡으면 실패로
    /// 끝난다. 감독기가 그것을 `Exited` 로 보여주지 못하면 GUI 는 영원히
    /// 기다린다.
    #[test]
    fn a_child_that_dies_on_its_own_is_noticed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let supervisor = start_supervised(|| {
            let binary = dir.path().join("quitter");
            std::fs::write(&binary, "#!/bin/sh\nexit 3\n").expect("write");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
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

    /// `stop()` 뒤에 `state()` 가 `Running` 으로 남아 있으면, 그 위에 세운
    /// 폴링 루프는 죽은 노드를 기다리며 영원히 돈다. `Supervisor` 를 드롭하지
    /// 않은 채로 확인한다 -- Drop 경로는 `dropping_the_supervisor_stops_the_node`
    /// 가 이미 덮고 있고, 이 결함은 Drop 없이 `stop()` 만 불렀을 때 드러난다.
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
