use super::{
    get_path, invalid_arg, resolve_schematic_ownership, sch_hierarchy::paper_dimensions,
    sch_shapes::saved_shape_bounds, ToolDef,
};
use crate::{mcp::protocol::CallToolResult, tool};
use copperpour_sexp::{
    bounds::Bbox,
    drawing_sheet::DrawingArea,
    geometry::round6,
    parse_sexp,
    schematic::{
        find_lib_symbol, parse_lib_pin, read_schematic, symbol_instance,
        symbol_parts_for_body_style, LabelKind, SymbolBounds,
    },
    text_bounds::{
        check_text_extent, free_note_bounds, label_placement, pin_text_bounds, saved_field_bounds,
        symbol_field_extent, unwrapped_text_box_bounds, LabelPlacement, TextExtent,
    },
    transform_pin,
    writer::read_consistent,
    SexpError, SexpNode,
};
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn tool_def() -> ToolDef {
    tool!(
        "check_schematic_page_fit",
        "Check one saved schematic against its inner frame and title block without changing it. \
         Reports per-item overflow and intrusion in millimetres. fits is null when there is \
         no known conflict but the drawing area or some item bounds could not be checked.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to the saved .kicad_sch file" }
            },
            "required": ["schematic"]
        }),
        |args, _ctx| async move { handle_check_schematic_page_fit(args) }
    )
}

fn handle_check_schematic_page_fit(args: &Value) -> anyhow::Result<CallToolResult> {
    let requested = get_path(args, "schematic")?;
    let path = match requested.canonicalize() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::file_not_found(&requested),
                format!("Schematic not found: {}", requested.display()),
            ));
        }
        result => result?,
    };
    let (_, tree) = match read_schematic(&path) {
        Err(error @ SexpError::Parse { .. }) => {
            return Ok(invalid_arg("schematic", &error.to_string()));
        }
        result => result?,
    };
    if tree.head() != Some("kicad_sch") {
        return Ok(invalid_arg("schematic", "must be a KiCad schematic"));
    }
    let mut plot = PlotSettings::default();
    let (source, area) = drawing_area(&path, paper_size(&tree), &mut plot);
    let mut findings = Vec::new();
    let mut unmeasured = Vec::new();
    let mut measured_count = 0;
    if let Ok(area) = area {
        let title = from_bbox(area.title_block);
        let lib = tree
            .find("lib_symbols")
            .map(|s| s.find_all("symbol"))
            .unwrap_or_default();
        for node in tree.children().unwrap_or_default() {
            let Some(kind) = node.head() else { continue };
            if matches!(
                kind,
                "version"
                    | "generator"
                    | "generator_version"
                    | "uuid"
                    | "paper"
                    | "title_block"
                    | "page"
                    | "lib_symbols"
                    | "sheet_instances"
                    | "symbol_instances"
                    | "embedded_files"
                    | "embedded_fonts"
                    | "group"
                    | "bus_alias"
            ) {
                continue;
            }
            let id = json!({ "kind": kind, "uuid": node.find_str("uuid") });
            let mut measured = Measurement {
                frame: area.frame,
                ..Default::default()
            };
            let coverage = if kind == "symbol" {
                measure_symbol(node, &lib, title, &plot, &mut measured)
            } else {
                measure_item(node, kind, title, &plot, &mut measured)
            };
            measured.record(coverage);
            if let Some(reason) = measured.missing.first() {
                let mut missed = id.clone();
                missed["reason"] = json!(reason);
                if measured.missing.len() > 1 {
                    missed["reasons"] = json!(measured.missing);
                }
                unmeasured.push(missed);
            }
            if let Some(bounds) = measured.bounds {
                measured_count += usize::from(measured.missing.is_empty());
                if let Some(finding) = violation(id, bounds, measured.intrudes, area.frame) {
                    findings.push(finding);
                }
            }
        }
    }
    let complete = area.is_ok() && unmeasured.is_empty();
    let fits = if findings.is_empty() {
        complete.then_some(true)
    } else {
        Some(false)
    };
    Ok(CallToolResult::json(&json!({
        "schematic": path,
        "drawing_sheet": source,
        "drawing_area": area.ok().map(|a| json!({
            "frame": bbox_json(from_bbox(a.frame)), "title_block": bbox_json(from_bbox(a.title_block))
        })),
        "drawing_area_unavailable": area.err(),
        "fits": fits,
        "coverage_complete": complete,
        "measured_count": measured_count,
        "unmeasured": unmeasured,
        "findings": findings
    })))
}

