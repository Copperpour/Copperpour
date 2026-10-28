use crate::error::{Error, Result};
use crate::sexp::{atom, qstr, tagged, SexpNode};
use crate::types::{fmt_f64, Stroke};

#[derive(Debug, Clone)]
pub struct Wire {
    pub start: (f64, f64),
    pub end: (f64, f64),
    pub uuid: String,
    pub stroke: Option<Stroke>,
}

impl Wire {
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Wire {
            start: (x1, y1),
            end: (x2, y2),
            uuid: uuid::Uuid::new_v4().to_string(),
            stroke: None,
        }
    }

    pub fn from_sexp(node: &SexpNode) -> Result<Self> {
        let pts = node.find("pts").ok_or(Error::MissingField("pts"))?;
        let xys: Vec<&SexpNode> = pts.find_all("xy");

        let parse_xy = |n: &SexpNode| -> Option<(f64, f64)> {
            let s = n.scalar_args();
            let x = s.first()?.parse().ok()?;
            let y = s.get(1)?.parse().ok()?;
            Some((x, y))
        };

        let start = xys.first().and_then(|n| parse_xy(n)).unwrap_or((0.0, 0.0));
        let end = xys.get(1).and_then(|n| parse_xy(n)).unwrap_or((0.0, 0.0));
        let uuid = node.get_value("uuid").unwrap_or("").to_owned();
        let stroke = node.find("stroke").and_then(Stroke::from_sexp);

        Ok(Wire {
            start,
            end,
            uuid,
            stroke,
        })
    }

    pub fn to_sexp(&self) -> SexpNode {
        let (x1, y1) = self.start;
        let (x2, y2) = self.end;
        let pts = tagged(
            "pts",
            vec![
                tagged("xy", vec![atom(fmt_f64(x1)), atom(fmt_f64(y1))]),
                tagged("xy", vec![atom(fmt_f64(x2)), atom(fmt_f64(y2))]),
            ],
        );
        let mut c = vec![atom("wire"), pts];
        if let Some(s) = &self.stroke {
            c.push(s.to_sexp());
        }
        c.push(tagged("uuid", vec![qstr(self.uuid.clone())]));
        SexpNode::List(c)
    }

    pub fn length(&self) -> f64 {
        let (dx, dy) = (self.end.0 - self.start.0, self.end.1 - self.start.1);
        (dx * dx + dy * dy).sqrt()
    }

    pub fn is_horizontal(&self) -> bool {
        (self.start.1 - self.end.1).abs() < 1e-9
    }
    pub fn is_vertical(&self) -> bool {
        (self.start.0 - self.end.0).abs() < 1e-9
    }

    pub fn midpoint(&self) -> (f64, f64) {
        (
            (self.start.0 + self.end.0) / 2.0,
            (self.start.1 + self.end.1) / 2.0,
        )
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.start.0 += dx;
        self.start.1 += dy;
        self.end.0 += dx;
        self.end.1 += dy;
    }

    pub fn touches(&self, x: f64, y: f64) -> bool {
        let eq = |p: (f64, f64)| (p.0 - x).abs() < 1e-9 && (p.1 - y).abs() < 1e-9;
        eq(self.start) || eq(self.end)
    }
}

// ---- WireCollection ---------------------------------------------------------

pub struct WireCollection {
    wires: Vec<Wire>,
}

impl WireCollection {
    pub fn new(wires: Vec<Wire>) -> Self {
        WireCollection { wires }
    }

    pub fn len(&self) -> usize {
        self.wires.len()
    }
    pub fn is_empty(&self) -> bool {
        self.wires.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Wire> {
        self.wires.iter()
    }
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Wire> {
        self.wires.iter_mut()
    }
    pub fn get(&self, i: usize) -> Option<&Wire> {
        self.wires.get(i)
    }
    pub fn get_mut(&mut self, i: usize) -> Option<&mut Wire> {
        self.wires.get_mut(i)
    }
    pub fn push(&mut self, w: Wire) {
        self.wires.push(w);
    }
    pub fn as_slice(&self) -> &[Wire] {
        &self.wires
    }
    pub fn into_vec(self) -> Vec<Wire> {
        self.wires
    }

    pub fn remove_by_uuid(&mut self, uuid: &str) -> Option<Wire> {
        let idx = self.wires.iter().position(|w| w.uuid == uuid)?;
        Some(self.wires.remove(idx))
    }
    pub fn retain<F: FnMut(&Wire) -> bool>(&mut self, f: F) {
        self.wires.retain(f);
    }

    pub fn at_point(&self, x: f64, y: f64) -> Vec<&Wire> {
        self.wires.iter().filter(|w| w.touches(x, y)).collect()
    }

    pub fn within_circle(&self, x: f64, y: f64, radius: f64) -> Vec<&Wire> {
        self.wires
            .iter()
            .filter(|w| {
                let (mx, my) = w.midpoint();
                dist(mx, my, x, y) <= radius
            })
            .collect()
    }

    pub fn within_rectangle(&self, x1: f64, y1: f64, x2: f64, y2: f64) -> Vec<&Wire> {
        let (xmin, xmax) = (x1.min(x2), x1.max(x2));
        let (ymin, ymax) = (y1.min(y2), y1.max(y2));
        let in_r = |p: (f64, f64)| p.0 >= xmin && p.0 <= xmax && p.1 >= ymin && p.1 <= ymax;
        self.wires
            .iter()
            .filter(|w| in_r(w.start) || in_r(w.end))
            .collect()
    }
}

impl std::ops::Index<usize> for WireCollection {
    type Output = Wire;
    fn index(&self, i: usize) -> &Wire {
        &self.wires[i]
    }
}
impl<'a> IntoIterator for &'a WireCollection {
    type Item = &'a Wire;
    type IntoIter = std::slice::Iter<'a, Wire>;
    fn into_iter(self) -> Self::IntoIter {
        self.wires.iter()
    }
}

fn dist(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (ax - bx, ay - by);
    (dx * dx + dy * dy).sqrt()
}
