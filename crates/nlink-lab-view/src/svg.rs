//! A scene as SVG: what the desktop app exports, light on white.

use std::fmt::Write as _;

use crate::graph::NodeKind;
use crate::scene::{Scene, Tone};

/// Margin around the scene.
const MARGIN: f32 = 40.0;

fn colour(tone: Tone) -> &'static str {
    match tone {
        Tone::Normal => "#3b4252",
        Tone::Muted => "#9aa3b2",
        Tone::Accent => "#2e6bd3",
        Tone::Good => "#2f9e5b",
        Tone::Warn => "#d08a17",
        Tone::Bad => "#c93a3a",
    }
}

fn fill(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Host => "#eef1f6",
        NodeKind::Router => "#dfe9fb",
        NodeKind::Container => "#e8f4ec",
        NodeKind::Bridge => "#fdf3dc",
        NodeKind::External => "#f1e8f7",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `scene` as an SVG document.
pub fn to_svg(scene: &Scene) -> String {
    let Some(b) = scene.bounds else {
        return "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"0\" height=\"0\"/>\n".into();
    };
    let (x, y, w, h) = (
        b.x - MARGIN,
        b.y - MARGIN,
        b.w + 2.0 * MARGIN,
        b.h + 2.0 * MARGIN,
    );
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x:.1} {y:.1} {w:.1} {h:.1}" width="{w:.0}" height="{h:.0}" font-family="sans-serif">"#
    );
    let _ = writeln!(
        out,
        r##"<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" fill="#ffffff"/>"##
    );
    for r in &scene.regions {
        let _ = writeln!(
            out,
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="14" fill="#f6f8fb" stroke="#c9d1de" stroke-dasharray="6 4"/>"##,
            r.rect.x, r.rect.y, r.rect.w, r.rect.h
        );
        let _ = writeln!(
            out,
            r##"<text x="{:.1}" y="{:.1}" font-size="12" fill="#6b7586">{}</text>"##,
            r.rect.x + 10.0,
            r.rect.y + 16.0,
            esc(&r.label)
        );
    }
    for e in &scene.edges {
        let d = match e.control {
            Some(c) => format!(
                "M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1}",
                e.from.x, e.from.y, c.x, c.y, e.to.x, e.to.y
            ),
            None => format!(
                "M{:.1} {:.1} L{:.1} {:.1}",
                e.from.x, e.from.y, e.to.x, e.to.y
            ),
        };
        let dash = if e.partitioned {
            r#" stroke-dasharray="2 6""#
        } else if e.dashed {
            r#" stroke-dasharray="7 5""#
        } else {
            ""
        };
        let _ = writeln!(
            out,
            r#"<path d="{d}" fill="none" stroke="{}" stroke-width="{:.1}"{dash}/>"#,
            colour(e.tone),
            e.width
        );
        for end in &e.ends {
            if let Some(label) = &end.label {
                let _ = writeln!(
                    out,
                    r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="#5c6677" text-anchor="middle">{}</text>"##,
                    end.at.x,
                    end.at.y - 4.0,
                    esc(label)
                );
            }
            if let Some(badge) = &end.badge {
                let _ = writeln!(
                    out,
                    r#"<text x="{:.1}" y="{:.1}" font-size="10" fill="{}" text-anchor="middle">{}</text>"#,
                    end.at.x,
                    end.at.y + 9.0,
                    colour(badge.tone),
                    esc(&badge.text)
                );
            }
        }
        if let Some(rate) = &e.rate {
            let mid = crate::scene::point_at(e, 0.5);
            let _ = writeln!(
                out,
                r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="#4c566a" text-anchor="middle">{}</text>"##,
                mid.x,
                mid.y - 6.0,
                esc(rate)
            );
        }
    }
    for n in &scene.nodes {
        let r = n.rect;
        let stroke = if n.selected {
            colour(Tone::Accent)
        } else {
            colour(n.tone)
        };
        let width = if n.selected { 2.5 } else { 1.2 };
        match n.kind {
            NodeKind::Bridge => {
                let c = r.center();
                let _ = writeln!(
                    out,
                    r#"<ellipse cx="{:.1}" cy="{:.1}" rx="{:.1}" ry="{:.1}" fill="{}" stroke="{stroke}" stroke-width="{width}"/>"#,
                    c.x,
                    c.y,
                    r.w / 2.0,
                    r.h / 2.0,
                    fill(n.kind)
                );
            }
            kind => {
                let rx = if kind == NodeKind::Router { 12 } else { 5 };
                let dash = if kind == NodeKind::Container {
                    r#" stroke-dasharray="5 3""#
                } else {
                    ""
                };
                let _ = writeln!(
                    out,
                    r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="{rx}" fill="{}" stroke="{stroke}" stroke-width="{width}"{dash}/>"#,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    fill(kind)
                );
            }
        }
        let c = r.center();
        let (label_y, sub_y) = if n.sublabel.is_some() {
            (c.y - 2.0, c.y + 12.0)
        } else {
            (c.y + 4.0, 0.0)
        };
        let _ = writeln!(
            out,
            r##"<text x="{:.1}" y="{label_y:.1}" font-size="13" fill="#20262f" text-anchor="middle">{}</text>"##,
            c.x,
            esc(&n.label)
        );
        if let Some(sub) = &n.sublabel {
            let _ = writeln!(
                out,
                r##"<text x="{:.1}" y="{sub_y:.1}" font-size="9" fill="#6b7586" text-anchor="middle">{}</text>"##,
                c.x,
                esc(sub)
            );
        }
    }
    out.push_str("</svg>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphOptions, TopoGraph, sample};
    use crate::layout::{LayoutKind, Positions, layout};
    use crate::scene::{Overlays, build};

    #[test]
    fn a_scene_is_a_well_formed_svg() {
        let g = TopoGraph::from_topology(&sample(), &GraphOptions::default());
        let pos = layout(&g, LayoutKind::Layered, &Positions::new());
        let imps = Default::default();
        let s = build(
            &g,
            &pos,
            &Overlays {
                metrics: None,
                impairments: &imps,
                selected: None,
                addresses: false,
                rates: false,
            },
        );
        let svg = to_svg(&s);
        assert!(svg.starts_with("<svg "));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<ellipse").count(), 1, "the network");
        assert_eq!(svg.matches("<path").count(), g.edges.len());
        assert!(svg.contains(">r1</text>"));
        assert!(svg.contains(r#"stroke-dasharray="5 3""#), "the container");
        assert_eq!(esc("a<b&\"c\""), "a&lt;b&amp;&quot;c&quot;");
    }
}
