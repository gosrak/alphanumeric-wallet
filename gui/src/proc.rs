//! 우리가 띄운 노드 프로세스를 재는 자리.
//!
//! `iced` 를 모른다 -- lib 크레이트의 규율이다. 남의 노드(External 모드)에는
//! 프로세스가 없으므로 여기 있는 모든 함수가 `None` 을 돌려줄 수 있고,
//! `None` 은 "0" 이 아니라 "잴 수 없다"는 뜻이다.

use std::path::Path;
use std::time::Instant;

/// 리눅스의 USER_HZ. `sysconf(_SC_CLK_TCK)` 가 정답이지만 리눅스에서 100 이
/// 아닌 경우가 실질적으로 없고, 그 하나를 위해 libc 호출을 들이지 않는다.
const USER_HZ: f32 = 100.0;

#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    /// Which process this tick count came from. `cpu_percent` refuses to
    /// diff two samples with different pids -- see its doc comment.
    pub pid: u32,
    pub ticks: u64,
    pub at: Instant,
}

/// `/proc/<pid>/stat` 의 utime + stime.
///
/// **마지막 `)` 뒤부터 센다.** comm 필드는 괄호로 감싸이고 그 안에 공백과
/// 괄호를 담을 수 있어서, 줄 앞에서부터 공백으로 자르면 이름 하나에 필드가
/// 통째로 밀린다. 괄호 뒤 첫 필드가 state 이고, utime 은 그로부터 12번째,
/// stime 은 13번째다.
pub fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// `/proc/<pid>/status` 의 `VmRSS` (KiB).
pub fn parse_rss_kib(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// 두 표본 사이의 CPU 사용률(%). 코어 하나를 꽉 쓰면 100.
///
/// 한 표본으로는 프로세스 시작 이래의 평균밖에 안 나오는데 그건 지금 부하가
/// 아니다. 그래서 두 표본을 요구하고, 시간이 안 흘렀거나 카운터가 뒤로 가면
/// (pid 재사용) 음수·무한대를 그리느니 `None` 을 돌려준다.
///
/// **두 표본의 pid 가 다르면 무조건 `None`.** 재시작으로 자식이 바뀌면
/// 호출자가 `prev_cpu_sample` 을 지워 주는 게 정상 경로지만, 그 지움을
/// 잊는 미래의 호출자가 있어도 여기서 막힌다 -- 서로 다른 프로세스의 틱을
/// 빼는 것은 표본 두 개가 아니라 아무것도 잰 게 아니다. 표본 하나로는 값이
/// 안 나오는 것과 같은 이유로, 재시작 뒤에는 사실상 표본이 하나뿐이다.
pub fn cpu_percent(prev: &CpuSample, now: &CpuSample) -> Option<f32> {
    if prev.pid != now.pid {
        return None;
    }
    let elapsed = now.at.checked_duration_since(prev.at)?.as_secs_f32();
    if elapsed <= 0.0 {
        return None;
    }
    let ticks = now.ticks.checked_sub(prev.ticks)?;
    Some((ticks as f32 / USER_HZ) / elapsed * 100.0)
}

pub fn read_cpu_sample(pid: u32) -> Option<CpuSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(CpuSample {
        pid,
        ticks: parse_cpu_ticks(&stat)?,
        at: Instant::now(),
    })
}

pub fn read_rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_rss_kib(&status)
}

/// What the CPU gauge fills against: ONE busy core, 0..=1. `cpu_percent`
/// above counts 100 per busy core, so 100.0 fills the bar and more than one
/// core clamps there.
///
/// Not a share of the whole machine. The wallet's node is one modest process
/// on a many-core box, so that share never left the bar's first pixel: 70.5
/// on 32 cores is 2.2 %, and the figure a person wants from this gauge is
/// "how hard is the node working", not "how much of this machine is it".
pub fn core_share(cpu_percent: f32) -> Option<f32> {
    if !cpu_percent.is_finite() {
        return None;
    }
    Some((cpu_percent / 100.0).clamp(0.0, 1.0))
}

/// Where the memory gauge fills: 2 GiB. The wallet's own node was measured at
/// 594 MB resident (sub-project D), so a real reading sits near a third of
/// the bar and its growth is visible. A share of the machine's total put the
/// same reading at 1 % on this box.
pub const RSS_FULL_KIB: u64 = 2 * 1024 * 1024;

/// Resident memory against `RSS_FULL_KIB`, 0..=1. The figure beside the bar
/// is the real one (`view::console::fmt_kib`); this is only how far it is
/// drawn.
pub fn rss_share(rss_kib: Option<u64>) -> Option<f32> {
    rss_kib.map(|rss| (rss as f32 / RSS_FULL_KIB as f32).clamp(0.0, 1.0))
}

