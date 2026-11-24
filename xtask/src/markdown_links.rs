//! Relative Markdown link scanning for repository documentation checks and
//! `cargo xtask collect-api-migrations`.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// A relative link found in a Markdown file, ignoring external URLs and
/// same-file `#fragment` links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeLink {
    /// 1-based line number in the source document.
    pub line: usize,
    /// Byte range of the raw destination (excluding any optional title) in the
    /// source text.
    pub span: Range<usize>,
    /// The raw destination string (may include `#fragment` or `?query`).
    pub target: String,
    /// The path portion of `target`, before any `#` or `?`.
    pub path: String,
}

/// Relative links that are intentionally materialized only when bundled assets
/// are installed (not stored beside the source Markdown in the repository).
#[cfg(test)]
const INSTALL_TIME_LINKS: &[(&str, &str)] = &[(
    "crates/copperpour/assets/skills/copperpour/SKILL.md",
    "references/reliability-contract.md",
)];

/// Extracts every relative Markdown link `[...](target)` (and image
/// `![...](target)`) from `text`, skipping fenced code blocks and inline code
/// spans.
pub fn relative_links(text: &str) -> Vec<RelativeLink> {
    let mut links = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut offset = 0usize;

    for (idx, segment) in text.split_inclusive('\n').enumerate() {
        let line_no = idx + 1;
        let line = segment.trim_end_matches(['\r', '\n']);

        if let Some((fc, flen)) = fence {
            if is_closing_fence(line, fc, flen) {
                fence = None;
            }
            offset += segment.len();
            continue;
        }
        if let Some(opened) = opening_fence(line) {
            fence = Some(opened);
            offset += segment.len();
            continue;
        }

        if !scan_reference_definition(line, line_no, offset, &mut links) {
            scan_line_links(line, line_no, offset, &mut links);
        }
        offset += segment.len();
    }

    links
}

/// Parses a Markdown reference-style link definition (`[label]: dest`).
/// Returns `true` when the line is a definition so inline scanning does not
/// also treat it as ordinary text.
fn scan_reference_definition(
    line: &str,
    line_no: usize,
    line_offset: usize,
    out: &mut Vec<RelativeLink>,
) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent > 3 || !trimmed.starts_with('[') {
        return false;
    }
    let bytes = line.as_bytes();
    let Some(close_bracket) = find_closing_bracket(bytes, indent + 1) else {
        return false;
    };
    if bytes.get(close_bracket + 1) != Some(&b':') {
        return false;
    }
    let mut i = close_bracket + 2;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return true;
    }
    let (dest_start, dest_end) = if bytes[i] == b'<' {
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'>' {
            if bytes[i] == b'\\' {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return true;
        }
        (start, i + 1)
    } else {
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        (start, i)
    };
    let target = &line[dest_start..dest_end];
    if let Some(path) = relative_path_part(target) {
        out.push(RelativeLink {
            line: line_no,
            span: (line_offset + dest_start)..(line_offset + dest_end),
            target: target.to_string(),
            path: path.to_string(),
        });
    }
    true
}

fn opening_fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return None;
    }
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let count = trimmed.bytes().take_while(|&b| b == first).count();
    if count < 3 {
        return None;
    }
    if first == b'`' && trimmed[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

fn is_closing_fence(line: &str, fence_char: u8, min_len: usize) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return false;
    }
    let count = trimmed.bytes().take_while(|&b| b == fence_char).count();
    count >= min_len && trimmed[count..].trim().is_empty()
}

fn skip_code_span(bytes: &[u8], i: usize) -> usize {
    let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let after_open = i + ticks;
    let mut j = after_open;
    while j < bytes.len() {
        if bytes[j] == b'`' {
            let run = bytes[j..].iter().take_while(|&&b| b == b'`').count();
            j += run;
            if run == ticks {
                return j;
            }
        } else {
            j += 1;
        }
    }
    after_open
}

fn scan_line_links(line: &str, line_no: usize, line_offset: usize, out: &mut Vec<RelativeLink>) {
    let bytes = line.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i = (i + 2).min(bytes.len());
            }
            b'`' => {
                i = skip_code_span(bytes, i);
            }
            b'[' => {
                let Some(close_bracket) = find_closing_bracket(bytes, i + 1) else {
                    i += 1;
                    continue;
                };
                if bytes.get(close_bracket + 1) != Some(&b'(') {
                    i += 1;
                    continue;
                }
                let Some((dest_start, dest_end, close_paren)) =
                    parse_destination(bytes, close_bracket + 2)
                else {
                    i += 1;
                    continue;
                };
                // Also scan inside the link label (e.g. `[![alt](img.png)](target.md)`).
                scan_line_links(
                    &line[i + 1..close_bracket],
                    line_no,
                    line_offset + i + 1,
                    out,
                );
                let target = &line[dest_start..dest_end];
                if let Some(path) = relative_path_part(target) {
                    out.push(RelativeLink {
                        line: line_no,
                        span: (line_offset + dest_start)..(line_offset + dest_end),
                        target: target.to_string(),
                        path: path.to_string(),
                    });
                }
                i = close_paren + 1;
            }
            _ => i += 1,
        }
    }
}

fn find_closing_bracket(bytes: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => i = skip_code_span(bytes, i),
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Parses the destination inside `(...)`, returning `(dest_start, dest_end, close_paren)`.
fn parse_destination(bytes: &[u8], mut i: usize) -> Option<(usize, usize, usize)> {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }

    if bytes[i] == b'<' {
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'>' {
            if bytes[i] == b'\\' {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        let end = i + 1;
        i = end;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b')' => return Some((start, end, i)),
                _ => i += 1,
            }
        }
        return None;
    }

    let start = i;
    let mut paren_depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                if paren_depth == 0 {
                    return Some((start, i, i));
                }
                paren_depth -= 1;
                i += 1;
            }
            b if b.is_ascii_whitespace() => {
                let end = i;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b')' => return Some((start, end, i)),
                        _ => i += 1,
                    }
                }
                return None;
            }
            _ => i += 1,
        }
    }
    None
}

