//! SVG parsing and flattening for `import_svg_logo`.
//!
//! Splits into two layers so the geometry math is testable without any SVG
//! library involved:
//!   - `SvgSegment`/`flatten_subpaths`/`cubic_bezier_points` — pure path-flattening
//!     math (curves → straight-line polygon points). No dependency on `usvg`'s types.
//!   - `extract_polygons` — the thin `usvg` integration that walks a parsed SVG
//!     tree and adapts its path segments into `SvgSegment`s.
//!
//! KiCAD's polygon format (`PolyLine`/`gr_poly`) only supports straight points
//! and circular arcs, not cubic Bezier curves, so any curved SVG path must be
//! flattened into a point sequence before it can become filled artwork.

/// A 2D point in whatever coordinate space the caller is working in (SVG user
/// units until scaled, millimeters after).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    pub fn new(x: f64, y: f64) -> Self {
        Point2 { x, y }
    }
}

/// One command from an SVG path's `d` attribute, decoupled from any specific
/// SVG parsing library's types so the flattening logic below can be unit
/// tested with hand-built segment lists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SvgSegment {
    MoveTo(Point2),
    LineTo(Point2),
    QuadTo(Point2, Point2),
    CubicTo(Point2, Point2, Point2),
    Close,
}

/// Number of straight-line segments used to approximate one Bezier curve.
/// Fixed (not adaptive) subdivision — plenty for silkscreen/copper artwork,
/// where visual fidelity at fabrication scale matters more than curve-fitting
/// precision.
const BEZIER_SEGMENTS: usize = 16;

/// Sample a cubic Bezier curve into `BEZIER_SEGMENTS` points (excluding the
/// start point `p0`, which the caller already has as the current position).
pub fn cubic_bezier_points(p0: Point2, p1: Point2, p2: Point2, p3: Point2) -> Vec<Point2> {
    (1..=BEZIER_SEGMENTS)
        .map(|i| {
            let t = i as f64 / BEZIER_SEGMENTS as f64;
            let mt = 1.0 - t;
            let x = mt * mt * mt * p0.x
                + 3.0 * mt * mt * t * p1.x
                + 3.0 * mt * t * t * p2.x
                + t * t * t * p3.x;
            let y = mt * mt * mt * p0.y
                + 3.0 * mt * mt * t * p1.y
                + 3.0 * mt * t * t * p2.y
                + t * t * t * p3.y;
            Point2::new(x, y)
        })
        .collect()
}

/// Elevate a quadratic Bezier (single control point) to the two control
/// points of an equivalent cubic Bezier, so quadratic curves can reuse
/// `cubic_bezier_points`. Standard exact conversion: cubic control points
/// sit 2/3 of the way from each endpoint toward the quadratic control point.
pub fn quad_to_cubic(p0: Point2, control: Point2, p2: Point2) -> (Point2, Point2) {
    let c1 = Point2::new(
        p0.x + 2.0 / 3.0 * (control.x - p0.x),
        p0.y + 2.0 / 3.0 * (control.y - p0.y),
    );
    let c2 = Point2::new(
        p2.x + 2.0 / 3.0 * (control.x - p2.x),
        p2.y + 2.0 / 3.0 * (control.y - p2.y),
    );
    (c1, c2)
}

