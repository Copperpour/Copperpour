//! Small deterministic AP214 writer for rectangular package approximations.
//!
//! Each box is an independent manifold solid: 8 shared vertices, 12 shared
//! edges and 6 outward planar faces. Solids are not Boolean-fused. No CAD
//! kernel or external process is used at runtime.
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoxSolid {
    pub name: String,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub color: [f64; 3],
}

impl BoxSolid {
    pub fn validate(&self) -> Result<(), String> {
        for axis in 0..3 {
            // Allow only floating-point subtraction roundoff at the origin;
            // this is not a general geometry tolerance or a dimension default.
            let roundoff =
                4.0 * f64::EPSILON * self.min[axis].abs().max(self.max[axis].abs()).max(1.0);
            if !self.min[axis].is_finite()
                || !self.max[axis].is_finite()
                || self.min[axis].abs() > 10_000.
                || self.max[axis].abs() > 10_000.
                || self.max[axis] - self.min[axis] + roundoff < 0.001
            {
                return Err(
                    "box extents must be finite, within +/-10000 mm, with dimensions >= 0.001 mm"
                        .into(),
                );
            }
        }
        if self
            .color
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err("RGB components must be finite numbers from 0 to 1".into());
        }
        step_string(&self.name)?;
        Ok(())
    }
}