fn paper_size(tree: &SexpNode) -> Result<(f64, f64), &'static str> {
    let Some(paper) = tree.find("paper") else {
        return paper_dimensions("A4").map_err(|_| "invalid_paper_size");
    };
    let kind = paper
        .get(1)
        .and_then(SexpNode::as_str)
        .ok_or("invalid_paper_size")?;
    if kind == "User" {
        return Err("unsupported_paper_size");
    }
    let (w, h) = paper_dimensions(kind).map_err(|_| "invalid_paper_size")?;
    match paper.get(2) {
        None => Ok((w, h)),
        Some(value) if value.as_str() == Some("portrait") && paper.get(3).is_none() => Ok((h, w)),
        _ => Err("invalid_paper_size"),
    }
}

fn drawing_area(
    path: &Path,
    paper: Result<(f64, f64), &'static str>,
    plot: &mut PlotSettings,
) -> (Value, Result<DrawingArea, &'static str>) {
    let owner = match resolve_schematic_ownership(path) {
        Ok(Some(owner)) => owner,
        Ok(None) => {
            return (
                json!({ "kind": "builtin" }),
                paper.and_then(DrawingArea::builtin),
            )
        }
        Err(_) => return unresolved(json!({}), "unresolved_project_owner"),
    };
    let project = owner.project_file;
    let source = json!({ "project": project });
    let Ok(text) = read_consistent(&project) else {
        return unresolved(source, "unreadable_project");
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Map<String, Value>>(&text) else {
        return unresolved(source, "unreadable_project");
    };
    *plot = PlotSettings::from_project(&settings);
    plot.multiple_sheet_placements = owner.instance_paths.len() > 1;
    let setting = match settings.get("schematic") {
        None => None,
        Some(Value::Object(schematic)) => schematic.get("page_layout_descr_file"),
        _ => return unresolved(source, "invalid_worksheet_setting"),
    };
    let configured = match setting {
        None => "",
        Some(Value::String(name)) => name,
        _ => return unresolved(source, "invalid_worksheet_setting"),
    };
    if configured.is_empty() {
        return (
            json!({ "kind": "builtin", "project": project }),
            paper.and_then(DrawingArea::builtin),
        );
    }
    let mut source = json!({ "project": project, "configured_path": configured });
    let name = configured
        .strip_prefix("${KIPRJMOD}/")
        .or_else(|| configured.strip_prefix("${KIPRJMOD}\\"))
        .unwrap_or(configured);
    let worksheet = copperpour_sexp::writer::parent_directory(&project).join(name);
    if name.is_empty() || name.contains('$') || !worksheet.is_file() {
        return unresolved(source, "unresolved_worksheet_path");
    }
    let Ok(text) = read_consistent(&worksheet) else {
        return unresolved(source, "unreadable_worksheet");
    };
    let Ok(tree) = parse_sexp(&text) else {
        return unresolved(source, "invalid_worksheet");
    };
    source["kind"] = json!("custom");
    let geometry = paper.and_then(|paper| DrawingArea::custom(paper, &tree));
    (source, geometry)
}

fn unresolved(
    mut source: Value,
    reason: &'static str,
) -> (Value, Result<DrawingArea, &'static str>) {
    source["kind"] = json!("unresolved");
    source["reason"] = json!(reason);
    (source, Err(reason))
}

type Point = (f64, f64);
const DEFAULT_PLOT_PEN_MM: f64 = 0.1524;

struct PlotSettings {
    pen: f64,
    label_size_ratio: f64,
    text_offset_ratio: f64,
    junction_diameter: f64,
    min_junction_diameter: f64,
    wire_width: f64,
    variable_wire_width: bool,
    intersheet_refs: bool,
    overbar_ratio: f64,
    nondefault_unit_names: bool,
    multiple_sheet_placements: bool,
}

impl Default for PlotSettings {
    fn default() -> Self {
        Self {
            pen: DEFAULT_PLOT_PEN_MM,
            label_size_ratio: 0.375,
            text_offset_ratio: 0.15,
            junction_diameter: 0.9144,
            min_junction_diameter: 0.25908,
            wire_width: DEFAULT_PLOT_PEN_MM,
            variable_wire_width: false,
            intersheet_refs: false,
            overbar_ratio: 1.23,
            nondefault_unit_names: false,
            multiple_sheet_placements: false,
        }
    }
}

impl PlotSettings {
    fn from_project(project: &serde_json::Map<String, Value>) -> Self {
        let drawing = project.get("schematic").and_then(|s| s.get("drawing"));
        let number = |key, default| {
            drawing
                .and_then(|d| d.get(key))
                .and_then(Value::as_f64)
                .unwrap_or(default)
        };
        let classes = project
            .get("net_settings")
            .and_then(|n| n.get("classes"))
            .and_then(Value::as_array);
        let wire_width = |class: &Value| class["wire_width"].as_f64().unwrap_or(6.0);
        let default_width_mils = classes
            .and_then(|classes| classes.iter().find(|c| c["name"] == "Default"))
            .map_or(6.0, wire_width);
        let variable_wire_width = classes.is_some_and(|classes| {
            classes
                .iter()
                .any(|class| wire_width(class) != default_width_mils)
        });
        let choice = drawing
            .and_then(|d| d["junction_size_choice"].as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(3);
        let multiplier = [0.0, 1.7, 4.0, 6.0, 9.0, 12.0]
            .get(choice)
            .copied()
            .unwrap_or(6.0);
        Self {
            pen: number("default_line_thickness", 6.0) * 0.0254,
            label_size_ratio: number("label_size_ratio", 0.375),
            text_offset_ratio: number("text_offset_ratio", 0.15),
            junction_diameter: (default_width_mils * multiplier * 0.0254).max(0.0001),
            min_junction_diameter: default_width_mils * 1.7 * 0.0254,
            wire_width: default_width_mils * 0.0254,
            variable_wire_width,
            intersheet_refs: drawing
                .and_then(|d| d["intersheets_ref_show"].as_bool())
                .unwrap_or(false),
            overbar_ratio: number("overbar_offset_ratio", 1.23),
            nondefault_unit_names: project.get("schematic").is_some_and(|s| {
                s["subpart_id_separator"]
                    .as_u64()
                    .is_some_and(|v| (1..=126).contains(&v))
                    || s["subpart_first_id"]
                        .as_u64()
                        .is_some_and(|v| (49..=122).contains(&v) && v != 65)
            }),
            multiple_sheet_placements: false,
        }
    }

    fn default_text_pen(&self) -> Result<(), &'static str> {
        if (self.pen - DEFAULT_PLOT_PEN_MM).abs() < 1e-9 {
            Ok(())
        } else {
            Err("project_text_pen_not_measured")
        }
    }

    fn overbar(&self, text: &str) -> Result<(), &'static str> {
        if self.overbar_ratio != 1.23 && text.contains("~{") {
            Err("project_overbar_offset_not_measured")
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct Measurement {
    frame: Bbox,
    bounds: Option<SymbolBounds>,
    intrudes: bool,
    missing: Vec<&'static str>,
}

impl Measurement {
    fn record(&mut self, result: Result<(), &'static str>) {
        if let Err(reason) = result {
            if !self.missing.contains(&reason) {
                self.missing.push(reason);
            }
        }
    }

    fn include(&mut self, bounds: SymbolBounds, intrudes: bool) {
        if let Some(previous) = &mut self.bounds {
            previous.include(bounds.min_x, bounds.min_y);
            previous.include(bounds.max_x, bounds.max_y);
        } else {
            self.bounds = Some(bounds);
        }
        self.intrudes |= intrudes;
    }

    fn include_box(
        &mut self,
        bounds: SymbolBounds,
        title: SymbolBounds,
    ) -> Result<(), &'static str> {
        if ![bounds.min_x, bounds.min_y, bounds.max_x, bounds.max_y]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err("invalid_item_geometry");
        }
        self.include(bounds, bounds.interiors_intersect(title));
        Ok(())
    }

    fn include_segment(&mut self, (a, b): (Point, Point), title: SymbolBounds) {
        self.include(SymbolBounds::spanning(a, b), title.segment_enters(a, b));
    }

    fn include_stroked_segment(
        &mut self,
        (a, b): (Point, Point),
        radius: f64,
        title: SymbolBounds,
    ) {
        let closest = |(x, y): Point| {
            (
                x.clamp(title.min_x, title.max_x),
                y.clamp(title.min_y, title.max_y),
            )
        };
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length2 = dx * dx + dy * dy;
        let distance = |(x, y): Point, (u, v): Point| (x - u).hypot(y - v);
        let intrudes = title.segment_enters(a, b)
            || [a, b].into_iter().any(|p| distance(p, closest(p)) < radius)
            || corners(title).into_iter().any(|p| {
                let at = if length2 == 0.0 {
                    0.0
                } else {
                    (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length2).clamp(0.0, 1.0)
                };
                distance(p, (a.0 + at * dx, a.1 + at * dy)) < radius
            });
        self.include(grow(SymbolBounds::spanning(a, b), radius), intrudes);
    }

    fn include_circle(&mut self, (x, y): Point, inner: f64, outer: f64, title: SymbolBounds) {
        let near =
            (x - x.clamp(title.min_x, title.max_x)).hypot(y - y.clamp(title.min_y, title.max_y));
        let far = (x - title.min_x)
            .abs()
            .max((x - title.max_x).abs())
            .hypot((y - title.min_y).abs().max((y - title.max_y).abs()));
        self.include(
            grow(SymbolBounds::spanning((x, y), (x, y)), outer),
            near < outer && far > inner,
        );
    }

    fn include_rect_edges(&mut self, bounds: SymbolBounds, pen: f64, title: SymbolBounds) {
        let edges = corners(bounds);
        for i in 0..edges.len() {
            self.include_stroked_segment((edges[i], edges[(i + 1) % edges.len()]), pen, title);
        }
    }

    fn include_uncertain_outline(&mut self, bounds: SymbolBounds, title: SymbolBounds) {
        if bounds.interiors_intersect(title) || frame_overflow(bounds, self.frame) != [0.0; 4] {
            self.record(Err("patterned_geometry_not_measured"));
        } else {
            self.include(bounds, false);
        }
    }
}

fn corners(b: SymbolBounds) -> [Point; 4] {
    [
        (b.min_x, b.min_y),
        (b.max_x, b.min_y),
        (b.max_x, b.max_y),
        (b.min_x, b.max_y),
    ]
}

fn measure_symbol(
    node: &SexpNode,
    library: &[&SexpNode],
    title: SymbolBounds,
    plot: &PlotSettings,
    measured: &mut Measurement,
) -> Result<(), &'static str> {
    let mut instance = symbol_instance(node).ok_or("invalid_symbol")?;
    point(node.find("at")).ok_or("invalid_symbol")?;
    if !instance.rotation.is_finite() {
        return Err("invalid_symbol");
    }
    let lib = find_lib_symbol(library, &instance);
    let placements = node
        .find("instances")
        .into_iter()
        .flat_map(|records| records.find_all("project"))
        .flat_map(|project| project.find_all("path"))
        .collect::<Vec<_>>();
    let reference_varies = plot.multiple_sheet_placements
        || placements
            .iter()
            .any(|placement| placement.find_str("reference") != Some(instance.reference.as_str()));
    let unit_varies = placements
        .iter()
        .any(|placement| placement.find_f64("unit") != Some(f64::from(instance.unit)));
    if reference_varies {
        measured.record(Err("per_instance_reference_not_measured"));
    }
    if unit_varies {
        measured.record(Err("per_instance_unit_not_measured"));
        // Unit zero selects only the parts shared by all placements.
        instance.unit = 0;
    }
    if let Some(lib) = lib {
        let alternates: std::collections::HashSet<&str> = node
            .find_all("pin")
            .into_iter()
            .filter(|pin| pin.find("alternate").is_some())
            .filter_map(|pin| pin.get(1).and_then(SexpNode::as_str))
            .collect();
        let style = node
            .find_f64("body_style")
            .or_else(|| node.find_f64("convert"))
            .unwrap_or(1.0);
        if (1.0..=f64::from(u32::MAX)).contains(&style) && style.fract() == 0.0 {
            let style = style as u32;
            let transform = instance.pin_transform();
            for part in symbol_parts_for_body_style(lib, instance.unit, style) {
                for child in part.children().unwrap_or_default() {
                    let result = match child.head() {
                        Some("rectangle" | "polyline" | "arc" | "circle" | "bezier") => {
                            saved_shape_bounds(child)
                                .ok_or("invalid_symbol_shape")
                                .and_then(|b| {
                                    let bounds =
                                        grow(b.placed(transform), pen_radius(child, plot)?);
                                    if patterned_stroke(child) || patterned_fill(child) {
                                        measured.include_uncertain_outline(bounds, title);
                                        return Ok(());
                                    }
                                    if bounds.interiors_intersect(title) {
                                        measured.record(Err(
                                            "shape_title_block_intersection_not_measured",
                                        ));
                                    }
                                    if child.head() == Some("bezier") {
                                        if frame_overflow(bounds, measured.frame) != [0.0; 4] {
                                            measured.record(Err(
                                                "bezier_frame_intersection_not_measured",
                                            ));
                                        }
                                    } else {
                                        measured.include(bounds, false);
                                    }
                                    Ok(())
                                })
                        }
                        Some("text" | "text_box") => {
                            Err("library_symbol_graphic_text_not_measured")
                        }
                        Some("pin") => (|| {
                            let pin = parse_lib_pin(child).ok_or("invalid_pin_geometry")?;
                            if pin.hidden {
                                return Ok(());
                            }
                            if alternates.contains(pin.number.as_str()) {
                                return Err("pin_alternate_not_measured");
                            }
                            let a = pin.rotation.to_radians();
                            let tip = transform_pin(pin.local_x, pin.local_y, transform);
                            let root = transform_pin(
                                pin.local_x + pin.length * a.cos(),
                                pin.local_y + pin.length * a.sin(),
                                transform,
                            );
                            if ![tip.0, tip.1, root.0, root.1]
                                .into_iter()
                                .all(f64::is_finite)
                            {
                                return Err("invalid_pin_geometry");
                            }
                            measured.include_stroked_segment((tip, root), plot.pen / 2.0, title);
                            if pin.electrical_type == "no_connect"
                                || child.get(2).and_then(SexpNode::as_str) != Some("line")
                            {
                                return Err("pin_decoration_not_measured");
                            }
                            Ok(())
                        })(),
                        _ => continue,
                    };
                    measured.record(result);
                }
            }
            for text in pin_text_bounds(
                lib,
                &instance,
                style,
                plot.pen,
                plot.text_offset_ratio,
                move |number| !alternates.contains(number),
                plot.overbar_ratio == 1.23,
            ) {
                let result = text.and_then(|b| measured.include_box(b, title));
                measured.record(result);
            }
        } else {
            measured.record(Err("invalid_symbol_body_style"));
        }
    } else {
        measured.record(Err("unresolved_symbol_bounds"));
    }
    if node.find_str("dnp") == Some("yes") {
        measured.record(Err("dnp_marker_not_measured"));
    }
    // Check the parts separately: the space between a field and its symbol
    // is not an intrusion, even if their reported envelope covers that space.
    for property in node.find_all("property") {
        let field = symbol_field_extent(property, &instance, lib).and_then(|extent| {
            if let Some(extent) = extent {
                if property.get(1).and_then(SexpNode::as_str) == Some("Reference") {
                    if reference_varies || unit_varies {
                        return Ok(());
                    }
                    if lib.is_some_and(|lib| {
                        copperpour_sexp::text_bounds::unit_count(lib) > 1
                            && (plot.nondefault_unit_names
                                || lib.find("unit_name").is_some()
                                || lib
                                    .find_all("symbol")
                                    .iter()
                                    .any(|unit| unit.find("unit_name").is_some()))
                    }) {
                        return Err("symbol_unit_suffix_not_measured");
                    }
                }
                if lib.is_some_and(|lib| lib.find_str("power") == Some("local"))
                    && property.get(1).and_then(SexpNode::as_str) == Some("Value")
                {
                    measured.record(Err("local_power_icon_not_measured"));
                }
                plot.default_text_pen()?;
                plot.overbar(&extent.text)?;
                measure_text(property, extent, title, measured)?;
            }
            Ok(())
        });
        measured.record(field);
    }
    Ok(())
}

