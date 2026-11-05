//! Tool responses tell the caller which tool to use next, in string literals
//! that nothing compiles against the registry. `estimate_cost` told callers to
//! "use generate_bom with supply chain data" for a tool that has never existed
//! (#92). `asset_references.rs` checks the bundled skills for the same class;
//! this checks the server's own text.

use copperpour_core::router::{meta_tools, registry, ToolRouter};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Words after which the next `snake_case` word is meant as a tool to call.
const TRIGGERS: &[&str] = &[
    "use", "run", "call", "try", "invoke", "via", "with", "then", "first",
];

/// Words that follow a trigger in tool text but are not tools: vocabulary
/// and values, not parameters, which come from the schemas.
const NOT_TOOLS: &[&str] = &[
    // A property of each `set_board_stackup` layer, nested below the
    // top-level properties this check reads.
    "thickness_mm",
    // An `ipc_failure.kind` value of `check_kicad_ui`.
    "timed_out",
];

fn source_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../copperpour-core/src");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(
        !files.is_empty(),
        "no copperpour-core sources found to check"
    );
    files
}

/// The contents of the string literals on a line, split at unescaped quotes.
fn string_literals(line: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut current = None::<String>;
    let mut escaped = false;
    for ch in line.chars() {
        match (&mut current, ch) {
            (Some(text), _) if escaped => {
                text.push(ch);
                escaped = false;
            }
            (Some(_), '\\') => escaped = true,
            (Some(text), '"') => {
                literals.push(std::mem::take(text));
                current = None;
            }
            (Some(text), _) => text.push(ch),
            (None, '"') => current = Some(String::new()),
            (None, _) => {}
        }
    }
    literals
}

/// The lines of `source` to check, with their 0-based numbers: every line
/// except comments and the items gated by `#[cfg(test)]`, whose strings are
/// fixtures.
///
/// A gated item ends at its first line indented no deeper than the attribute,
/// unless that line closes the item (`}`). Production code often follows a
/// `#[cfg(test)]` item, so the scan must not stop there. If a line inside the
/// item is less indented, such as a raw string's content, the skip ends early
/// and the rest of the item is scanned: a false alarm, never a missed file.
fn scanned_lines(source: &str) -> Vec<(usize, &str)> {
    fn indent(line: &str) -> usize {
        line.len() - line.trim_start().len()
    }
    let mut lines = Vec::new();
    // While skipping: the attribute's indent, and whether the gated item's
    // first line (after any further attributes) has been seen.
    let mut skip: Option<(usize, bool)> = None;
    for (lineno, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some((depth, started)) = skip {
            if trimmed.is_empty() || indent(line) > depth {
                continue;
            }
            if !started {
                let header = trimmed.starts_with("#[") || trimmed.starts_with("///");
                skip = Some((depth, !header));
                continue;
            }
            skip = None;
            if trimmed.starts_with('}') {
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("#[cfg(test)]") {
            skip = Some((indent(line), !rest.trim().is_empty()));
            continue;
        }
        if !trimmed.starts_with("//") {
            lines.push((lineno, line));
        }
    }
    lines
}

/// `snake_case` words with at least two parts that directly follow a trigger
/// word, with any backticks around them removed.
fn tool_like_words_after_triggers(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| {
            let trigger = pair[0].trim_matches(|c: char| !c.is_ascii_alphabetic());
            TRIGGERS.contains(&trigger.to_ascii_lowercase().as_str())
        })
        .filter_map(|pair| {
            let word = pair[1].trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
            let parts: Vec<&str> = word.split('_').collect();
            (word.starts_with(|c: char| c.is_ascii_lowercase())
                && parts.len() >= 2
                && parts.iter().all(|part| {
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                }))
            .then(|| word.to_string())
        })
        .collect()
}

#[test]
fn tool_text_names_only_tools_that_exist() {
    let tools = registry::ALL_TOOLSETS
        .iter()
        .flat_map(|ts| registry::tools_for(ts.name).unwrap_or_default());
    let mut known: BTreeSet<String> = tools.clone().map(|d| d.name.to_string()).collect();
    known.extend(
        meta_tools::meta_tool_descriptions_for(true)
            .into_iter()
            .map(|d| d.name),
    );
    known.extend(
        ToolRouter::new()
            .all_toolsets()
            .iter()
            .map(|t| t.name.to_string()),
    );
    let parameters: BTreeSet<String> = tools
        .filter_map(|d| {
            d.input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        })
        .flatten()
        .collect();

    let mut phantom = Vec::new();
    for path in source_files() {
        let source = std::fs::read_to_string(&path).unwrap();
        for (lineno, line) in scanned_lines(&source) {
            for literal in string_literals(line) {
                for word in tool_like_words_after_triggers(&literal) {
                    if known.contains(&word)
                        || parameters.contains(&word)
                        || NOT_TOOLS.contains(&word.as_str())
                    {
                        continue;
                    }
                    phantom.push(format!(
                        "{}:{}: `{word}` reads like a tool but is in no toolset",
                        path.file_name().unwrap().to_string_lossy(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(
        phantom.is_empty(),
        "tool text names tools that do not exist:\n  {}\n\n\
         Either the tool was renamed or removed, or the text invented it. If the \
         word is not a tool, add it to NOT_TOOLS.",
        phantom.join("\n  ")
    );
}

#[test]
fn the_scan_finds_a_phantom_in_a_string_literal() {
    let line = r#"        "Component costs are rough averages — use generate_bom for pricing","#;
    let literals = string_literals(line);
    assert_eq!(literals.len(), 1);
    assert_eq!(
        tool_like_words_after_triggers(&literals[0]),
        ["generate_bom"]
    );
    assert_eq!(
        tool_like_words_after_triggers("then call `export_bom`, then use the file"),
        ["export_bom"]
    );
    assert!(string_literals(r#"let x = 1; // use generate_bom"#).is_empty());
}

#[test]
fn the_scan_skips_only_the_items_gated_by_cfg_test() {
    let source = r#"fn before() -> &'static str { "use before_tool" }
#[cfg(test)] mod inline_tests;
fn after_inline() -> &'static str { "use after_inline" }
#[cfg(test)]
#[allow(dead_code)]
/// Doc.
mod tests {
    fn fixture() -> &'static str {
        "use test_fixture"
    }

    #[cfg(test)]
    fn nested() {}
}
fn after_mod() -> &'static str { "use after_mod" }
struct Flags {
    #[cfg(test)]
    test_field: bool,
    real_field: bool,
}
// use commented_out
fn last() -> &'static str { "use last_tool" }
"#;
    let scanned: Vec<&str> = scanned_lines(source)
        .into_iter()
        .map(|(_, line)| line.trim())
        .collect();
    assert_eq!(
        scanned,
        [
            r#"fn before() -> &'static str { "use before_tool" }"#,
            r#"fn after_inline() -> &'static str { "use after_inline" }"#,
            r#"fn after_mod() -> &'static str { "use after_mod" }"#,
            "struct Flags {",
            "real_field: bool,",
            "}",
            r#"fn last() -> &'static str { "use last_tool" }"#,
        ]
    );
}
