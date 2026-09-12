//! F6: the node process this wallet drives (or the external one it points
//! at instead).
//!
//! Widget assembly only. `Supervisor::state()` is a mutex lock, cheap enough
//! for every `view()`; the log tail is real file I/O and is read on the
//! `ConsoleTick` cadence instead (`App::node_log_tail`'s own doc comment
//! says why) -- this module only ever reads that field, never the file.

use iced::widget::{column, row, text, Space};
use iced::{Alignment, Color, Element, Length};

use alphanumeric_gui::node::NodeState;

use crate::app::{App, Message, NodeSource, Screen};
use crate::theme;
use crate::view::console::{fmt_bytes, fmt_kib, or_dash};
use crate::view::{field, kit};

/// Only ever called from the `Owned` branch of `view()`. `None` means
/// `ensure_owned_node`'s config step failed before a `Supervisor` existed --
/// a known fact, so it reads "NOT STARTED", not a dash. A `Failed` message
/// is drawn in full under the badge.
fn state_badge(state: Option<&NodeState>) -> (String, Color) {
    match state {
        None => ("NOT STARTED".to_string(), theme::MUTED),
        Some(NodeState::Starting) => ("STARTING".to_string(), theme::WARNING),
        Some(NodeState::Running { pid }) => (format!("RUNNING pid {pid}"), theme::ACCENT),
        Some(NodeState::Exited { code: Some(c) }) => (format!("EXITED code {c}"), theme::DANGER),
        Some(NodeState::Exited { code: None }) => ("EXITED".to_string(), theme::DANGER),
        Some(NodeState::Failed { .. }) => ("FAILED".to_string(), theme::DANGER),
        Some(NodeState::Stopped) => ("STOPPED".to_string(), theme::MUTED),
    }
}

/// The full reason under the badge: a `Failed` node's own message, or --
/// when no supervisor was ever built -- the start error. The badge is
/// short; this line must never be lost to it.
fn failure_text(state: Option<&NodeState>, start_error: Option<&str>) -> Option<String> {
    match state {
        Some(NodeState::Failed { message }) => Some(message.clone()),
        None => start_error.map(|reason| reason.to_string()),
        _ => None,
    }
}

/// CPU and MEMORY describe a process, so they are drawn only while a
/// supervisor exists. With none (`NOT STARTED`) the last `ConsoleTick`'s
/// reading can still be sitting there for up to 2 s, and a gauge beside
/// NOT STARTED reads as a process that is running. DISK is the data
/// directory, which exists either way, and stays.
fn shows_process_meters(state: Option<&NodeState>) -> bool {
    state.is_some()
}