fn measure_text(
    node: &SexpNode,
    extent: TextExtent,
    title: SymbolBounds,
    measured: &mut Measurement,
) -> Result<(), &'static str> {
    check_text_extent(node, &extent)?;
    measured.include_box(extent.bounds, title)?;
    Ok(())
}

fn measure_item(
    node: &SexpNode,
    kind: &str,
    title: SymbolBounds,
    plot: &PlotSettings,
    measured: &mut Measurement,
) -> Result<(), &'static str> {
    let invalid = "invalid_item_geometry";
    let at = || point(node.find("at")).ok_or(invalid);
    let bounds = match kind {
        "text" => match free_note_bounds(node)? {
            None => return Ok(()),
            Some(bounds) => {
                plot.default_text_pen()?;
                bounds
            }
        },
        "label" | "global_label" | "hierarchical_label" => {
            let label = match kind {
                "label" => LabelKind::NetLabel,
                "global_label" => LabelKind::GlobalLabel,
                _ => LabelKind::HierarchicalLabel,
            };
            let main = measure_label(node, label, title, plot, measured);
            measured.record(main.map(|_| ()));
            for field in node.find_all("property") {
                let name = field.get(1).and_then(SexpNode::as_str);
                if name == Some("Intersheetrefs") {
                    if plot.intersheet_refs {
                        measured.record(Err("unresolved_intersheet_references"));
                    }
                    continue;
                }
                let result = plot.default_text_pen().and_then(|()| {
                    plot.overbar(field.get(2).and_then(SexpNode::as_str).unwrap_or_default())?;
                    let offset = label_field_offset(label, main)?;
                    include_optional(measured, saved_field_bounds(field, offset, false), title)
                });
                measured.record(result);
            }
            if kind == "global_label" && plot.intersheet_refs {
                measured.record(Err("unresolved_intersheet_references"));
            }
            return Ok(());
        }
        "wire" | "bus" => {
            let points = node.find("pts").ok_or(invalid)?.find_all("xy");
            let [a, b] = points.as_slice() else {
                return Err(invalid);
            };
            measured.include_segment(
                (
                    point(Some(a)).ok_or(invalid)?,
                    point(Some(b)).ok_or(invalid)?,
                ),
                title,
            );
            return Ok(());
        }
        "bus_entry" => {
            let (x, y) = at()?;
            let (dx, dy) = point(node.find("size")).ok_or(invalid)?;
            let width = width(node)?;
            let pen = if width == 0.0 {
                if plot.variable_wire_width {
                    measured.record(Err("bus_entry_netclass_width_not_measured"));
                    0.0
                } else {
                    plot.wire_width.max(0.0847) / 2.0
                }
            } else {
                pen_radius(node, plot)?
            };
            let end = (x + dx, y + dy);
            measured.include_stroked_segment(((x, y), end), pen, title);
            return Ok(());
        }
        "junction" => {
            let diameter = match node.find("diameter") {
                Some(value) => value
                    .get_f64(1)
                    .filter(|d| d.is_finite() && *d >= 0.0)
                    .ok_or(invalid)?,
                None => 0.0,
            };
            let (x, y) = at()?;
            let diameter = if diameter == 0.0 {
                plot.junction_diameter
            } else {
                diameter
            };
            let radius = if diameter <= 0.0001 {
                return Ok(());
            } else if plot.variable_wire_width {
                measured.record(Err("junction_netclass_width_not_measured"));
                diameter / 2.0
            } else {
                diameter.max(plot.min_junction_diameter) / 2.0
            };
            measured.include_circle((x, y), 0.0, radius, title);
            return Ok(());
        }
        "no_connect" => {
            let (x, y) = at()?;
            // KiCad plots a 48 mil cross with the usual 6 mil pen.
            let half_line = 1.2192 / 2.0;
            let pen = plot.pen / 2.0;
            for flip in [-1.0, 1.0] {
                measured.include_stroked_segment(
                    (
                        (x - half_line, y - flip * half_line),
                        (x + half_line, y + flip * half_line),
                    ),
                    pen,
                    title,
                );
            }
            return Ok(());
        }
        "circle" => {
            let center = point(node.find("center")).ok_or(invalid)?;
            let radius = node
                .find_f64("radius")
                .filter(|r| r.is_finite() && *r > 0.0)
                .ok_or(invalid)?;
            let pen = pen_radius(node, plot)?;
            if patterned_stroke(node) || patterned_fill(node) {
                measured.include_uncertain_outline(
                    grow(SymbolBounds::spanning(center, center), radius + pen),
                    title,
                );
                return Ok(());
            }
            let hollow = node
                .find("fill")
                .and_then(|f| f.find_str("type"))
                .is_none_or(|fill| fill == "none");
            let inner = if hollow { (radius - pen).max(0.0) } else { 0.0 };
            measured.include_circle(center, inner, radius + pen, title);
            return Ok(());
        }
        "rectangle" | "polyline" | "arc" | "bezier" => {
            let b = saved_shape_bounds(node).ok_or(invalid)?;
            let pen = pen_radius(node, plot)?;
            let fill = node.find("fill").and_then(|f| f.find_str("type"));
            if patterned_stroke(node) || patterned_fill(node) {
                measured.include_uncertain_outline(grow(b, pen), title);
                return Ok(());
            }
            if kind == "rectangle" && fill.is_none_or(|f| f == "none") {
                measured.include_rect_edges(b, pen, title);
                return Ok(());
            }
            if kind == "rectangle" {
                return measured.include_box(grow(b, pen), title);
            }
            if kind == "polyline" && fill.is_none_or(|f| f == "none") {
                let vertices = node.find("pts").ok_or(invalid)?.find_all("xy");
                for pair in vertices.windows(2) {
                    let a = point(Some(pair[0])).ok_or(invalid)?;
                    let b = point(Some(pair[1])).ok_or(invalid)?;
                    measured.include_stroked_segment((a, b), pen, title);
                }
                return Ok(());
            }
            let bounds = grow(b, pen);
            let known_points = match kind {
                "arc" => ["start", "mid", "end"]
                    .into_iter()
                    .filter_map(|tag| point(node.find(tag)))
                    .collect::<Vec<_>>(),
                "bezier" => {
                    let points = node.find("pts").ok_or(invalid)?.find_all("xy");
                    [points.first(), points.last()]
                        .into_iter()
                        .filter_map(|node| point(node.copied()))
                        .collect()
                }
                _ => Vec::new(),
            };
            for p in known_points {
                measured.include_circle(p, 0.0, pen, title);
            }
            if bounds.interiors_intersect(title) {
                measured.record(Err("shape_title_block_intersection_not_measured"));
            }
            if kind == "bezier" {
                // Bezier control points enclose the curve but need not lie on it.
                if frame_overflow(bounds, measured.frame) != [0.0; 4] {
                    measured.record(Err("bezier_frame_intersection_not_measured"));
                }
                if measured.missing.is_empty() {
                    measured.include(bounds, false);
                }
                return Ok(());
            }
            measured.include(bounds, false);
            return Ok(());
        }
        "sheet" | "text_box" => {
            let bounds = if kind == "text_box" && node.find("start").is_some() {
                SymbolBounds::spanning(
                    point(node.find("start")).ok_or(invalid)?,
                    point(node.find("end")).ok_or(invalid)?,
                )
            } else {
                let (x, y) = at()?;
                let (w, h) = point(node.find("size")).ok_or(invalid)?;
                SymbolBounds::spanning((x, y), (x + w, y + h))
            };
            let pen = pen_radius(node, plot)?;
            let fill = node.find("fill");
            let filled = fill.is_some_and(|fill| {
                fill.find_str("type").is_some_and(|kind| kind != "none")
                    || fill
                        .find("color")
                        .and_then(|color| color.get_f64(4))
                        .is_some_and(|alpha| alpha > 0.0)
            });
            if patterned_fill(node) {
                if bounds.interiors_intersect(title)
                    || frame_overflow(bounds, measured.frame) != [0.0; 4]
                {
                    measured.record(Err("patterned_fill_not_measured"));
                }
            } else if filled {
                measured.include_box(bounds, title)?;
            }
            if pen > 0.0 && patterned_stroke(node) {
                measured.include_uncertain_outline(grow(bounds, pen), title);
            } else if pen > 0.0 {
                measured.include_rect_edges(bounds, pen, title);
            }
            if kind == "text_box" {
                plot.default_text_pen()?;
                return include_optional(measured, unwrapped_text_box_bounds(node, bounds), title);
            }
            if node.find_str("dnp") == Some("yes") {
                measured.record(Err("dnp_marker_not_measured"));
            }
            for field in node.find_all("property") {
                let is_filename = matches!(
                    field.get(1).and_then(SexpNode::as_str),
                    Some("Sheetfile" | "Sheet file")
                );
                let result = plot.default_text_pen().and_then(|()| {
                    plot.overbar(field.get(2).and_then(SexpNode::as_str).unwrap_or_default())?;
                    include_optional(
                        measured,
                        saved_field_bounds(field, (0.0, 0.0), is_filename),
                        title,
                    )
                });
                measured.record(result);
            }
            for pin in node.find_all("pin") {
                let result =
                    measure_label(pin, LabelKind::HierarchicalLabel, title, plot, measured);
                measured.record(result.map(|_| ()));
            }
            return Ok(());
        }
        _ => return Err("unsupported_item_type"),
    };
    measured.include_box(bounds, title)
}