fn real(value: f64) -> String {
    let value = if value == 0. { 0. } else { value };
    let mut text = format!("{value:.9}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

pub(crate) fn step_string(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|b| (32..=126).contains(&b) && b != b'\\')
    {
        return Err(
            "name must contain 1..128 printable ASCII characters excluding backslash".into(),
        );
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

#[derive(Default)]
struct Writer {
    entities: Vec<String>,
}
impl Writer {
    fn entity(&mut self, value: impl Into<String>) -> usize {
        self.entities.push(value.into());
        self.entities.len()
    }
    fn point(&mut self, p: [f64; 3]) -> usize {
        self.entity(format!(
            "CARTESIAN_POINT('',({},{},{}))",
            real(p[0]),
            real(p[1]),
            real(p[2])
        ))
    }
    fn direction(&mut self, p: [f64; 3]) -> usize {
        self.entity(format!(
            "DIRECTION('',({},{},{}))",
            real(p[0]),
            real(p[1]),
            real(p[2])
        ))
    }
    fn axis(&mut self, origin: usize, normal: [f64; 3], x: [f64; 3]) -> usize {
        let n = self.direction(normal);
        let x = self.direction(x);
        self.entity(format!("AXIS2_PLACEMENT_3D('',#{origin},#{n},#{x})"))
    }
    fn solid(&mut self, b: &BoxSolid) -> usize {
        let [x0, y0, z0] = b.min;
        let [x1, y1, z1] = b.max;
        let points = [
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let point_ids = points.map(|p| self.point(p));
        let vertices = point_ids.map(|id| self.entity(format!("VERTEX_POINT('',#{id})")));
        let faces = [
            ([0, 3, 2, 1], [0., 0., -1.]),
            ([4, 5, 6, 7], [0., 0., 1.]),
            ([0, 1, 5, 4], [0., -1., 0.]),
            ([1, 2, 6, 5], [1., 0., 0.]),
            ([2, 3, 7, 6], [0., 1., 0.]),
            ([3, 0, 4, 7], [-1., 0., 0.]),
        ];
        let mut edges = BTreeMap::new();
        let mut face_ids = Vec::new();
        for (corners, normal) in faces {
            let mut oriented = Vec::new();
            for i in 0..4 {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                let key = (a.min(b), a.max(b));
                let edge = *edges.entry(key).or_insert_with(|| {
                    let (lo, hi) = key;
                    let delta = std::array::from_fn(|j| points[hi][j] - points[lo][j]);
                    let length = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
                    let direction = self.direction(delta.map(|v| v / length));
                    let vector = self.entity(format!("VECTOR('',#{direction},{})", real(length)));
                    let line = self.entity(format!("LINE('',#{},#{vector})", point_ids[lo]));
                    self.entity(format!(
                        "EDGE_CURVE('',#{},#{},#{line},.T.)",
                        vertices[lo], vertices[hi]
                    ))
                });
                oriented.push(self.entity(format!(
                    "ORIENTED_EDGE('',*,*,#{edge},{})",
                    if a < b { ".T." } else { ".F." }
                )));
            }
            let edge_loop = self.entity(format!("EDGE_LOOP('',({}))", refs(&oriented)));
            let bound = self.entity(format!("FACE_OUTER_BOUND('',#{edge_loop},.T.)"));
            let delta: [f64; 3] =
                std::array::from_fn(|j| points[corners[1]][j] - points[corners[0]][j]);
            let length = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
            let axis = self.axis(point_ids[corners[0]], normal, delta.map(|v| v / length));
            let plane = self.entity(format!("PLANE('',#{axis})"));
            face_ids.push(self.entity(format!("ADVANCED_FACE('',(#{bound}),#{plane},.T.)")));
        }
        let shell = self.entity(format!("CLOSED_SHELL('',({}))", refs(&face_ids)));
        self.entity(format!(
            "MANIFOLD_SOLID_BREP({},#{shell})",
            step_string(&b.name).expect("validated name")
        ))
    }
    fn style(&mut self, solid: usize, rgb: [f64; 3]) -> usize {
        let color = self.entity(format!(
            "COLOUR_RGB('',{},{},{})",
            real(rgb[0]),
            real(rgb[1]),
            real(rgb[2])
        ));
        let fill_color = self.entity(format!("FILL_AREA_STYLE_COLOUR('',#{color})"));
        let fill = self.entity(format!("FILL_AREA_STYLE('',(#{fill_color}))"));
        let surface_fill = self.entity(format!("SURFACE_STYLE_FILL_AREA(#{fill})"));
        let surface = self.entity(format!("SURFACE_SIDE_STYLE('',(#{surface_fill}))"));
        let usage = self.entity(format!("SURFACE_STYLE_USAGE(.BOTH.,#{surface})"));
        let assignment = self.entity(format!("PRESENTATION_STYLE_ASSIGNMENT((#{usage}))"));
        self.entity(format!("STYLED_ITEM('',(#{assignment}),#{solid})"))
    }
}
fn refs(ids: &[usize]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn write_step(name: &str, solids: &[BoxSolid]) -> Result<String, String> {
    let name = step_string(name)?;
    if solids.is_empty() || solids.len() > 257 {
        return Err("a model requires 1..257 boxes".into());
    }
    for solid in solids {
        solid.validate()?;
    }
    let mut w = Writer::default();
    let app = w.entity("APPLICATION_CONTEXT('automotive design')");
    w.entity(format!(
        "APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"
    ));
    let pc = w.entity(format!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let product = w.entity(format!("PRODUCT({name},{name},'',(#{pc}))"));
    let formation = w.entity(format!("PRODUCT_DEFINITION_FORMATION('','',#{product})"));
    let pdc = w.entity(format!(
        "PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"
    ));
    let pd = w.entity(format!(
        "PRODUCT_DEFINITION('design','',#{formation},#{pdc})"
    ));
    let pds = w.entity(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
    let mm = w.entity("(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.))");
    let rad = w.entity("(NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.))");
    let sterad = w.entity("(NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT())");
    let uncertainty = w.entity(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-7),#{mm},'distance_accuracy_value','')"
    ));
    let context=w.entity(format!("(GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty})) GLOBAL_UNIT_ASSIGNED_CONTEXT((#{mm},#{rad},#{sterad})) REPRESENTATION_CONTEXT('',''))"));
    let origin = w.point([0.; 3]);
    let axis = w.axis(origin, [0., 0., 1.], [1., 0., 0.]);
    let mut items = vec![axis];
    let mut styles = Vec::new();
    for solid in solids {
        let id = w.solid(solid);
        items.push(id);
        styles.push(w.style(id, solid.color));
    }
    let rep = w.entity(format!(
        "ADVANCED_BREP_SHAPE_REPRESENTATION({name},({}),#{context})",
        refs(&items)
    ));
    w.entity(format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{rep})"));
    w.entity(format!(
        "MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',({}),#{context})",
        refs(&styles)
    ));
    let mut text=String::from("ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('Copperpour rectangular package approximation'),'2;1');\nFILE_NAME('generated.step','1970-01-01T00:00:00',('Copperpour'),(''),'Copperpour','Copperpour','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n");
    for (i, entity) in w.entities.iter().enumerate() {
        writeln!(text, "#{}={entity};", i + 1).expect("String write");
    }
    text.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cube() -> BoxSolid {
        BoxSolid {
            name: "body".into(),
            min: [-2., -3., 0.3],
            max: [2., 3., 2.3],
            color: [0.2, 0.3, 0.4],
        }
    }
    #[test]
    fn step_box_is_deterministic_and_has_closed_shared_topology() {
        let step = write_step("O'Brien", &[cube()]).unwrap();
        assert_eq!(step, write_step("O'Brien", &[cube()]).unwrap());
        assert!(step.contains("FILE_SCHEMA(('AUTOMOTIVE_DESIGN'))"));
        assert!(step.contains("O''Brien"));
        for (kind, count) in [
            ("VERTEX_POINT(", 8),
            ("EDGE_CURVE(", 12),
            ("ORIENTED_EDGE(", 24),
            ("ADVANCED_FACE(", 6),
            ("CLOSED_SHELL(", 1),
            ("MANIFOLD_SOLID_BREP(", 1),
            ("COLOUR_RGB(", 1),
        ] {
            assert_eq!(step.matches(kind).count(), count, "{kind}")
        }
        let mut use_counts = BTreeMap::<usize, Vec<bool>>::new();
        for line in step.lines().filter(|l| l.contains("=ORIENTED_EDGE(")) {
            let after = line.split("*,*,#").nth(1).unwrap();
            let (edge, orientation) = after.split_once(',').unwrap();
            use_counts
                .entry(edge.parse().unwrap())
                .or_default()
                .push(orientation.starts_with(".T."));
        }
        assert_eq!(use_counts.len(), 12);
        for uses in use_counts.values() {
            assert_eq!(uses.len(), 2);
            assert_ne!(uses[0], uses[1]);
        }
        // Every entity reference resolves. Numbers in strings are deliberately
        // absent from this fixture, making this an independent graph check.
        let mut ids = std::collections::BTreeSet::new();
        let mut references = Vec::new();
        for line in step.lines().filter(|l| l.starts_with('#')) {
            let (id, entity) = line.split_once('=').unwrap();
            assert!(ids.insert(id[1..].parse::<usize>().unwrap()));
            for tail in entity.split('#').skip(1) {
                let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
                references.push(digits.parse::<usize>().unwrap());
            }
        }
        assert!(references.iter().all(|id| ids.contains(id)));
    }
    #[test]
    fn step_faces_are_planar_outward_and_edge_loops_are_contiguous() {
        fn refs(entity: &str) -> Vec<usize> {
            entity
                .split('#')
                .skip(1)
                .map(|tail| {
                    tail.chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse()
                        .unwrap()
                })
                .collect()
        }
        fn xyz(entity: &str) -> [f64; 3] {
            let numbers = entity.split_once(",(").unwrap().1.trim_end_matches(')');
            let v: Vec<f64> = numbers.split(',').map(|n| n.parse().unwrap()).collect();
            [v[0], v[1], v[2]]
        }
        fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
            (0..3).map(|i| a[i] * b[i]).sum()
        }
        let b = cube();
        let centre: [f64; 3] = std::array::from_fn(|i| (b.min[i] + b.max[i]) / 2.);
        let mut w = Writer::default();
        w.solid(&b);
        let entity = |id: usize| w.entities[id - 1].as_str();
        let vertex_point = |id: usize| xyz(entity(refs(entity(id))[0]));
        for face in w
            .entities
            .iter()
            .filter(|s| s.starts_with("ADVANCED_FACE("))
        {
            let face_refs = refs(face);
            let bound = entity(face_refs[0]);
            let edge_loop = entity(refs(bound)[0]);
            let oriented = refs(edge_loop);
            let mut points = Vec::new();
            let mut ends = Vec::new();
            for id in oriented {
                let oriented = entity(id);
                let edge = refs(entity(refs(oriented)[0]));
                let (start, end) = if oriented.ends_with(".T.)") {
                    (edge[0], edge[1])
                } else {
                    (edge[1], edge[0])
                };
                points.push(vertex_point(start));
                ends.push(vertex_point(end));
            }
            for i in 0..4 {
                assert_eq!(ends[i], points[(i + 1) % 4]);
            }
            let axis = entity(refs(entity(face_refs[1]))[0]);
            let axis_refs = refs(axis);
            let normal = xyz(entity(axis_refs[1]));
            let origin = xyz(entity(axis_refs[0]));
            let u: [f64; 3] = std::array::from_fn(|i| points[1][i] - points[0][i]);
            let v: [f64; 3] = std::array::from_fn(|i| points[2][i] - points[1][i]);
            let cross = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            assert!(
                dot(cross, normal) > 0.,
                "face loop disagrees with surface normal"
            );
            assert!(
                dot(std::array::from_fn(|i| points[0][i] - centre[i]), normal) > 0.,
                "inward face"
            );
            for p in points {
                assert!(dot(std::array::from_fn(|i| p[i] - origin[i]), normal).abs() < 1e-12);
            }
        }
    }
    #[test]
    fn step_minimum_dimensions_at_nonzero_origins_do_not_fail_from_roundoff() {
        let b = BoxSolid {
            name: "minimum".into(),
            min: [1., 2., 3.],
            max: [1.001, 2.001, 3.001],
            color: [0.5; 3],
        };
        assert!(write_step("minimum", std::slice::from_ref(&b)).is_ok());
        let mut too_small = b;
        too_small.max[0] = 1.000999;
        assert!(write_step("too small", &[too_small]).is_err());
    }
    #[test]
    fn step_refuses_bad_dimensions_colors_names_and_counts() {
        assert!(write_step("empty", &[]).is_err());
        assert!(write_step("many", &vec![cube(); 258]).is_err());
        for x in [f64::NAN, f64::INFINITY, -2., -1.9995, 10001.] {
            let mut b = cube();
            b.max[0] = x;
            assert!(write_step("bad", &[b]).is_err());
        }
        let mut b = cube();
        b.color[0] = f64::NAN;
        assert!(write_step("bad", &[b]).is_err());
        for name in ["", "line\nbreak", "unicode☃", "escape\\X2\\"] {
            assert!(write_step(name, &[cube()]).is_err());
        }
    }
}