/// Flatten a sequence of SVG path segments into one or more closed polygon
/// outlines (point lists). A `MoveTo` starts a new subpath; a `Close` (or the
/// next `MoveTo`/end of input) ends the current one. Subpaths with fewer than
/// 2 points (degenerate — e.g. a lone `MoveTo`) are dropped.
pub fn flatten_subpaths(segments: impl IntoIterator<Item = SvgSegment>) -> Vec<Vec<Point2>> {
    let mut polygons = Vec::new();
    let mut current: Vec<Point2> = Vec::new();
    let mut last = Point2::new(0.0, 0.0);

    let close_current = |current: &mut Vec<Point2>, polygons: &mut Vec<Vec<Point2>>| {
        if current.len() > 1 {
            polygons.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };

    for seg in segments {
        match seg {
            SvgSegment::MoveTo(p) => {
                close_current(&mut current, &mut polygons);
                current.push(p);
                last = p;
            }
            SvgSegment::LineTo(p) => {
                current.push(p);
                last = p;
            }
            SvgSegment::QuadTo(control, p) => {
                let (c1, c2) = quad_to_cubic(last, control, p);
                current.extend(cubic_bezier_points(last, c1, c2, p));
                last = p;
            }
            SvgSegment::CubicTo(c1, c2, p) => {
                current.extend(cubic_bezier_points(last, c1, c2, p));
                last = p;
            }
            SvgSegment::Close => {
                close_current(&mut current, &mut polygons);
            }
        }
    }
    close_current(&mut current, &mut polygons);
    polygons
}

/// Scale and translate flattened SVG-unit polygons into board millimeters.
/// Scale is uniform (aspect-ratio preserving), derived from `target_width_mm`
/// against the SVG's native width; `offset_x`/`offset_y` place the top-left
/// of the scaled artwork on the board.
pub fn scale_and_place(
    polygons: &[Vec<Point2>],
    svg_width: f64,
    target_width_mm: f64,
    offset_x: f64,
    offset_y: f64,
) -> Vec<Vec<(f64, f64)>> {
    let scale = if svg_width > 0.0 {
        target_width_mm / svg_width
    } else {
        1.0
    };
    polygons
        .iter()
        .map(|poly| {
            poly.iter()
                .map(|p| (p.x * scale + offset_x, p.y * scale + offset_y))
                .collect()
        })
        .collect()
}

/// A parsed SVG, reduced to its native size and flattened fillable outlines.
pub struct SvgLogo {
    pub width: f64,
    pub height: f64,
    pub polygons: Vec<Vec<Point2>>,
}

/// Parse SVG content and flatten every path node's outline(s) into closed
/// polygons. Every `Path` node in the tree is treated as fillable artwork
/// (no stroke-only exclusion, no color/layer separation) — appropriate for a
/// single-color silkscreen/copper logo import.
pub fn extract_polygons(svg_content: &str) -> anyhow::Result<SvgLogo> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_content, &opt)
        .map_err(|e| anyhow::anyhow!("Failed to parse SVG: {e}"))?;

    let size = tree.size();
    let mut polygons = Vec::new();
    collect_path_polygons(tree.root(), &mut polygons);

    Ok(SvgLogo {
        width: size.width() as f64,
        height: size.height() as f64,
        polygons,
    })
}

fn collect_path_polygons(group: &usvg::Group, out: &mut Vec<Vec<Point2>>) {
    for node in group.children() {
        match node {
            usvg::Node::Path(path) => {
                // usvg resolves segments to absolute (root) coordinates already,
                // so no manual transform composition is needed here.
                let segments = path.data().segments().map(adapt_segment);
                out.extend(flatten_subpaths(segments));
            }
            usvg::Node::Group(inner) => collect_path_polygons(inner, out),
            // Images and text are out of scope for a v1 logo import.
            usvg::Node::Image(_) | usvg::Node::Text(_) => {}
        }
    }
}