fn patterned_stroke(node: &SexpNode) -> bool {
    node.find("stroke")
        .and_then(|stroke| stroke.find_str("type"))
        .is_some_and(|kind| !matches!(kind, "default" | "solid"))
}

fn patterned_fill(node: &SexpNode) -> bool {
    node.find("fill")
        .and_then(|fill| fill.find_str("type"))
        .is_some_and(|kind| !matches!(kind, "none" | "outline" | "background" | "color" | "solid"))
}

fn label_field_offset(
    kind: LabelKind,
    text_offset: Result<Point, &'static str>,
) -> Result<Point, &'static str> {
    // SCH_FIELD::Plot applies the main text displacement only to global labels.
    if kind == LabelKind::GlobalLabel {
        text_offset
    } else {
        Ok((0.0, 0.0))
    }
}

fn measure_label(
    node: &SexpNode,
    kind: LabelKind,
    title: SymbolBounds,
    plot: &PlotSettings,
    measured: &mut Measurement,
) -> Result<(f64, f64), &'static str> {
    plot.default_text_pen()?;
    if plot.text_offset_ratio != 0.15
        || (kind == LabelKind::GlobalLabel && plot.label_size_ratio != 0.375)
    {
        return Err("project_label_ratios_not_measured");
    }
    let LabelPlacement {
        extent,
        field_offset,
        outline,
    } = label_placement(node, kind).ok_or("invalid_item_geometry")?;
    let text = check_text_extent(node, &extent).and_then(|()| plot.overbar(&extent.text));
    if let Some(outline) = outline {
        // A hierarchical flag does not depend on the label string's width.
        if text.is_ok() || kind == LabelKind::HierarchicalLabel {
            measured.include_box(outline, title)?;
        }
    }
    let text = text.and_then(|()| measured.include_box(extent.run, title));
    measured.record(text);
    Ok(field_offset)
}

