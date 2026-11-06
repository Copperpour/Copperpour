//! `list_schematic_texts`, `edit_schematic_text` and `delete_schematic_text`:
//! a sheet's free text items, listed with the sheet's revision, and one of them
//! rewritten or removed by UUID under that revision.
//!
//! A text item is a plain `(text …)` at the top level of the sheet, what
//! `add_schematic_text` writes. Labels, fields, text boxes and the text a
//! library symbol draws are not listed and cannot be selected. An edit replaces
//! only the quoted string, so the item keeps its UUID, position, effects and
//! every other byte of the file. Both writes prepare their text with
//! `SchematicCommand`, only on a sheet that still has the `DocumentRevision`
//! the list returned.

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::library::skip_sexp_token;
use crate::tools::sch_batch::escaped_text;
use crate::tools::sch_hierarchy::commit_sheets;
use crate::tools::{get_path, invalid_arg, require_str, ToolContext, ToolDef};
use copperpour_sexp::schematic::parse_at;
use copperpour_sexp::writer::{apply_edits, find_direct_child_blocks};
use copperpour_sexp::{
    parse_sexp, prepare_command, read_consistent, DocumentRevision, FileTransition, ItemId,
    SchematicCommand, SexpEdit, SexpNode,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const EDIT: &str = "edit_schematic_text";
const DELETE: &str = "delete_schematic_text";

pub(super) fn list_tool() -> ToolDef {
    tool!(
        "list_schematic_texts",
        "List a sheet's free text items, as add_schematic_text writes them: each one's uuid, \
         text, position and rotation. Labels, fields, text boxes and text inside library \
         symbols are not listed. Returns the sheet's revision, which edit_schematic_text and \
         delete_schematic_text require.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" }
            },
            "required": ["schematic"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_list_schematic_texts(args, ctx).await }
    )
}

pub(super) fn edit_tool() -> ToolDef {
    tool!(
        "edit_schematic_text",
        "Replace one free text item's text, by the uuid and revision list_schematic_texts \
         returned. Only the quoted string changes; the item keeps its uuid, position, rotation \
         and effects. On a sheet placed more than once, the change reaches every instance of \
         the sheet. Refused when the sheet changed since that listing, or while KiCad holds \
         the sheet or its project's root schematic open. Reports the text read back from the \
         file and the sheet's new revision.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                "uuid": { "type": "string", "description": "The text item's uuid, as list_schematic_texts reports it" },
                "revision": { "type": "string", "description": "The revision list_schematic_texts returned for this sheet" },
                "text": { "type": "string", "minLength": 1, "description": "The new text, written as add_schematic_text writes it: line breaks as \\n, carriage returns dropped" }
            },
            "required": ["schematic", "uuid", "revision", "text"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_edit_schematic_text(args, ctx).await }
    )
}

pub(super) fn delete_tool() -> ToolDef {
    tool!(
        "delete_schematic_text",
        "Delete one free text item, by the uuid and revision list_schematic_texts returned. \
         On a sheet placed more than once, the change reaches every instance of the sheet. \
         Refused when the sheet changed since that listing, when a group lists the item as a \
         member, or while KiCad holds the sheet or its project's root schematic open. Reports \
         the item removed and the sheet's new revision.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                "uuid": { "type": "string", "description": "The text item's uuid, as list_schematic_texts reports it" },
                "revision": { "type": "string", "description": "The revision list_schematic_texts returned for this sheet" }
            },
            "required": ["schematic", "uuid", "revision"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_delete_schematic_text(args, ctx).await }
    )
}

/// One free text item as it stands in the sheet.
#[derive(Debug, Clone)]
struct SavedText {
    uuid: Option<String>,
    text: String,
    at: (f64, f64, f64),
    /// The `(text …)` block.
    block: (usize, usize),
    /// Its quoted string, quotes included.
    string: (usize, usize),
}

impl SavedText {
    fn json(&self) -> Value {
        json!({
            "uuid": self.uuid,
            "text": self.text,
            "x": self.at.0,
            "y": self.at.1,
            "rotation": self.at.2
        })
    }
}

/// A sheet as saved: its top-level items, its text items, the kinds of the
/// top-level items that carry each UUID, and the groups that list each member
/// UUID. A uuid is judged among the top-level items, the ones KiCad finds a
/// group's members among; unlike `indexed_uuid_items`, a duplicate refuses
/// only the calls that name it, so a sheet that holds one can still be listed.
struct Sheet {
    content: String,
    revision: String,
    /// Each top-level item's uuid and block, in file order.
    items: Vec<(Option<String>, (usize, usize))>,
    texts: Vec<SavedText>,
    kinds: BTreeMap<String, Vec<String>>,
    groups: BTreeMap<String, Vec<String>>,
}

