//! `repair_instance_metadata`: a sheet's placed-symbol instance records made
//! to match its project, so the writes that refuse them as `stale_target`
//! can proceed (#128).
//!
//! A record is dropped only when it is dead. Under the owning project's name,
//! that is a path its sheet tree does not give the file, which eeschema prunes
//! on loading the project (`eeschema/sch_screen.cpp:1628-1636` at 10.0.6) and
//! `add_hierarchical_sheet` relinks by. Under another name, it is a record of a
//! project no candidate `.kicad_pro` defines: the candidates are the project
//! files in the sheet's directory and those above it, among which
//! `resolve_schematic_ownership` finds the owner. A record of a project a
//! candidate defines may be that project's, sharing the sheet: it is kept and
//! named, and the repair is refused. A project in another directory that places
//! the sheet through `../` is not a candidate, and its records are dropped;
//! KiCad supports sharing a sheet file between projects but its Schematic
//! Editor manual does not recommend it (Adding hierarchical subsheets to a
//! design). The owning project's missing paths are added, each with the
//! reference and unit of a dropped record at that path, as a renamed project's
//! records keep theirs, else with the symbol's Reference and unit.

use crate::hex::encode_hex;
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::sch_hierarchy::commit_sheets;
use crate::tools::{get_path, invalid_arg, opt_str, ToolContext, ToolDef};
use copperpour_schematic_editor::Schematic;
use copperpour_sexp::writer::{apply_edits, find_direct_child_blocks};
use copperpour_sexp::{
    parse_sexp, prepare_command, read_consistent, DocumentRevision, FileTransition,
    SchematicCommand, SexpEdit,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const OPERATION: &str = "repair_instance_metadata";

pub(super) fn tool() -> ToolDef {
    tool!(
        "repair_instance_metadata",
        "Repair the symbol instance records that make writes to a sheet fail as \
         stale_target: drop the owning project's paths its sheet tree no longer gives the \
         file and records of projects no candidate project file defines, and add the owning \
         project's missing paths. A record of another candidate project is kept and named, \
         and the repair refused. Defaults to a dry run; apply requires its plan_revision \
         and is refused while KiCad holds the sheet or its project's root schematic open.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                "dry_run": {
                    "type": "boolean",
                    "default": true,
                    "description": "Report the plan without changing the sheet"
                },
                "expected_plan_revision": {
                    "type": "string",
                    "description": "Exact plan_revision returned by a current dry run; required for apply"
                }
            },
            "required": ["schematic"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_repair_instance_metadata(args, ctx).await }
    )
}

/// The project a sheet's records are judged against.
struct Owner {
    project: String,
    /// The paths the project's sheet tree gives the file, sorted.
    paths: Vec<String>,
    /// The project's root schematic, or the sheet itself when no project
    /// owns it; the transaction is journaled beside it.
    root: PathBuf,
}

impl Owner {
    fn of(path: &Path, own_uuid: &str) -> Result<Self, CallToolResult> {
        match crate::tools::resolve_schematic_ownership(path) {
            Err(error) => Err(error.into_tool_result()),
            Ok(None) => Ok(Self {
                project: crate::tools::project_name_for(path),
                paths: vec![format!("/{own_uuid}")],
                root: path.to_path_buf(),
            }),
            Ok(Some(owner)) => {
                // Eeschema keys a root listed with another uuid, or a second
                // top-level sheet, by paths this project's walk does not give.
                crate::tools::sch_annotate::sole_top_level_sheet(
                    &owner.project_file,
                    &owner.root_schematic,
                    &owner.root_uuid,
                )
                .map_err(|reason| invalid_arg("schematic", &reason))?;
                Ok(Self {
                    project: crate::tools::project_name_for(&owner.project_file),
                    paths: owner.instance_paths,
                    root: owner.root_schematic,
                })
            }
        }
    }
}

/// Whether another project's record is dead, and why: dead when no candidate
/// project file defines its project.
fn judge(project: &str, candidates: &BTreeSet<PathBuf>) -> Result<String, String> {
    match candidates
        .iter()
        .find(|file| crate::tools::project_name_for(file) == project)
    {
        Some(file) => Err(format!(
            "{} defines project '{project}', which may share this sheet",
            file.display()
        )),
        None => Ok(format!("no candidate project file defines '{project}'")),
    }
}

/// One record of the sheet, and the symbols that carry it.
#[derive(Default)]
struct Entry {
    reason: String,
    symbols: Vec<String>,
}

type Entries = BTreeMap<(String, String), Entry>;

fn note(entries: &mut Entries, project: &str, path: &str, reason: String, symbol: &str) {
    let entry = entries
        .entry((project.to_owned(), path.to_owned()))
        .or_default();
    entry.reason = reason;
    entry.symbols.push(symbol.to_owned());
}

