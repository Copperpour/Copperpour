pub mod parser;
pub mod writer;

/// A node in a KiCad S-expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum SexpNode {
    /// Unquoted token — keywords, numbers, booleans (`yes`/`no`/`true`/`false`).
    Atom(String),
    /// Double-quoted string.
    Str(String),
    /// `(child ...)` where `children[0]` is normally an Atom tag.
    List(Vec<SexpNode>),
}

impl SexpNode {
    // ---- structural ---------------------------------------------------------

    pub fn tag(&self) -> Option<&str> {
        if let SexpNode::List(c) = self {
            if let Some(SexpNode::Atom(s)) = c.first() {
                return Some(s.as_str());
            }
        }
        None
    }

    pub fn children(&self) -> &[SexpNode] {
        match self {
            SexpNode::List(c) => c.as_slice(),
            _ => &[],
        }
    }

    /// Children after the tag.
    pub fn args(&self) -> &[SexpNode] {
        let c = self.children();
        if c.is_empty() {
            &[]
        } else {
            &c[1..]
        }
    }

    pub fn find(&self, tag: &str) -> Option<&SexpNode> {
        self.args().iter().find(|c| c.tag() == Some(tag))
    }

    pub fn find_mut(&mut self, tag: &str) -> Option<&mut SexpNode> {
        if let SexpNode::List(c) = self {
            c.iter_mut().skip(1).find(|c| c.tag() == Some(tag))
        } else {
            None
        }
    }

    pub fn find_all(&self, tag: &str) -> Vec<&SexpNode> {
        self.args()
            .iter()
            .filter(|c| c.tag() == Some(tag))
            .collect()
    }

    // ---- value accessors ----------------------------------------------------

    pub fn text(&self) -> Option<&str> {
        match self {
            SexpNode::Atom(s) | SexpNode::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// First scalar (non-List) child after the tag.
    pub fn value(&self) -> Option<&str> {
        match self {
            SexpNode::List(c) => c.iter().skip(1).find_map(|n| n.text()),
            SexpNode::Atom(s) | SexpNode::Str(s) => Some(s.as_str()),
        }
    }

    pub fn scalar_args(&self) -> Vec<&str> {
        self.args().iter().filter_map(|c| c.text()).collect()
    }

    pub fn float_value(&self) -> Option<f64> {
        self.value()?.parse().ok()
    }

    pub fn bool_value(&self) -> Option<bool> {
        match self.value()? {
            "yes" | "true" => Some(true),
            "no" | "false" => Some(false),
            _ => None,
        }
    }

    // ---- shorthand getters --------------------------------------------------

    pub fn get_value(&self, tag: &str) -> Option<&str> {
        self.find(tag)?.value()
    }

    pub fn get_float(&self, tag: &str) -> Option<f64> {
        self.find(tag)?.float_value()
    }

    pub fn get_bool(&self, tag: &str) -> Option<bool> {
        self.find(tag)?.bool_value()
    }

    // ---- type tests ---------------------------------------------------------

    pub fn is_list(&self) -> bool {
        matches!(self, SexpNode::List(_))
    }
}

// ---- convenience constructors -----------------------------------------------

pub fn atom(s: impl Into<String>) -> SexpNode {
    SexpNode::Atom(s.into())
}
pub fn qstr(s: impl Into<String>) -> SexpNode {
    SexpNode::Str(s.into())
}
pub fn tagged(tag: &str, args: Vec<SexpNode>) -> SexpNode {
    let mut v = vec![atom(tag)];
    v.extend(args);
    SexpNode::List(v)
}