impl Sheet {
    fn parse(content: String) -> Result<Self, String> {
        let tree = parse_sexp(&content).map_err(|error| format!("it cannot be parsed: {error}"))?;
        if tree.head() != Some("kicad_sch") {
            return Err("its root is not (kicad_sch …)".to_owned());
        }
        let mut items = Vec::new();
        let mut texts = Vec::new();
        let mut kinds: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (start, end) in find_direct_child_blocks(&content, "kicad_sch") {
            let node = parse_sexp(&content[start..end])
                .map_err(|error| format!("the item at byte {start} cannot be parsed: {error}"))?;
            let kind = node.head().unwrap_or_default();
            let uuid = node.find_str("uuid");
            items.push((uuid.map(str::to_owned), (start, end)));
            if kind == "group" {
                let group = match (node.get(1), uuid) {
                    (Some(SexpNode::Str(name)), Some(id)) if !name.is_empty() => {
                        format!("group '{name}' ({id})")
                    }
                    (_, Some(id)) => format!("an unnamed group ({id})"),
                    (_, None) => "a group".to_owned(),
                };
                let members = node.find("members").and_then(SexpNode::children);
                for member in members.unwrap_or(&[]).iter().skip(1) {
                    if let Some(member) = member.as_str() {
                        groups
                            .entry(member.to_owned())
                            .or_default()
                            .push(group.clone());
                    }
                }
            }
            if let Some(uuid) = uuid {
                kinds
                    .entry(uuid.to_owned())
                    .or_default()
                    .push(kind.to_owned());
            }
            if kind != "text" {
                continue;
            }
            let at = |what: &str| format!("the text at byte {start} has no {what}");
            let Some(SexpNode::Str(text)) = node.get(1) else {
                return Err(at("quoted string"));
            };
            let string = skip_sexp_token(&content, start + 1)
                .and_then(|(_, head_end)| skip_sexp_token(&content, head_end))
                .filter(|&(open, _)| content.as_bytes()[open] == b'"')
                .ok_or_else(|| at("quoted string"))?;
            texts.push(SavedText {
                uuid: uuid.map(str::to_owned),
                text: text.clone(),
                at: parse_at(&node).ok_or_else(|| at("position"))?,
                block: (start, end),
                string,
            });
        }
        Ok(Sheet {
            revision: revision(&content),
            content,
            items,
            texts,
            kinds,
            groups,
        })
    }

    /// The index of the text item `uuid` names, or the refusal: no top-level
    /// item has it, several do, or it names another kind of item.
    fn text(&self, operation: &str, uuid: &str) -> Result<usize, CallToolResult> {
        let found = self
            .texts
            .iter()
            .position(|text| text.uuid.as_deref() == Some(uuid));
        match self.kinds.get(uuid).map(Vec::as_slice) {
            Some([kind]) if kind == "text" => {
                found.ok_or_else(|| stale(operation, uuid, "no text item has this uuid"))
            }
            Some([kind]) => Err(invalid_arg(
                "uuid",
                &format!("'{uuid}' is a {kind}, not a text item"),
            )),
            Some(kinds) if kinds.len() > 1 => Err(CallToolResult::error_kind(
                ToolErrorKind::AmbiguousTarget {
                    target: format!("schematic UUID {uuid}"),
                    candidates: kinds.to_vec(),
                },
                format!(
                    "{} items of the sheet carry the uuid '{uuid}' ({}), so no item was changed.",
                    kinds.len(),
                    kinds.join(", ")
                ),
            )),
            _ => Err(stale(operation, uuid, "no item of the sheet has this uuid")),
        }
    }
}

/// The sheet's revision as a caller holds it: the `DocumentRevision` of every
/// byte of the saved file.
fn revision(content: &str) -> String {
    DocumentRevision::of(content).to_string()
}

/// The sheet at `path` as saved, or the refusal saying why it cannot be read.
fn read_sheet(path: &Path) -> anyhow::Result<Result<Sheet, CallToolResult>> {
    if !path.is_file() {
        return Ok(Err(CallToolResult::error_kind(
            ToolErrorKind::FileNotFound {
                path: path.display().to_string(),
            },
            format!("Schematic '{}' not found", path.display()),
        )));
    }
    let content = read_consistent(path)?;
    Ok(Sheet::parse(content).map_err(|reason| {
        invalid_arg(
            "schematic",
            &format!("'{}' is not a readable sheet: {reason}", path.display()),
        )
    }))
}

fn stale(operation: &str, uuid: &str, reason: &str) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: uuid.to_owned(),
            reason: reason.to_owned(),
        },
        format!(
            "{operation} refused for uuid '{uuid}': {reason}. No item was changed; run \
             list_schematic_texts again and pass its uuid and revision."
        ),
    )
}

/// The sheet a write reads under its revision, and the text item it names.
struct Target {
    schematic: std::path::PathBuf,
    uuid: String,
    sheet: Sheet,
    index: usize,
}

impl Target {
    fn item(&self) -> &SavedText {
        &self.sheet.texts[self.index]
    }
}