fn listed(entries: &Entries, with_reason: bool) -> Vec<Value> {
    entries
        .iter()
        .map(|((project, path), entry)| {
            let mut symbols = entry.symbols.clone();
            symbols.sort();
            let mut item = json!({ "project": project, "path": path, "symbols": symbols });
            if with_reason {
                item["reason"] = json!(entry.reason);
            }
            item
        })
        .collect()
}

/// `content` with each placed symbol's `(instances …)` block cut out, line and
/// all: what a repair must leave byte for byte.
fn outside_instances(content: &str) -> String {
    let headed =
        |block: &str, head: &str| parse_sexp(block).is_ok_and(|node| node.head() == Some(head));
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(content, "kicad_sch") {
        let item = &content[start..end];
        if !headed(item, "symbol") {
            continue;
        }
        for (child_start, child_end) in find_direct_child_blocks(item, "symbol") {
            if headed(&item[child_start..child_end], "instances") {
                let (from, to) = whole_lines(content, start + child_start, start + child_end);
                edits.push(SexpEdit::delete(from, to));
            }
        }
    }
    apply_edits(content.to_owned(), edits)
}

/// `start..end` widened to its whole lines when nothing else is on them, as
/// `copperpour_sexp`'s own deletions widen theirs.
fn whole_lines(content: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = content[..start]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let line_end = content[end..]
        .find('\n')
        .map_or(content.len(), |newline| end + newline + 1);
    if content[line_start..start].trim().is_empty() && content[end..line_end].trim().is_empty() {
        (line_start, line_end)
    } else {
        (start, end)
    }
}

/// What the repair would do to the sheet as read.
struct Plan {
    owner: Owner,
    drop: Entries,
    add: Entries,
    keep: Entries,
    /// The sheet's text once repaired; `None` when nothing changes.
    prepared: Option<String>,
    /// Why the repaired sheet would still be refused, if it would.
    stale_after: Option<String>,
    revision: String,
}

impl Plan {
    fn json(&self, status: &str, applied: bool) -> Value {
        json!({
            "status": status,
            "project": self.owner.project,
            "instance_paths": self.owner.paths,
            "plan_revision": self.revision,
            "drop": listed(&self.drop, true),
            "add": listed(&self.add, false),
            "keep": listed(&self.keep, true),
            "stale_after": self.stale_after,
            "applied": applied,
            "source": "file"
        })
    }
}

