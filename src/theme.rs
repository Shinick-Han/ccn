//! Terminal palette shared by the dashboard.
//!
//! The colours are borrowed from AuraDeck's "Aura Cyber-Pastel" palette
//! (Catppuccin Mocha base) so the TUI and the HTML report read as one product.

use std::ffi::OsStr;
use std::sync::OnceLock;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub canvas: Color,
    pub card: Color,
    pub pill: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub accent_alt: Color,
    pub text: Color,
    pub muted: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    /// CCN bins: 1, 2-4, 5-10, 11-20, 21-50, >=51.
    pub ramp: [Color; 6],
}

pub const AURA: Palette = Palette {
    canvas: Color::Rgb(18, 18, 28),
    card: Color::Rgb(26, 27, 42),
    pill: Color::Rgb(45, 47, 70),
    border: Color::Rgb(62, 66, 94),
    border_focus: Color::Rgb(203, 166, 247),
    accent: Color::Rgb(166, 227, 161),
    accent_alt: Color::Rgb(245, 194, 231),
    text: Color::Rgb(205, 214, 244),
    muted: Color::Rgb(116, 124, 156),
    ok: Color::Rgb(166, 227, 161),
    warn: Color::Rgb(249, 226, 175),
    danger: Color::Rgb(243, 139, 168),
    ramp: [
        Color::Rgb(203, 166, 247),
        Color::Rgb(180, 190, 254),
        Color::Rgb(137, 220, 235),
        Color::Rgb(148, 226, 213),
        Color::Rgb(249, 226, 175),
        Color::Rgb(243, 139, 168),
    ],
};

/// Monochrome replacement used when colour is disabled: every surface, text and
/// semantic token maps to the terminal's default foreground/background.
pub const MONO: Palette = Palette {
    canvas: Color::Reset,
    card: Color::Reset,
    pill: Color::Reset,
    border: Color::Reset,
    border_focus: Color::Reset,
    accent: Color::Reset,
    accent_alt: Color::Reset,
    text: Color::Reset,
    muted: Color::Reset,
    ok: Color::Reset,
    warn: Color::Reset,
    danger: Color::Reset,
    ramp: [Color::Reset; 6],
};

