//! Palette, spacing, and widget styles.
//!
//! The structure of this module -- the style-function surface, the raised
//! gradients, the two shadow families, the ButtonKind split -- is ported from
//! `noid_gui/src/theme.rs` (Apache-2.0, Copyright (C) 2026 Paranoid Zero),
//! a working iced 0.14 application by the owner of this repository.
//!
//! The colours are NOT ported. Neutrals (background, the three surface steps,
//! the translucent line, ink, chrome) are kept because alphanumeric has no
//! counterpart -- `src/a9/ui.rs` is terminal UI and has no notion of a
//! background. Every chromatic colour and the whole text hierarchy come from
//! `src/a9/ui.rs` instead, so the wallet and the node console read as one
//! product. See `docs/superpowers/specs/2026-09-08-wallet-theme-design.md`.

use iced::border::Radius;
use iced::font;
use iced::theme::Palette;
use iced::widget::{
    button as button_widget, container, scrollable as scrollable_widget, text_input as input_widget,
};
use iced::Font;
use iced::{gradient, Background, Border, Color, Radians, Shadow, Theme, Vector};

// --- Fonts -------------------------------------------------------------------

/// Default face. Addresses, hashes, and amounts are the substance of this
/// application and all three are read character by character.
pub const TECH_FONT: Font = Font {
    family: font::Family::Name("Noto Sans Mono"),
    weight: font::Weight::Normal,
    stretch: font::Stretch::Normal,
    style: font::Style::Normal,
};

pub const BRAND_FONT: Font = Font {
    family: font::Family::Name("Noto Sans"),
    weight: font::Weight::Bold,
    stretch: font::Stretch::Normal,
    style: font::Style::Normal,
};

#[allow(dead_code)]
pub const BRAND_REGULAR_FONT: Font = Font {
    family: font::Family::Name("Noto Sans"),
    weight: font::Weight::Normal,
    stretch: font::Stretch::Normal,
    style: font::Style::Normal,
};

// --- Layout -----------------------------------------------------------------

pub const SPACING: u16 = 12;
pub const PADDING: u16 = 16;

/// Type scale. Screens used to spell sizes inline (`.size(20)`, `.size(13)`);
/// with four screens that was survivable and with the explorer and mining
/// screens coming it is not.
pub const TITLE: u16 = 28;
pub const HEADING: u16 = 20;
pub const BODY: u16 = 15;
pub const SMALL: u16 = 13;
pub const CAPTION: u16 = 12;

// --- Neutrals: kept from noid ------------------------------------------------

pub const BACKGROUND: Color = Color::from_rgb8(10, 12, 20);
pub const SURFACE: Color = Color::from_rgba8(39, 42, 58, 0.84);
pub const SURFACE_ALT: Color = Color::from_rgba8(50, 54, 72, 0.88);
pub const SURFACE_HIGH: Color = Color::from_rgba8(63, 68, 88, 0.93);
/// Translucent ON PURPOSE: the same rule sits over three surface steps and
/// reads correctly on each. Do not swap an opaque colour in -- use HAIRLINE
/// where a solid rule is wanted.
pub const LINE: Color = Color::from_rgba8(214, 224, 255, 0.14);
pub const LINE_STRONG: Color = Color::from_rgba8(224, 232, 255, 0.25);
pub const INK: Color = Color::from_rgb8(31, 33, 43);
pub const CHROME: Color = Color::from_rgba8(34, 37, 50, 0.86);

// --- Chromatic: copied from src/a9/ui.rs ------------------------------------
//
// The GUI does not depend on the node crate (spec 2.1), so these are copies,
// not imports. `the_palette_matches_what_was_copied_from_the_node` pins them.

/// `src/a9/ui.rs:38` UI_GREEN
pub const ACCENT: Color = Color::from_rgb8(59, 242, 173);
/// `src/a9/ui.rs:36` UI_CYAN
pub const CYAN: Color = Color::from_rgb8(40, 204, 217);
/// `src/a9/ui.rs:41` UI_LAVENDER -- second-tier emphasis (noid's PROOF slot)
pub const LAVENDER: Color = Color::from_rgb8(167, 165, 198);
/// `src/a9/ui.rs:39` UI_ORANGE
pub const ADVISORY: Color = Color::from_rgb8(237, 124, 51);
/// `src/a9/ui.rs:40` UI_PINK
pub const DANGER: Color = Color::from_rgb8(247, 111, 142);
/// Kept from noid: alphanumeric's palette has no yellow, and collapsing this
/// into ADVISORY would give "reversible caution" and "irreversible risk" the
/// same colour.
pub const WARNING: Color = Color::from_rgb8(231, 218, 61);