fn include_optional(
    measured: &mut Measurement,
    bounds: Result<Option<SymbolBounds>, &'static str>,
    title: SymbolBounds,
) -> Result<(), &'static str> {
    if let Some(bounds) = bounds? {
        measured.include_box(bounds, title)?;
    }
    Ok(())
}

fn point(node: Option<&SexpNode>) -> Option<Point> {
    let node = node?;
    let (x, y) = (node.get_f64(1)?, node.get_f64(2)?);
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn width(node: &SexpNode) -> Result<f64, &'static str> {
    match node.find("stroke").and_then(|stroke| stroke.find("width")) {
        Some(width) => width
            .get_f64(1)
            .filter(|w| w.is_finite())
            .ok_or("invalid_item_geometry"),
        None => Ok(0.0),
    }
}

fn pen_radius(node: &SexpNode, plot: &PlotSettings) -> Result<f64, &'static str> {
    Ok(match width(node)? {
        0.0 => plot.pen,
        positive @ 0.0.. => positive.max(0.0847),
        _ => 0.0, // KiCad uses a negative pen width to suppress the outline.
    } / 2.0)
}

fn grow(bounds: SymbolBounds, radius: f64) -> SymbolBounds {
    SymbolBounds {
        min_x: bounds.min_x - radius,
        min_y: bounds.min_y - radius,
        max_x: bounds.max_x + radius,
        max_y: bounds.max_y + radius,
    }
}

