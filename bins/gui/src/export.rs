//! Exports of a topology: the drawing as SVG or PNG (the graph alone, not
//! the window), or its declaration as Graphviz DOT or Mermaid.

use nlink_lab::Topology;
use nlink_lab_view::Scene;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Svg,
    Png,
    Dot,
    Mermaid,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Svg, Format::Png, Format::Dot, Format::Mermaid];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Svg => "svg",
            Format::Png => "png",
            Format::Dot => "dot",
            Format::Mermaid => "mmd",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Svg => "SVG",
            Format::Png => "PNG",
            Format::Dot => "DOT",
            Format::Mermaid => "Mermaid",
        }
    }
}

/// The bytes of `format`. The drawing is the scene as laid out, without
/// the selection.
pub fn render(format: Format, scene: &Scene, topology: &Topology) -> Result<Vec<u8>, String> {
    Ok(match format {
        Format::Svg => nlink_lab_view::svg::to_svg(scene).into_bytes(),
        Format::Png => png(&nlink_lab_view::svg::to_svg(scene))?,
        Format::Dot => nlink_lab_view::diagram::topology_to_dot(topology).into_bytes(),
        Format::Mermaid => nlink_lab_view::diagram::topology_to_mermaid(topology).into_bytes(),
    })
}

/// Twice the SVG's size, for a sharp image.
const PNG_SCALE: f32 = 2.0;

fn png(svg: &str) -> Result<Vec<u8>, String> {
    rasterize(svg)?.encode_png().map_err(|e| e.to_string())
}

/// The fonts an export's text is drawn with: the system's, `sans-serif`
/// being one the system has (fontdb takes it for Arial, which a Linux
/// desktop rarely does, and drops text it finds no font for).
fn fonts() -> resvg::usvg::Options<'static> {
    let mut options = resvg::usvg::Options::default();
    let db = options.fontdb_mut();
    db.load_system_fonts();
    let families: Vec<String> = db
        .faces()
        .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
        .collect();
    let preferred = [
        "Noto Sans",
        "DejaVu Sans",
        "Liberation Sans",
        "Cantarell",
        "Fira Sans",
        "Arial",
        "Helvetica",
    ];
    let family = preferred
        .iter()
        .find(|p| families.iter().any(|f| f == *p))
        .map(|p| p.to_string())
        .or_else(|| families.first().cloned());
    if let Some(family) = family {
        db.set_sans_serif_family(family);
    }
    options
}

fn rasterize(svg: &str) -> Result<resvg::tiny_skia::Pixmap, String> {
    let options = fonts();
    let tree = resvg::usvg::Tree::from_str(svg, &options).map_err(|e| e.to_string())?;
    let size = tree
        .size()
        .to_int_size()
        .scale_by(PNG_SCALE)
        .ok_or("empty drawing")?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or("the drawing is too large")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(PNG_SCALE, PNG_SCALE),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab_view::graph::GraphOptions;
    use nlink_lab_view::layout::{LayoutKind, layout};
    use nlink_lab_view::scene::{Overlays, build};
    use nlink_lab_view::{Positions, TopoGraph};

    #[test]
    fn every_format_renders() {
        let t = nlink_lab::parser::parse(
            "lab \"x\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay 5ms }\n",
        )
        .unwrap();
        let g = TopoGraph::from_topology(&t, &GraphOptions::default());
        let pos = layout(&g, LayoutKind::Layered, &Positions::new());
        let none = Default::default();
        let scene = build(
            &g,
            &pos,
            &Overlays {
                metrics: None,
                impairments: &none,
                selected: None,
                addresses: true,
                rates: false,
            },
        );
        let svg = String::from_utf8(render(Format::Svg, &scene, &t).unwrap()).unwrap();
        assert!(svg.starts_with("<svg") && svg.contains(">a<"), "{svg}");
        let png = render(Format::Png, &scene, &t).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        if let Some(dir) = std::env::var_os("NLINK_LAB_GUI_SNAPSHOTS") {
            std::fs::write(std::path::Path::new(&dir).join("export.png"), &png).unwrap();
        }
        let dot = String::from_utf8(render(Format::Dot, &scene, &t).unwrap()).unwrap();
        assert!(dot.starts_with("graph \"x\""), "{dot}");
        let mmd = String::from_utf8(render(Format::Mermaid, &scene, &t).unwrap()).unwrap();
        assert!(mmd.contains("graph LR"), "{mmd}");
    }

    #[test]
    fn png_text_is_drawn() {
        if fonts().fontdb.is_empty() {
            eprintln!("no system fonts: nothing to draw text with");
            return;
        }
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="20"><text x="2" y="15" font-family="sans-serif" font-size="12">r1 eth0</text></svg>"#;
        let pixmap = rasterize(svg).unwrap();
        assert!(
            pixmap.pixels().iter().any(|p| p.alpha() > 0),
            "the text was dropped"
        );
    }
}
