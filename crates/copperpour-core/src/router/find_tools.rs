use super::registry;
use crate::mcp::protocol::{CallToolResult, McpToolDescription};
use crate::tools::{invalid_arg, require_str, response_limit, BoundedList, ToolContext};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::HashSet;

const MAX_QUERY_CHARS: usize = 200;
const MIN_LIMIT: usize = 1;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

pub(super) fn description() -> McpToolDescription {
    McpToolDescription {
        name: "find_tools".to_string(),
        description: "Search registered KiCad tools by name or capability, including tools in \
                      unloaded toolsets. Returns each match's tool name, description, toolset and \
                      load state. Call load_toolset with the reported toolset to expose its tools."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "maxLength": MAX_QUERY_CHARS,
                    "description": "Non-blank tool name or capability keywords. Matching is case-insensitive."
                },
                "limit": {
                    "type": "integer",
                    "minimum": MIN_LIMIT,
                    "maximum": MAX_LIMIT,
                    "default": DEFAULT_LIMIT
                }
            },
            "required": ["query"]
        }),
    }
}

pub(super) async fn handle_find_tools(args: &Value, ctx: &ToolContext) -> CallToolResult {
    let query = match require_str(args, "query") {
        Ok(query) => query,
        Err(error) => return error,
    };
    if query.chars().nth(MAX_QUERY_CHARS).is_some() {
        return invalid_arg(
            "query",
            &format!("must be at most {MAX_QUERY_CHARS} characters"),
        );
    }
    let query = query.trim();
    if query.is_empty() {
        return invalid_arg("query", "must not be blank");
    }

    let limit = match response_limit(args, DEFAULT_LIMIT) {
        Ok(limit) if (MIN_LIMIT..=MAX_LIMIT).contains(&limit) => limit,
        Ok(_) => {
            return invalid_arg(
                "limit",
                &format!("must be between {MIN_LIMIT} and {MAX_LIMIT}"),
            )
        }
        Err(error) => return error,
    };

    let query = query.to_lowercase();
    let query_words = words(&query).collect::<HashSet<_>>();
    let mut ranked = Vec::new();
    for (toolset_order, (toolset, definitions)) in registry::toolsets_with_tools().enumerate() {
        for def in definitions {
            if let Some(rank) = match_rank(&query, &query_words, def.name, def.description) {
                ranked.push(((rank, toolset_order, def.name), toolset, def));
            }
        }
    }
    ranked.sort_unstable_by_key(|(key, _, _)| *key);

    let results = BoundedList::new(&ranked, limit);
    let active: HashSet<_> = ctx.router.active_names().await.into_iter().collect();
    let matches: Vec<_> = results
        .items
        .iter()
        .map(|(_, toolset, def)| {
            json!({
                "tool": def.name,
                "toolset": toolset,
                "description": def.description,
                "loaded": active.contains(*toolset)
            })
        })
        .collect();
    CallToolResult::json(&json!({"matches": matches, "truncated": results.truncated}))
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

fn match_rank(
    query: &str,
    query_words: &HashSet<&str>,
    name: &str,
    description: &str,
) -> Option<(u8, Reverse<usize>)> {
    // Registered names are already lowercase snake_case.
    if name == query {
        return Some((1, Reverse(0)));
    }
    if !query_words.is_empty()
        && query_words
            .iter()
            .all(|word| name.split('_').any(|name_word| name_word == *word))
    {
        return Some((2, Reverse(0)));
    }
    if name.contains(query) {
        return Some((3, Reverse(0)));
    }

    let description = description.to_lowercase();
    let description_words = words(&description).collect::<HashSet<_>>();
    let shared = query_words.intersection(&description_words).count();
    if shared == 0 {
        return None;
    }
    let tier = if shared == query_words.len() { 4 } else { 5 };
    Some((tier, Reverse(shared)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_tools_ranks_name_tiers_before_description_tiers() {
        let rank = |query: &str, name, description| {
            match_rank(query, &words(query).collect(), name, description)
        };
        let ranked = [
            rank("route_trace", "route_trace", ""),
            rank("trace route", "route_trace", ""),
            rank("route_tr", "route_trace", ""),
            rank(
                "route trace board",
                "example",
                "Route a trace across the board",
            ),
            rank("route trace board", "example", "Route a trace"),
            rank("route trace board", "example", "Route"),
        ];
        assert!(ranked.iter().all(Option::is_some));
        assert!(
            ranked.windows(2).all(|pair| pair[0] < pair[1]),
            "{ranked:?}"
        );
        assert_eq!(rank("trace", "example", "Retraced paths"), None);
        assert_eq!(rank("!!!", "example", "Retraced paths"), None);
    }
}