fn plan(path: &Path, content: &str) -> anyhow::Result<Result<Plan, CallToolResult>> {
    let schematic = match Schematic::from_source(path, content) {
        Ok(schematic) => schematic,
        Err(error) => {
            return Ok(Err(invalid_arg(
                "schematic",
                &format!("'{}' is not a readable sheet: {error}", path.display()),
            )))
        }
    };
    let Some(own_uuid) = schematic.uuid.clone() else {
        return Ok(Err(invalid_arg(
            "schematic",
            &format!(
                "'{}' has no (uuid …), which every instance path of its symbols starts from",
                path.display()
            ),
        )));
    };
    let owner = match Owner::of(path, &own_uuid) {
        Ok(owner) => owner,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let candidates = match crate::tools::candidate_project_files(path) {
        Ok(candidates) => candidates,
        Err(error) => return Ok(Err(error.into_tool_result())),
    };
    let (mut drop, mut add, mut keep) = (Entries::new(), Entries::new(), Entries::new());
    for symbol in &schematic.symbols {
        let name = symbol
            .reference()
            .filter(|reference| !reference.is_empty())
            .unwrap_or(symbol.uuid.as_str());
        let mut recorded = Vec::new();
        for instance in symbol.instances() {
            let (Some(project), Some(record)) = (&instance.project, &instance.path) else {
                let reason = "a record with no project name or no path".to_owned();
                let project = instance.project.clone().unwrap_or_default();
                let record = instance.path.clone().unwrap_or_default();
                note(&mut keep, &project, &record, reason, name);
                continue;
            };
            let (project, record) = (project.clone(), record.clone());
            if project == owner.project {
                if !owner.paths.contains(&record) {
                    let reason = format!(
                        "not a sheet instance of this file in project '{}', which eeschema \
                         prunes on loading it",
                        owner.project
                    );
                    note(&mut drop, &project, &record, reason, name);
                } else if recorded.contains(&record) {
                    let reason = "repeats a record the symbol keeps".to_owned();
                    note(&mut drop, &project, &record, reason, name);
                } else {
                    recorded.push(record);
                }
                continue;
            }
            match judge(&project, &candidates) {
                Ok(reason) => note(&mut drop, &project, &record, reason, name),
                Err(reason) => note(&mut keep, &project, &record, reason, name),
            }
        }
        for missing in owner
            .paths
            .iter()
            .filter(|wanted| !recorded.contains(wanted))
        {
            note(&mut add, &owner.project, missing, String::new(), name);
        }
    }

    // A project no candidate defines is dead in every record, so its whole
    // block goes; `drop` is sorted by project, so each name comes once.
    let mut dropped: Vec<String> = drop
        .keys()
        .map(|(project, _)| project.clone())
        .filter(|project| *project != owner.project)
        .collect();
    dropped.dedup();
    let command = match SchematicCommand::repair_symbol_instances(
        content,
        &owner.project,
        &owner.paths,
        &dropped,
        OPERATION,
    ) {
        Ok(command) => command,
        Err(error) => {
            return Ok(Err(invalid_arg(
                "schematic",
                &format!("its instance records cannot be repaired: {error}"),
            )))
        }
    };
    let prepared = match command {
        Some(command) => {
            let prepared = prepare_command(path, content, &command)?.0;
            #[cfg(test)]
            let prepared = fault::prospective(prepared);
            if outside_instances(&prepared) != outside_instances(content) {
                return Ok(Err(CallToolResult::error_kind(
                    ToolErrorKind::StaleTarget {
                        target: path.display().to_string(),
                        reason: "the repair would change the sheet beyond its symbols' \
                                 instance records"
                            .to_owned(),
                    },
                    format!(
                        "{OPERATION} checked the sheet it would write and nothing was written: \
                         it would change more than the symbols' instance records"
                    ),
                )));
            }
            Some(prepared)
        }
        None => None,
    };
    // Judged as every write judges the sheet, against the repaired text.
    let mut after = match Schematic::from_source(path, prepared.as_deref().unwrap_or(content)) {
        Ok(after) => after,
        Err(error) => {
            return Ok(Err(CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: path.display().to_string(),
                    reason: format!("the repaired sheet would not parse: {error}"),
                },
                format!("{OPERATION} checked the sheet it would write and nothing was written"),
            )))
        }
    };
    let stale_after = match crate::tools::sheet_instance_context(path, &mut after) {
        Ok(context) => crate::tools::validate_sheet_instance_state(path, &after, &context)
            .err()
            .map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    };

    let mut digest = Sha256::new();
    for field in [
        OPERATION,
        &DocumentRevision::of(content).to_string(),
        prepared.as_deref().unwrap_or(""),
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    for (project, record) in keep.keys() {
        digest.update(project.as_bytes());
        digest.update([0]);
        digest.update(record.as_bytes());
        digest.update([0]);
    }
    Ok(Ok(Plan {
        owner,
        drop,
        add,
        keep,
        prepared,
        stale_after,
        revision: encode_hex(&digest.finalize()),
    }))
}

async fn handle_repair_instance_metadata(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let schematic = get_path(args, "schematic")?;
    let dry_run = match args.get("dry_run") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(dry_run)) => *dry_run,
        Some(_) => return Ok(invalid_arg("dry_run", "must be a boolean")),
    };
    let expected = opt_str(args, "expected_plan_revision");
    if !dry_run && expected.is_none() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_owned(),
                reason: "required when dry_run is false".to_owned(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }
    if !schematic.is_file() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::FileNotFound {
                path: schematic.display().to_string(),
            },
            format!("Schematic '{}' not found", schematic.display()),
        ));
    }
    let content = read_consistent(&schematic)?;
    let plan = match plan(&schematic, &content)? {
        Ok(plan) => plan,
        Err(refusal) => return Ok(refusal),
    };
    // `partial`: the repair applies, and the sheet is still refused for a
    // reason `stale_after` names, such as a unit or reference it does not repair.
    let status = match (
        plan.keep.is_empty(),
        plan.prepared.is_some(),
        plan.stale_after.is_some(),
    ) {
        (false, _, _) => "blocked",
        (true, false, _) => "noop",
        (true, true, false) => "ready",
        (true, true, true) => "partial",
    };
    if dry_run {
        return Ok(CallToolResult::json(&plan.json(status, false)));
    }
    if !plan.keep.is_empty() {
        let reasons: Vec<String> = plan
            .keep
            .iter()
            .map(|((project, path), entry)| format!("{project}:{path}: {}", entry.reason))
            .collect();
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: OPERATION.to_owned(),
                reasons: reasons.clone(),
            },
            format!(
                "No record was changed: these records are not proven dead, so the sheet is \
                 not repaired: {}",
                reasons.join("; ")
            ),
        ));
    }
    if expected != Some(plan.revision.as_str()) {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::StaleTarget {
                target: schematic.display().to_string(),
                reason: "the sheet or its project changed since the dry run that returned \
                         this plan_revision"
                    .to_owned(),
            },
            "No record was changed: run repair_instance_metadata as a dry run again and pass \
             its plan_revision.",
        ));
    }
    let Some(prepared) = plan.prepared.clone() else {
        return Ok(CallToolResult::json(&plan.json(status, false)));
    };
    let transition = FileTransition::replace(&schematic, content.clone(), prepared.clone());
    #[cfg(test)]
    crate::tools::write_hook::run(&schematic);
    if let Err(refusal) = commit_sheets(&plan.owner.root, &[schematic.as_path()], vec![transition])?
    {
        return Ok(refusal);
    }
    #[cfg(test)]
    fault::committed(&schematic);
    match read_consistent(&schematic) {
        // The file holds the checked text byte for byte, so the plan's
        // `stale_after` is the readback's.
        Ok(on_disk) if on_disk == prepared => Ok(CallToolResult::json(&plan.json("applied", true))),
        Ok(_) => Ok(crate::tools::mutation_outcome_uncertain(
            &schematic,
            OPERATION,
            "the file on disk is not the text the transaction wrote",
        )),
        Err(error) => Ok(crate::tools::mutation_outcome_uncertain(
            &schematic,
            OPERATION,
            format!("the sheet could not be read back: {error}"),
        )),
    }
}