fn from_bbox((min_x, min_y, max_x, max_y): Bbox) -> SymbolBounds {
    SymbolBounds {
        min_x,
        min_y,
        max_x,
        max_y,
    }
}

fn bbox_json(b: SymbolBounds) -> Value {
    json!({
        "x_min": round6(b.min_x), "y_min": round6(b.min_y),
        "x_max": round6(b.max_x), "y_max": round6(b.max_y)
    })
}

fn frame_overflow(bounds: SymbolBounds, frame: Bbox) -> [f64; 4] {
    let (l, t, r, b) = frame;
    [
        l - bounds.min_x,
        t - bounds.min_y,
        bounds.max_x - r,
        bounds.max_y - b,
    ]
    .map(|overflow| round6(overflow.max(0.0)))
}

fn violation(mut item: Value, bounds: SymbolBounds, intrudes: bool, frame: Bbox) -> Option<Value> {
    let [left, top, right, bottom] = frame_overflow(bounds, frame);
    if !intrudes && [left, top, right, bottom] == [0.0; 4] {
        return None;
    }
    item["frame_overflow_mm"] =
        json!({ "left": left, "top": top, "right": right, "bottom": bottom });
    item["title_block_intrusion"] = json!(intrudes);
    item["bounds"] = bbox_json(bounds);
    Some(item)
}

#[cfg(test)]
mod tests;