/// 디렉터리 전체 크기. 없는 디렉터리는 `None` -- 0 이 아니다. 노드가 아직
/// 체인을 안 받았을 때와 "0바이트짜리 체인"은 다른 이야기다.
///
/// 못 읽는 부분이 하나라도 있으면 역시 `None` 이다. 건너뛰고 나머지를
/// 더하면 실제보다 작은 숫자가 **정답처럼** 칸에 앉는다. 예외는 걷는 사이에
/// 사라진 항목뿐이다 -- 노드가 지운 임시 파일의 크기는 0 이 맞다.
pub fn dir_size_bytes(path: &Path) -> Option<u64> {
    if !path.is_dir() {
        return None;
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if counts_as_gone(&e) => continue,
            Err(_) => return None,
        };
        for entry in entries {
            let (entry_path, meta) = match entry.and_then(|e| e.metadata().map(|m| (e.path(), m))) {
                Ok(pair) => pair,
                Err(e) if counts_as_gone(&e) => continue,
                Err(_) => return None,
            };
            if meta.is_dir() {
                stack.push(entry_path);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Some(total)
}

/// 읽으려던 사이에 없어졌다. 크기를 모르는 게 아니라 0 인 경우다.
fn counts_as_gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    /// /proc/<pid>/stat 의 comm 필드는 괄호로 감싸이고 **공백과 괄호를 담을 수
    /// 있다**. 앞에서부터 공백으로 자르면 그 이름 하나에 필드가 밀린다.
    /// 마지막 ')' 뒤부터 세는 것이 유일하게 맞는 방법이다.
    #[test]
    fn cpu_ticks_survive_a_comm_field_with_spaces_and_parens() {
        let stat = "42 (al ph(a) num) S 1 42 42 0 -1 4194560 100 0 0 0 \
                    1234 567 0 0 20 0 8 0 99 0 0";
        assert_eq!(parse_cpu_ticks(stat), Some(1234 + 567));
    }

    #[test]
    fn cpu_ticks_from_a_plain_name() {
        let stat = "7 (alphanumeric) S 1 7 7 0 -1 0 0 0 0 0 300 45 0 0 20 0 4 0 1 0 0";
        assert_eq!(parse_cpu_ticks(stat), Some(345));
    }

    #[test]
    fn a_truncated_stat_line_is_none_not_zero() {
        assert_eq!(parse_cpu_ticks("42 (node) S 1 2 3"), None);
        assert_eq!(parse_cpu_ticks(""), None);
        assert_eq!(parse_cpu_ticks("no parens here at all"), None);
    }

    #[test]
    fn rss_is_read_out_of_the_status_block() {
        let status = "Name:\talphanumeric\nState:\tS (sleeping)\n\
                      VmPeak:\t 1234567 kB\nVmRSS:\t  608256 kB\nThreads:\t9\n";
        assert_eq!(parse_rss_kib(status), Some(608_256));
    }

    #[test]
    fn a_status_block_without_rss_is_none() {
        assert_eq!(parse_rss_kib("Name:\tx\nThreads:\t1\n"), None);
    }

    /// 한 표본만으로는 "프로세스가 시작된 이래의 평균"밖에 못 낸다. 그건 지금
    /// 부하가 아니다. 그래서 첫 표본에서는 값이 없어야 한다.
    #[test]
    fn one_sample_yields_no_percentage() {
        let s = CpuSample {
            pid: 42,
            ticks: 100,
            at: std::time::Instant::now(),
        };
        assert_eq!(cpu_percent(&s, &s), None);
    }

    #[test]
    fn cpu_percent_is_ticks_over_elapsed() {
        let t0 = std::time::Instant::now();
        let prev = CpuSample {
            pid: 42,
            ticks: 1_000,
            at: t0,
        };
        // 1초 동안 100틱(=1초어치, 리눅스 USER_HZ=100) 썼으면 코어 하나를 꽉 쓴 것.
        let now = CpuSample {
            pid: 42,
            ticks: 1_100,
            at: t0 + std::time::Duration::from_secs(1),
        };
        let pct = cpu_percent(&prev, &now).expect("two samples");
        assert!((pct - 100.0).abs() < 1.0, "expected ~100%, got {pct}");
    }

    /// 카운터가 줄어드는 일은 없어야 하지만, pid 가 재사용되면 그렇게 보인다.
    /// 음수 백분율을 그리느니 모른다고 하는 편이 낫다.
    #[test]
    fn a_backwards_counter_is_none() {
        let t0 = std::time::Instant::now();
        let prev = CpuSample {
            pid: 42,
            ticks: 500,
            at: t0,
        };
        let now = CpuSample {
            pid: 42,
            ticks: 100,
            at: t0 + std::time::Duration::from_secs(1),
        };
        assert_eq!(cpu_percent(&prev, &now), None);
    }

    /// 재시작으로 자식이 바뀌면 두 표본은 서로 다른 프로세스의 것이다. 새
    /// pid 가 우연히 더 많은 틱을 쌓아 왔으면 말이 되는 듯한 숫자가 나오고,
    /// 더 적으면 `checked_sub` 가 막아 주지만 그건 우연이지 규칙이 아니다.
    /// pid 가 다르면 표본이 몇 개든 잰 게 아니다 -- 표본 하나와 같은 답,
    /// `None`.
    #[test]
    fn a_pid_change_across_two_samples_is_none_even_though_ticks_only_grew() {
        let t0 = std::time::Instant::now();
        let prev = CpuSample {
            pid: 42,
            ticks: 500,
            at: t0,
        };
        // 새 프로세스가 오래 전부터 떠 있었다면 그 자체로 틱이 이전 표본보다
        // 많을 수 있다 -- checked_sub 만으로는 이 경우를 못 잡는다.
        let now = CpuSample {
            pid: 43,
            ticks: 999_999,
            at: t0 + std::time::Duration::from_secs(1),
        };
        assert_eq!(cpu_percent(&prev, &now), None);
    }

    #[test]
    fn a_directory_size_adds_up_its_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), vec![0u8; 1000]).expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub").join("b"), vec![0u8; 2000]).expect("write");
        assert_eq!(dir_size_bytes(dir.path()), Some(3000));
    }

    #[test]
    fn a_missing_directory_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(dir_size_bytes(&dir.path().join("absent")), None);
    }

    /// 못 읽는 하위 디렉터리를 건너뛰고 나머지를 더하면 DISK 칸이 실제보다
    /// 작은 숫자를 **정답처럼** 보인다. 모르는 부분이 있으면 모른다고 한다.
    #[test]
    fn an_unreadable_subdirectory_makes_the_size_unknown_not_smaller() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), vec![0u8; 1000]).expect("a");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("mkdir");
        std::fs::write(locked.join("b"), vec![0u8; 2000]).expect("b");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // root(와 CAP_DAC_OVERRIDE)는 권한을 무시한다. 그런 곳에서는 이 테스트가
        // 증명할 것이 없으니 건너뛴다 -- uid 가 아니라 실제로 읽히는지로 판단한다.
        if std::fs::read_dir(&locked).is_ok() {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
                .expect("chmod back");
            return;
        }
        let size = dir_size_bytes(dir.path());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");
        assert_eq!(size, None);
    }

    /// 걷는 사이에 사라진 항목(노드가 지운 임시 파일)은 오류가 아니다 --
    /// 없어진 파일의 크기는 0 이 맞다. 그 경우까지 `None` 이 되면 DISK 칸이
    /// 이유 없이 깜빡인다.
    #[test]
    fn an_entry_that_vanished_mid_walk_is_simply_not_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), vec![0u8; 1000]).expect("a");
        assert!(counts_as_gone(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(!counts_as_gone(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert_eq!(dir_size_bytes(dir.path()), Some(1000));
    }

    /// `cpu_percent` counts 100 per busy core, and the gauge is drawn against
    /// ONE core: the wallet's node is a single-threaded neighbour on a big
    /// box, and a share of the whole machine left the bar visually empty at
    /// every load it ever reaches.
    #[test]
    fn the_cpu_gauge_fills_at_one_busy_core() {
        assert_eq!(core_share(0.0), Some(0.0));
        let half = core_share(50.0).expect("finite");
        assert!((half - 0.5).abs() < 1e-6, "{half}");
        assert_eq!(core_share(100.0), Some(1.0));
        assert_eq!(core_share(340.0), Some(1.0), "more than one core clamps");
        assert_eq!(core_share(f32::NAN), None);
    }

    /// Measured: the wallet's own node sits at 594 MB RSS (sub-project D).
    /// `RSS_FULL_KIB` is the ceiling the bar fills at, chosen so that reading
    /// lands near a third rather than against the stop.
    #[test]
    fn the_memory_gauge_fills_at_the_reference_ceiling() {
        let measured = rss_share(Some(594 * 1024)).expect("known");
        assert!((0.2..0.4).contains(&measured), "{measured}");
        assert_eq!(rss_share(Some(RSS_FULL_KIB)), Some(1.0));
        assert_eq!(rss_share(Some(RSS_FULL_KIB * 4)), Some(1.0), "clamped");
        assert_eq!(rss_share(Some(0)), Some(0.0));
        assert_eq!(rss_share(None), None);
    }
}
