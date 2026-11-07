use crate::sexp::{atom, qstr, tagged, SexpNode};

// ---- float formatting -------------------------------------------------------

pub fn fmt_f64(v: f64) -> String {
    let s = format!("{:.6}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

// ---- At ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct At {
    pub x: f64,
    pub y: f64,
    pub rotation: Option<f64>,
}

impl At {
    pub fn new(x: f64, y: f64) -> Self {
        At {
            x,
            y,
            rotation: None,
        }
    }

    pub fn with_rotation(x: f64, y: f64, rotation: f64) -> Self {
        At {
            x,
            y,
            rotation: Some(rotation),
        }
    }

    pub fn from_sexp(node: &SexpNode) -> Option<Self> {
        let s = node.scalar_args();
        let x: f64 = s.first()?.parse().ok()?;
        let y: f64 = s.get(1)?.parse().ok()?;
        let rotation = s.get(2).and_then(|v| v.parse().ok());
        Some(At { x, y, rotation })
    }

    pub fn to_sexp(&self) -> SexpNode {
        let mut args = vec![atom(fmt_f64(self.x)), atom(fmt_f64(self.y))];
        if let Some(r) = self.rotation {
            args.push(atom(fmt_f64(r)));
        }
        tagged("at", args)
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
    }

    pub fn distance_to(&self, other: &At) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

// ---- Property ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Property {
    /// KiCad 10 writes a bare `private` token before the name of a field that
    /// stays out of the BOM and netlist: `(property private "MPN" "X" …)`.
    pub private: bool,
    pub name: String,
    pub value: String,
    /// Trailing sub-nodes after name+value (at, effects, show_pin_number, …).
    pub sub_nodes: Vec<SexpNode>,
}

/// Shift every `(at …)` among `nodes` by `(dx, dy)`.
///
/// A field's `(at …)` is absolute in `.kicad_sch`, not an offset from its
/// owner, so moving a symbol, sheet or label has to move its fields too. One
/// that will not parse is left exactly as it was.
pub fn translate_field_ats(nodes: &mut [SexpNode], dx: f64, dy: f64) {
    for node in nodes {
        if node.tag() == Some("at") {
            if let Some(mut at) = At::from_sexp(node) {
                at.translate(dx, dy);
                *node = at.to_sexp();
            }
        }
    }
}

impl Property {
    /// Move the field by `(dx, dy)`; see [`translate_field_ats`].
    pub fn translate(&mut self, dx: f64, dy: f64) {
        translate_field_ats(&mut self.sub_nodes, dx, dy);
    }

    /// Whether the field's text is hidden, as KiCad 10's reader takes it: on
    /// the property, where KiCad 10 writes it, `(hide yes)` (`parseSchField`
    /// reads a yes or a no); inside its `(effects …)` also `(hide)` and a bare
    /// `hide`, as older files have it (`parseEDA_TEXT`, `parseMaybeAbsentBool`).
    pub fn is_hidden(&self) -> bool {
        fn hides_in_effects(node: &SexpNode) -> bool {
            match node {
                SexpNode::Atom(token) => token == "hide",
                _ => node.tag() == Some("hide") && node.value() != Some("no"),
            }
        }
        self.sub_nodes.iter().any(|node| {
            (node.tag() == Some("hide") && node.value() == Some("yes"))
                || (node.tag() == Some("effects") && node.args().iter().any(hides_in_effects))
        })
    }

    /// Hide or show the field's text. A hidden field gets `(hide yes)` right
    /// after its `(at …)`, as KiCad writes it; showing it removes the flag
    /// wherever it stands. Nothing else about the field changes.
    pub fn set_hidden(&mut self, hidden: bool) {
        fn hides(node: &SexpNode) -> bool {
            match node {
                SexpNode::Atom(token) => token == "hide",
                _ => node.tag() == Some("hide"),
            }
        }
        for node in &mut self.sub_nodes {
            if node.tag() != Some("effects") {
                continue;
            }
            if let SexpNode::List(children) = node {
                children.retain(|child| !hides(child));
            }
        }
        self.sub_nodes.retain(|node| node.tag() != Some("hide"));
        if hidden {
            let after_at = self
                .sub_nodes
                .iter()
                .position(|node| node.tag() == Some("at"))
                .map_or(0, |i| i + 1);
            self.sub_nodes
                .insert(after_at, tagged("hide", vec![atom("yes")]));
        }
    }

    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Property {
            private: false,
            name: name.into(),
            value: value.into(),
            sub_nodes: vec![],
        }
    }

    pub fn from_sexp(node: &SexpNode) -> Option<Self> {
        let mut args = node.args();
        // Only a bare atom is the flag; a quoted "private" is a field name.
        let private = matches!(args.first(), Some(SexpNode::Atom(token)) if token == "private");
        if private {
            args = &args[1..];
        }
        let name = args.first()?.text()?.to_owned();
        let value = args.get(1)?.text()?.to_owned();
        let sub_nodes = args
            .iter()
            .skip(2)
            .filter(|n| n.is_list())
            .cloned()
            .collect();
        Some(Property {
            private,
            name,
            value,
            sub_nodes,
        })
    }

    pub fn to_sexp(&self) -> SexpNode {
        let mut children = vec![atom("property")];
        if self.private {
            children.push(atom("private"));
        }
        children.push(qstr(self.name.clone()));
        children.push(qstr(self.value.clone()));
        children.extend(self.sub_nodes.iter().cloned());
        SexpNode::List(children)
    }
}