fn adapt_segment(seg: usvg::tiny_skia_path::PathSegment) -> SvgSegment {
    use usvg::tiny_skia_path::PathSegment;
    match seg {
        PathSegment::MoveTo(p) => SvgSegment::MoveTo(Point2::new(p.x as f64, p.y as f64)),
        PathSegment::LineTo(p) => SvgSegment::LineTo(Point2::new(p.x as f64, p.y as f64)),
        PathSegment::QuadTo(c, p) => SvgSegment::QuadTo(
            Point2::new(c.x as f64, c.y as f64),
            Point2::new(p.x as f64, p.y as f64),
        ),
        PathSegment::CubicTo(c1, c2, p) => SvgSegment::CubicTo(
            Point2::new(c1.x as f64, c1.y as f64),
            Point2::new(c2.x as f64, c2.y as f64),
            Point2::new(p.x as f64, p.y as f64),
        ),
        PathSegment::Close => SvgSegment::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_points_starts_near_control_and_ends_at_p3() {
        let p0 = Point2::new(0.0, 0.0);
        let p1 = Point2::new(0.0, 10.0);
        let p2 = Point2::new(10.0, 10.0);
        let p3 = Point2::new(10.0, 0.0);
        let pts = cubic_bezier_points(p0, p1, p2, p3);
        assert_eq!(pts.len(), BEZIER_SEGMENTS);
        let last = pts.last().unwrap();
        assert!((last.x - p3.x).abs() < 1e-9);
        assert!((last.y - p3.y).abs() < 1e-9);
    }

    #[test]
    fn quad_to_cubic_produces_control_points_between_endpoints_and_control() {
        let p0 = Point2::new(0.0, 0.0);
        let control = Point2::new(5.0, 10.0);
        let p2 = Point2::new(10.0, 0.0);
        let (c1, c2) = quad_to_cubic(p0, control, p2);
        // c1 should be 2/3 of the way from p0 to control.
        assert!((c1.x - 10.0 / 3.0).abs() < 1e-9);
        assert!((c1.y - 20.0 / 3.0).abs() < 1e-9);
        // c2 should be 2/3 of the way from p2 to control.
        assert!((c2.x - 20.0 / 3.0).abs() < 1e-9);
        assert!((c2.y - 20.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn flatten_subpaths_single_closed_triangle() {
        let segments = vec![
            SvgSegment::MoveTo(Point2::new(0.0, 0.0)),
            SvgSegment::LineTo(Point2::new(10.0, 0.0)),
            SvgSegment::LineTo(Point2::new(5.0, 10.0)),
            SvgSegment::Close,
        ];
        let polys = flatten_subpaths(segments);
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].len(), 3);
    }

    #[test]
    fn flatten_subpaths_two_separate_subpaths() {
        let segments = vec![
            SvgSegment::MoveTo(Point2::new(0.0, 0.0)),
            SvgSegment::LineTo(Point2::new(1.0, 0.0)),
            SvgSegment::LineTo(Point2::new(1.0, 1.0)),
            SvgSegment::Close,
            SvgSegment::MoveTo(Point2::new(5.0, 5.0)),
            SvgSegment::LineTo(Point2::new(6.0, 5.0)),
            SvgSegment::LineTo(Point2::new(6.0, 6.0)),
            SvgSegment::Close,
        ];
        let polys = flatten_subpaths(segments);
        assert_eq!(polys.len(), 2);
        assert_eq!(polys[0].len(), 3);
        assert_eq!(polys[1].len(), 3);
    }

    #[test]
    fn flatten_subpaths_drops_degenerate_lone_moveto() {
        let segments = vec![SvgSegment::MoveTo(Point2::new(0.0, 0.0)), SvgSegment::Close];
        let polys = flatten_subpaths(segments);
        assert!(polys.is_empty());
    }

    #[test]
    fn flatten_subpaths_curve_expands_into_many_points() {
        let segments = vec![
            SvgSegment::MoveTo(Point2::new(0.0, 0.0)),
            SvgSegment::CubicTo(
                Point2::new(0.0, 10.0),
                Point2::new(10.0, 10.0),
                Point2::new(10.0, 0.0),
            ),
            SvgSegment::Close,
        ];
        let polys = flatten_subpaths(segments);
        assert_eq!(polys.len(), 1);
        // 1 start point + BEZIER_SEGMENTS sampled points.
        assert_eq!(polys[0].len(), 1 + BEZIER_SEGMENTS);
    }

    #[test]
    fn scale_and_place_scales_uniformly_and_offsets() {
        let polygons = vec![vec![Point2::new(0.0, 0.0), Point2::new(100.0, 50.0)]];
        // SVG is 100 units wide; target 10mm wide => scale = 0.1.
        let placed = scale_and_place(&polygons, 100.0, 10.0, 2.0, 3.0);
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0][0], (2.0, 3.0));
        assert_eq!(placed[0][1], (12.0, 8.0));
    }

    #[test]
    fn scale_and_place_zero_width_falls_back_to_identity_scale() {
        let polygons = vec![vec![Point2::new(5.0, 5.0)]];
        let placed = scale_and_place(&polygons, 0.0, 10.0, 0.0, 0.0);
        assert_eq!(placed[0][0], (5.0, 5.0));
    }

    #[test]
    fn extract_polygons_parses_simple_rect_path() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <path d="M0 0 L100 0 L100 100 L0 100 Z" fill="black"/>
        </svg>"##;
        let logo = extract_polygons(svg).expect("should parse");
        assert_eq!(logo.width, 100.0);
        assert_eq!(logo.height, 100.0);
        assert_eq!(logo.polygons.len(), 1);
        assert_eq!(logo.polygons[0].len(), 4);
    }

    #[test]
    fn extract_polygons_handles_curved_path() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50">
            <path d="M0 0 C0 25 25 25 25 0 Z" fill="black"/>
        </svg>"##;
        let logo = extract_polygons(svg).expect("should parse");
        assert_eq!(logo.polygons.len(), 1);
        // More than 2 points proves the curve was flattened, not dropped.
        assert!(logo.polygons[0].len() > 2);
    }

    #[test]
    fn extract_polygons_rejects_invalid_svg() {
        let result = extract_polygons("not an svg at all");
        assert!(result.is_err());
    }

    #[test]
    fn extract_polygons_empty_svg_has_no_polygons() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"></svg>"##;
        let logo = extract_polygons(svg).expect("should parse");
        assert!(logo.polygons.is_empty());
    }
}