/// `HH:MM:SS` from a second count. The node's own `/stats` reports uptime as
/// seconds; this is display only, not arithmetic anyone else depends on.
fn fmt_duration(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Uptime only means something for a process that is alive. `node_stats()`
/// persists across failed re-queries and restarts, so dropping this guard
/// would show a dead or freshly restarted process's **previous run**
/// uptime as if it were current.
fn uptime_line(running: bool, uptime_secs: Option<u64>) -> String {
    if !running {
        return "—".to_string();
    }
    uptime_secs
        .map(fmt_duration)
        .unwrap_or_else(|| "—".to_string())
}

pub fn view(app: &App) -> Element<'_, Message> {
    let settings = kit::action(
        "F7 SETTINGS",
        kit::Act::Plain,
        Some(Message::Show(Screen::Settings)),
    );

    if app.node_source == NodeSource::External {
        // No supervisor in External mode -- no pid, CPU, memory or log to
        // measure. Say so and point at settings instead of drawing zeros.
        return kit::screen(kit::tag_panel(
            "NODE",
            theme::CYAN,
            column![
                text(format!(
                    "This wallet uses an external node -- {}",
                    app.node_url
                ))
                .size(f32::from(theme::BODY))
                .wrapping(iced::widget::text::Wrapping::Glyph),
                settings,
            ]
            .spacing(f32::from(theme::SPACING)),
        ));
    }

    let state = app.node_state();
    let running = matches!(state, Some(NodeState::Running { .. }));
    let (badge, badge_colour) = state_badge(state.as_ref());
    let dash = || "—".to_string();

    let mut process = column![row![
        kit::badge(badge, badge_colour),
        Space::new().width(Length::Fill),
        text("UPTIME").size(12).color(theme::DIM),
        text(uptime_line(
            running,
            app.node_stats().and_then(|s| s.uptime_secs)
        ))
        .size(13)
        .color(theme::TEXT),
    ]
    .spacing(8)
    .align_y(Alignment::Center)]
    .spacing(10);
    if let Some(reason) = failure_text(state.as_ref(), app.node_start_error()) {
        process = process.push(
            text(reason)
                .size(f32::from(theme::SMALL))
                .color(theme::DANGER)
                .wrapping(iced::widget::text::Wrapping::Glyph),
        );
    }
    if shows_process_meters(state.as_ref()) {
        let cpu = app.cpu_share();
        process = process
            .push(kit::meter(
                "CPU",
                cpu,
                theme::ACCENT,
                cpu.map(|r| format!("{:.1}%", r * 100.0))
                    .unwrap_or_else(dash),
            ))
            .push(kit::meter(
                "MEMORY",
                app.mem_share(),
                theme::WARNING,
                app.process_rss_kib().map(fmt_kib).unwrap_or_else(dash),
            ));
    }
    process = process
        .push(kit::telemetry(
            "DISK",
            app.process_disk_bytes().map(fmt_bytes).unwrap_or_else(dash),
            theme::ACCENT,
        ))
        .push(
            row![
                kit::action("RESTART NODE", kit::Act::Care, Some(Message::RetryNode)),
                settings,
            ]
            .spacing(8),
        );

    let status = app.wallet.as_ref().and_then(|w| w.node_status.as_ref());
    let data = column![
        kit::kv_row("SOURCE", "Run by this wallet"),
        kit::kv_row("NODE URL", app.node_url.clone()),
        field(
            "DIRECTORY",
            app.data_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(dash),
        ),
        kit::kv_row(
            "HEIGHT",
            format!(
                "{} / {}",
                or_dash(status.and_then(|s| s.height)),
                or_dash(status.and_then(|s| s.network_height))
            ),
        ),
    ]
    .spacing(8);

    kit::screen(
        column![
            kit::split(
                app.narrow(),
                kit::tag_panel("PROCESS", theme::CYAN, process),
                1,
                kit::tag_panel("DATA", theme::MUTED, data),
                1,
            ),
            kit::tag_panel("LOG", theme::MUTED, kit::terminal(app.node_log_tail(), "—")),
        ]
        .spacing(f32::from(theme::SPACING)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owned mode where the supervisor was never even built (a missing
    /// binary, no home directory) is a known fact, not an unknown.
    #[test]
    fn no_supervisor_in_owned_mode_reads_as_not_started() {
        assert_eq!(state_badge(None).0, "NOT STARTED");
    }

    #[test]
    fn a_running_state_names_its_pid() {
        assert!(state_badge(Some(&NodeState::Running { pid: 4242 }))
            .0
            .contains("4242"));
    }

    #[test]
    fn an_exited_state_names_its_code() {
        assert!(state_badge(Some(&NodeState::Exited { code: Some(1) }))
            .0
            .contains('1'));
    }

    /// The badge says FAILED; the message is drawn under it in full
    /// (`view`), so the reason is never lost to a short badge.
    #[test]
    fn a_failed_state_is_red() {
        let (label, colour) = state_badge(Some(&NodeState::Failed {
            message: "port 7178 in use".to_string(),
        }));
        assert_eq!(label, "FAILED");
        assert_eq!(colour, theme::DANGER);
    }

    #[test]
    fn the_full_failure_reason_is_what_the_screen_shows() {
        let failed = NodeState::Failed {
            message: "port 7178 in use".to_string(),
        };
        assert_eq!(
            failure_text(Some(&failed), None).as_deref(),
            Some("port 7178 in use")
        );
        assert_eq!(
            failure_text(None, Some("no binary")).as_deref(),
            Some("no binary")
        );
        assert_eq!(
            failure_text(Some(&NodeState::Running { pid: 1 }), Some("stale")),
            None,
            "a running node shows no stale start error"
        );
        assert_eq!(failure_text(None, None), None);
    }

    #[test]
    fn no_supervisor_draws_no_process_gauges() {
        assert!(!shows_process_meters(None));
        assert!(shows_process_meters(Some(&NodeState::Starting)));
        assert!(shows_process_meters(Some(&NodeState::Stopped)));
    }

    #[test]
    fn fmt_duration_renders_hours_minutes_seconds() {
        assert_eq!(fmt_duration(0), "00:00:00");
        assert_eq!(fmt_duration(3_661), "01:01:01");
        assert_eq!(fmt_duration(59), "00:00:59");
    }

    #[test]
    fn running_with_a_value_gives_the_formatted_duration() {
        assert_eq!(uptime_line(true, Some(3_661)), "01:01:01");
    }

    #[test]
    fn running_without_a_value_gives_the_dash() {
        assert_eq!(uptime_line(true, None), "—");
    }

    /// `node_stats()` outlives the process it describes -- it is kept, not
    /// cleared, across a failed refetch or a `RetryNode` restart. A dead or
    /// just-restarted process must never show its PREVIOUS run's uptime as
    /// though it were current, even though a value is sitting right there.
    #[test]
    fn not_running_gives_the_dash_even_when_a_value_is_present() {
        assert_eq!(uptime_line(false, Some(3_661)), "—");
    }
}
