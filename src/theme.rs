//! Terminal palette shared by the dashboard.
//!
//! The colours are borrowed from AuraDeck's "Aura Cyber-Pastel" palette
//! (Catppuccin Mocha base) so the TUI and the HTML report read as one product.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
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

/// Colour for a health grade (`A`..`E`).
///
/// Semantics: `A` is calm mint, `B` sky blue, `C` amber, `D` peach and `E`
/// alarm pink. Any character outside `A`..`E` (including lowercase) is treated
/// as unknown and rendered in the muted grey so it recedes.
pub fn grade_color(_grade: char) -> Color {
    match _grade {
        'A' => AURA.ok,
        'B' => Color::Rgb(137, 220, 235),
        'C' => AURA.warn,
        'D' => Color::Rgb(250, 179, 135),
        'E' => AURA.danger,
        _ => AURA.muted,
    }
}

/// Colour for a 0..=1 risk score: calm below 0.35, warn to 0.6, danger above.
///
/// Semantics: thresholds are half-open, so `0.35` is already a warning and
/// `0.6` is already dangerous. A `NaN` score carries no information and falls
/// back to muted.
pub fn risk_color(_risk: f64) -> Color {
    if _risk.is_nan() {
        return AURA.muted;
    }
    if _risk < 0.35 {
        AURA.ok
    } else if _risk < 0.60 {
        AURA.warn
    } else {
        AURA.danger
    }
}

/// Ramp colour for CCN bin `index` out of `total` bins.
///
/// Semantics: the bin index is stretched proportionally across the six-entry
/// ramp (`index * 6 / total`), so the first bin is mauve and the last is coral
/// regardless of how many bins the histogram has. Out-of-range indices clamp to
/// the final ramp entry, and an empty histogram is muted.
pub fn bin_color(_index: usize, _total: usize) -> Color {
    if _total == 0 {
        return AURA.muted;
    }
    let slot = _index.saturating_mul(6) / _total;
    AURA.ramp[slot.min(5)]
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
        assert_eq!(grade_color('A'), AURA.ok);
        assert_eq!(grade_color('B'), Color::Rgb(137, 220, 235));
        assert_eq!(grade_color('C'), AURA.warn);
        assert_eq!(grade_color('D'), Color::Rgb(250, 179, 135));
        assert_eq!(grade_color('E'), AURA.danger);
        assert_eq!(grade_color('Z'), AURA.muted);
    }

    #[test]
    fn risk_thresholds_are_half_open() {
        assert_eq!(risk_color(0.0), AURA.ok);
        assert_eq!(risk_color(0.349), AURA.ok);
        assert_eq!(risk_color(0.35), AURA.warn);
        assert_eq!(risk_color(0.599), AURA.warn);
        assert_eq!(risk_color(0.6), AURA.danger);
        assert_eq!(risk_color(1.0), AURA.danger);
        assert_eq!(risk_color(f64::NAN), AURA.muted);
    }

    #[test]
    fn bin_color_maps_onto_the_ramp() {
        assert_eq!(bin_color(0, 6), AURA.ramp[0]);
        assert_eq!(bin_color(5, 6), AURA.ramp[5]);
        assert_eq!(bin_color(99, 6), AURA.ramp[5]);
        assert_eq!(bin_color(0, 0), AURA.muted);
    }

    #[test]
    fn ramp_is_not_a_placeholder() {
        assert!(AURA.ramp.iter().any(|c| *c != AURA.ramp[0]));
    }
}