// ---- Effects (preserved verbatim) ------------------------------------------

#[derive(Debug, Clone)]
pub struct Effects(pub SexpNode);

impl Effects {
    pub fn from_sexp(node: &SexpNode) -> Option<Self> {
        Some(Effects(node.clone()))
    }
    pub fn to_sexp(&self) -> SexpNode {
        self.0.clone()
    }
}

// ---- Stroke (preserved verbatim) -------------------------------------------

#[derive(Debug, Clone)]
pub struct Stroke(pub SexpNode);

impl Stroke {
    pub fn from_sexp(node: &SexpNode) -> Option<Self> {
        Some(Stroke(node.clone()))
    }
    pub fn to_sexp(&self) -> SexpNode {
        self.0.clone()
    }
}

// ---- ChangeSet --------------------------------------------------------------

/// A human-readable record of mutations made to a schematic, suitable for
/// returning as an MCP tool response.
#[derive(Debug, Default, Clone)]
pub struct ChangeSet {
    changes: Vec<String>,
}

impl ChangeSet {
    pub fn new() -> Self {
        ChangeSet::default()
    }

    pub fn record(&mut self, msg: impl Into<String>) {
        self.changes.push(msg.into());
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// All recorded changes as a newline-joined string.
    pub fn summary(&self) -> String {
        self.changes.join("\n")
    }

    pub fn changes(&self) -> &[String] {
        &self.changes
    }
}

impl std::fmt::Display for ChangeSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary())
    }
}

#[cfg(test)]
mod private_property_tests {
    use super::*;
    use crate::sexp::parser::parse;
    use crate::sexp::writer::write;

    fn parse_prop(source: &str) -> Property {
        Property::from_sexp(&parse(source).unwrap()).unwrap()
    }

    #[test]
    fn a_private_property_keeps_its_name_value_and_flag() {
        let property = parse_prop("(property private \"MPN\" \"X-1\" (at 1 2 0))");
        assert_eq!(property.name, "MPN");
        assert_eq!(property.value, "X-1");
        assert!(property.private);
        assert_eq!(property.sub_nodes.len(), 1);
    }

    #[test]
    fn a_private_property_round_trips() {
        let source = "(property private \"MPN\" \"X-1\" (at 1 2 0))";
        let written = write(&parse_prop(source).to_sexp());
        let again = parse_prop(&written);
        assert!(again.private, "{written}");
        assert_eq!(again.name, "MPN", "{written}");
        assert_eq!(again.value, "X-1", "{written}");
        let words: Vec<&str> = written.split_whitespace().collect();
        assert_eq!(
            &words[..3],
            ["(property", "private", "\"MPN\""],
            "{written}"
        );
    }

