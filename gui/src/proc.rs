//! Where the node process we started gets measured.
//!
//! No `iced` here -- a lib crate's discipline. Someone else's node (External
//! mode) has no process, so every function here can return `None`, and
//! `None` means "cannot be measured", not "zero".

use std::path::Path;
use std::time::Instant;

/// Linux's USER_HZ. `sysconf(_SC_CLK_TCK)` is the correct way to get it, but
/// on Linux it is practically never anything but 100, and that one case
/// isn't worth pulling in a libc call.
const USER_HZ: f32 = 100.0;

#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    /// Which process this tick count came from. `cpu_percent` refuses to
    /// diff two samples with different pids -- see its doc comment.
    pub pid: u32,
    pub ticks: u64,
    pub at: Instant,
}

/// `/proc/<pid>/stat`'s utime + stime.
///
/// **Counts from the last `)` onward.** The comm field is wrapped in
/// parentheses and can itself hold spaces and parentheses, so splitting on
/// whitespace from the start of the line shifts every field over by however
/// much that one name ate. The first field after the parenthesis is state;
/// utime is the 12th field from there, stime the 13th.
pub fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// `/proc/<pid>/status`'s `VmRSS` (KiB).
pub fn parse_rss_kib(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// CPU usage (%) between two samples. Pegging a single core reads 100.
///
/// A single sample only yields the average since the process started, and
/// that isn't the current load. So two samples are required, and when no
/// time has passed or the counter runs backward (pid reuse), this returns
/// `None` rather than drawing a negative number or an infinity.
///
/// **Unconditionally `None` when the two samples' pids differ.** The normal
/// path is for the caller to clear `prev_cpu_sample` when a restart swaps in
/// a new child, but this catches it too, in case some future caller forgets
/// that clear -- subtracting ticks between two different processes isn't two
/// samples, it's nothing measured at all. For the same reason one sample
/// yields no value, a restart effectively leaves only one sample behind.
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

/// A directory's total size. A directory that doesn't exist is `None` -- not
/// 0. The node not having fetched the chain yet is a different story from a
/// "zero-byte chain".
///
/// Any single part that can't be read also makes this `None`. Skipping it
/// and adding up the rest would sit a smaller-than-real number in the field
/// **as if it were the right answer**. The one exception is an entry that
/// vanished mid-walk -- a temp file the node deleted really is 0 bytes.
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

/// Gone by the time this tried to read it. Not an unknown size -- a 0 one.
fn counts_as_gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/proc/<pid>/stat`'s comm field is wrapped in parentheses and **can
    /// itself hold spaces and parentheses**. Splitting on whitespace from the
    /// front shifts every field over by that one name. Counting from the
    /// last ')' onward is the only way to get this right.
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

    /// A single sample can only yield "the average since the process
    /// started". That isn't the current load. So the first sample must
    /// produce no value at all.
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
        // Spending 100 ticks (= 1 second's worth, Linux USER_HZ=100) in 1
        // second of wall time means pegging one core.
        let now = CpuSample {
            pid: 42,
            ticks: 1_100,
            at: t0 + std::time::Duration::from_secs(1),
        };
        let pct = cpu_percent(&prev, &now).expect("two samples");
        assert!((pct - 100.0).abs() < 1.0, "expected ~100%, got {pct}");
    }

    /// The counter should never go down, but pid reuse can make it look like
    /// it did. Better to say "unknown" than to draw a negative percentage.
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

    /// When a restart swaps the child, the two samples belong to different
    /// processes. If the new pid happens to have racked up more ticks, the
    /// result looks plausible; if fewer, `checked_sub` catches it, but that
    /// is luck, not a rule. However many samples there are, a pid mismatch
    /// means nothing was measured -- the same answer as a single sample:
    /// `None`.
    #[test]
    fn a_pid_change_across_two_samples_is_none_even_though_ticks_only_grew() {
        let t0 = std::time::Instant::now();
        let prev = CpuSample {
            pid: 42,
            ticks: 500,
            at: t0,
        };
        // A new process that has been running for a long time can, on its
        // own, have more ticks than the previous sample -- `checked_sub`
        // alone can't catch this case.
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

    /// Skipping an unreadable subdirectory and adding up the rest would show
    /// the DISK field a smaller-than-real number **as if it were correct**.
    /// When part of it is unknown, say so.
    #[test]
    fn an_unreadable_subdirectory_makes_the_size_unknown_not_smaller() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), vec![0u8; 1000]).expect("a");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("mkdir");
        std::fs::write(locked.join("b"), vec![0u8; 2000]).expect("b");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // root (and CAP_DAC_OVERRIDE) ignores permissions. This test proves
        // nothing there, so it's skipped -- judged by whether the read
        // actually succeeds, not by uid.
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

    /// An entry that vanished mid-walk (a temp file the node deleted) is not
    /// an error -- a gone file's size really is 0. Making that case `None`
    /// too would make the DISK field flicker for no reason.
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
