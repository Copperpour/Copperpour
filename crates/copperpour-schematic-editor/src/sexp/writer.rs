use super::SexpNode;

/// KiCAD's own writer indents with tabs; this crate historically used two
/// spaces, so every typed-model write reindented the whole sheet and turned a
/// one-symbol edit into a whole-file diff (#210).
pub const KICAD_INDENT: &str = "\t";
/// What this crate emitted before [`write_with_indent`] existed.
pub const LEGACY_INDENT: &str = "  ";

pub fn write(node: &SexpNode) -> String {
    write_with_indent(node, LEGACY_INDENT)
}

/// As [`write`], but indenting each level with `indent`.
///
/// Callers that loaded a file should pass the indent that file already used,
/// so a targeted edit stays a targeted diff.
pub fn write_with_indent(node: &SexpNode, indent: &str) -> String {
    let mut buf = String::with_capacity(16384);
    write_node(node, &mut buf, 0, indent);
    buf.push('\n');
    buf
}

/// The indentation unit a KiCAD file already uses, from its first indented
/// line: a tab, or the run of spaces that opens it. Falls back to KiCAD's own
/// tab for a file with nothing to learn from.
pub fn detect_indent(source: &str) -> String {
    for line in source.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() {
            continue;
        }
        let lead = &line[..line.len() - trimmed.len()];
        if lead.starts_with('\t') {
            return "\t".to_string();
        }
        if !lead.is_empty() {
            return lead.to_string();
        }
    }
    KICAD_INDENT.to_string()
}

fn write_node(node: &SexpNode, buf: &mut String, depth: usize, indent: &str) {
    match node {
        SexpNode::Atom(s) => buf.push_str(s),
        SexpNode::Str(s) => {
            buf.push('"');
            for c in s.chars() {
                match c {
                    '"' => buf.push_str("\\\""),
                    '\\' => buf.push_str("\\\\"),
                    '\n' => buf.push_str("\\n"),
                    '\t' => buf.push_str("\\t"),
                    '\r' => buf.push_str("\\r"),
                    c => buf.push(c),
                }
            }
            buf.push('"');
        }
        SexpNode::List(children) => {
            if children.is_empty() {
                buf.push_str("()");
                return;
            }

            let has_list_child = children.iter().skip(1).any(|c| c.is_list());

            buf.push('(');

            if depth == 0 {
                // Root: tag on same line, each child on its own indented line.
                for (i, child) in children.iter().enumerate() {
                    if i == 0 {
                        write_node(child, buf, 1, indent);
                    } else {
                        buf.push('\n');
                        write_indent(buf, 1, indent);
                        write_node(child, buf, 1, indent);
                    }
                }
                buf.push('\n');
            } else if has_list_child {
                // Multi-line: scalars inline after tag, sub-lists on new lines.
                for (i, child) in children.iter().enumerate() {
                    if i == 0 {
                        write_node(child, buf, depth + 1, indent);
                    } else if child.is_list() {
                        buf.push('\n');
                        write_indent(buf, depth + 1, indent);
                        write_node(child, buf, depth + 1, indent);
                    } else {
                        buf.push(' ');
                        write_node(child, buf, depth + 1, indent);
                    }
                }
                // KiCAD closes a node that has list children on its own line at
                // the node's own indent. Collapsing it onto the last child —
                // `(uuid "j1"))` — differs on every such node in the file, which
                // is most of them (#210).
                buf.push('\n');
                write_indent(buf, depth, indent);
            } else {
                // All scalars: single line.
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        buf.push(' ');
                    }
                    write_node(child, buf, depth + 1, indent);
                }
            }

            buf.push(')');
        }
    }
}

fn write_indent(buf: &mut String, depth: usize, indent: &str) {
    for _ in 0..depth {
        buf.push_str(indent);
    }
}