fn target(args: &Value, operation: &str) -> anyhow::Result<Result<Target, CallToolResult>> {
    let schematic = get_path(args, "schematic")?;
    let (uuid, revision) = match (require_str(args, "uuid"), require_str(args, "revision")) {
        (Ok(uuid), Ok(revision)) => (uuid, revision),
        (Err(refusal), _) | (_, Err(refusal)) => return Ok(Err(refusal)),
    };
    let sheet = match read_sheet(&schematic)? {
        Ok(sheet) => sheet,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if sheet.revision != revision {
        return Ok(Err(stale(
            operation,
            uuid,
            "the sheet changed since list_schematic_texts returned this revision",
        )));
    }
    let index = match sheet.text(operation, uuid) {
        Ok(index) => index,
        Err(refusal) => return Ok(Err(refusal)),
    };
    Ok(Ok(Target {
        schematic,
        uuid: uuid.to_owned(),
        sheet,
        index,
    }))
}

/// Commit `command` on the sheet as read, which `target` found at the listed
/// revision. The text it would write must pass `check` against the sheet read,
/// or nothing is written. The write is refused while KiCad holds the sheet or its
/// project's root open, or when the sheet changed after the read; the file is
/// then read back. `Err` is the refusal or the uncertain outcome.
fn write(
    target: &Target,
    operation: &str,
    command: SchematicCommand,
    check: impl Fn(&Sheet, &Sheet) -> Result<(), String>,
) -> anyhow::Result<Result<Sheet, CallToolResult>> {
    let schematic = &target.schematic;
    let before = &target.sheet;
    let (prepared, _) = prepare_command(schematic, &before.content, &command)?;
    #[cfg(test)]
    let prepared = fault::prospective(prepared);
    let after = match Sheet::parse(prepared.clone())
        .and_then(|after| check(before, &after).map(|()| after))
    {
        Ok(after) => after,
        Err(reason) => {
            return Ok(Err(CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: schematic.display().to_string(),
                    reason: reason.clone(),
                },
                format!(
                    "{operation} checked the sheet it would write and nothing was written: \
                     {reason}"
                ),
            )))
        }
    };
    let root = match crate::tools::resolve_schematic_ownership(schematic) {
        Ok(ownership) => ownership.map(|ownership| ownership.root_schematic),
        Err(error) => return Ok(Err(error.into_tool_result())),
    };
    let transition = FileTransition::replace(schematic, before.content.clone(), prepared.clone());
    #[cfg(test)]
    crate::tools::write_hook::run(schematic);
    if let Err(refusal) = commit_sheets(
        root.as_deref().unwrap_or(schematic),
        &[schematic.as_path()],
        vec![transition],
    )? {
        return Ok(Err(refusal));
    }
    #[cfg(test)]
    fault::committed(schematic);
    let uncertain = |reason: String| {
        Ok(Err(crate::tools::mutation_outcome_uncertain(
            schematic, operation, reason,
        )))
    };
    match read_consistent(schematic) {
        // The file holds the checked text byte for byte, so `after` is its
        // read-back.
        Ok(on_disk) if on_disk == prepared => Ok(Ok(after)),
        Ok(_) => uncertain("the file on disk is not the text the transaction wrote".to_owned()),
        Err(error) => uncertain(format!("the sheet could not be read back: {error}")),
    }
}

/// Faults a test puts in a write's way: a prospective text in place of the one
/// the command prepared, or a save over the file once the commit is done.
#[cfg(test)]
mod fault {
    use std::cell::RefCell;
    use std::path::Path;

    thread_local! {
        static PROSPECTIVE: RefCell<Option<String>> = const { RefCell::new(None) };
        static COMMITTED: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(super) fn set_prospective(text: String) {
        PROSPECTIVE.with(|slot| *slot.borrow_mut() = Some(text));
    }

    pub(super) fn set_committed(text: String) {
        COMMITTED.with(|slot| *slot.borrow_mut() = Some(text));
    }

    pub(super) fn prospective(prepared: String) -> String {
        PROSPECTIVE
            .with(|slot| slot.borrow_mut().take())
            .unwrap_or(prepared)
    }

    pub(super) fn committed(path: &Path) {
        if let Some(text) = COMMITTED.with(|slot| slot.borrow_mut().take()) {
            std::fs::write(path, text).unwrap();
        }
    }
}

/// The sheet's top-level items in file order, each as its bytes, `uuid`'s as
/// `None`.
fn others<'s>(sheet: &'s Sheet, uuid: &str) -> Vec<Option<&'s str>> {
    sheet
        .items
        .iter()
        .map(|(id, (start, end))| {
            (id.as_deref() != Some(uuid)).then(|| &sheet.content[*start..*end])
        })
        .collect()
}

/// Text `index`'s bytes before and after its quoted string.
fn around_string(sheet: &Sheet, index: usize) -> (&str, &str) {
    let SavedText { block, string, .. } = &sheet.texts[index];
    let content = sheet.content.as_str();
    (&content[block.0..string.0], &content[string.1..block.1])
}

/// Whether `after` is `before` with only text `uuid`'s string changed, to
/// `text`: every other top-level item byte for byte in its place, and the
/// text's own bytes but its string.
fn edited(before: &Sheet, after: &Sheet, uuid: &str, text: &str) -> Result<(), String> {
    if others(before, uuid) != others(after, uuid) {
        return Err(format!("a top-level item other than text {uuid} changed"));
    }
    let named = |sheet: &Sheet| {
        sheet
            .texts
            .iter()
            .position(|item| item.uuid.as_deref() == Some(uuid))
    };
    let (Some(old), Some(new)) = (named(before), named(after)) else {
        return Err(format!("the sheet holds no text {uuid}"));
    };
    if around_string(before, old) != around_string(after, new) {
        return Err(format!("text {uuid} changed beyond its string"));
    }
    match after.texts[new].text.as_str() {
        found if found == text => Ok(()),
        found => Err(format!("text {uuid} reads {found:?}, not {text:?}")),
    }
}