/// `src/a9/ui.rs:28` UI_LABEL -- body and labels
pub const TEXT: Color = Color::from_rgb8(230, 230, 230);
/// `src/a9/ui.rs:45` UI_VALUE -- figures that carry the meaning of a screen
pub const VALUE: Color = Color::from_rgb8(255, 255, 255);
/// `src/a9/ui.rs:35` UI_MUTED
pub const MUTED: Color = Color::from_rgb8(170, 170, 170);
/// `src/a9/ui.rs:29` UI_DIM -- inactive
pub const DIM: Color = Color::from_rgb8(128, 128, 128);
/// `src/a9/ui.rs:48` UI_FAINT -- the weakest text still meant to be read
pub const FAINT: Color = Color::from_rgb8(90, 97, 105);
/// `src/a9/ui.rs:50` UI_HAIRLINE -- opaque rule
pub const HAIRLINE: Color = Color::from_rgb8(58, 64, 72);

/// The application theme. `primary` and `success` are both ACCENT: this wallet
/// has one affirmative colour and a second one would only invite the question
/// of which is which.
pub fn alphanumeric_theme() -> Theme {
    Theme::custom(
        "alphanumeric".to_string(),
        Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: ACCENT,
            success: ACCENT,
            warning: WARNING,
            danger: DANGER,
        },
    )
}

// --- Containers and surfaces: ported from noid_gui/src/theme.rs ------------
//
// Radii, alphas, and shadow offsets are carried over unchanged -- those are
// the numbers a working application already tuned. Named colours (TEXT,
// SURFACE, CYAN, ...) already resolve to this crate's palette because they
// share names with noid's (see the module doc comment above); no rewrite was
// needed there. Where noid instead inlined a literal that is a translucent
// form of one of ITS OWN palette constants, that relationship is re-expressed
// against ours rather than left as a hard-coded value.

fn soft_shadow() -> Shadow {
    Shadow {
        color: Color::from_rgba8(5, 7, 13, 0.20),
        offset: Vector::new(0.0, 2.0),
        blur_radius: 6.0,
    }
}

pub fn root(_: &Theme) -> container::Style {
    container::Style::default()
        .background(
            gradient::Linear::new(Radians(0.0))
                .add_stop(0.0, Color::from_rgb8(7, 9, 15))
                .add_stop(0.48, Color::from_rgb8(14, 17, 27))
                // noid's third stop was a literal equal to its own BACKGROUND;
                // this is that constant, not a re-typed copy of its digits.
                .add_stop(1.0, BACKGROUND),
        )
        .color(TEXT)
}

pub fn surface(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(SURFACE)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(8.0),
        },
        shadow: soft_shadow(),
        snap: true,
    }
}

pub fn surface_alt(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(SURFACE_ALT)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(6.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn top_bar(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Gradient(
            gradient::Linear::new(Radians(0.0))
                .add_stop(0.0, Color::from_rgba8(24, 27, 39, 0.62))
                .add_stop(0.46, Color::from_rgba8(39, 43, 58, 0.72))
                .add_stop(1.0, Color::from_rgba8(53, 57, 72, 0.66))
                .into(),
        )),
        border: Border {
            color: Color::from_rgba8(230, 237, 255, 0.20),
            width: 1.0,
            radius: Radius::from(10.0),
        },
        shadow: Shadow {
            color: Color::from_rgba8(1, 2, 7, 0.48),
            offset: Vector::new(0.0, 5.0),
            blur_radius: 13.0,
        },
        snap: true,
    }
}