/// Faults a test puts in a repair's way: a prepared text in place of the one
/// the command gave, or a save over the file once the commit is done.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use crate::mcp::handler::McpHandler;
    use crate::mcp::protocol::ToolContent;
    use crate::tools::ServerConfig;

    /// The `dead` and `shared` leaf's own uuid: the path of the record it keeps
    /// from being a project's top-level sheet. See the fixtures' README.
    const LEAF_ROOT: &str = "/28a9c51f-ac88-4382-857b-2bd9f911524d";
    const OWNER_A: &str =
        "/0283ae14-46d1-4035-9465-08c9d058cabd/df588be5-ee17-47a5-bff4-d80bef7e7401";
    const OWNER_B: &str =
        "/0283ae14-46d1-4035-9465-08c9d058cabd/b0fe0e7a-f8ee-431e-b018-f905325d2651";
    const OTHER: &str =
        "/e7c1f0eb-0430-4fe2-ae43-7be549a9eaf2/514ce7ab-4c9f-496e-980a-e62e9fef36a0";
    const MID: &str = "/10e35b97-2768-43db-bbd4-069efaa8d3dc/75a3c0c9-a81d-4a69-ae60-c93bcb839c87";
    const NESTED_OWNER: &str = "/bfac27e2-2c6c-46a8-9315-c9b7161f625a/\
        bdff1d21-81be-45f6-8bc4-79b6cdf6dc49/75a3c0c9-a81d-4a69-ae60-c93bcb839c87";

    fn fixture(case: &str, name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/instance_repair")
                .join(case)
                .join(name),
        )
        .expect("the fixture is committed")
    }

    /// The case's files in `directory`, and its `leaf.kicad_sch`.
    fn case_in(directory: &Path, case: &str) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/instance_repair")
            .join(case);
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), directory.join(entry.file_name())).unwrap();
        }
        directory.join("leaf.kicad_sch")
    }

    fn case(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let leaf = case_in(dir.path(), name);
        (dir, leaf)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    async fn repair(sheet: &Path, mut args: Value) -> CallToolResult {
        args["schematic"] = json!(sheet.to_str().unwrap());
        served(OPERATION, args).await
    }

    async fn served(tool: &str, args: Value) -> CallToolResult {
        let handler = McpHandler::new(ServerConfig {
            auto_load_toolsets: true,
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 128, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            }))
            .await
            .expect("tools/call receives a response");
        assert!(response.error.is_none(), "tool errors are MCP results");
        serde_json::from_value(response.result.expect("a result")).expect("an MCP result")
    }

    async fn dry_run(sheet: &Path) -> Value {
        body(&repair(sheet, json!({})).await)
    }

    async fn apply(sheet: &Path, revision: &str) -> CallToolResult {
        repair(
            sheet,
            json!({ "dry_run": false, "expected_plan_revision": revision }),
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

    /// The `(project, path)` records, references and units each symbol of the
    /// saved sheet carries, in file order.
    fn records(path: &Path) -> Vec<Vec<(String, String, String, u32)>> {
        Schematic::load(path)
            .unwrap()
            .symbols
            .iter()
            .map(|symbol| {
                symbol
                    .instances()
                    .into_iter()
                    .map(|instance| {
                        (
                            instance.project.unwrap(),
                            instance.path.unwrap(),
                            instance.reference.unwrap(),
                            instance.unit.unwrap(),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    /// The check every refused write runs, on the saved sheet.
    fn refusal(path: &Path) -> Option<String> {
        let mut schematic = Schematic::load(path).unwrap();
        let context = crate::tools::sheet_instance_context(path, &mut schematic).unwrap();
        crate::tools::validate_sheet_instance_state(path, &schematic, &context)
            .err()
            .map(|error| error.to_string())
    }

    /// `source` with each `(project "<project>" …)` block KiCad wrote at three
    /// tabs cut out, line and all.
    fn without_project(source: &str, project: &str) -> String {
        let open = format!("\n\t\t\t(project \"{project}\"\n");
        let mut kept = source.to_owned();
        while let Some(start) = kept.find(&open) {
            let end = start + kept[start..].find("\n\t\t\t)\n").unwrap() + "\n\t\t\t)".len();
            kept.replace_range(start..end, "");
        }
        kept
    }

    #[tokio::test]
    async fn a_dead_record_is_dropped_and_writes_are_no_longer_refused() {
        let (_dir, leaf) = case("dead");
        let before = read(&leaf);
        let stale = refusal(&leaf).expect("the fixture reproduces the refusal");
        assert!(stale.contains(&format!("leaf:{LEAF_ROOT}")), "{stale}");

        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["project"], "owner");
        assert_eq!(plan["instance_paths"], json!([OWNER_B, OWNER_A]));
        assert_eq!(plan["drop"].as_array().unwrap().len(), 1);
        assert_eq!(plan["drop"][0]["project"], "leaf");
        assert_eq!(plan["drop"][0]["path"], LEAF_ROOT);
        assert_eq!(plan["drop"][0]["symbols"], json!(["R1", "R2"]));
        assert_eq!(plan["add"], json!([]));
        assert_eq!(plan["keep"], json!([]));
        assert_eq!(plan["stale_after"], Value::Null);
        assert_eq!(plan["applied"], false);
        assert_eq!(read(&leaf), before, "a dry run writes nothing");

        let applied = body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["applied"], true);
        // Only the dead blocks go; every other byte of the sheet stays.
        assert_eq!(read(&leaf), without_project(&before, "leaf"));
        let owner = |reference: &str| {
            [OWNER_B, OWNER_A]
                .map(|path| ("owner".to_owned(), path.to_owned(), reference.to_owned(), 1))
                .to_vec()
        };
        // Eeschema wrote R2 first.
        assert_eq!(records(&leaf), vec![owner("R2"), owner("R1")]);
        assert_eq!(refusal(&leaf), None);

        let again = dry_run(&leaf).await;
        assert_eq!(again["status"], "noop");
        assert_eq!(
            body(&apply(&leaf, again["plan_revision"].as_str().unwrap()).await)["status"],
            "noop"
        );
    }

    #[tokio::test]
    async fn apply_needs_the_revision_of_a_current_dry_run() {
        let (_dir, leaf) = case("dead");
        let before = read(&leaf);
        let missing = repair(&leaf, json!({ "dry_run": false })).await;
        assert!(refused(&missing, "invalid_argument").contains("plan revision"));
        refused(&apply(&leaf, "not-a-revision").await, "stale_target");
        assert_eq!(read(&leaf), before);
    }

    /// The shared case's `other` record is what a project in a sibling
    /// directory that places `../owner/leaf.kicad_sch` leaves in the file. No
    /// candidate project file defines `other`, so it is dropped with the
    /// record `kicad-cli` resaved under no project, and the dry run names both.
    #[tokio::test]
    async fn records_of_projects_no_candidate_defines_are_dropped() {
        let (_dir, leaf) = case("shared");
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["keep"], json!([]));
        let dropped: Vec<(&str, &str)> = plan["drop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["project"].as_str().unwrap(),
                    entry["path"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(dropped, vec![("", LEAF_ROOT), ("other", OTHER)]);
        assert_eq!(plan["drop"][1]["symbols"], json!(["R1", "R2"]));

        body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        let owner = |reference: &str| {
            [OWNER_B, OWNER_A]
                .map(|path| ("owner".to_owned(), path.to_owned(), reference.to_owned(), 1))
                .to_vec()
        };
        assert_eq!(records(&leaf), vec![owner("R2"), owner("R1")]);
        assert_eq!(refusal(&leaf), None);
    }

    /// A project of the record's name in a directory above the sheet keeps
    /// the record, though its root does not place the sheet.
    #[tokio::test]
    async fn a_project_file_of_the_records_name_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("leaf.kicad_pro"),
            fixture("dead", "owner.kicad_pro"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("leaf.kicad_sch"),
            fixture("dead", "leaf.kicad_sch"),
        )
        .unwrap();
        let below = dir.path().join("owner");
        std::fs::create_dir(&below).unwrap();
        let leaf = case_in(&below, "dead");
        let before = read(&leaf);

        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "blocked");
        assert_eq!(plan["keep"][0]["project"], "leaf");
        assert_eq!(plan["keep"][0]["path"], LEAF_ROOT);
        let reason = plan["keep"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("leaf.kicad_pro"), "{reason}");
        refused(
            &apply(&leaf, plan["plan_revision"].as_str().unwrap()).await,
            "plan_blocked",
        );
        assert_eq!(read(&leaf), before);
    }

    /// The nested case's leaf keeps the record of `mid`, whose root it was
    /// placed in before `mid.kicad_sch` joined `owner`, and lacks `owner`'s.
    #[tokio::test]
    async fn a_nested_sheet_gains_its_projects_path() {
        let (_dir, leaf) = case("nested");
        assert!(refusal(&leaf).is_some());
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["drop"][0]["project"], "mid");
        assert_eq!(plan["drop"][0]["path"], MID);
        assert_eq!(
            plan["add"],
            json!([{ "project": "owner", "path": NESTED_OWNER, "symbols": ["R1"] }])
        );
        assert_eq!(plan["stale_after"], Value::Null);

        body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        assert_eq!(
            records(&leaf),
            vec![vec![(
                "owner".to_owned(),
                NESTED_OWNER.to_owned(),
                "R1".to_owned(),
                1
            )]]
        );
        assert_eq!(refusal(&leaf), None);
    }

    /// KiCad 10 keys a second top-level sheet, or a root the project file
    /// gives another uuid, by paths the project's walk does not give. No tool
    /// here lists a second top-level sheet, so the entry is written into the
    /// project file `create_project` wrote, as `sch_annotate`'s test does.
    #[tokio::test]
    async fn a_project_with_another_top_level_sheet_is_refused() {
        const NIL: &str = "00000000-0000-0000-0000-000000000000";
        let root =
            |uuid: &str| json!({ "uuid": uuid, "name": "owner", "filename": "owner.kicad_sch" });
        let second = json!({ "uuid": "11111111-1111-1111-1111-111111111111", "name": "second",
            "filename": "second.kicad_sch" });
        for (sheets, refusal) in [
            (json!([root(NIL), second]), "top-level sheets"),
            (
                json!([root("22222222-2222-2222-2222-222222222222")]),
                "gives the root the uuid",
            ),
        ] {
            let (dir, leaf) = case("dead");
            let before = read(&leaf);
            let mut settings: Value =
                serde_json::from_str(&fixture("dead", "owner.kicad_pro")).unwrap();
            settings["schematic"]["top_level_sheets"] = sheets;
            std::fs::write(dir.path().join("owner.kicad_pro"), settings.to_string()).unwrap();
            let message = refused(&repair(&leaf, json!({})).await, "invalid_argument");
            assert!(message.contains(refusal), "{message}");
            assert_eq!(read(&leaf), before);
        }
    }

    const LOCK: &str = r#"{"username":"copperpour-test","hostname":"test-host"}"#;

    #[tokio::test]
    async fn a_sheet_kicad_holds_open_is_not_repaired() {
        let (dir, leaf) = case("dead");
        let before = read(&leaf);
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        std::fs::write(dir.path().join("~leaf.kicad_sch.lck"), LOCK).unwrap();
        refused(&apply(&leaf, &revision).await, "conflict");
        assert_eq!(read(&leaf), before);
    }

    /// Eeschema locks only a project's root schematic, so the root's lock
    /// guards the sheets below it.
    #[tokio::test]
    async fn a_sheet_below_a_root_kicad_holds_open_is_not_repaired() {
        let (dir, leaf) = case("dead");
        let before = read(&leaf);
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        std::fs::write(dir.path().join("~owner.kicad_sch.lck"), LOCK).unwrap();
        refused(&apply(&leaf, &revision).await, "conflict");
        assert_eq!(read(&leaf), before);
    }

    /// A save between the read and the commit, which the transaction refuses.
    #[tokio::test]
    async fn a_sheet_saved_after_the_read_is_not_written() {
        let (_dir, leaf) = case("dead");
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        crate::tools::write_hook::set(misreferenced());
        refused(&apply(&leaf, &revision).await, "conflict");
        assert_eq!(read(&leaf), misreferenced());
    }

    /// The renamed case: project `old`, its leaf placed twice and annotated R1
    /// and R2 at one placement and R3 and R4 at the other, renamed `new` by
    /// renaming its files. The leaf still records `old` at `new`'s own paths;
    /// the repair keeps each placement's reference, writing the leaf as
    /// eeschema saves it in project `new`.
    #[tokio::test]
    async fn a_renamed_projects_records_keep_their_references() {
        const RENAMED_A: &str =
            "/4e248827-0def-461b-af97-fa182d8a4130/24f983cc-504c-4e18-bfb8-690f4a305c3f";
        const RENAMED_B: &str =
            "/4e248827-0def-461b-af97-fa182d8a4130/9b6ba800-6b78-4f4d-9f50-4a619da31709";
        let (_dir, leaf) = case("renamed");
        let before = read(&leaf);
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["project"], "new");
        assert_eq!(plan["keep"], json!([]));
        assert_eq!(
            plan["add"],
            json!([
                { "project": "new", "path": RENAMED_A, "symbols": ["R1", "R2"] },
                { "project": "new", "path": RENAMED_B, "symbols": ["R1", "R2"] }
            ])
        );
        body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        assert_eq!(
            read(&leaf),
            before.replace("(project \"old\"", "(project \"new\"")
        );
        let references: Vec<Vec<String>> = records(&leaf)
            .into_iter()
            .map(|symbol| symbol.into_iter().map(|record| record.2).collect())
            .collect();
        assert_eq!(references, [["R1", "R3"], ["R2", "R4"]]);
        assert_eq!(refusal(&leaf), None);
    }

    /// A sheet no project owns is its own project, keyed at its own uuid: the
    /// dead leaf alone keeps the `leaf` record and drops `owner`'s.
    #[tokio::test]
    async fn a_loose_sheet_is_judged_as_its_own_project() {
        let dir = tempfile::tempdir().unwrap();
        let leaf = dir.path().join("leaf.kicad_sch");
        std::fs::write(&leaf, fixture("dead", "leaf.kicad_sch")).unwrap();
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["project"], "leaf");
        assert_eq!(plan["instance_paths"], json!([LEAF_ROOT]));
        let dropped: Vec<(&str, &str)> = plan["drop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["project"].as_str().unwrap(),
                    entry["path"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(dropped, vec![("owner", OWNER_B), ("owner", OWNER_A)]);
    }

    /// A project file named after the leaf beside it makes the leaf a root of
    /// its own as well as `owner`'s sheet: two owners, refused by the
    /// resolver.
    #[tokio::test]
    async fn a_sheet_two_projects_own_is_a_conflict() {
        let (dir, leaf) = case("dead");
        let before = read(&leaf);
        std::fs::write(
            dir.path().join("leaf.kicad_pro"),
            fixture("dead", "owner.kicad_pro"),
        )
        .unwrap();
        refused(&repair(&leaf, json!({})).await, "conflict");
        assert_eq!(read(&leaf), before);
    }

    #[tokio::test]
    async fn a_missing_or_uuidless_sheet_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        refused(
            &repair(&dir.path().join("absent.kicad_sch"), json!({})).await,
            "file_not_found",
        );
        // An explicit corruption: the leaf without the `(uuid …)` every path
        // of its symbols starts from.
        let (_dir, leaf) = case("dead");
        let source = fixture("dead", "leaf.kicad_sch").replacen(
            "\n\t(uuid \"28a9c51f-ac88-4382-857b-2bd9f911524d\")",
            "",
            1,
        );
        std::fs::write(&leaf, &source).unwrap();
        let message = refused(&repair(&leaf, json!({})).await, "invalid_argument");
        assert!(message.contains("has no (uuid"), "{message}");
        assert_eq!(read(&leaf), source);
    }

    /// An explicit corruption no tool writes: R2's `leaf` record without its
    /// path string. It is kept, and the sheet not repaired.
    #[tokio::test]
    async fn a_record_with_no_path_is_kept() {
        let (_dir, leaf) = case("dead");
        let source = fixture("dead", "leaf.kicad_sch").replacen(
            &format!("(path \"{LEAF_ROOT}\""),
            "(path",
            1,
        );
        std::fs::write(&leaf, &source).unwrap();
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "blocked");
        let reason = plan["keep"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("no project name or no path"), "{reason}");
        let message = refused(
            &apply(&leaf, plan["plan_revision"].as_str().unwrap()).await,
            "plan_blocked",
        );
        assert!(message.contains("not proven dead"), "{message}");
        assert_eq!(read(&leaf), source);
    }

    /// An explicit corruption: R2's first `owner` record written twice. The
    /// repeat is dropped.
    #[tokio::test]
    async fn a_repeated_record_is_dropped() {
        let (_dir, leaf) = case("dead");
        let record = format!(
            "\t\t\t\t(path \"{OWNER_B}\"\n\t\t\t\t\t(reference \"R2\")\n\
             \t\t\t\t\t(unit 1)\n\t\t\t\t)\n"
        );
        let source =
            fixture("dead", "leaf.kicad_sch").replacen(&record, &format!("{record}{record}"), 1);
        assert_ne!(source, fixture("dead", "leaf.kicad_sch"));
        std::fs::write(&leaf, &source).unwrap();
        let plan = dry_run(&leaf).await;
        let repeat = plan["drop"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["project"] == "owner")
            .expect("the repeat is dropped");
        assert_eq!(repeat["path"], OWNER_B);
        assert!(repeat["reason"].as_str().unwrap().contains("repeats"));
        body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        assert_eq!(refusal(&leaf), None);
    }

    /// `delete_sheet` drops the deleted placement's path from the leaf; the
    /// leaf as saved before the delete, put back, still records it.
    #[tokio::test]
    async fn a_path_its_project_no_longer_gives_the_file_is_dropped() {
        let (dir, leaf) = case("dead");
        let root = dir.path().join("owner.kicad_sch");
        let deleted = served(
            "delete_sheet",
            json!({ "schematic": root.to_str().unwrap(), "sheet_name": "Leaf B" }),
        )
        .await;
        assert!(!deleted.is_error, "{}", text(&deleted));
        std::fs::write(&leaf, fixture("dead", "leaf.kicad_sch")).unwrap();

        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "ready");
        assert_eq!(plan["instance_paths"], json!([OWNER_A]));
        let dropped: Vec<(&str, &str)> = plan["drop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["project"].as_str().unwrap(),
                    entry["path"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(dropped, vec![("leaf", LEAF_ROOT), ("owner", OWNER_B)]);

        body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        let owner = |reference: &str| {
            vec![(
                "owner".to_owned(),
                OWNER_A.to_owned(),
                reference.to_owned(),
                1,
            )]
        };
        assert_eq!(records(&leaf), vec![owner("R2"), owner("R1")]);
        assert_eq!(refusal(&leaf), None);
    }

    /// The dead fixture with R2's first `owner` record naming `C2`, an explicit
    /// corruption no tool writes: a reference that keeps no designator prefix.
    fn misreferenced() -> String {
        fixture("dead", "leaf.kicad_sch").replacen("(reference \"R2\")", "(reference \"C2\")", 1)
    }

    #[tokio::test]
    async fn a_repair_that_leaves_the_sheet_refused_is_partial() {
        let (_dir, leaf) = case("dead");
        std::fs::write(&leaf, misreferenced()).unwrap();
        let plan = dry_run(&leaf).await;
        assert_eq!(plan["status"], "partial");
        let stale = plan["stale_after"].as_str().unwrap();
        assert!(stale.contains("reference"), "{stale}");

        let applied = body(&apply(&leaf, plan["plan_revision"].as_str().unwrap()).await);
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["stale_after"], plan["stale_after"]);
        assert_eq!(read(&leaf), without_project(&misreferenced(), "leaf"));
        assert!(refusal(&leaf).is_some());
    }

    #[tokio::test]
    async fn apply_refuses_a_sheet_changed_since_the_dry_run() {
        let (_dir, leaf) = case("dead");
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        std::fs::write(&leaf, misreferenced()).unwrap();
        refused(&apply(&leaf, &revision).await, "stale_target");
        assert_eq!(read(&leaf), misreferenced());
    }

    /// A fault, not a sheet: the prepared text swapped for the repair's own
    /// with a value changed too, which no correct command writes.
    #[tokio::test]
    async fn a_text_changing_more_than_instance_records_is_not_written() {
        let (_dir, leaf) = case("dead");
        let before = read(&leaf);
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        fault::set_prospective(without_project(&before, "leaf").replace("\"10k\"", "\"12k\""));
        let message = refused(&apply(&leaf, &revision).await, "stale_target");
        assert!(
            message.contains("more than the symbols' instance records"),
            "{message}"
        );
        assert_eq!(read(&leaf), before);
    }

    #[tokio::test]
    async fn a_save_over_the_repaired_sheet_is_an_uncertain_outcome() {
        let (_dir, leaf) = case("dead");
        let before = read(&leaf);
        let revision = dry_run(&leaf).await["plan_revision"]
            .as_str()
            .unwrap()
            .to_owned();
        fault::set_committed(before.clone());
        refused(&apply(&leaf, &revision).await, "mutation_outcome_uncertain");
        assert_eq!(read(&leaf), before);
    }

    /// The handler's own checks, called past the dispatch whose schema would
    /// refuse a non-boolean `dry_run` first.
    #[tokio::test]
    async fn the_handler_checks_its_arguments_without_the_schema() {
        let (_dir, leaf) = case("dead");
        let before = read(&leaf);
        let context = ToolContext::new(
            ServerConfig::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        let schematic = leaf.to_str().unwrap();
        for (args, field) in [
            (
                json!({ "schematic": schematic, "dry_run": "no" }),
                "dry_run",
            ),
            (
                json!({ "schematic": schematic, "dry_run": false }),
                "expected_plan_revision",
            ),
        ] {
            let result = handle_repair_instance_metadata(&args, &context)
                .await
                .unwrap();
            let message = refused(&result, "invalid_argument");
            assert!(message.contains(field), "{message}");
        }
        assert_eq!(read(&leaf), before);
    }
}