/// Whether `after` is `before` without item `uuid`: every other top-level item
/// byte for byte, in order, and none of that uuid.
fn deleted(before: &Sheet, after: &Sheet, uuid: &str) -> Result<(), String> {
    if let Some(kinds) = after.kinds.get(uuid) {
        return Err(format!("the sheet still holds {kinds:?} with uuid {uuid}"));
    }
    let kept: Vec<_> = others(before, uuid).into_iter().flatten().collect();
    let found: Vec<_> = others(after, uuid).into_iter().flatten().collect();
    if found != kept {
        return Err(format!("a top-level item other than {uuid} changed"));
    }
    Ok(())
}

async fn handle_list_schematic_texts(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let schematic = get_path(args, "schematic")?;
    let sheet = match read_sheet(&schematic)? {
        Ok(sheet) => sheet,
        Err(refusal) => return Ok(refusal),
    };
    Ok(CallToolResult::json(&json!({
        "count": sheet.texts.len(),
        "texts": sheet.texts.iter().map(SavedText::json).collect::<Vec<_>>(),
        "revision": sheet.revision,
        "source": "file"
    })))
}

async fn handle_edit_schematic_text(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    // Carriage returns are dropped as `escaped_text` drops them, so this is
    // the text KiCad will read back.
    let text = match require_str(args, "text") {
        Ok(text) => text.replace('\r', ""),
        Err(refusal) => return Ok(refusal),
    };
    if text.is_empty() {
        return Ok(invalid_arg(
            "text",
            "is empty once carriage returns are dropped; delete_schematic_text removes a \
             text item",
        ));
    }
    let target = match target(args, EDIT)? {
        Ok(target) => target,
        Err(refusal) => return Ok(refusal),
    };
    let (uuid, item) = (target.uuid.as_str(), target.item());
    if text == item.text {
        return Ok(invalid_arg(
            "text",
            &format!("is the text of '{uuid}' already"),
        ));
    }
    let (start, end) = item.string;
    let candidate = apply_edits(
        target.sheet.content.clone(),
        vec![SexpEdit::replace(
            start,
            end,
            format!("\"{}\"", escaped_text(&text)),
        )],
    );
    let command = SchematicCommand::replace_item_from_document(
        &target.sheet.content,
        &candidate,
        ItemId::new(uuid)?,
        EDIT,
    )?;
    // The command must give back the candidate byte for byte, and the
    // candidate must be the sheet with only the text's string changed.
    let check = |before: &Sheet, after: &Sheet| {
        if after.content != candidate {
            return Err("the command would write other bytes than its candidate".to_owned());
        }
        edited(before, after, uuid, &text)
    };
    let after = match write(&target, EDIT, command, check)? {
        Ok(after) => after,
        Err(refusal) => return Ok(refusal),
    };
    // `edited` proved the items come back in their order.
    let landed = &after.texts[target.index];
    Ok(CallToolResult::json(&json!({
        "uuid": landed.uuid,
        "old_text": item.text,
        "text": landed.text,
        "x": landed.at.0,
        "y": landed.at.1,
        "rotation": landed.at.2,
        "revision": after.revision,
        "source": "file"
    })))
}