pub fn status_panel(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(CHROME)),
        border: Border {
            // noid used a translucent form of its own CYAN here; this is the
            // same relationship expressed against ours.
            color: Color { a: 0.18, ..CYAN },
            width: 1.0,
            radius: Radius::from(8.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn status_capsule(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(SURFACE)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(6.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn title_style(background: Color) -> container::Style {
    container::Style {
        text_color: Some(INK),
        background: Some(Background::Color(background)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: Radius::from(4.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

#[allow(dead_code)]
pub fn title_bar_cyan(_: &Theme) -> container::Style {
    title_style(CYAN)
}

#[allow(dead_code)]
pub fn title_bar_accent(_: &Theme) -> container::Style {
    title_style(ACCENT)
}

#[allow(dead_code)]
pub fn table_header(_: &Theme) -> container::Style {
    container::Style {
        border: Border {
            radius: Radius::from(3.0),
            ..Border::default()
        },
        ..title_style(ACCENT)
    }
}

pub fn divider(_: &Theme) -> container::Style {
    container::Style::default().background(LINE)
}

pub fn status_dot(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        container::Style::default()
            .background(color)
            .border(Border {
                color,
                width: 0.0,
                radius: Radius::from(99.0),
            })
    }
}

#[allow(dead_code)]
pub fn advisory_badge(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(ADVISORY),
        background: Some(Background::Color(Color {
            a: 0.18,
            ..ADVISORY
        })),
        border: Border {
            color: Color {
                a: 0.76,
                ..ADVISORY
            },
            width: 1.25,
            radius: Radius::from(99.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn advisory_card(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(CHROME)),
        border: Border {
            color: Color {
                a: 0.68,
                ..ADVISORY
            },
            width: 1.0,
            radius: Radius::from(8.0),
        },
        shadow: soft_shadow(),
        snap: true,
    }
}

/// A result card whose border is the verdict's own colour.
///
/// `advisory_card`'s fixed ADVISORY border framed every outcome the send
/// screen can reach, so a success was cautioned and the two verdicts that
/// mean money may already have moved were framed more softly than the text
/// inside them. The frame and the text now come from one source, so they
/// cannot tell different stories.
pub fn verdict_card(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(CHROME)),
        border: Border {
            color: Color { a: 0.68, ..colour },
            width: 1.0,
            radius: Radius::from(8.0),
        },
        shadow: soft_shadow(),
        snap: true,
    }
}

#[allow(dead_code)]
pub fn overlay(_: &Theme) -> container::Style {
    container::Style::default().background(Color::from_rgba8(12, 13, 19, 0.78))
}

// --- Buttons: ported from noid_gui/src/theme.rs -----------------------------
//
// Five kinds, each answering for the four `iced_widget-0.14.2` button states
// (`button.rs:471`: `Active | Hovered | Pressed | Disabled` -- no fifth).
// Disabled is its own style rather than a dimmed active one; the two must
// never converge, or a button that can no longer be pressed still reads as
// though it can. `consolidation_button` (`theme.rs:914-`) is not ported --
// it belongs to a UTXO-consolidation screen this wallet does not have --
// and neither is `language_choice` (`theme.rs:730-`): it is noid's
// first-run language picker, outside this task's produced interface.
//
// Radii, alphas, and shadow offsets are carried over unchanged. Where noid
// inlined a literal that is an EXACT translucent form of one of ITS OWN
// palette constants (same RGB, different alpha), that relationship is
// re-expressed against ours -- the `status_panel` rule from Task 4.
//
// The Command/CommandActive background fills go further: noid's literals
// there are not alpha variants of CYAN, they are CYAN darkened by an amount
// with no single exact value across all three channels (noid's own literals
// are not perfectly proportional to noid's own CYAN either). Each is
// reproduced as `shade(CYAN, amount)`, where `amount` is the mean over r/g/b
// of `1 - noid_literal_channel / noid_CYAN_channel` (noid CYAN = (103, 215,
// 246)), rounded to two decimals, applied to THIS crate's CYAN. The
// per-arm comments below give the source ratios and the reconstruction
// residual against noid's own numbers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Command,
    CommandActive,
    /// Cyan outline: look/next actions (RECEIVE, REVIEW).
    Outline,
    /// Orange outline: actions to be careful with (RESTART NODE, REVEAL SEED).
    /// noid: `consolidation_button`.
    Caution,
}

fn primary_button_shadow(status: button_widget::Status) -> Shadow {
    if matches!(status, button_widget::Status::Pressed) {
        Shadow {
            color: Color::from_rgba8(1, 2, 6, 0.44),
            offset: Vector::new(0.0, 1.5),
            blur_radius: 3.0,
        }
    } else {
        Shadow {
            color: Color::from_rgba8(1, 2, 6, 0.58),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 8.0,
        }
    }
}

pub fn colored_primary(color: Color, status: button_widget::Status) -> button_widget::Style {
    if matches!(status, button_widget::Status::Disabled) {
        return disabled_button_style();
    }

    let hovered = matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    );
    let pressed = matches!(status, button_widget::Status::Pressed);
    let base = if pressed {
        shade(color, 0.16)
    } else if hovered {
        tint(color, 0.10)
    } else {
        color
    };

    button_widget::Style {
        background: Some(raised_gradient(base)),
        text_color: INK,
        border: Border {
            color: Color {
                a: 0.38,
                ..tint(color, 0.68)
            },
            width: if pressed { 1.0 } else { 1.15 },
            radius: Radius::from(6.0),
        },
        shadow: primary_button_shadow(status),
        snap: true,
    }
}

fn raised_gradient(color: Color) -> Background {
    Background::Gradient(
        gradient::Linear::new(Radians(0.0))
            .add_stop(0.0, shade(color, 0.17))
            .add_stop(0.42, color)
            .add_stop(1.0, tint(color, 0.18))
            .into(),
    )
}

fn tint(color: Color, amount: f32) -> Color {
    Color {
        r: color.r + (1.0 - color.r) * amount,
        g: color.g + (1.0 - color.g) * amount,
        b: color.b + (1.0 - color.b) * amount,
        ..color
    }
}

fn shade(color: Color, amount: f32) -> Color {
    Color {
        r: color.r * (1.0 - amount),
        g: color.g * (1.0 - amount),
        b: color.b * (1.0 - amount),
        ..color
    }
}

fn disabled_button_style() -> button_widget::Style {
    button_widget::Style {
        background: Some(raised_gradient(Color::from_rgba8(38, 42, 55, 0.72))),
        text_color: DIM,
        border: Border {
            // noid: exact translucent form of its own LINE (214, 224, 255)
            // at alpha 0.09 versus LINE's own 0.14.
            color: Color { a: 0.09, ..LINE },
            width: 1.0,
            radius: Radius::from(6.0),
        },
        shadow: Shadow {
            color: Color::from_rgba8(1, 2, 6, 0.18),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 4.0,
        },
        snap: true,
    }
}

fn neutral_button_shadow(status: button_widget::Status, visible: bool) -> Shadow {
    let pressed = matches!(status, button_widget::Status::Pressed);
    if !visible && !pressed {
        return Shadow::default();
    }

    Shadow {
        color: Color::from_rgba8(1, 2, 6, if pressed { 0.30 } else { 0.40 }),
        offset: Vector::new(0.0, if pressed { 1.0 } else { 3.0 }),
        blur_radius: if pressed { 2.0 } else { 7.0 },
    }
}

pub fn button(kind: ButtonKind, status: button_widget::Status) -> button_widget::Style {
    if matches!(status, button_widget::Status::Disabled) {
        return disabled_button_style();
    }

    let hovered = matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    );
    let pressed = matches!(status, button_widget::Status::Pressed);

    match kind {
        ButtonKind::Primary => colored_primary(ACCENT, status),
        ButtonKind::Secondary => {
            let base = if pressed {
                shade(SURFACE_ALT, 0.14)
            } else if hovered {
                tint(SURFACE_HIGH, 0.06)
            } else {
                SURFACE_ALT
            };
            button_widget::Style {
                background: Some(raised_gradient(base)),
                text_color: TEXT,
                border: Border {
                    color: if hovered {
                        Color {
                            a: 0.42,
                            ..LINE_STRONG
                        }
                    } else {
                        Color { a: 0.22, ..LINE }
                    },
                    width: 1.0,
                    radius: Radius::from(6.0),
                },
                shadow: neutral_button_shadow(status, true),
                snap: true,
            }
        }
        ButtonKind::Ghost => {
            let base = if pressed {
                Color::from_rgba8(44, 49, 65, 0.82)
            } else if hovered {
                Color::from_rgba8(55, 61, 80, 0.74)
            } else {
                Color::from_rgba8(34, 38, 51, 0.30)
            };
            button_widget::Style {
                background: Some(raised_gradient(base)),
                text_color: if hovered { TEXT } else { MUTED },
                border: Border {
                    color: if hovered {
                        // noid: exact translucent form of its own
                        // LINE_STRONG (224, 232, 255) at alpha 0.22 versus
                        // its own 0.25.
                        Color {
                            a: 0.22,
                            ..LINE_STRONG
                        }
                    } else {
                        // noid: exact translucent form of its own LINE
                        // (214, 224, 255) at alpha 0.07 versus its own 0.14.
                        Color { a: 0.07, ..LINE }
                    },
                    width: 1.0,
                    radius: Radius::from(5.0),
                },
                shadow: neutral_button_shadow(status, hovered),
                snap: true,
            }
        }
        ButtonKind::Command => {
            let base = if pressed {
                // noid: Color::from_rgba8(58, 116, 137, 0.34), a shade of
                // its own CYAN (103, 215, 246) -- per-channel amount
                // 0.437/0.460/0.443, mean 0.45. Reconstructing noid's own
                // CYAN at amount 0.45 gives (57, 118, 135), 1-2 units off
                // noid's literal.
                Color {
                    a: 0.34,
                    ..shade(CYAN, 0.45)
                }
            } else if hovered {
                // noid: Color::from_rgba8(46, 86, 108, 0.30), a shade of
                // its own CYAN -- per-channel amount 0.553/0.600/0.561,
                // mean 0.57.
                Color {
                    a: 0.30,
                    ..shade(CYAN, 0.57)
                }
            } else {
                // Kept verbatim: not a clean derivation of noid's CYAN --
                // per-channel amount spreads 0.742/0.872/0.863, far too
                // wide to be one shade. Also not an exact alpha-only match
                // to CHROME (34, 37, 50): g and b are each 1 unit off with
                // no consistent ratio. Independent literal.
                Color::from_rgba8(34, 38, 51, 0.18)
            };
            button_widget::Style {
                background: Some(raised_gradient(base)),
                text_color: TEXT,
                border: Border {
                    color: if hovered {
                        // noid: exact translucent form of its own CYAN
                        // (103, 215, 246) at alpha 0.22.
                        Color { a: 0.22, ..CYAN }
                    } else {
                        // noid: exact translucent form of its own LINE
                        // (214, 224, 255) at alpha 0.05 versus its own 0.14.
                        Color { a: 0.05, ..LINE }
                    },
                    width: 1.0,
                    radius: Radius::from(5.0),
                },
                shadow: neutral_button_shadow(status, hovered),
                snap: true,
            }
        }
        ButtonKind::CommandActive => {
            let base = if pressed {
                // noid: Color::from_rgba8(50, 132, 158, 0.46), a shade of
                // its own CYAN -- per-channel amount 0.515/0.386/0.358,
                // mean 0.42. The widest spread of the five (0.16 apart):
                // noid's own literal is not perfectly proportional to
                // noid's own CYAN either. Reconstructing noid's CYAN at
                // amount 0.42 gives (60, 125, 143) against noid's literal
                // (50, 132, 158): per-channel diffs R=10, G=7, B=15 -- max
                // 15, mean ~10.8. The worst fit of the five.
                Color {
                    a: 0.46,
                    ..shade(CYAN, 0.42)
                }
            } else if hovered {
                // noid: Color::from_rgba8(51, 119, 145, 0.42), a shade of
                // its own CYAN -- per-channel amount 0.505/0.447/0.411,
                // mean 0.45.
                Color {
                    a: 0.42,
                    ..shade(CYAN, 0.45)
                }
            } else {
                // noid: Color::from_rgba8(43, 91, 113, 0.38), a shade of
                // its own CYAN -- per-channel amount 0.583/0.577/0.541,
                // mean 0.57.
                Color {
                    a: 0.38,
                    ..shade(CYAN, 0.57)
                }
            };
            button_widget::Style {
                background: Some(raised_gradient(base)),
                text_color: CYAN,
                border: Border {
                    // noid: exact translucent form of its own CYAN
                    // (103, 215, 246) at alpha 0.38.
                    color: Color { a: 0.38, ..CYAN },
                    width: 1.0,
                    radius: Radius::from(5.0),
                },
                shadow: neutral_button_shadow(status, true),
                snap: true,
            }
        }
        ButtonKind::Outline => {
            let base = if pressed {
                Color { a: 0.20, ..CYAN }
            } else if hovered {
                Color { a: 0.12, ..CYAN }
            } else {
                Color::TRANSPARENT
            };
            button_widget::Style {
                background: Some(Background::Color(base)),
                text_color: CYAN,
                border: Border {
                    color: Color {
                        a: if hovered { 0.95 } else { 0.72 },
                        ..CYAN
                    },
                    width: 1.25,
                    radius: Radius::from(6.0),
                },
                shadow: Shadow::default(),
                snap: true,
            }
        }
        ButtonKind::Caution => {
            let base = if pressed {
                Color {
                    a: 0.36,
                    ..ADVISORY
                }
            } else if hovered {
                Color {
                    a: 0.29,
                    ..ADVISORY
                }
            } else {
                Color {
                    a: 0.20,
                    ..ADVISORY
                }
            };
            button_widget::Style {
                background: Some(raised_gradient(base)),
                text_color: ADVISORY,
                border: Border {
                    color: Color {
                        a: if hovered { 0.95 } else { 0.72 },
                        ..ADVISORY
                    },
                    width: if hovered { 1.5 } else { 1.25 },
                    radius: Radius::from(6.0),
                },
                shadow: neutral_button_shadow(status, true),
                snap: true,
            }
        }
    }
}

pub fn text_input(_: &Theme, status: input_widget::Status) -> input_widget::Style {
    let focused = matches!(status, input_widget::Status::Focused { .. });
    let disabled = matches!(status, input_widget::Status::Disabled);
    input_widget::Style {
        background: Background::Color(if disabled { INK } else { SURFACE }),
        border: Border {
            // The focus ring is the accent, not a brighter grey: this style
            // now paints every passphrase field in the wallet (unlock, new,
            // and confirm, all on setup), and a field that does not visibly
            // hold the keyboard is a passphrase typed into the wrong box.
            color: if focused { ACCENT } else { LINE },
            width: if focused { 1.5 } else { 1.0 },
            radius: Radius::from(6.0),
        },
        icon: MUTED,
        placeholder: if disabled { FAINT } else { DIM },
        value: if disabled { DIM } else { VALUE },
        selection: Color { a: 0.35, ..ACCENT },
    }
}

/// A field that is read from, never typed into. It is a `text_input` so the
/// characters can be selected and copied, and it must not look like one, or
/// it invites an edit that cannot happen.
///
/// No caller since the receive screen shows its address as wrapped text
/// (sub-project F, I5): the input scrolled the tail of the address out of a
/// half-width panel. Kept, with its test, for the next read-only field.
#[allow(dead_code)]
pub fn selectable_address(_: &Theme, _: input_widget::Status) -> input_widget::Style {
    input_widget::Style {
        background: Background::Color(SURFACE_ALT),
        border: Border {
            color: HAIRLINE,
            width: 1.0,
            radius: Radius::from(6.0),
        },
        icon: MUTED,
        placeholder: FAINT,
        value: VALUE,
        selection: Color { a: 0.35, ..ACCENT },
    }
}

pub fn scrollable(theme: &Theme, status: scrollable_widget::Status) -> scrollable_widget::Style {
    let mut style = scrollable_widget::default(theme, status);
    let active = matches!(
        status,
        scrollable_widget::Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        } | scrollable_widget::Status::Dragged {
            is_vertical_scrollbar_dragged: true,
            ..
        }
    );

    style.vertical_rail.background = Some(Background::Color(Color {
        // noid: exact translucent form of its own CYAN (103, 215, 246) at
        // alpha 0.10 active / 0.05 idle, re-expressed against this crate's
        // own CYAN.
        a: if active { 0.10 } else { 0.05 },
        ..CYAN
    }));
    style.vertical_rail.border = Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: Radius::from(99.0),
    };
    style.vertical_rail.scroller.background = Background::Color(Color {
        // noid: already symbolic against its own CYAN, ported unchanged.
        a: if active { 0.92 } else { 0.48 },
        ..CYAN
    });
    style.vertical_rail.scroller.border = Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: Radius::from(99.0),
    };
    style
}

