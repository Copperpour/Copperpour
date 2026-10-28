use super::SexpNode;
use crate::error::{Error, Result};

pub fn parse(input: &str) -> Result<SexpNode> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    parse_node(bytes, &mut pos)
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_node(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    skip_ws(b, pos);
    if *pos >= b.len() {
        return Err(Error::Parse {
            pos: *pos,
            msg: "Unexpected end of input".into(),
        });
    }
    match b[*pos] {
        b'(' => parse_list(b, pos),
        b'"' => parse_string(b, pos),
        _ => parse_atom(b, pos),
    }
}

fn parse_list(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    *pos += 1; // consume '('
    let mut children = Vec::new();
    loop {
        skip_ws(b, pos);
        if *pos >= b.len() {
            return Err(Error::Parse {
                pos: *pos,
                msg: "Unclosed parenthesis".into(),
            });
        }
        if b[*pos] == b')' {
            *pos += 1;
            break;
        }
        children.push(parse_node(b, pos)?);
    }
    Ok(SexpNode::List(children))
}

fn parse_string(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    *pos += 1; // consume '"'
    let mut s: Vec<u8> = Vec::new();
    loop {
        if *pos >= b.len() {
            return Err(Error::Parse {
                pos: *pos,
                msg: "Unterminated string".into(),
            });
        }
        match b[*pos] {
            b'"' => {
                *pos += 1;
                break;
            }
            b'\\' => {
                *pos += 1;
                if *pos >= b.len() {
                    return Err(Error::Parse {
                        pos: *pos,
                        msg: "Unterminated escape".into(),
                    });
                }
                match b[*pos] {
                    b'"' => s.push(b'"'),
                    b'\\' => s.push(b'\\'),
                    b'n' => s.push(b'\n'),
                    b't' => s.push(b'\t'),
                    b'r' => s.push(b'\r'),
                    c => {
                        s.push(b'\\');
                        s.push(c);
                    }
                }
                *pos += 1;
            }
            c => {
                s.push(c);
                *pos += 1;
            }
        }
    }
    Ok(SexpNode::Str(String::from_utf8_lossy(&s).into_owned()))
}

fn parse_atom(b: &[u8], pos: &mut usize) -> Result<SexpNode> {
    let start = *pos;
    while *pos < b.len() && !matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'"') {
        *pos += 1;
    }
    if *pos == start {
        return Err(Error::Parse {
            pos: *pos,
            msg: format!("Unexpected character: '{}'", b[*pos] as char),
        });
    }
    Ok(SexpNode::Atom(
        String::from_utf8_lossy(&b[start..*pos]).into_owned(),
    ))
}
