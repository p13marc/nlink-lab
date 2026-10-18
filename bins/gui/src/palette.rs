//! The canvas's colours, light or dark after the theme: the same roles the
//! SVG export uses (`nlink_lab_view::scene::Tone`, node kinds).

use iced::{Color, Theme};
use nlink_lab_view::NodeKind;
use nlink_lab_view::scene::Tone;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub background: Color,
    pub region_fill: Color,
    pub region_stroke: Color,
    pub text: Color,
    pub text_muted: Color,
    pub hover: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn of(theme: &Theme) -> Self {
        if theme.extended_palette().is_dark {
            Self {
                dark: true,
                background: rgb(0x1b1f27),
                region_fill: rgb(0x232934),
                region_stroke: rgb(0x3f4859),
                text: rgb(0xe5e9f0),
                text_muted: rgb(0x9aa3b2),
                hover: Color {
                    a: 0.35,
                    ..rgb(0x88c0d0)
                },
            }
        } else {
            Self {
                dark: false,
                background: rgb(0xfbfcfe),
                region_fill: rgb(0xf3f6fa),
                region_stroke: rgb(0xc9d1de),
                text: rgb(0x20262f),
                text_muted: rgb(0x6b7586),
                hover: Color {
                    a: 0.30,
                    ..rgb(0x2e6bd3)
                },
            }
        }
    }

    pub fn tone(&self, tone: Tone) -> Color {
        match (tone, self.dark) {
            (Tone::Normal, false) => rgb(0x5c6677),
            (Tone::Normal, true) => rgb(0x8b95a7),
            (Tone::Muted, false) => rgb(0xa9b1bf),
            (Tone::Muted, true) => rgb(0x566072),
            (Tone::Accent, false) => rgb(0x2e6bd3),
            (Tone::Accent, true) => rgb(0x6aa2ff),
            (Tone::Good, false) => rgb(0x2f9e5b),
            (Tone::Good, true) => rgb(0x5cc98a),
            (Tone::Warn, false) => rgb(0xc27c0e),
            (Tone::Warn, true) => rgb(0xe8b04b),
            (Tone::Bad, false) => rgb(0xc93a3a),
            (Tone::Bad, true) => rgb(0xf07070),
        }
    }

    pub fn fill(&self, kind: NodeKind) -> Color {
        match (kind, self.dark) {
            (NodeKind::Host, false) => rgb(0xeef1f6),
            (NodeKind::Host, true) => rgb(0x2b313d),
            (NodeKind::Router, false) => rgb(0xdfe9fb),
            (NodeKind::Router, true) => rgb(0x233554),
            (NodeKind::Container, false) => rgb(0xe8f4ec),
            (NodeKind::Container, true) => rgb(0x223a2d),
            (NodeKind::Bridge, false) => rgb(0xfdf3dc),
            (NodeKind::Bridge, true) => rgb(0x43391f),
            (NodeKind::External, false) => rgb(0xf1e8f7),
            (NodeKind::External, true) => rgb(0x3a2b45),
        }
    }
}
