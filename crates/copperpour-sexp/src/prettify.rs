//! KiCad's S-expression layout: a port of `KICAD_FORMAT::Prettify`
//! (`common/io/kicad/kicad_io_utils.cpp`, KiCad 10.0.8) in its normal mode, the
//! one the board and footprint writers use. KiCad prints a file compactly and
//! then runs this pass over it, so a writer that prints the same tokens in the
//! same order and passes them through `prettify` produces KiCad's bytes.

/// `source` laid out as KiCad lays out its files: every list on its own line,
/// indented by one tab per depth, a list without sub-lists kept on its line,
/// runs of `(xy …)` points sharing a line up to column 99, and a newline at
/// the end. Whitespace between tokens may be anything; it is normalised.
///
/// Byte for byte the same as KiCad's pass, which works on bytes too: columns
/// count bytes, and a quoted string is copied unchanged.
pub fn prettify(source: &str) -> String {
    const INDENT: u8 = b'\t';
    // Runs of (xy …) lists stay on one line until this column.
    const XY_COLUMN_LIMIT: usize = 99;
    // Whitespace past this column inside a list becomes a newline.
    const TOKEN_WRAP_COLUMN: usize = 72;

    let bytes = source.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + bytes.len() / 4);
    let indent = |out: &mut Vec<u8>, depth: usize| out.extend(std::iter::repeat_n(INDENT, depth));
    let is_space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
    let next_non_space = |from: usize| bytes[from..].iter().copied().find(|&b| !is_space(b));
    let is_xy = |at: usize| bytes.get(at + 1..at + 4) == Some(b"xy ".as_slice());

    let mut depth = 0usize;
    let mut last_non_space = 0u8;
    let mut in_quote = false;
    let mut inserted_space = false;
    let mut in_multi_line_list = false;
    let mut in_xy = false;
    let mut column = 0usize;
    let mut backslashes = 0usize;

    for (at, &byte) in bytes.iter().enumerate() {
        let next = next_non_space(at);
        if is_space(byte) && !in_quote {
            if !inserted_space
                && depth > 0
                && last_non_space != b'('
                && next != Some(b')')
                && next != Some(b'(')
            {
                if in_xy || column < TOKEN_WRAP_COLUMN {
                    out.push(b' ');
                    column += 1;
                } else {
                    out.push(b'\n');
                    indent(&mut out, depth);
                    column = depth;
                    in_multi_line_list = true;
                }
                inserted_space = true;
            }
            continue;
        }
        inserted_space = false;
        if byte == b'(' && !in_quote {
            let current_is_xy = is_xy(at);
            if out.is_empty() {
                out.push(b'(');
                column += 1;
            } else if in_xy && current_is_xy && column < XY_COLUMN_LIMIT {
                out.extend_from_slice(b" (");
                column += 2;
            } else {
                out.push(b'\n');
                indent(&mut out, depth);
                out.push(b'(');
                column = depth + 1;
            }
            in_xy = current_is_xy;
            depth += 1;
        } else if byte == b')' && !in_quote {
            depth = depth.saturating_sub(1);
            if last_non_space == b')' || in_multi_line_list {
                out.push(b'\n');
                indent(&mut out, depth);
                out.push(b')');
                column = depth + 1;
                in_multi_line_list = false;
            } else {
                out.push(b')');
                column += 1;
            }
        } else {
            // A quote ends a string unless an odd number of backslashes escapes it.
            if byte == b'\\' {
                backslashes += 1;
            } else if byte == b'"' && backslashes.is_multiple_of(2) {
                in_quote = !in_quote;
            }
            if byte != b'\\' {
                backslashes = 0;
            }
            out.push(byte);
            column += 1;
        }
        last_non_space = byte;
    }
    out.push(b'\n');
    String::from_utf8(out).expect("prettify keeps the bytes of a UTF-8 source")
}

#[cfg(test)]
mod tests {
    use super::prettify;

    #[test]
    fn lists_go_on_their_own_lines_and_leaf_lists_stay_inline() {
        assert_eq!(
            prettify(r#"(footprint "R" (version 20260206) (pad "1" smd rect (at 0 0) (size 1 1)))"#),
            "(footprint \"R\"\n\t(version 20260206)\n\t(pad \"1\" smd rect\n\t\t(at 0 0)\n\t\t(size 1 1)\n\t)\n)\n"
        );
    }

    #[test]
    fn points_share_a_line_until_column_99() {
        let points: String = (0..12).map(|i| format!("(xy {i}.25 -{i}.5) ")).collect();
        let pretty = prettify(&format!("(fp_poly (pts {points}) (layer \"F.Fab\"))"));
        let lines: Vec<&str> = pretty.lines().collect();
        assert_eq!(lines[1], "\t(pts");
        assert!(
            lines[2].starts_with("\t\t(xy 0.25 -0.5) (xy 1.25 -1.5)"),
            "{pretty}"
        );
        assert!(lines[3].starts_with("\t\t(xy "), "the run wraps: {pretty}");
        assert!(lines[2].len() < 110, "{pretty}");
    }

    /// KiCad's rule for long token runs such as a group's members: whitespace
    /// at or past column 72 becomes a newline indented one level deeper, and
    /// the list then closes on its own line.
    #[test]
    fn a_long_run_of_tokens_wraps_after_column_72() {
        let members = ["\"0123456789\""; 8].join(" ");
        assert_eq!(
            prettify(&format!("(members {members})")),
            format!(
                "(members {}\n\t{}\n)\n",
                ["\"0123456789\""; 5].join(" "),
                ["\"0123456789\""; 3].join(" ")
            )
        );
    }

    #[test]
    fn quoted_text_is_copied_unchanged() {
        assert_eq!(
            prettify(r#"(property "Value" "a (b)  \"c\\" (at 0 0))"#),
            "(property \"Value\" \"a (b)  \\\"c\\\\\"\n\t(at 0 0)\n)\n"
        );
    }
}