async fn handle_delete_schematic_text(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let target = match target(args, DELETE)? {
        Ok(target) => target,
        Err(refusal) => return Ok(refusal),
    };
    let (uuid, item) = (target.uuid.as_str(), target.item());
    // KiCad keeps a group's members together; a member deleted here would
    // leave the group without it.
    if let Some(groups) = target.sheet.groups.get(uuid) {
        let reasons: Vec<String> = groups
            .iter()
            .map(|group| format!("{group} lists it as a member"))
            .collect();
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: DELETE.to_owned(),
                reasons: reasons.clone(),
            },
            format!(
                "Text '{uuid}' was not deleted: {}. Remove it from the group in KiCad, then \
                 delete it.",
                reasons.join("; ")
            ),
        ));
    }
    let command = SchematicCommand::delete_item(&target.sheet.content, ItemId::new(uuid)?, DELETE)?;
    let check = |before: &Sheet, after: &Sheet| deleted(before, after, uuid);
    let after = match write(&target, DELETE, command, check)? {
        Ok(after) => after,
        Err(refusal) => return Ok(refusal),
    };
    Ok(CallToolResult::json(&json!({
        "deleted": item.json(),
        "revision": after.revision,
        "source": "file"
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::handler::McpHandler;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// KiCad 10.0.6's sheet of six free texts: see its README.
    const SHEET: &str = "schematic_texts_kicad10.kicad_sch";
    const PLAIN: &str = "40894e37-894c-44d5-ae33-c8e4e9698a41";
    const TWO_LINES: &str = "a065c718-bbfa-4223-b6a0-9bf4c1eb9358";
    const QUOTED: &str = "3b7acc9f-f585-43ba-8a32-23f5a595c079";
    const GROUPED: &str = "ed2c0b10-61a7-40dc-abbd-ef466f734dab";
    const TURNED: &str = "eb4a9af4-2c22-4dbd-9eb1-7b7caee2242e";
    const UNICODE: &str = "4ff58621-eb4d-4742-8091-6668723e2c5f";
    const LABEL: &str = "43a0d47e-d268-4dbb-b842-f6eeaafedcd8";
    /// The two-line free text on `title_block_hierarchy/ampli_ht.kicad_sch`.
    const FILTER: &str = "4fee597b-5b3d-4d5f-9246-cb5aaa802827";
    const GROUP: &str = "0f6a8c1e-5b2d-4c3e-9a7f-1d2e3f4a5b6c";

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .expect("the fixture is committed")
    }

    /// `content` under the fixture's name, in a directory of its own.
    fn sheet_with(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SHEET);
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    async fn call(tool: &str, sheet: &Path, mut args: Value) -> CallToolResult {
        args["schematic"] = json!(sheet.to_str().unwrap());
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 56, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    async fn edit(sheet: &Path, uuid: &str, revision: &str, text: &str) -> CallToolResult {
        call(
            "edit_schematic_text",
            sheet,
            json!({ "uuid": uuid, "revision": revision, "text": text }),
        )
        .await
    }

    async fn delete(sheet: &Path, uuid: &str, revision: &str) -> CallToolResult {
        call(
            "delete_schematic_text",
            sheet,
            json!({ "uuid": uuid, "revision": revision }),
        )
        .await
    }

    fn text(result: &CallToolResult) -> &str {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("expected text content");
        };
        text
    }

    fn body(result: &CallToolResult) -> Value {
        assert!(!result.is_error, "{}", text(result));
        serde_json::from_str(text(result)).unwrap()
    }

    fn refused(result: &CallToolResult, kind: &str) -> String {
        assert_eq!(
            extract_error_kind(result).as_deref(),
            Some(kind),
            "{}",
            text(result)
        );
        text(result).to_string()
    }

    async fn listed(sheet: &Path) -> Value {
        body(&call("list_schematic_texts", sheet, json!({})).await)
    }

    async fn revision_of(sheet: &Path) -> String {
        listed(sheet).await["revision"].as_str().unwrap().to_owned()
    }

    fn parsed(content: String) -> Sheet {
        Sheet::parse(content).unwrap()
    }

    /// `source` with its one `old` replaced by `new`. Every expected file is
    /// built this way, from text written here, not read by the module.
    fn swapped(source: &str, old: &str, new: &str) -> String {
        assert_eq!(source.matches(old).count(), 1, "{old}");
        source.replacen(old, new, 1)
    }

    /// The fixture with the text KiCad wrote as `(text "old"` reading `new`.
    fn with_string(old: &str, new: &str) -> String {
        swapped(
            &fixture(SHEET),
            &format!("(text \"{old}\""),
            &format!("(text \"{new}\""),
        )
    }

    /// A 1.27 mm text item justified left bottom, as KiCad 10.0.6 saves it,
    /// with its line.
    fn kicad_text(string: &str, at: &str, uuid: &str) -> String {
        format!(
            "\t(text \"{string}\"\n\t\t(exclude_from_sim no)\n\t\t(at {at})\n\t\t(effects\n\
             \t\t\t(font\n\t\t\t\t(size 1.27 1.27)\n\t\t\t)\n\t\t\t(justify left bottom)\n\t\t)\n\
             \t\t(uuid \"{uuid}\")\n\t)\n"
        )
    }

    fn plain_note() -> String {
        kicad_text("Plain note", "25.4 25.4 0", PLAIN)
    }

    /// `source` without `item`, as `kicad_text` writes it.
    fn without(source: &str, item: &str) -> String {
        swapped(source, item, "")
    }

    /// No Copperpour tool writes a group: this is the fixture with a group of
    /// `GROUPED` before its closing parenthesis, which KiCad 10.0.6's
    /// `kicad-cli sch upgrade --force` keeps byte for byte.
    fn grouped() -> String {
        let source = fixture(SHEET);
        let close = source.trim_end().len() - 1;
        format!(
            "{}\t(group \"Notes\"\n\t\t(uuid \"{GROUP}\")\n\t\t(members \"{GROUPED}\")\n\t)\n{}",
            &source[..close],
            &source[close..]
        )
    }

    #[tokio::test]
    async fn only_the_sheets_free_texts_are_listed() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let reply = listed(&path).await;
        assert_eq!(reply["count"], 6);
        assert_eq!(reply["source"], "file");
        assert_eq!(
            reply["revision"],
            DocumentRevision::of(&fixture(SHEET)).to_string()
        );
        let texts: BTreeMap<&str, &Value> = reply["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| (text["uuid"].as_str().unwrap(), text))
            .collect();
        for (uuid, string, x, y, rotation) in [
            (PLAIN, "Plain note", 25.4, 25.4, 0.0),
            (TWO_LINES, "First line\nSecond line", 25.4, 38.1, 0.0),
            (QUOTED, "Say \"hi\" from C:\\new", 25.4, 50.8, 0.0),
            (GROUPED, "Grouped note", 25.4, 63.5, 0.0),
            (TURNED, "Turned note", 76.2, 38.1, 90.0),
            (UNICODE, "R = 4k7 Ω ±1%", 25.4, 76.2, 0.0),
        ] {
            assert_eq!(
                texts[uuid],
                &json!({ "uuid": uuid, "text": string, "x": x, "y": y, "rotation": rotation })
            );
        }
        // Neither the label nor the `(text "A")` that `Device:Ammeter_AC`
        // draws in its library body is a text item.
        assert!(fixture(SHEET).contains("\t\t\t\t(text \"A\""));
        assert!(!texts.contains_key(LABEL));
    }

    #[tokio::test]
    async fn a_text_box_is_not_a_text_item() {
        // KiCad 10.0.6's rigid-block sheet: a text box and labels, no free text.
        let source = fixture("rigid_block_move_kicad10.kicad_sch");
        let text_box = "5e6a0c1d-8f3b-4c2a-9d7e-1b2c3d4e5f61";
        assert!(source.contains(&format!("(uuid \"{text_box}\")")));
        let (_dir, path) = sheet_with(&source);
        let reply = listed(&path).await;
        assert_eq!(reply["count"], 0);
        let revision = reply["revision"].as_str().unwrap();
        refused(
            &edit(&path, text_box, revision, "X").await,
            "invalid_argument",
        );
        refused(&delete(&path, text_box, revision).await, "invalid_argument");
        assert_eq!(read(&path), source);
    }

    #[test]
    fn the_revision_covers_every_byte() {
        let source = fixture(SHEET);
        assert_eq!(revision(&source), revision(&source));
        let a3 = source.replacen("(paper \"A4\")", "(paper \"A3\")", 1);
        assert_ne!(revision(&source), revision(&a3));
    }

    /// Every text KiCad 10 saves carries a uuid (`saveText` writes one); this
    /// is the fixture with one taken out, as an older file may hold it.
    #[tokio::test]
    async fn a_text_without_a_uuid_is_listed_and_cannot_be_named() {
        let source = fixture(SHEET).replacen(&format!("\t\t(uuid \"{PLAIN}\")\n"), "", 1);
        let (_dir, path) = sheet_with(&source);
        let reply = listed(&path).await;
        assert_eq!(reply["count"], 6);
        let texts = reply["texts"].as_array().unwrap();
        let plain = texts.iter().find(|text| text["text"] == "Plain note");
        assert_eq!(plain.unwrap()["uuid"], Value::Null);
        let revision = reply["revision"].as_str().unwrap();
        refused(&edit(&path, PLAIN, revision, "X").await, "stale_target");
        refused(&delete(&path, PLAIN, revision).await, "stale_target");
        assert_eq!(read(&path), source);
    }

    #[tokio::test]
    async fn a_missing_file_or_one_that_is_not_a_sheet_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(SHEET);
        refused(
            &call("list_schematic_texts", &missing, json!({})).await,
            "file_not_found",
        );
        refused(&edit(&missing, PLAIN, "0/0", "X").await, "file_not_found");
        let project = dir.path().join("complex_hierarchy.kicad_sch");
        let source = fixture("title_block_hierarchy/complex_hierarchy.kicad_pro");
        std::fs::write(&project, &source).unwrap();
        refused(
            &call("list_schematic_texts", &project, json!({})).await,
            "invalid_argument",
        );
        refused(&delete(&project, PLAIN, "0/0").await, "invalid_argument");
        assert_eq!(read(&project), source);
    }

    #[tokio::test]
    async fn only_the_quoted_string_changes() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        // The turned text keeps its bold 2 mm font and its justification; the
        // other's string holds characters of two bytes.
        for (uuid, old_text, new_text) in [
            (TURNED, "Turned note", "Edited"),
            (UNICODE, "R = 4k7 Ω ±1%", "R = 10 kΩ ±5 %"),
        ] {
            std::fs::write(&path, fixture(SHEET)).unwrap();
            let revision = revision_of(&path).await;
            let reply = body(&edit(&path, uuid, &revision, new_text).await);
            assert_eq!(read(&path), with_string(old_text, new_text));
            assert_eq!(reply["uuid"], uuid);
            assert_eq!(reply["old_text"], old_text);
            assert_eq!(reply["text"], new_text);
            assert_eq!(reply["source"], "file");
            assert_eq!(reply["revision"], revision_of(&path).await);
        }
        let unchanged = read(&path);
        let same = edit(&path, UNICODE, &revision_of(&path).await, "R = 10 kΩ ±5 %").await;
        refused(&same, "invalid_argument");
        assert_eq!(read(&path), unchanged);
    }

    /// A line break, a quote and a backslash are escaped as KiCad escapes them;
    /// a tab is written `\t`, which KiCad reads as a tab; a carriage return is
    /// dropped, as `add_schematic_text` drops it.
    #[tokio::test]
    async fn line_breaks_quotes_and_backslashes_are_escaped() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        body(&edit(&path, PLAIN, &revision, "a\r\nb \"c\" \\d\te").await);
        assert_eq!(
            read(&path),
            with_string("Plain note", r#"a\nb \"c\" \\d\te"#)
        );
        let texts = listed(&path).await["texts"].clone();
        let plain = texts
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["uuid"] == PLAIN);
        assert_eq!(plain.unwrap()["text"], "a\nb \"c\" \\d\te");
    }

    #[tokio::test]
    async fn a_deleted_text_takes_its_line_with_it() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        let reply = body(&delete(&path, TWO_LINES, &revision).await);
        let two_lines = kicad_text(r"First line\nSecond line", "25.4 38.1 0", TWO_LINES);
        assert_eq!(read(&path), without(&fixture(SHEET), &two_lines));
        assert_eq!(reply["deleted"]["uuid"], TWO_LINES);
        assert_eq!(reply["deleted"]["text"], "First line\nSecond line");
        assert_eq!(reply["source"], "file");
        let after = listed(&path).await;
        assert_eq!(after["count"], 5);
        assert_eq!(reply["revision"], after["revision"]);
    }

    #[tokio::test]
    async fn a_stale_revision_changes_nothing() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let first = revision_of(&path).await;
        // One edit moves the sheet on, so the listing it was made under no
        // longer holds for another.
        body(&edit(&path, TURNED, &first, "Moved on").await);
        let moved_on = read(&path);
        refused(&edit(&path, PLAIN, &first, "X").await, "stale_target");
        refused(&delete(&path, PLAIN, &first).await, "stale_target");
        assert_eq!(read(&path), moved_on);
    }

    #[tokio::test]
    async fn a_uuid_that_names_no_text_item_is_refused() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        let absent = "00000000-0000-0000-0000-000000000000";
        for (uuid, kind) in [(LABEL, "invalid_argument"), (absent, "stale_target")] {
            refused(&edit(&path, uuid, &revision, "X").await, kind);
            refused(&delete(&path, uuid, &revision).await, kind);
        }
        assert_eq!(read(&path), fixture(SHEET));
    }

    /// No tool writes two items of one uuid: this is the fixture with the
    /// label's uuid given to a text.
    #[tokio::test]
    async fn two_items_of_one_uuid_are_ambiguous() {
        let source = fixture(SHEET).replacen(
            &format!("(uuid \"{PLAIN}\")"),
            &format!("(uuid \"{LABEL}\")"),
            1,
        );
        let (_dir, path) = sheet_with(&source);
        let revision = revision_of(&path).await;
        let message = refused(&delete(&path, LABEL, &revision).await, "ambiguous_target");
        assert!(message.contains("text, label"), "{message}");
        refused(
            &edit(&path, LABEL, &revision, "X").await,
            "ambiguous_target",
        );
        assert_eq!(read(&path), source);
    }

    #[tokio::test]
    async fn a_grouped_text_is_not_deleted() {
        let (_dir, path) = sheet_with(&grouped());
        let revision = revision_of(&path).await;
        let message = refused(&delete(&path, GROUPED, &revision).await, "plan_blocked");
        assert!(
            message.contains(&format!("group 'Notes' ({GROUP})")),
            "{message}"
        );
        assert_eq!(read(&path), grouped());
        // Its text may change; the group keeps it.
        body(&edit(&path, GROUPED, &revision, "Still grouped").await);
        assert!(read(&path).contains(&format!("(members \"{GROUPED}\")")));
    }

    #[tokio::test]
    async fn the_schema_refuses_what_it_does_not_declare() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        for (tool, args) in [
            ("list_schematic_texts", json!({ "uuid": PLAIN })),
            (
                "edit_schematic_text",
                json!({ "uuid": PLAIN, "revision": revision, "text": "X", "x": 1 }),
            ),
            ("edit_schematic_text", json!({ "uuid": PLAIN, "text": "X" })),
            (
                "edit_schematic_text",
                json!({ "uuid": PLAIN, "revision": revision, "text": "" }),
            ),
            (
                "edit_schematic_text",
                json!({ "uuid": 7, "revision": revision, "text": "X" }),
            ),
            ("delete_schematic_text", json!({ "uuid": PLAIN })),
        ] {
            refused(&call(tool, &path, args).await, "invalid_argument");
        }
        assert_eq!(read(&path), fixture(SHEET));
    }

    #[tokio::test]
    async fn the_handlers_check_their_arguments_without_the_schema() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        let context = ToolContext::new(
            ServerConfig::default(),
            Arc::new(crate::router::ToolRouter::new()),
        );
        let schematic = path.to_str().unwrap();
        for args in [
            json!({ "schematic": schematic, "uuid": PLAIN, "text": "X" }),
            json!({ "schematic": schematic, "uuid": PLAIN, "revision": revision, "text": "" }),
            json!({ "schematic": schematic, "uuid": PLAIN, "revision": revision, "text": "\r\r" }),
        ] {
            let result = handle_edit_schematic_text(&args, &context).await.unwrap();
            refused(&result, "invalid_argument");
        }
        let args = json!({ "schematic": schematic, "revision": revision });
        let result = handle_delete_schematic_text(&args, &context).await.unwrap();
        refused(&result, "invalid_argument");
        assert_eq!(read(&path), fixture(SHEET));
    }

    #[tokio::test]
    async fn a_sheet_kicad_holds_open_is_not_written() {
        let (dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        let lock = r#"{"username":"copperpour-test","hostname":"test-host"}"#;
        std::fs::write(dir.path().join(format!("~{SHEET}.lck")), lock).unwrap();
        refused(&edit(&path, PLAIN, &revision, "X").await, "conflict");
        refused(&delete(&path, PLAIN, &revision).await, "conflict");
        assert_eq!(read(&path), fixture(SHEET));
    }

    /// Eeschema locks only a project's root schematic, so the root's lock
    /// guards the sheets below it: KiCad's `complex_hierarchy` demo, whose
    /// `ampli_ht.kicad_sch`, placed twice, holds a free text Eeschema wrote.
    #[tokio::test]
    async fn a_sheet_below_a_root_kicad_holds_open_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "complex_hierarchy.kicad_pro",
            "complex_hierarchy.kicad_sch",
            "ampli_ht.kicad_sch",
        ] {
            let source = fixture(&format!("title_block_hierarchy/{name}"));
            std::fs::write(dir.path().join(name), source).unwrap();
        }
        let child = dir.path().join("ampli_ht.kicad_sch");
        let root_lock = dir.path().join("~complex_hierarchy.kicad_sch.lck");
        let revision = revision_of(&child).await;
        std::fs::write(
            &root_lock,
            r#"{"username":"copperpour-test","hostname":"test-host"}"#,
        )
        .unwrap();
        refused(&edit(&child, FILTER, &revision, "Filter").await, "conflict");
        refused(&delete(&child, FILTER, &revision).await, "conflict");
        assert_eq!(
            read(&child),
            fixture("title_block_hierarchy/ampli_ht.kicad_sch")
        );
        // With the root closed the same edit lands, in the one file both
        // placements read, and changes only the string.
        std::fs::remove_file(&root_lock).unwrap();
        body(&edit(&child, FILTER, &revision, "Filter").await);
        assert_eq!(
            read(&child),
            swapped(
                &fixture("title_block_hierarchy/ampli_ht.kicad_sch"),
                r#"(text "Filter:\nFc =1000Hz""#,
                r#"(text "Filter""#,
            )
        );
    }

    #[tokio::test]
    async fn a_sheet_saved_after_the_read_is_left_as_saved() {
        for deleting in [false, true] {
            let (_dir, path) = sheet_with(&fixture(SHEET));
            let revision = revision_of(&path).await;
            let saved = with_string("Turned note", "Saved in between");
            crate::tools::write_hook::set(saved.clone());
            let result = if deleting {
                delete(&path, PLAIN, &revision).await
            } else {
                edit(&path, PLAIN, &revision, "X").await
            };
            assert!(!crate::tools::write_hook::armed());
            refused(&result, "conflict");
            assert_eq!(read(&path), saved);
        }
    }

    /// A text the command would write that changes more than the one item,
    /// even a line between items, or leaves it in place, is refused before any
    /// byte reaches the file.
    #[tokio::test]
    async fn a_prospective_sheet_that_fails_its_check_is_not_written() {
        let (_dir, path) = sheet_with(&fixture(SHEET));
        let revision = revision_of(&path).await;
        let edited = with_string("Plain note", "X");
        let two_changed = swapped(&edited, "(text \"Turned note\"", "(text \"Also changed\"");
        let a_line_more = swapped(&edited, "\n\t(label \"NET_A\"", "\n\n\t(label \"NET_A\"");
        for prospective in [two_changed, a_line_more] {
            fault::set_prospective(prospective);
            refused(&edit(&path, PLAIN, &revision, "X").await, "stale_target");
        }
        fault::set_prospective(fixture(SHEET));
        refused(&delete(&path, PLAIN, &revision).await, "stale_target");
        assert_eq!(read(&path), fixture(SHEET));
    }

    /// A save over the file once the commit is done leaves the outcome unknown.
    #[tokio::test]
    async fn a_file_changed_after_the_commit_is_uncertain() {
        for deleting in [false, true] {
            let (_dir, path) = sheet_with(&fixture(SHEET));
            let revision = revision_of(&path).await;
            let saved = with_string("Turned note", "Saved after");
            fault::set_committed(saved.clone());
            let result = if deleting {
                delete(&path, PLAIN, &revision).await
            } else {
                edit(&path, PLAIN, &revision, "X").await
            };
            refused(&result, "mutation_outcome_uncertain");
            assert_eq!(read(&path), saved);
        }
    }

    #[test]
    fn the_check_finds_any_change_but_the_one() {
        let before = parsed(fixture(SHEET));
        let unchanged = parsed(fixture(SHEET));
        let relabel = |source: &str| swapped(source, "(label \"NET_A\"", "(label \"NET_B\"");
        let source = with_string("Plain note", "New");
        let one = parsed(source.clone());
        let two = parsed(swapped(&source, "(text \"Turned note\"", "(text \"Also\""));
        let and_a_label = parsed(relabel(&source));
        let new_note = kicad_text("New", "25.4 25.4 0", PLAIN);
        let resized = new_note.replacen("(size 1.27 1.27)", "(size 2 2)", 1);
        let and_its_font = parsed(swapped(&source, &new_note, &resized));
        assert_eq!(edited(&before, &one, PLAIN, "New"), Ok(()));
        assert!(edited(&before, &unchanged, PLAIN, "New").is_err());
        assert!(edited(&before, &one, PLAIN, "Other").is_err());
        assert!(edited(&before, &two, PLAIN, "New").is_err());
        assert!(edited(&before, &and_a_label, PLAIN, "New").is_err());
        assert!(edited(&before, &and_its_font, PLAIN, "New").is_err());

        let without_plain = without(&fixture(SHEET), &plain_note());
        let gone = parsed(without_plain.clone());
        assert_eq!(deleted(&before, &gone, PLAIN), Ok(()));
        assert!(deleted(&before, &unchanged, PLAIN).is_err());
        assert!(deleted(&before, &gone, TURNED).is_err());
        let grouped_note = kicad_text("Grouped note", "25.4 63.5 0", GROUPED);
        let two_gone = parsed(without(&without_plain, &grouped_note));
        assert!(deleted(&before, &two_gone, PLAIN).is_err());
        assert!(deleted(&before, &parsed(relabel(&without_plain)), PLAIN).is_err());
    }
}