/// Pick the palette for an explicit colour decision.
///
/// `palette_from(false)` is [`AURA`] exactly; `palette_from(true)` is
/// [`MONO`]. This is the pure half of [`palette`] and is what tests should use
/// so they never depend on the ambient environment.
pub fn palette_from(no_color: bool) -> Palette {
    if no_color {
        MONO
    } else {
        AURA
    }
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

/// The process palette, resolved once from the environment.
///
/// Colour is disabled when `NO_COLOR` is present and non-empty: that is the
/// de-facto standard (<https://no-color.org>), so `NO_COLOR=` (empty) still
/// means colour is allowed. `main` sets the variable before any rendering;
/// the first call here caches the decision for the rest of the process.
pub fn palette() -> Palette {
    *PALETTE
        .get_or_init(|| palette_from(no_color_requested(std::env::var_os("NO_COLOR").as_deref())))
}

/// Whether a `NO_COLOR` value disables colour: present and non-empty.
fn no_color_requested(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Whether this palette is the monochrome fallback used for `NO_COLOR`.
pub fn is_monochrome(palette: &Palette) -> bool {
    palette.canvas == Color::Reset && palette.text == Color::Reset
}

/// The monochrome palette has no colour to distinguish grades, so a grade
/// lookup returns the terminal default; this also keeps the hard-coded
/// grade-B/D hues from leaking into monochrome output.
///
/// Semantics: `A` is calm mint, `B` sky blue, `C` amber, `D` peach and `E`
/// alarm pink. Any character outside `A`..`E` (including lowercase) is treated
/// as unknown and rendered in the muted grey so it recedes.
fn grade_color_in(palette: Palette, grade: char) -> Color {
    if is_monochrome(&palette) {
        return Color::Reset;
    }
    match grade {
        'A' => palette.ok,
        'B' => Color::Rgb(137, 220, 235),
        'C' => palette.warn,
        'D' => Color::Rgb(250, 179, 135),
        'E' => palette.danger,
        _ => palette.muted,
    }
}

/// Colour for a health grade (`A`..`E`), honouring `NO_COLOR`.
pub fn grade_color(grade: char) -> Color {
    grade_color_in(palette(), grade)
}

/// Colour for a 0..=1 risk score from an explicit palette.
///
/// Semantics: thresholds are half-open, so `0.35` is already a warning and
/// `0.6` is already dangerous. A `NaN` score carries no information and falls
/// back to muted.
fn risk_color_in(palette: Palette, risk: f64) -> Color {
    if risk.is_nan() {
        return palette.muted;
    }
    if risk < 0.35 {
        palette.ok
    } else if risk < 0.60 {
        palette.warn
    } else {
        palette.danger
    }
}

/// Colour for a 0..=1 risk score, honouring `NO_COLOR`.
pub fn risk_color(risk: f64) -> Color {
    risk_color_in(palette(), risk)
}

/// Ramp colour for CCN bin `index` out of `total` bins, from an explicit
/// palette.
///
/// Semantics: the bin index is stretched proportionally across the six-entry
/// ramp (`index * 6 / total`), so the first bin is mauve and the last is coral
/// regardless of how many bins the histogram has. Out-of-range indices clamp to
/// the final ramp entry, and an empty histogram is muted.
fn bin_color_in(palette: Palette, index: usize, total: usize) -> Color {
    if total == 0 {
        return palette.muted;
    }
    let slot = index.saturating_mul(6) / total;
    palette.ramp[slot.min(5)]
}

/// Ramp colour for CCN bin `index` out of `total` bins, honouring `NO_COLOR`.
pub fn bin_color(index: usize, total: usize) -> Color {
    bin_color_in(palette(), index, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aura_base_surfaces_are_aura_deck_values() {
        assert_eq!(AURA.canvas, Color::Rgb(18, 18, 28));
        assert_eq!(AURA.card, Color::Rgb(26, 27, 42));
    }

    #[test]
    fn grade_letters_map_to_documented_colors() {
        assert_eq!(grade_color_in(AURA, 'A'), AURA.ok);
        assert_eq!(grade_color_in(AURA, 'B'), Color::Rgb(137, 220, 235));
        assert_eq!(grade_color_in(AURA, 'C'), AURA.warn);
        assert_eq!(grade_color_in(AURA, 'D'), Color::Rgb(250, 179, 135));
        assert_eq!(grade_color_in(AURA, 'E'), AURA.danger);
        assert_eq!(grade_color_in(AURA, 'Z'), AURA.muted);
    }

    #[test]
    fn risk_thresholds_are_half_open() {
        assert_eq!(risk_color_in(AURA, 0.0), AURA.ok);
        assert_eq!(risk_color_in(AURA, 0.349), AURA.ok);
        assert_eq!(risk_color_in(AURA, 0.35), AURA.warn);
        assert_eq!(risk_color_in(AURA, 0.599), AURA.warn);
        assert_eq!(risk_color_in(AURA, 0.6), AURA.danger);
        assert_eq!(risk_color_in(AURA, 1.0), AURA.danger);
        assert_eq!(risk_color_in(AURA, f64::NAN), AURA.muted);
    }

    #[test]
    fn bin_color_maps_onto_the_ramp() {
        assert_eq!(bin_color_in(AURA, 0, 6), AURA.ramp[0]);
        assert_eq!(bin_color_in(AURA, 5, 6), AURA.ramp[5]);
        assert_eq!(bin_color_in(AURA, 99, 6), AURA.ramp[5]);
        assert_eq!(bin_color_in(AURA, 0, 0), AURA.muted);
    }

    #[test]
    fn ramp_is_not_a_placeholder() {
        assert!(AURA.ramp.iter().any(|c| *c != AURA.ramp[0]));
    }

    #[test]
    fn palette_from_false_is_exactly_aura() {
        assert_eq!(palette_from(false), AURA);
    }

    #[test]
    fn palette_from_true_is_monochrome_everywhere() {
        let mono = palette_from(true);
        assert!(is_monochrome(&mono));
        assert!(!is_monochrome(&AURA));
        assert_eq!(
            [
                mono.canvas,
                mono.card,
                mono.pill,
                mono.border,
                mono.border_focus,
                mono.accent,
                mono.accent_alt,
                mono.text,
                mono.muted,
                mono.ok,
                mono.warn,
                mono.danger,
            ],
            [Color::Reset; 12]
        );
        assert_eq!(mono.ramp, [Color::Reset; 6]);
    }

    #[test]
    fn no_color_is_present_and_non_empty() {
        assert!(!no_color_requested(None), "an unset variable keeps colour");
        assert!(
            !no_color_requested(Some(OsStr::new(""))),
            "an empty value keeps colour"
        );
        for value in ["1", "0", "anything"] {
            assert!(
                no_color_requested(Some(OsStr::new(value))),
                "NO_COLOR={value:?} must disable colour"
            );
        }
    }

    #[test]
    fn monochrome_semantic_colors_are_reset() {
        let mono = palette_from(true);
        assert_eq!(grade_color_in(mono, 'A'), Color::Reset);
        assert_eq!(grade_color_in(mono, 'B'), Color::Reset);
        assert_eq!(grade_color_in(mono, 'D'), Color::Reset);
        assert_eq!(grade_color_in(mono, 'Z'), Color::Reset);
        assert_eq!(risk_color_in(mono, 0.0), Color::Reset);
        assert_eq!(risk_color_in(mono, 0.5), Color::Reset);
        assert_eq!(risk_color_in(mono, 1.0), Color::Reset);
        assert_eq!(risk_color_in(mono, f64::NAN), Color::Reset);
        assert_eq!(bin_color_in(mono, 0, 6), Color::Reset);
        assert_eq!(bin_color_in(mono, 5, 6), Color::Reset);
        assert_eq!(bin_color_in(mono, 0, 0), Color::Reset);
    }
}