// --- Sub-project F parts, ported from noid_gui/src/theme.rs (Apache-2.0,
// Copyright (C) 2026 Paranoid Zero) ------------------------------------------

/// A section tag's filled label (`ACTIVE ADDRESS`, `HISTORY`). The shape
/// `title_bar_cyan`/`title_bar_accent` already had, for any colour.
pub fn tag(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| title_style(colour)
}

/// A data table's header row. noid: `utxo_table_header`.
pub fn table_head(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(MUTED),
        background: Some(Background::Color(SURFACE_ALT)),
        border: Border {
            color: Color { a: 0.50, ..CYAN },
            width: 1.0,
            radius: Radius::from(3.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// One data-table row, a button so a row can be selected. noid: `utxo_row`,
/// with the selection in LAVENDER (A mapped noid's PROOF slot to it).
/// `Disabled` (a row with no action) is deliberately drawn like `Active`.
pub fn table_row_button(
    alternate: bool,
    selected: bool,
    status: button_widget::Status,
) -> button_widget::Style {
    let hovered = matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    );
    let background = if selected {
        Color {
            a: if hovered { 0.24 } else { 0.16 },
            ..LAVENDER
        }
    } else if hovered {
        SURFACE_HIGH
    } else if alternate {
        SURFACE_ALT
    } else {
        SURFACE
    };
    button_widget::Style {
        background: Some(Background::Color(background)),
        text_color: TEXT,
        border: Border {
            color: if selected {
                LAVENDER
            } else {
                Color::TRANSPARENT
            },
            width: if selected { 1.0 } else { 0.0 },
            radius: Radius::from(2.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A function-key cap in the command bar. noid: `key_cap`.
pub fn key_cap(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(INK),
        background: Some(Background::Color(colour)),
        border: Border {
            color: Color::from_rgba8(228, 250, 255, 0.28),
            width: 1.0,
            radius: Radius::from(3.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The command bar's frame. noid: `command_bar`.
pub fn command_bar(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        background: Some(Background::Color(CHROME)),
        border: Border {
            color: LINE_STRONG,
            width: 1.0,
            radius: Radius::from(8.0),
        },
        shadow: soft_shadow(),
        snap: true,
    }
}

/// Log output: darker than any surface so it reads as a terminal.
pub fn terminal(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(MUTED),
        background: Some(Background::Color(Color::from_rgb8(5, 7, 13))),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(4.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A small outlined state badge (`RUNNING pid 4242`, `MINING`).
pub fn badge(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(colour),
        background: Some(Background::Color(Color { a: 0.08, ..colour })),
        border: Border {
            color: Color { a: 0.50, ..colour },
            width: 1.0,
            radius: Radius::from(3.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A big either/or card (node source, create/restore).
pub fn choice_card(selected: bool, status: button_widget::Status) -> button_widget::Style {
    let hovered = matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    );
    button_widget::Style {
        background: Some(Background::Color(if selected {
            Color { a: 0.08, ..ACCENT }
        } else if hovered {
            SURFACE_HIGH
        } else {
            SURFACE_ALT
        })),
        text_color: TEXT,
        border: Border {
            color: if selected {
                ACCENT
            } else if hovered {
                LINE_STRONG
            } else {
                LINE
            },
            width: 1.0,
            radius: Radius::from(6.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// A drop-down (F5's payout address): an input's frame, the accent ring
/// while it is open, so it reads as a field that holds a choice.
pub fn pick_list(
    _: &Theme,
    status: iced::widget::pick_list::Status,
) -> iced::widget::pick_list::Style {
    use iced::widget::pick_list::Status;
    let open = matches!(status, Status::Opened { .. });
    let hovered = matches!(
        status,
        Status::Hovered | Status::Opened { is_hovered: true }
    );
    iced::widget::pick_list::Style {
        text_color: VALUE,
        placeholder_color: DIM,
        handle_color: if open { ACCENT } else { MUTED },
        background: Background::Color(if hovered { SURFACE_HIGH } else { SURFACE }),
        border: Border {
            color: if open {
                ACCENT
            } else if hovered {
                LINE_STRONG
            } else {
                LINE
            },
            width: if open { 1.5 } else { 1.0 },
            radius: Radius::from(6.0),
        },
    }
}

/// The list a drop-down opens. Opaque, unlike the panel surfaces: it lies
/// over other widgets, and a translucent list let the cards beneath show
/// through its rows.
pub fn pick_list_menu(_: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: Background::Color(Color {
            a: 1.0,
            ..SURFACE_HIGH
        }),
        border: Border {
            color: LINE_STRONG,
            width: 1.0,
            radius: Radius::from(6.0),
        },
        text_color: TEXT,
        selected_text_color: INK,
        selected_background: Background::Color(ACCENT),
        shadow: Shadow::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chromatic half of the palette is COPIED from the node's terminal UI
    /// (`src/a9/ui.rs`), because the GUI must not depend on the node crate.
    /// A copy drifts silently; this test is what makes a change to either side
    /// a change a person has to make on purpose in both places.
    ///
    /// It is not a synchronisation mechanism. It cannot see `ui.rs`. It pins
    /// what was copied so the copy cannot be edited by accident.
    #[test]
    fn the_palette_matches_what_was_copied_from_the_node() {
        // src/a9/ui.rs:38 UI_GREEN
        assert_eq!(ACCENT, Color::from_rgb8(59, 242, 173));
        // src/a9/ui.rs:36 UI_CYAN
        assert_eq!(CYAN, Color::from_rgb8(40, 204, 217));
        // src/a9/ui.rs:41 UI_LAVENDER
        assert_eq!(LAVENDER, Color::from_rgb8(167, 165, 198));
        // src/a9/ui.rs:39 UI_ORANGE
        assert_eq!(ADVISORY, Color::from_rgb8(237, 124, 51));
        // src/a9/ui.rs:40 UI_PINK
        assert_eq!(DANGER, Color::from_rgb8(247, 111, 142));
        // src/a9/ui.rs:28 UI_LABEL
        assert_eq!(TEXT, Color::from_rgb8(230, 230, 230));
        // src/a9/ui.rs:45 UI_VALUE
        assert_eq!(VALUE, Color::from_rgb8(255, 255, 255));
        // src/a9/ui.rs:35 UI_MUTED
        assert_eq!(MUTED, Color::from_rgb8(170, 170, 170));
        // src/a9/ui.rs:29 UI_DIM
        assert_eq!(DIM, Color::from_rgb8(128, 128, 128));
        // src/a9/ui.rs:48 UI_FAINT
        assert_eq!(FAINT, Color::from_rgb8(90, 97, 105));
        // src/a9/ui.rs:50 UI_HAIRLINE
        assert_eq!(HAIRLINE, Color::from_rgb8(58, 64, 72));
    }

    /// WARNING is the one colour NOT taken from the node: alphanumeric's
    /// palette has no yellow. Folding it into ADVISORY would paint "you can
    /// fix this and resend" the same colour as "this may already be on the
    /// chain", which is the distinction this branch exists to keep.
    #[test]
    fn warning_is_its_own_colour_and_not_advisory() {
        assert_eq!(WARNING, Color::from_rgb8(231, 218, 61));
        assert_ne!(WARNING, ADVISORY);
        assert_ne!(WARNING, DANGER);
    }

    /// The four text weights must be distinguishable, or the wallet screen's
    /// balance reads as a caption.
    #[test]
    fn the_text_hierarchy_has_four_distinct_steps() {
        let steps = [VALUE, TEXT, MUTED, DIM];
        for (index, first) in steps.iter().enumerate() {
            for second in &steps[index + 1..] {
                assert_ne!(first, second);
            }
        }
    }

    /// The three surface steps exist so nesting reads as depth. If two of them
    /// resolve to the same fill, a card inside a panel becomes invisible.
    #[test]
    fn the_surface_steps_are_three_distinct_fills() {
        let theme = alphanumeric_theme();
        let fills = [
            surface(&theme).background,
            surface_alt(&theme).background,
            status_panel(&theme).background,
        ];
        for (index, first) in fills.iter().enumerate() {
            for second in &fills[index + 1..] {
                assert_ne!(
                    format!("{first:?}"),
                    format!("{second:?}"),
                    "two surface steps render identically"
                );
            }
        }
    }

    /// Table rows alternate so a long address list stays readable. A version
    /// that ignored its argument would pass every other test here. (Moved
    /// from the container style `table_row`, deleted with no callers once
    /// every table row became a `table_row_button`.)
    #[test]
    fn table_rows_alternate() {
        use iced::widget::button::Status;
        assert_ne!(
            format!(
                "{:?}",
                table_row_button(false, false, Status::Active).background
            ),
            format!(
                "{:?}",
                table_row_button(true, false, Status::Active).background
            ),
        );
    }

    /// The shadow is deliberately NOT part of this comparison. It is computed
    /// from `status` directly (`primary_button_shadow`,
    /// `neutral_button_shadow`), so it differs between states no matter what
    /// the arm's own colour logic does -- including when that logic has been
    /// deleted. A distinctness test the shadow can satisfy on its own proves
    /// nothing about the thing it exists to check.
    fn appearance(style: &button_widget::Style) -> String {
        format!(
            "{:?}|{:?}|{:?}",
            style.background, style.text_color, style.border
        )
    }

    /// Every kind must look different in every state, and disabled must never
    /// look like active. A button that reads as pressable when it is not is
    /// how someone clicks Send twice.
    #[test]
    fn each_button_kind_is_distinct_in_each_state() {
        use iced::widget::button::Status;
        let kinds = [
            ButtonKind::Primary,
            ButtonKind::Secondary,
            ButtonKind::Ghost,
            ButtonKind::Command,
            ButtonKind::CommandActive,
            ButtonKind::Outline,
            ButtonKind::Caution,
        ];
        for kind in kinds {
            let active = appearance(&button(kind, Status::Active));
            let hovered = appearance(&button(kind, Status::Hovered));
            let pressed = appearance(&button(kind, Status::Pressed));
            let disabled = appearance(&button(kind, Status::Disabled));
            assert_ne!(active, hovered, "{kind:?}: hover is invisible");
            assert_ne!(hovered, pressed, "{kind:?}: press is invisible");
            assert_ne!(active, disabled, "{kind:?}: disabled reads as pressable");
        }
    }

    /// The affirmative button carries the theme's accent. If this drifts, the
    /// primary action on every screen stops agreeing with the theme's
    /// `primary` and `success`.
    #[test]
    fn the_primary_button_is_the_accent() {
        use iced::widget::button::Status;
        assert_eq!(
            format!("{:?}", button(ButtonKind::Primary, Status::Active)),
            format!("{:?}", colored_primary(ACCENT, Status::Active)),
        );
    }

    /// A focused field must be visibly focused. Two of the four states looking
    /// the same is how someone types a passphrase into an unfocused box.
    #[test]
    fn a_focused_input_does_not_look_like_an_idle_one() {
        use iced::widget::text_input::Status;
        let theme = alphanumeric_theme();
        let active = format!("{:?}", text_input(&theme, Status::Active));
        let focused = format!(
            "{:?}",
            text_input(&theme, Status::Focused { is_hovered: false })
        );
        let disabled = format!("{:?}", text_input(&theme, Status::Disabled));
        assert_ne!(active, focused);
        assert_ne!(active, disabled);
    }

    /// The address field is read from, not typed into. It must not advertise
    /// itself as editable.
    #[test]
    fn a_selectable_address_is_not_dressed_as_an_editable_field() {
        use iced::widget::text_input::Status;
        let theme = alphanumeric_theme();
        assert_ne!(
            format!("{:?}", selectable_address(&theme, Status::Active)),
            format!("{:?}", text_input(&theme, Status::Active)),
        );
    }

    /// A table row with nothing to do on press is `Disabled` to iced. It must
    /// look like every other row, not like a greyed-out control -- the recent
    /// activity table is all such rows.
    #[test]
    fn a_table_row_without_an_action_is_not_drawn_disabled() {
        use iced::widget::button::Status;
        for (alternate, selected) in [(false, false), (true, false), (false, true)] {
            assert_eq!(
                table_row_button(alternate, selected, Status::Disabled).background,
                table_row_button(alternate, selected, Status::Active).background,
            );
        }
    }
}
