//! The reserved areas of a KiCad drawing sheet, independent of the editor.

use crate::{bounds::Bbox, SexpNode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawingArea {
    /// The inner frame at the centreline of its stroke.
    pub frame: Bbox,
    pub title_block: Bbox,
}

impl DrawingArea {
    pub fn builtin(paper_mm: (f64, f64)) -> Result<Self, &'static str> {
        let (w, h) = plotted_size(paper_mm)?;
        // KiCad's drawing_sheet_default_description.cpp: 10 mm margins, a
        // 2 mm zone band, and (110, 34) to (2, 2) from the bottom-right margin.
        let area = Self {
            frame: (12.0, 12.0, w - 12.0, h - 12.0),
            title_block: (w - 120.0, h - 44.0, w - 12.0, h - 12.0),
        };
        area.validate()
    }

    /// Recognize the conventional repeated frame and explicitly marked title
    /// block. Unmarked rectangles are indistinguishable from arbitrary artwork.
    pub fn custom(paper_mm: (f64, f64), worksheet: &SexpNode) -> Result<Self, &'static str> {
        if !matches!(worksheet.head(), Some("kicad_wks" | "page_layout")) {
            return Err("invalid_worksheet");
        }
        let (w, h) = plotted_size(paper_mm)?;
        let setup = worksheet.find("setup").ok_or("missing_worksheet_setup")?;
        let margin = |tag| {
            setup
                .find_f64(tag)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or("invalid_worksheet_margin")
        };
        let outer = (
            margin("left_margin")?,
            margin("top_margin")?,
            w - margin("right_margin")?,
            h - margin("bottom_margin")?,
        );
        let rectangles = worksheet.find_all("rect");
        let frame = unique(rectangles.iter().filter_map(|rect| {
            if rect.find("option").is_some()
                || rect.find_f64("repeat") != Some(2.0)
                || rectangle(rect, outer, (0.0, 0.0)) != Some(outer)
            {
                return None;
            }
            let inner = rectangle(
                rect,
                outer,
                (rect.find_f64("incrx")?, rect.find_f64("incry")?),
            )?;
            (contains(outer, inner) && inner != outer).then_some(inner)
        }))
        .ok_or("unrecognized_frame")?;
        let title = unique(rectangles.into_iter().filter(|rect| {
            rect.find_str("comment")
                .is_some_and(|s| s.eq_ignore_ascii_case("rect around the title block"))
        }))
        .ok_or("unrecognized_title_block")?;
        if title.find("option").is_some()
            || (title.find("repeat").is_some() && title.find_f64("repeat") != Some(1.0))
        {
            return Err("unrecognized_title_block");
        }
        let title_block = rectangle(title, outer, (0.0, 0.0)).ok_or("unrecognized_title_block")?;
        Self { frame, title_block }.validate()
    }

    fn validate(self) -> Result<Self, &'static str> {
        let valid =
            |(l, t, r, b): Bbox| [l, t, r, b].into_iter().all(f64::is_finite) && l < r && t < b;
        if valid(self.frame) && valid(self.title_block) && contains(self.frame, self.title_block) {
            Ok(self)
        } else {
            Err("invalid_worksheet_geometry")
        }
    }
}

fn plotted_size((w, h): (f64, f64)) -> Result<(f64, f64), &'static str> {
    let mils = |mm: f64| (mm / 0.0254).round() * 0.0254;
    let (w, h) = (mils(w), mils(h));
    if [w, h].into_iter().all(|v| v.is_finite() && v > 0.0) {
        Ok((w, h))
    } else {
        Err("invalid_paper_size")
    }
}

fn unique<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

fn rectangle(rect: &SexpNode, (l, t, r, b): Bbox, (dx, dy): (f64, f64)) -> Option<Bbox> {
    let point = |tag| {
        let pos = rect.find(tag)?;
        let (x, y) = (pos.get_f64(1)? + dx, pos.get_f64(2)? + dy);
        let (x, y) = match pos.get(3).map(SexpNode::as_str) {
            Some(Some("ltcorner")) => (l + x, t + y),
            Some(Some("rtcorner")) => (r - x, t + y),
            Some(Some("lbcorner")) => (l + x, b - y),
            None | Some(Some("rbcorner")) => (r - x, b - y),
            _ => return None,
        };
        (x.is_finite() && y.is_finite()).then_some((x, y))
    };
    let ((x1, y1), (x2, y2)) = (point("start")?, point("end")?);
    Some((x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)))
}

fn contains((l, t, r, b): Bbox, (x1, y1, x2, y2): Bbox) -> bool {
    l <= x1 && t <= y1 && r >= x2 && b >= y2
}
