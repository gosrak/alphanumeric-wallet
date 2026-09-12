//! Entry point for the alphanumeric wallet.

mod app;
mod theme;
mod view;
mod widgets;

use iced::{window, Size};

/// The window's application id: the X11 WM_CLASS and the Wayland app_id. A
/// launcher's `StartupWMClass` (and a .desktop file named after it) is how a
/// desktop ties the running window to its icon in the dock.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const APP_ID: &str = "alphanumeric-wallet";

/// The alphanumeric logo -- the green checkered cube the community uses as
/// its Discord server icon -- with rounded corners. Embedded, so the icon
/// travels with the binary. `None` if it somehow fails to decode: a missing
/// icon must never stop a wallet from opening.
fn window_icon() -> Option<window::Icon> {
    let decoded = image::load_from_memory(include_bytes!("../assets/icon/alphanumeric-wallet.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = decoded.dimensions();
    window::icon::from_rgba(decoded.into_raw(), width, height).ok()
}

fn window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(1000.0, 700.0),
        min_size: Some(Size::new(760.0, 560.0)),
        position: window::Position::Centered,
        icon: window_icon(),
        // Linux only: the other platforms' settings have no application id.
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_string(),
            ..Default::default()
        },
        ..window::Settings::default()
    }
}

fn main() -> iced::Result {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--screenshot")) {
        if let Some(dir) = args.next() {
            let _ = app::SCREENSHOT_DIR.set(std::path::PathBuf::from(dir));
        }
    }

    iced::application(app::App::new, app::App::update, app::App::view)
        .title("alphanumeric wallet")
        .subscription(app::App::subscription)
        .window(window_settings())
        .theme(|_: &_| theme::alphanumeric_theme())
        // Bundled rather than resolved from the system: a wallet that renders
        // differently on two machines is a wallet whose address column can be
        // ambiguous on one of them. 1.9MB, OFL-1.1 (see assets/fonts).
        .font(include_bytes!("../assets/fonts/NotoSansMono-Regular.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/NotoSans-Regular.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/NotoSans-Bold.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/NotoSansSymbols-Bold.ttf").as_slice())
        // CJK is bundled now although every user-facing string is English
        // today: if a translation is ever decided on, the font work is already
        // done. 168KB.
        .font(include_bytes!("../assets/fonts/NotoSansCJKsc-UI.otf").as_slice())
        .default_font(theme::TECH_FONT)
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The icon is decoded when the window opens. A bad asset must show up
    // here, not as a wallet that opens without one -- or, worse, not at all.
    #[test]
    fn the_window_icon_decodes() {
        assert!(window_icon().is_some());
    }

    // The launcher (alphanumeric-wallet.desktop, StartupWMClass) finds the
    // running window by this id, which becomes the X11 WM_CLASS and the
    // Wayland app_id.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_window_carries_the_launcher_id() {
        assert_eq!(window_settings().platform_specific.application_id, APP_ID);
        assert_eq!(APP_ID, "alphanumeric-wallet");
    }
}