/// Returns the relative path portion of a link destination, or `None` for
/// external URLs, same-file anchors, and `<url>` template placeholders.
fn relative_path_part(target: &str) -> Option<&str> {
    if target.is_empty() || target == "<url>" {
        return None;
    }
    let unwrapped = target
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(target);
    if unwrapped.is_empty()
        || unwrapped.starts_with('#')
        || unwrapped.starts_with("http://")
        || unwrapped.starts_with("https://")
        || unwrapped.starts_with("mailto:")
    {
        return None;
    }
    let path = unwrapped.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Lexically resolves `rel_target` relative to `base_file`'s parent directory
/// without touching the filesystem.
pub fn resolve(base_file: &Path, rel_target: &str) -> PathBuf {
    let base_dir = base_file.parent().unwrap_or_else(|| Path::new(""));
    normalize(&base_dir.join(rel_target))
}

/// Lexically collapses `.` and `..` components so paths can be compared even
/// when targets do not exist on disk.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                None | Some(Component::ParentDir | Component::CurDir) => {
                    out.push(component.as_os_str());
                }
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Formats `path` relative to `root` with forward slashes for stable diagnostics.
pub fn rel_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Returns every Markdown file under `root` (skipping build/vendored dirs).
pub fn markdown_files(root: &Path) -> Vec<PathBuf> {
    crate::text_files(root)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect()
}

/// Checks every relative Markdown link across `root` and returns descriptions
/// of links whose target path does not exist.
#[cfg(test)]
pub fn broken_relative_links(root: &Path) -> Vec<String> {
    let mut broken = Vec::new();
    for file in markdown_files(root) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel_file = rel_display(root, &file);
        for link in relative_links(&text) {
            if INSTALL_TIME_LINKS
                .iter()
                .any(|&(f, t)| f == rel_file && t == link.path)
            {
                continue;
            }
            let resolved = resolve(&file, &link.path);
            if !resolved.exists() {
                broken.push(format!(
                    "{rel_file}:{}: `{}` -> missing `{}`",
                    link.line,
                    link.target,
                    rel_display(root, &resolved)
                ));
            }
        }
    }
    broken
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_code_fences_inline_code_external_and_anchors() {
        let md = "\
See [ok](docs/guide.md#intro) and [titled](a.md \"title\").
Ignore [web](https://example.com), [mail](mailto:a@b), [sec](#local), [tpl](<url>).
Ignore `[inline](missing.md)` and escaped \\[not](missing.md).
```markdown
[fenced](missing.md)
```
~~~
[tilde-fenced](missing.md)
~~~
Nested [`arr[0]`](real.md) still matches.
";
        let found: Vec<(usize, String, String)> = relative_links(md)
            .into_iter()
            .map(|l| (l.line, l.target, l.path))
            .collect();
        assert_eq!(
            found,
            vec![
                (1, "docs/guide.md#intro".into(), "docs/guide.md".into()),
                (1, "a.md".into(), "a.md".into()),
                (10, "real.md".into(), "real.md".into()),
            ]
        );
    }

    #[test]
    fn scans_reference_defs_nested_images_bracketed_links_and_unclosed_backticks() {
        let md = "\
Unclosed `tick before [after](one.md).
Bracketed [note: see [inner](two.md)].
Linked image [![alt](img.png)](three.md).
[ref]: docs/four.md#sec
";
        let found: Vec<(usize, String, String)> = relative_links(md)
            .into_iter()
            .map(|l| (l.line, l.target, l.path))
            .collect();
        assert_eq!(
            found,
            vec![
                (1, "one.md".into(), "one.md".into()),
                (2, "two.md".into(), "two.md".into()),
                (3, "img.png".into(), "img.png".into()),
                (3, "three.md".into(), "three.md".into()),
                (4, "docs/four.md#sec".into(), "docs/four.md".into()),
            ]
        );
    }

    #[test]
    fn normalize_preserves_leading_parent_dirs_and_clamps_at_root() {
        assert_eq!(
            normalize(Path::new("../../docs/a.md")),
            PathBuf::from("../../docs/a.md")
        );
        assert_eq!(
            normalize(Path::new("/a/../../b.md")),
            PathBuf::from("/b.md")
        );
        assert_eq!(
            normalize(Path::new("docs/api-migrations/../TROUBLESHOOTING.md")),
            PathBuf::from("docs/TROUBLESHOOTING.md")
        );
    }

    #[test]
    fn broken_relative_links_reports_missing_target() {
        let dir = std::env::temp_dir().join(format!(
            "copperpour-xtask-broken-links-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.md"), "# Ok\n").unwrap();
        std::fs::write(
            dir.join("README.md"),
            "Good [ok](ok.md) and bad [gone](missing.md#sec).\n",
        )
        .unwrap();

        let broken = broken_relative_links(&dir);
        assert_eq!(broken.len(), 1, "{broken:?}");
        assert!(broken[0].contains("README.md:1"), "{broken:?}");
        assert!(broken[0].contains("missing.md#sec"), "{broken:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_relative_markdown_links_resolve() {
        let broken = broken_relative_links(&crate::repo_root());
        assert!(
            broken.is_empty(),
            "repository Markdown files contain broken relative links:\n  {}",
            broken.join("\n  ")
        );
    }
}