    #[test]
    fn a_field_named_private_is_not_a_flag() {
        let property = parse_prop("(property \"private\" \"X\")");
        assert_eq!(property.name, "private");
        assert!(!property.private);
        let written = write(&property.to_sexp());
        let words: Vec<&str> = written.split_whitespace().collect();
        assert_eq!(&words[..2], ["(property", "\"private\""], "{written}");
        assert!(!parse_prop(&written).private, "{written}");
    }
}

#[cfg(test)]
mod hidden_property_tests {
    use super::*;
    use crate::sexp::parser::parse;
    use crate::sexp::writer::write;

    fn parse_prop(source: &str) -> Property {
        Property::from_sexp(&parse(source).unwrap()).unwrap()
    }

    fn words(property: &Property) -> String {
        write(&property.to_sexp()).split_whitespace().collect()
    }

    /// KiCad 10 writes only `(hide yes)` on the property. The other forms
    /// are written here: those inside `(effects …)` are what its reader takes
    /// from older files, and a bare `hide` or `(hide)` on the property is one
    /// it refuses, which therefore hides nothing.
    #[test]
    fn a_field_is_hidden_where_kicad_writes_the_flag_and_where_it_reads_it() {
        let hidden = [
            "(property \"F\" \"v\" (at 1 2 0) (hide yes) (show_name no))",
            "(property \"F\" \"v\" (at 1 2 0) (effects (font (size 1 1)) (hide yes)))",
            "(property \"F\" \"v\" (at 1 2 0) (effects (font (size 1 1)) (hide)))",
            "(property \"F\" \"v\" (at 1 2 0) (effects (font (size 1 1)) hide))",
        ];
        for source in hidden {
            assert!(parse_prop(source).is_hidden(), "{source}");
        }
        let shown = [
            "(property \"F\" \"v\" (at 1 2 0))",
            "(property \"F\" \"v\" (at 1 2 0) (hide no))",
            "(property \"F\" \"v\" (at 1 2 0) (hide))",
            "(property \"F\" \"v\" (at 1 2 0) hide)",
            "(property \"F\" \"v\" (at 1 2 0) (effects (font (size 1 1)) (hide no)))",
        ];
        for source in shown {
            assert!(!parse_prop(source).is_hidden(), "{source}");
        }
    }

    #[test]
    fn hiding_puts_the_flag_right_after_the_position_and_nothing_else_moves() {
        let mut property = parse_prop(
            "(property \"F\" \"v\" (at 1 2 0) (show_name no) (effects (font (size 1 1))))",
        );
        property.set_hidden(true);
        assert!(property.is_hidden());
        assert_eq!(
            words(&property),
            "(property\"F\"\"v\"(at120)(hideyes)(show_nameno)(effects(font(size11))))"
        );
    }

    #[test]
    fn showing_takes_the_flag_out_wherever_it_stands() {
        let mut property = parse_prop(
            "(property \"F\" \"v\" (at 1 2 0) (hide yes) (effects (font (size 1 1)) (hide yes)))",
        );
        property.set_hidden(false);
        assert!(!property.is_hidden());
        assert_eq!(
            words(&property),
            "(property\"F\"\"v\"(at120)(effects(font(size11))))"
        );
    }

    #[test]
    fn showing_takes_out_the_bare_hide_of_an_older_file() {
        let mut property =
            parse_prop("(property \"F\" \"v\" (at 1 2 0) (effects (font (size 1 1)) hide))");
        assert!(property.is_hidden());
        property.set_hidden(false);
        assert!(!property.is_hidden());
        assert_eq!(
            words(&property),
            "(property\"F\"\"v\"(at120)(effects(font(size11))))"
        );
    }

    #[test]
    fn hiding_twice_writes_one_flag() {
        let mut property = parse_prop("(property \"F\" \"v\" (at 1 2 0))");
        property.set_hidden(true);
        property.set_hidden(true);
        assert_eq!(write(&property.to_sexp()).matches("hide").count(), 1);
    }
}
