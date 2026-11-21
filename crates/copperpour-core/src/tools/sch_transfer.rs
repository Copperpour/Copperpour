//! Identity-preserving transfer between saved, single-instance sheets.
//!
//! Planning edits raw object blocks, not typed round trips. KiCad independently
//! checks the complete candidate hierarchy before the journaled write begins.

use super::{cli, invalid_arg, ToolContext, ToolDef};
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::tool;
use anyhow::{ensure, Context, Result};
use copperpour_sexp::writer::{find_balanced_block, find_direct_child_blocks};
use copperpour_sexp::{
    apply_edits, parse_sexp, read_consistent, FileTransition, SexpEdit, SexpNode,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const OPERATION: &str = "move_schematic_objects_between_sheets";

pub(super) fn tool() -> ToolDef {
    tool!(
        "move_schematic_objects_between_sheets",
        "Move existing circuit objects between explicit hierarchy instances in one saved \
         project, preserving UUIDs, references and relative geometry. Defaults to dry-run. \
         Apply requires kicad-cli for before/after netlist and ERC checks. Both sheets and the \
         project PCB must be closed. Reused source/destination sheets, buses, hierarchical \
         label moves, boundary connections and local-label collisions are refused with \
         explicit rewiring diagnostics. Apply requires the plan_revision of a current dry \
         run. Updates matching footprint paths and renamed nets on the same-named PCB in \
         the same transaction. Refuses collisions and unsafe net-name dependencies. Recovery completes the journaled move \
         forward unless a file it read has changed. Never deletes a source sheet.",
        json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "schematic": {"type":"string", "description":"Project entry .kicad_sch beside its same-named .kicad_pro"},
                "source_sheet_instance": {"type":"string", "description":"/<top-level instance UUID>[/<sheet UUID>...], using the project-declared top-level identity and child sheet-symbol UUIDs"},
                "destination_sheet_instance": {"type":"string"},
                "object_uuids": {"type":"array", "minItems":1, "uniqueItems":true, "items":{"type":"string"}},
                "offset_x_mm": {"type":"number"}, "offset_y_mm": {"type":"number"},
                "dry_run": {"type":"boolean", "default":true},
                "expected_plan_revision": {"type":"string", "description":"Exact plan_revision returned by a current dry run; required for apply"}
            },
            "required":["schematic","source_sheet_instance","destination_sheet_instance","object_uuids","offset_x_mm","offset_y_mm"]
        }),
        |args, ctx| async move { handle_move_schematic_objects_between_sheets(args, ctx).await }
    )
}

#[derive(Debug)]
struct Request {
    schematic: PathBuf,
    source_sheet_instance: String,
    destination_sheet_instance: String,
    object_uuids: Vec<String>,
    offset_x_mm: f64,
    offset_y_mm: f64,
    dry_run: bool,
    expected_plan_revision: Option<String>,
}

const ARGUMENTS: [&str; 8] = [
    "schematic",
    "source_sheet_instance",
    "destination_sheet_instance",
    "object_uuids",
    "offset_x_mm",
    "offset_y_mm",
    "dry_run",
    "expected_plan_revision",
];

impl Request {
    /// Read every argument by name, refusing unknown, missing and mistyped ones
    /// before anything is planned.
    fn from_args(args: &Value) -> std::result::Result<Self, CallToolResult> {
        let Some(object) = args.as_object() else {
            return Err(invalid_arg("arguments", "must be an object"));
        };
        if let Some(unknown) = object.keys().find(|key| !ARGUMENTS.contains(&key.as_str())) {
            return Err(invalid_arg(unknown, "is not an argument of this tool"));
        }
        let text = |field: &str| {
            args[field]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_arg(field, "a string is required"))
        };
        let number = |field: &str| {
            args[field]
                .as_f64()
                .ok_or_else(|| invalid_arg(field, "a number is required"))
        };
        let object_uuids = args["object_uuids"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| invalid_arg("object_uuids", "an array of UUID strings is required"))?;
        let dry_run = match &args["dry_run"] {
            Value::Null => true,
            Value::Bool(dry_run) => *dry_run,
            _ => return Err(invalid_arg("dry_run", "must be a boolean")),
        };
        let expected_plan_revision = match &args["expected_plan_revision"] {
            Value::Null => None,
            Value::String(revision) => Some(revision.clone()),
            _ => return Err(invalid_arg("expected_plan_revision", "must be a string")),
        };
        if object_uuids
            .iter()
            .any(|uuid| uuid::Uuid::parse_str(uuid).is_err())
        {
            return Err(invalid_arg("object_uuids", "every entry must be a UUID"));
        }
        let instance = |field: &str| {
            let path = text(field)?;
            let valid = path.strip_prefix('/').is_some_and(|tail| {
                tail.split('/')
                    .all(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
            });
            if valid {
                Ok(path)
            } else {
                Err(invalid_arg(
                    field,
                    "must be a /<root UUID>/<sheet UUID>... path",
                ))
            }
        };
        let offset = |field: &str| {
            let value = number(field)?;
            if value.is_finite() && value.abs() <= 100_000.0 {
                Ok(value)
            } else {
                Err(invalid_arg(field, "must be finite and within 100000 mm"))
            }
        };
        let schematic = PathBuf::from(text("schematic")?);
        if schematic.extension().is_none_or(|ext| ext != "kicad_sch") {
            return Err(invalid_arg("schematic", "must be the root .kicad_sch file"));
        }
        Ok(Self {
            schematic,
            source_sheet_instance: instance("source_sheet_instance")?,
            destination_sheet_instance: instance("destination_sheet_instance")?,
            object_uuids,
            offset_x_mm: offset("offset_x_mm")?,
            offset_y_mm: offset("offset_y_mm")?,
            dry_run,
            expected_plan_revision,
        })
    }
}

struct Document {
    before: String,
    after: String,
}

struct Plan {
    root: PathBuf,
    documents: BTreeMap<PathBuf, Document>,
    board: Option<PathBuf>,
    instances: BTreeMap<String, PathBuf>,
    netlist_identities: BTreeMap<String, BTreeSet<String>>,
    report: Value,
}

const OPTIONAL_INPUTS: [&str; 2] = ["sym-lib-table", "fp-lib-table"];

#[derive(Debug, thiserror::Error)]
#[error("The saved project inputs no longer match the dry run")]
struct StalePlanInputs(PathBuf);

#[derive(Debug, thiserror::Error)]
#[error("A project transaction journal is pending")]
struct PendingTransaction {
    journal: PathBuf,
    directory: PathBuf,
}

fn blocked(reason: impl ToString) -> CallToolResult {
    let reason = reason.to_string();
    CallToolResult::error_kind(
        ToolErrorKind::PlanBlocked {
            operation: OPERATION.into(),
            reasons: vec![reason.clone()],
        },
        format!("Transfer was not applied. {reason}"),
    )
}

fn stale_plan(target: &Path) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: target.display().to_string(),
            reason: "the project or verification changed since the dry run".into(),
        },
        "Transfer was not applied: run move_schematic_objects_between_sheets as a dry run again and pass its plan_revision.",
    )
}

async fn handle_move_schematic_objects_between_sheets(
    args: &Value,
    ctx: &ToolContext,
) -> Result<CallToolResult> {
    use crate::outcome::{self, OutcomeStatus};
    let result = match transfer(args, ctx).await {
        Ok(result) => result,
        Err(error) => blocked(format!("{error:#}")),
    };
    let uncertain = crate::mcp::error::extract_error_kind(&result)
        .is_some_and(|kind| kind == "mutation_outcome_uncertain");
    let status = if uncertain {
        OutcomeStatus::Uncertain
    } else if result.is_error {
        OutcomeStatus::Failed
    } else {
        OutcomeStatus::Complete
    };
    let retry = match status {
        OutcomeStatus::Uncertain => Some(outcome::inspect_before_retry()),
        OutcomeStatus::Failed => Some(outcome::retry_whole_request()),
        _ => None,
    };
    Ok(outcome::attach(
        result,
        outcome::summary(
            status,
            args["schematic"].as_str().unwrap_or("unresolved schematic"),
            "saved_project",
            1,
            usize::from(status == OutcomeStatus::Complete),
            usize::from(status != OutcomeStatus::Complete),
            retry,
        ),
    ))
}

async fn transfer(args: &Value, ctx: &ToolContext) -> Result<CallToolResult> {
    let request = match Request::from_args(args) {
        Ok(request) => request,
        Err(refusal) => return Ok(refusal),
    };
    if !request.dry_run && request.expected_plan_revision.is_none() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "expected_plan_revision".to_owned(),
                reason: "required when dry_run is false".to_owned(),
            },
            "Apply requires the plan revision returned by a current dry run.",
        ));
    }
    let mut plan = match Plan::prepare(&request) {
        Ok(plan) => plan,
        Err(error) if error.is::<StalePlanInputs>() => {
            return Ok(stale_plan(&error.downcast::<StalePlanInputs>()?.0));
        }
        Err(error) if error.is::<PendingTransaction>() => {
            let pending = error.downcast::<PendingTransaction>()?;
            return Ok(super::stuck_transaction_journal_result(
                &pending.journal,
                &pending.directory,
                OPERATION,
            ));
        }
        Err(error) => return Ok(blocked(format!("{error:#}"))),
    };
    if let Some(error) = super::pcb_board::refuse_if_project_locked_by_kicad(
        &plan.root.with_extension("kicad_pro"),
        "cross-sheet transfer",
    ) {
        return Ok(error);
    }
    for path in plan.instances.values().collect::<BTreeSet<_>>() {
        if let Err(error) = copperpour_sexp::writer::ensure_kicad_design_document_is_closed(path) {
            return Ok(blocked(error));
        }
    }
    if let Some(board) = &plan.board {
        if let Some(error) = closed_board(ctx, board).await? {
            return Ok(error);
        }
    }
    match verify_with_kicad(&plan, &ctx.config.kicad_cli).await {
        Ok(Some(verified)) => {
            if let Err(error) = plan.finalize_pcb(&verified) {
                return Ok(blocked(format!("{error:#}")));
            }
            plan.report["connectivity_impacts"] = json!(verified.impacts);
            plan.report["verification"] = json!({"status":"passed", "source":"kicad_cli", "netlist":"equivalent_pin_partitions", "erc":"no_new_violations"});
        }
        Ok(None) if request.dry_run => {
            plan.report["verification"] =
                json!({"status":"not_run", "source":"kicad_cli", "reason":"kicad_cli_unavailable"});
            plan.report["connectivity_impacts"] = Value::Null;
            if plan.board.is_some() {
                plan.report["pcb_net_name_mapping"] = Value::Null;
            }
        }
        Ok(None) => {
            return Ok(blocked(
                "kicad-cli is required; no unverified apply is supported",
            ))
        }
        Err(error) => {
            return Ok(blocked(format!(
                "KiCad candidate verification failed: {error:#}"
            )))
        }
    }
    plan.report["affected_files"] = json!(plan
        .documents
        .iter()
        .filter(|(_, doc)| doc.before != doc.after)
        .map(|(path, _)| path)
        .collect::<Vec<_>>());
    let revision = plan.revision(&request);
    plan.report["plan_revision"] = json!(revision);
    if request.dry_run {
        plan.report["status"] = json!("planned");
        return Ok(CallToolResult::json(&plan.report));
    }
    if request.expected_plan_revision.as_deref() != Some(revision.as_str()) {
        return Ok(stale_plan(&plan.root));
    }
    // A long CLI verification must not leave the board-open observation stale.
    if let Some(board) = &plan.board {
        if let Some(error) = closed_board(ctx, board).await? {
            return Ok(error);
        }
    } else if plan.root.with_extension("kicad_pcb").try_exists()? {
        return Ok(blocked(
            "A project PCB appeared during verification; plan again",
        ));
    }
    for path in optional_project_inputs(&plan.root) {
        if !plan.documents.contains_key(&path) && path.try_exists()? {
            return Ok(stale_plan(&plan.root));
        }
    }
    if let Some(error) = super::pcb_board::refuse_if_project_locked_by_kicad(
        &plan.root.with_extension("kicad_pro"),
        "cross-sheet transfer",
    ) {
        return Ok(error);
    }
    let commit = match copperpour_sexp::try_commit_file_transaction(
        plan.root.parent().unwrap(),
        plan.transitions(),
    ) {
        Ok(commit) => commit,
        Err(failure) => return Ok(transaction_failure_result(failure)),
    };
    Ok(report_committed_transfer(plan, &commit.id))
}

fn transaction_failure_result(failure: copperpour_sexp::TransactionFailure) -> CallToolResult {
    use copperpour_sexp::{SexpError, TransactionConflictKind as Phase};
    match (failure.error, failure.journal) {
        (
            SexpError::Conflict { path }
            | SexpError::TransactionConflict {
                path,
                kind: Phase::BeforeImage,
                ..
            },
            None,
        ) => super::revision_conflict_result(&path, OPERATION),
        (
            SexpError::TransactionConflict {
                path,
                journal,
                kind: Phase::Recovery,
                ..
            },
            _,
        ) => super::stuck_transaction_journal_result(&journal, &path, OPERATION),
        (
            SexpError::TransactionConflict {
                path,
                journal,
                kind: Phase::AfterImage,
                ..
            },
            _,
        ) => super::transaction_after_image_result(&journal, &path, OPERATION),
        (error, None) => blocked(error),
        // A retained journal, which may be an older one replayed first.
        (error, Some(journal)) => CallToolResult::error_kind(
            ToolErrorKind::MutationOutcomeUncertain {
                operation: OPERATION.into(),
                path: journal.display().to_string(),
                reason: error.to_string(),
            },
            format!(
                "Transfer persistence is uncertain: journal {} remains, so files may already \
                 have been written. Run `copperpour transaction status` on the project \
                 directory before retrying. {error}",
                journal.display(),
            ),
        ),
    }
}

fn report_committed_transfer(mut plan: Plan, transaction_id: &str) -> CallToolResult {
    for (path, doc) in &plan.documents {
        if !read_consistent(path).is_ok_and(|source| source == doc.after) {
            return super::mutation_outcome_uncertain(
                path,
                OPERATION,
                format!(
                    "Transaction {transaction_id} completed but readback differs from the verified candidate. Inspect the saved project before retrying."
                ),
            );
        }
    }
    plan.report["status"] = json!("moved");
    plan.report["transaction_id"] = json!(transaction_id);
    CallToolResult::json(&plan.report)
}

async fn closed_board(ctx: &ToolContext, board: &Path) -> Result<Option<CallToolResult>> {
    let state = super::live_board::observe(ctx, board, |_, _| Ok(())).await?;
    Ok(board_state_refusal(&state, board))
}

fn board_state_refusal(
    state: &super::live_board::LiveBoard<()>,
    board: &Path,
) -> Option<CallToolResult> {
    use super::live_board::LiveBoard;
    use copperpour_ipc::UnreachableReason::{NoListener, NotConfigured};
    match state {
        LiveBoard::NotOpen { .. }
        | LiveBoard::NeverReached {
            reason: NoListener, ..
        } => super::pcb_board::board_lock_refusal(board),
        // Candidates mean several endpoints answered and one could not be probed.
        LiveBoard::NeverReached {
            reason: NotConfigured,
            candidates,
            ..
        } if candidates.is_empty() => super::pcb_board::board_lock_refusal(board),
        _ => Some(blocked("Cannot prove that the project PCB is closed. Close it cleanly and retry; live or uncertain board state cannot accept saved schematic-path edits.")),
    }
}

impl Plan {
    /// Every bound file: changed ones are replaced, unchanged ones are held
    /// as read-set guards, locked and checked but never written.
    fn transitions(&self) -> Vec<FileTransition> {
        let mut transitions: Vec<_> = self
            .documents
            .iter()
            .map(|(path, doc)| {
                if doc.before == doc.after {
                    FileTransition::guard(path, &doc.before)
                } else {
                    FileTransition::replace(path, &doc.before, &doc.after)
                }
            })
            .collect();
        for path in optional_project_inputs(&self.root)
            .chain(std::iter::once(self.root.with_extension("kicad_pcb")))
        {
            if !self.documents.contains_key(&path) {
                transitions.push(FileTransition::guard_absent(path));
            }
        }
        if let Some(lock) =
            copperpour_sexp::writer::kicad_project_lock_path(&self.root.with_extension("kicad_pro"))
        {
            transitions.push(FileTransition::guard_absent(lock));
        }
        transitions
    }

    /// Binds the request and every bound file's before and after image, so an
    /// apply refuses unless it would write exactly what the dry run reported.
    fn revision(&self, request: &Request) -> String {
        let inputs = Self::input_revision(request, &self.documents);
        let mut digest = Sha256::new();
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(inputs.as_bytes());
        for (path, doc) in &self.documents {
            field(path.as_os_str().as_encoded_bytes());
            field(doc.after.as_bytes());
        }
        field(self.report["verification"].to_string().as_bytes());
        field(self.report["connectivity_impacts"].to_string().as_bytes());
        field(self.report["pcb_path_mapping"].to_string().as_bytes());
        field(self.report["pcb_net_name_mapping"].to_string().as_bytes());
        format!("{inputs}.{}", crate::hex::encode_hex(&digest.finalize()))
    }

    fn input_revision(request: &Request, documents: &BTreeMap<PathBuf, Document>) -> String {
        let mut digest = Sha256::new();
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(request.source_sheet_instance.as_bytes());
        field(request.destination_sheet_instance.as_bytes());
        for uuid in request.object_uuids.iter().collect::<BTreeSet<_>>() {
            field(uuid.as_bytes());
        }
        field(&request.offset_x_mm.to_le_bytes());
        field(&request.offset_y_mm.to_le_bytes());
        for (path, doc) in documents {
            field(path.as_os_str().as_encoded_bytes());
            field(doc.before.as_bytes());
        }
        crate::hex::encode_hex(&digest.finalize())
    }

    fn update_net_name_dependencies(&mut self, verified: &VerifiedTransfer) -> Result<()> {
        let mut mapping = BTreeMap::new();
        let mut destinations = BTreeSet::new();
        for impact in &verified.impacts {
            let old = impact["old_name"]
                .as_str()
                .context("Missing old net name")?;
            let new = impact["new_name"]
                .as_str()
                .context("Missing new net name")?;
            ensure!(
                !old.is_empty() && !new.is_empty(),
                "Cannot rename the unconnected pseudo-net"
            );
            ensure!(
                mapping.insert(old.to_owned(), new.to_owned()).is_none()
                    && destinations.insert(new.to_owned()),
                "Net-name mapping is not one-to-one"
            );
        }
        let pro_path = self.root.with_extension("kicad_pro");
        let project = &self.documents[&pro_path].before;
        if !mapping.is_empty() {
            check_project_net_dependencies(project, &mapping, self.board.is_some())?;
        }
        if let Some(board) = &self.board {
            let evidence = verified
                .board_nets
                .as_ref()
                .context("Missing native PCB netlist")?;
            let native_mapping = evidence
                .renames
                .iter()
                .map(|(old, (new, _))| (old.clone(), new.clone()))
                .collect::<BTreeMap<_, _>>();
            if native_mapping.is_empty() {
                return Ok(());
            }
            check_project_net_dependencies(project, &native_mapping, true)?;
            let (candidate, renamed) =
                move_pcb_net_names(&self.documents[board].after, &native_mapping)?;
            verify_pcb_pad_membership(
                &self.documents[board].before,
                evidence,
                &renamed,
                &self.instances,
            )?;
            let rules = self.root.with_extension("kicad_dru");
            if !renamed.is_empty() {
                if let Some(doc) = self.documents.get(&rules) {
                    super::custom_rules::check_net_name_independent_conditions(&doc.before)
                        .map_err(|reason| anyhow::anyhow!(
                            "Cannot rename PCB nets {renamed:?}: custom rules in '{}' depend on net names or cannot be checked: {reason}",
                            rules.display()
                        ))?;
                }
            }
            self.documents
                .get_mut(board)
                .context("Board was not staged")?
                .after = candidate;
            self.report["pcb_net_name_mapping"] = json!(renamed);
        }
        Ok(())
    }

    fn finalize_pcb(&mut self, verified: &VerifiedTransfer) -> Result<()> {
        if let (Some(board), Some(sheet)) = (&self.board, &verified.destination_sheet) {
            let paths = self.report["pcb_path_mapping"]
                .as_object()
                .context("Missing PCB path plan")?;
            let targets = paths
                .values()
                .map(|value| value.as_str().context("Invalid PCB destination path"))
                .collect::<Result<BTreeSet<_>>>()?;
            let doc = self
                .documents
                .get_mut(board)
                .context("Board was not staged")?;
            doc.after = update_pcb_sheet_metadata(&doc.after, &targets, sheet)?;
        }
        self.update_net_name_dependencies(verified)
    }

    fn prepare(request: &Request) -> Result<Self> {
        ensure!(
            request.offset_x_mm.is_finite() && request.offset_y_mm.is_finite(),
            "Offsets must be finite"
        );
        ensure!(
            request.offset_x_mm.abs() <= 100_000.0 && request.offset_y_mm.abs() <= 100_000.0,
            "Offsets exceed the supported schematic range"
        );
        let selected = request
            .object_uuids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        ensure!(
            !selected.is_empty() && selected.len() == request.object_uuids.len(),
            "Select unique, nonempty object UUIDs"
        );
        for uuid in &selected {
            uuid::Uuid::parse_str(uuid).context("Invalid selected UUID")?;
        }
        let root = std::fs::canonicalize(&request.schematic)?;
        ensure!(
            root.extension().is_some_and(|ext| ext == "kicad_sch"),
            "schematic must be a root .kicad_sch"
        );
        let directory = root.parent().unwrap();
        ensure!(
            root.with_extension("kicad_pro").is_file(),
            "Root schematic must have a same-named .kicad_pro"
        );
        if let Some(pending) = copperpour_sexp::inspect_file_transactions(directory)?
            .into_iter()
            .next()
        {
            return Err(PendingTransaction {
                journal: pending.journal,
                directory: directory.to_owned(),
            }
            .into());
        }
        let project = root
            .file_stem()
            .and_then(|name| name.to_str())
            .context("Project name is not Unicode")?;
        let mut documents = BTreeMap::new();
        let mut instances = BTreeMap::new();
        let pro = root.with_extension("kicad_pro");
        load_document(&pro, &mut documents)?;
        let roots = transfer_top_level_sheets(&root, &documents[&pro].before)?;
        let multi_root = roots.len() > 1;
        for (path, uuid) in roots {
            collect_hierarchy(
                &path,
                uuid.map(|uuid| format!("/{uuid}")),
                directory,
                &mut documents,
                &mut instances,
                &mut BTreeSet::new(),
            )?;
        }
        ensure!(
            instances
                .iter()
                .any(|(instance, path)| path == &root && !instance[1..].contains('/')),
            "The project entry schematic must be a declared top-level sheet"
        );
        if !multi_root {
            let root_tree = parse_sexp(&documents[&root].before)?;
            let root_uuid = root_tree
                .find_str("uuid")
                .context("Schematic lacks root UUID")?;
            ensure!(
                instances.get(&format!("/{root_uuid}")) == Some(&root),
                "KiCad's headless loader uses the file UUID for a lone top-level sheet; it differs from the project-declared UUID"
            );
        }
        for name in OPTIONAL_INPUTS {
            let path = directory.join(name);
            if path.try_exists()? {
                load_document(&path, &mut documents)?;
                let dependencies =
                    library_snapshot_inputs(directory, name, &documents[&path].before)?;
                for dependency in dependencies {
                    load_document(&dependency, &mut documents)?;
                }
            }
        }
        let rules = root.with_extension("kicad_dru");
        match std::fs::symlink_metadata(&rules) {
            Ok(info) => {
                ensure!(
                    info.file_type().is_file(),
                    "Custom rules '{}' must be a regular file, not a link or directory",
                    rules.display()
                );
                load_document(&rules, &mut documents)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Cannot inspect project custom rules"),
        }
        let board_path = root.with_extension("kicad_pcb");
        let board = if board_path.try_exists()? {
            let canonical = std::fs::canonicalize(&board_path)?;
            ensure!(canonical == board_path, "Project PCB must not be a symlink");
            load_document(&board_path, &mut documents)?;
            Some(board_path)
        } else {
            None
        };
        if !request.dry_run {
            if let Some((input, _)) = request
                .expected_plan_revision
                .as_deref()
                .and_then(|rev| rev.split_once('.'))
            {
                if input != Self::input_revision(request, &documents) {
                    return Err(StalePlanInputs(root).into());
                }
            }
        }
        let source_path = instances
            .get(&request.source_sheet_instance)
            .context("Source sheet instance does not exist in this project")?
            .clone();
        let destination_path = instances
            .get(&request.destination_sheet_instance)
            .context("Destination sheet instance does not exist in this project")?
            .clone();
        ensure!(
            source_path != destination_path,
            "Source and destination must be different schematic files"
        );
        for path in [&source_path, &destination_path] {
            ensure!(instances.values().filter(|value| *value == path).count() == 1,
                "Sheet '{}' has repeated instances. Moving one instance without cloning identities is unsupported", path.display());
        }
        let source = &documents[&source_path].before;
        let destination = &documents[&destination_path].before;
        let source_tree = parse_sexp(source)?;
        let destination_tree = parse_sexp(destination)?;
        ensure!(
            inherited_sim_exclusion(&request.source_sheet_instance, &instances, &documents)?
                == inherited_sim_exclusion(
                    &request.destination_sheet_instance,
                    &instances,
                    &documents,
                )?,
            "Moving between these sheets would change inherited simulation exclusion"
        );
        for (path, instance, content) in [
            (&source_path, &request.source_sheet_instance, source),
            (
                &destination_path,
                &request.destination_sheet_instance,
                destination,
            ),
        ] {
            validate_instances(
                path,
                content,
                project,
                instance,
                instance[1..].contains('/'),
            )?;
            let owner = super::resolve_schematic_ownership(path)
                .map_err(|error| {
                    anyhow::anyhow!("Cannot prove unique project ownership: {error:?}")
                })?
                .context("Sheet has no proven owning project")?;
            ensure!(
                super::same_schematic_document(&owner.project_file, &pro)
                    && owner.instance_paths.as_slice() == [instance.as_str()],
                "Sheet ownership or instance path differs from the saved project"
            );
        }
        for tree in [&source_tree, &destination_tree] {
            for group in tree.find_all("group") {
                let members = group
                    .find("members")
                    .context("Malformed schematic group")?
                    .children()
                    .context("Malformed group members")?;
                ensure!(!members.iter().skip(1).any(|member| member.as_str().is_some_and(|uuid| selected.contains(uuid))),
                    "Selected objects belong to a schematic group. Ungroup them explicitly before transferring; group membership cannot span sheet files");
            }
            for tag in [
                "bus",
                "bus_entry",
                "bus_alias",
                "netclass_flag",
                "directive_label",
            ] {
                ensure!(tree.find(tag).is_none(), "Affected sheet uses unsupported '{tag}'; resolve bus, netclass directive or legacy instance-table semantics before transfer");
            }
        }
        let items = object_blocks(source)?;
        ensure!(
            selected.iter().all(|uuid| items.contains_key(uuid)),
            "Every selected UUID must identify exactly one top-level source object"
        );
        let mut source_edits = Vec::new();
        let mut moved = Vec::new();
        let mut path_mapping = BTreeMap::new();
        let mut moved_references = BTreeSet::new();
        let mut netlist_identities: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut definitions = BTreeMap::new();
        let mut moved_ids = BTreeSet::new();
        for uuid in &selected {
            let (start, end) = items[uuid];
            let block = &source[start..end];
            let tree = parse_sexp(block)?;
            let kind = tree.head().context("Object has no type")?;
            ensure!(matches!(kind, "symbol" | "wire" | "junction" | "label" | "global_label" | "no_connect" | "text" | "text_box"),
                "Object {uuid} has unsupported type '{kind}'. Hierarchical labels and sheet ports require explicit boundary rewiring; sheets are never moved implicitly");
            collect_uuids(&tree, &mut moved_ids)?;
            let mut transferred = translate(block, request.offset_x_mm, request.offset_y_mm)?;
            if kind == "symbol" {
                let reference =
                    property(&tree, "Reference").context("Moved symbol lacks Reference")?;
                ensure!(
                    !reference.contains('?') && !reference.is_empty(),
                    "Annotate symbol {uuid} before transfer"
                );
                moved_references.insert(reference.to_owned());
                if !reference.starts_with('#') {
                    netlist_identities
                        .entry(reference.to_owned())
                        .or_default()
                        .insert(uuid.clone());
                }
                transferred = move_instance(
                    &transferred,
                    project,
                    &request.source_sheet_instance,
                    &request.destination_sheet_instance,
                    reference,
                )?;
                path_mapping.insert(
                    format!("{}/{uuid}", request.source_sheet_instance),
                    format!("{}/{uuid}", request.destination_sheet_instance),
                );
                let name = tree
                    .find_str("lib_name")
                    .or_else(|| tree.find_str("lib_id"))
                    .context("Symbol lacks library identity")?;
                let lib = library_definition(source, name)?
                    .context("Source symbol has no embedded library definition")?;
                ensure!(
                    parse_sexp(&lib)?.find("extends").is_none(),
                    "Derived library definitions require flattening before transfer"
                );
                if let Some(existing) = library_definition(destination, name)? {
                    ensure!(parse_sexp(&lib)? == parse_sexp(&existing)?, "Destination library definition '{name}' conflicts with the source; resolve it explicitly");
                } else {
                    definitions.insert(name.to_owned(), lib);
                }
            }
            source_edits.push(SexpEdit::delete(start, end));
            moved.push(json!({"uuid":uuid, "kind":kind}));
            moved.last_mut().unwrap()["block"] = json!(transferred);
        }
        // A physical symbol's units must stay together. Do not silently split
        // a footprint's unit association over two schematic paths.
        for path in instances.values().collect::<BTreeSet<_>>() {
            let tree = parse_sexp(&documents[path].before)?;
            for symbol in tree.find_all("symbol") {
                if property(symbol, "Reference")
                    .is_some_and(|reference| moved_references.contains(reference))
                {
                    ensure!(*path == source_path && symbol.find_str("uuid").is_some_and(|uuid| selected.contains(uuid)),
                        "All units of a moved reference must be selected in the source; a reference/unit conflict exists in '{}'", path.display());
                }
            }
            let mut ids = BTreeSet::new();
            for child in tree.children().unwrap_or_default() {
                if child.head() == Some("lib_symbols") {
                    continue;
                }
                if *path == source_path
                    && child
                        .find_str("uuid")
                        .is_some_and(|uuid| selected.contains(uuid))
                {
                    continue;
                }
                collect_uuids(child, &mut ids)?;
            }
            ensure!(
                ids.is_disjoint(&moved_ids),
                "Moved object or pin UUID conflicts with an unselected object in '{}'",
                path.display()
            );
        }
        check_boundary(&source_tree, &selected, false)?;
        let mut destination_after = destination.to_owned();
        if !definitions.is_empty() {
            let lib_range = find_direct_child_blocks(destination, "kicad_sch")
                .into_iter()
                .find(|&(start, end)| {
                    parse_sexp(&destination[start..end])
                        .is_ok_and(|node| node.head() == Some("lib_symbols"))
                })
                .context("Destination lacks lib_symbols container")?;
            let additions = definitions
                .values()
                .map(|block| format!("\n\t\t{block}"))
                .collect::<String>();
            destination_after = apply_edits(
                destination_after,
                vec![SexpEdit::insert(lib_range.1 - 1, additions)],
            );
        }
        // KiCad processes connectivity markers before placed symbols. Preserve
        // that ordering instead of appending a UUID-sorted mixture at the end.
        moved.sort_by_key(|item| item["kind"] == "symbol");
        let blocks = moved
            .iter_mut()
            .map(|item| {
                let block = item.as_object_mut().unwrap().remove("block").unwrap();
                format!("\n\t{}\n", block.as_str().unwrap())
            })
            .collect::<String>();
        let anchor = find_direct_child_blocks(&destination_after, "kicad_sch")
            .into_iter()
            .find(|&(start, end)| {
                parse_sexp(&destination_after[start..end]).is_ok_and(|node| {
                    matches!(
                        node.head(),
                        Some("symbol" | "sheet_instances" | "symbol_instances" | "embedded_fonts")
                    )
                })
            })
            .map(|(start, _)| start)
            .unwrap_or(
                find_balanced_block(&destination_after, destination_after.find('(').unwrap())
                    .context("Malformed destination")?
                    .1
                    - 1,
            );
        let line_start = destination_after[..anchor]
            .rfind('\n')
            .map_or(0, |pos| pos + 1);
        let anchor = if destination_after[line_start..anchor].trim().is_empty() {
            line_start
        } else {
            anchor
        };
        destination_after = apply_edits(destination_after, vec![SexpEdit::insert(anchor, blocks)]);
        check_boundary(&parse_sexp(&destination_after)?, &selected, true)?;
        let source_after = apply_edits(source.to_owned(), source_edits);
        parse_sexp(&source_after)?;
        validate_instances(
            &source_path,
            &source_after,
            project,
            &request.source_sheet_instance,
            request.source_sheet_instance[1..].contains('/'),
        )?;
        validate_instances(
            &destination_path,
            &destination_after,
            project,
            &request.destination_sheet_instance,
            request.destination_sheet_instance[1..].contains('/'),
        )?;
        documents.get_mut(&source_path).unwrap().after = source_after;
        documents.get_mut(&destination_path).unwrap().after = destination_after;
        let pcb_path_mapping = if let Some(path) = &board {
            // The CLI exports root-relative PCB identities even for a
            // multi-root project. Prove each such alias is unique rather than
            // deriving its owner from the entry schematic alone.
            let mut aliases = BTreeMap::new();
            if multi_root && !path_mapping.is_empty() {
                for (instance, file) in &instances {
                    let tree = parse_sexp(&documents[file].before)?;
                    for symbol in tree.find_all("symbol") {
                        let uuid = symbol.find_str("uuid").context("Symbol lacks UUID")?;
                        let full = format!("{instance}/{uuid}");
                        let relative = pcb_uuid_path(root_relative_symbol_path(&full)?)?;
                        *aliases.entry(relative).or_insert(0usize) += 1;
                    }
                }
            }
            let (candidate, changed) =
                move_pcb_paths(&documents[path].before, &path_mapping, multi_root, &aliases)?;
            documents
                .get_mut(path)
                .context("Board was not staged")?
                .after = candidate;
            changed
        } else {
            BTreeMap::new()
        };
        let files = documents
            .iter()
            .filter(|(_, doc)| doc.before != doc.after)
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>();
        let report = json!({
            "dry_run":request.dry_run, "source":"saved_project", "objects":moved,
            "source_sheet_instance":request.source_sheet_instance,
            "destination_sheet_instance":request.destination_sheet_instance,
            "offset_mm":{"x":request.offset_x_mm,"y":request.offset_y_mm},
            "path_mapping":path_mapping,
            "pcb_path_mapping":pcb_path_mapping, "pcb_net_name_mapping":{},
            "library_definitions_added":definitions.keys().collect::<Vec<_>>(),
            "affected_files":files, "pcb_present":board.is_some(),
            "connectivity_impacts":[],
            "limitations":["Saved-file operation; reload editors after completion", "Sheet-local net names can change with the hierarchy prefix"]
        });
        Ok(Self {
            root,
            documents,
            board,
            instances,
            netlist_identities,
            report,
        })
    }
}

fn library_snapshot_inputs(directory: &Path, table: &str, source: &str) -> Result<Vec<PathBuf>> {
    let parsed = parse_sexp(source)?;
    let expected = if table == "sym-lib-table" {
        "sym_lib_table"
    } else {
        "fp_lib_table"
    };
    ensure!(parsed.head() == Some(expected), "Invalid project {table}");
    let mut inputs = Vec::new();
    for library in parsed.find_all("lib") {
        let uri = library
            .find_str("uri")
            .context("Project library entry lacks a URI")?;
        let path = Path::new(uri);
        let relative = uri
            .strip_prefix("${KIPRJMOD}/")
            .or_else(|| uri.strip_prefix("${KIPRJMOD}\\"));
        let in_project = relative.is_none()
            && path.is_absolute()
            && names_project_file(directory, path)
                .with_context(|| format!("Cannot tell whether library '{uri}' is in the project; fix the path or use a project-relative URI"))?;
        let relative = match relative {
            Some(relative) => relative,
            None if in_project => {
                anyhow::bail!("Use a project-relative URI to snapshot library '{uri}'");
            }
            None if !path.is_absolute() && !uri.starts_with("${") => uri,
            None => {
                ensure!(
                    !uri.contains("KIPRJMOD"),
                    "Unsupported project-relative library URI '{uri}'"
                );
                continue;
            }
        };
        let relative = Path::new(relative);
        ensure!(
            relative.components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
            "Cannot safely stage library '{uri}'"
        );
        let path = directory.join(relative);
        ensure!(
            table == "sym-lib-table" && path.extension().is_some_and(|ext| ext == "kicad_sym"),
            "Cannot stage project-relative library '{uri}'; only .kicad_sym files are supported"
        );
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Cannot stage library '{uri}'"))?;
        ensure!(
            path == canonical && canonical.is_file(),
            "Project library '{uri}' must be a regular file without symlink aliases"
        );
        inputs.push(path);
    }
    Ok(inputs)
}

/// Whether an absolute path resolves into `directory`, which is canonical, even
/// through an alias such as a symlink or macOS's `/var` for `/private/var`.
/// Only entries confirmed missing are stepped over; a link that cannot be
/// resolved is an error, since it may point into the project. A link that
/// itself lives in the project counts as project state wherever it points: it
/// could be retargeted without changing any guarded file.
fn names_project_file(directory: &Path, path: &Path) -> std::io::Result<bool> {
    for prefix in path.ancestors() {
        match std::fs::symlink_metadata(prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if let Some(parent) = prefix.parent() {
                    if parent.canonicalize()?.starts_with(directory) {
                        return Ok(true);
                    }
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut entry = path;
    loop {
        match std::fs::symlink_metadata(entry) {
            Ok(_) => return Ok(entry.canonicalize()?.starts_with(directory)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match entry.parent() {
                Some(parent) => entry = parent,
                None => return Ok(false),
            },
            Err(error) => return Err(error),
        }
    }
}

/// Reads the captured project bytes that the plan revision and journal bind,
/// not the file again. Unlike [`super::listed_top_level_sheets`], a malformed
/// list or missing sheet is an error rather than a fallback to the entry
/// file: verifying and writing a hierarchy KiCad would not load is unsafe.
fn transfer_top_level_sheets(
    entry: &Path,
    project: &str,
) -> Result<Vec<(PathBuf, Option<String>)>> {
    let project: Value = serde_json::from_str(project).context("Invalid project JSON")?;
    let Some(list) = project
        .pointer("/schematic/top_level_sheets")
        .filter(|list| !list.is_null())
    else {
        return Ok(vec![(entry.to_owned(), None)]);
    };
    let list = list
        .as_array()
        .context("Malformed project top-level sheet list")?;
    if list.is_empty() {
        return Ok(vec![(entry.to_owned(), None)]);
    }
    list.iter()
        .map(|sheet| {
            let relative = sheet["filename"]
                .as_str()
                .context("Top-level sheet lacks a filename")?;
            ensure!(
                !relative.is_empty()
                    && !relative.contains("${")
                    && Path::new(relative).is_relative(),
                "Top-level sheet filename must be relative to the project directory"
            );
            let uuid = match sheet.get("uuid") {
                None => None,
                Some(uuid) => {
                    let text = uuid.as_str().context("Invalid top-level sheet UUID")?;
                    let uuid =
                        uuid::Uuid::parse_str(text).context("Invalid top-level sheet UUID")?;
                    (!uuid.is_nil()).then(|| uuid.to_string())
                }
            };
            Ok((entry.parent().unwrap().join(relative), uuid))
        })
        .collect()
}

fn validate_instances(
    path: &Path,
    source: &str,
    project: &str,
    instance: &str,
    is_child: bool,
) -> Result<()> {
    let schematic = copperpour_schematic_editor::Schematic::from_source(path, source)?;
    let context = super::SheetInstanceContext {
        project_name: project.into(),
        instance_path: instance.into(),
        instance_paths: vec![instance.into()],
        is_child_sheet: is_child,
    };
    super::validate_sheet_instance_state(path, &schematic, &context)
        .map_err(|error| anyhow::anyhow!("Stale hierarchy state: {error:?}"))
}

fn load_document(path: &Path, documents: &mut BTreeMap<PathBuf, Document>) -> Result<()> {
    if !documents.contains_key(path) {
        let before = read_consistent(path)?;
        documents.insert(
            path.to_path_buf(),
            Document {
                after: before.clone(),
                before,
            },
        );
    }
    Ok(())
}

fn collect_hierarchy(
    path: &Path,
    instance: Option<String>,
    directory: &Path,
    documents: &mut BTreeMap<PathBuf, Document>,
    instances: &mut BTreeMap<String, PathBuf>,
    visiting: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    ensure!(
        visiting.len() < super::sch_hierarchy::MAX_HIERARCHY_DEPTH,
        "Hierarchy depth limit exceeded"
    );
    let path = std::fs::canonicalize(path)
        .with_context(|| format!("Cannot read sheet '{}'", path.display()))?;
    ensure!(
        path.starts_with(directory),
        "Sheet escapes the root project directory"
    );
    ensure!(visiting.insert(path.clone()), "Cyclic sheet hierarchy");
    load_document(&path, documents)?;
    let tree = parse_sexp(&documents[&path].before)?;
    ensure!(
        tree.head() == Some("kicad_sch"),
        "Invalid schematic root in '{}'",
        path.display()
    );
    // KiCad 6 keeps references for the whole hierarchy in the root's table.
    ensure!(
        tree.find("symbol_instances").is_none(),
        "Project sheet '{}' uses unsupported 'symbol_instances'; resave the project in KiCad 10 before transfer",
        path.display()
    );
    let root_uuid = tree.find_str("uuid").context("Schematic lacks root UUID")?;
    uuid::Uuid::parse_str(root_uuid).context("Invalid schematic root UUID")?;
    let instance = instance.unwrap_or_else(|| format!("/{root_uuid}"));
    ensure!(
        instances.insert(instance.clone(), path.clone()).is_none(),
        "Conflicting hierarchy instance {instance}"
    );
    for sheet in tree.find_all("sheet") {
        let file = property(sheet, "Sheetfile").context("Sheet lacks Sheetfile")?;
        ensure!(
            !file.contains("${") && Path::new(file).is_relative(),
            "Variable/absolute sheet paths are unsupported"
        );
        let uuid = sheet.find_str("uuid").context("Sheet lacks UUID")?;
        uuid::Uuid::parse_str(uuid).context("Invalid sheet UUID")?;
        collect_hierarchy(
            &path.parent().unwrap().join(file),
            Some(format!("{instance}/{uuid}")),
            directory,
            documents,
            instances,
            visiting,
        )?;
    }
    visiting.remove(&path);
    Ok(())
}

fn inherited_sim_exclusion(
    mut instance: &str,
    instances: &BTreeMap<String, PathBuf>,
    documents: &BTreeMap<PathBuf, Document>,
) -> Result<bool> {
    let mut excluded = false;
    while let Some((parent, uuid)) = instance
        .rsplit_once('/')
        .filter(|(parent, _)| !parent.is_empty())
    {
        let path = instances
            .get(parent)
            .context("Missing parent sheet instance")?;
        let tree = parse_sexp(&documents[path].before)?;
        let placement = tree
            .find_all("sheet")
            .into_iter()
            .find(|sheet| sheet.find_str("uuid") == Some(uuid))
            .context("Missing parent sheet placement")?;
        match placement.find_str("exclude_from_sim") {
            Some("yes") => excluded = true,
            Some("no") | None => {}
            Some(_) => anyhow::bail!("Invalid sheet simulation exclusion flag"),
        }
        instance = parent;
    }
    Ok(excluded)
}

fn property<'a>(tree: &'a SexpNode, name: &str) -> Option<&'a str> {
    tree.find_all("property")
        .into_iter()
        .find(|node| node.get(1).and_then(SexpNode::as_str) == Some(name))?
        .get(2)?
        .as_str()
}

fn collect_uuids(tree: &SexpNode, ids: &mut BTreeSet<String>) -> Result<()> {
    if tree.head() == Some("uuid") {
        let uuid = tree
            .get(1)
            .and_then(SexpNode::as_str)
            .context("Malformed UUID")?;
        uuid::Uuid::parse_str(uuid).context("Invalid object or pin UUID")?;
        ensure!(ids.insert(uuid.to_owned()), "Duplicate UUID {uuid}");
    }
    for child in tree.children().unwrap_or_default() {
        collect_uuids(child, ids)?;
    }
    Ok(())
}

fn object_blocks(source: &str) -> Result<BTreeMap<String, (usize, usize)>> {
    let mut items = BTreeMap::new();
    for (start, end) in find_direct_child_blocks(source, "kicad_sch") {
        let node = parse_sexp(&source[start..end])?;
        if let Some(uuid) = node.find_str("uuid") {
            ensure!(
                items.insert(uuid.to_owned(), (start, end)).is_none(),
                "Duplicate top-level UUID {uuid}"
            );
        }
    }
    Ok(items)
}

fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

fn translate(block: &str, dx: f64, dy: f64) -> Result<String> {
    let node = parse_sexp(block)?;
    let head = node.head().context("Missing object type")?;
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(block, head) {
        let part = &block[start..end];
        let tree = parse_sexp(part)?;
        match tree.head() {
            Some("at" | "start" | "end" | "xy") => {
                let x = tree.get_f64(1).context("Missing X coordinate")? + dx;
                let y = tree.get_f64(2).context("Missing Y coordinate")? + dy;
                ensure!(
                    x.is_finite() && y.is_finite() && x.abs() <= 100_000.0 && y.abs() <= 100_000.0,
                    "Invalid translated geometry"
                );
                // Preserve angle and any unknown trailing coordinate fields.
                let tail = tree.children().unwrap()[3..]
                    .iter()
                    .map(|node| node.as_str().context("Unsupported coordinate metadata"))
                    .collect::<Result<Vec<_>>>()?
                    .join(" ");
                let text = format!(
                    "({} {} {}{})",
                    tree.head().unwrap(),
                    copperpour_schematic_editor::types::fmt_f64(x),
                    copperpour_schematic_editor::types::fmt_f64(y),
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(" {tail}")
                    }
                );
                edits.push(SexpEdit::replace(start, end, text));
            }
            Some("property" | "pts") => {
                edits.push(SexpEdit::replace(start, end, translate(part, dx, dy)?))
            }
            _ => {}
        }
    }
    Ok(apply_edits(block.to_owned(), edits))
}

fn move_instance(
    block: &str,
    project: &str,
    old: &str,
    new: &str,
    reference: &str,
) -> Result<String> {
    let tree = parse_sexp(block)?;
    let projects = tree
        .find("instances")
        .context("Symbol lacks instances; save/repair hierarchy first")?
        .find_all("project");
    ensure!(
        projects.len() == 1 && projects[0].get(1).and_then(SexpNode::as_str) == Some(project),
        "Symbol has foreign or ambiguous project instances"
    );
    let paths = projects[0].find_all("path");
    ensure!(
        paths.len() == 1 && paths[0].get(1).and_then(SexpNode::as_str) == Some(old),
        "Symbol instance path is stale or repeated"
    );
    ensure!(
        paths[0].find_str("reference") == Some(reference),
        "Symbol reference disagrees with its instance record"
    );
    ensure!(
        paths[0].find_str("unit") == tree.find_str("unit"),
        "Symbol unit disagrees with its instance record"
    );
    let mut edits = Vec::new();
    for offset in copperpour_sexp::writer::find_block_starts(block, "path") {
        let (_, end) = find_balanced_block(block, offset).context("Malformed instance path")?;
        let part = &block[offset..end];
        let path_node = parse_sexp(part)?;
        ensure!(
            path_node.get(1).and_then(SexpNode::as_str) == Some(old),
            "Unsupported additional symbol path"
        );
        let children = find_direct_child_blocks(part, "path");
        let tail = children
            .first()
            .map(|&(start, _)| &part[start..part.len() - 1])
            .unwrap_or("");
        edits.push(SexpEdit::replace(
            offset,
            end,
            format!("(path {} {tail})", quote(new)),
        ));
    }
    Ok(apply_edits(block.to_owned(), edits))
}

fn library_definition(source: &str, name: &str) -> Result<Option<String>> {
    let Some((start, end)) = find_direct_child_blocks(source, "kicad_sch")
        .into_iter()
        .find(|&(start, end)| {
            parse_sexp(&source[start..end]).is_ok_and(|node| node.head() == Some("lib_symbols"))
        })
    else {
        return Ok(None);
    };
    let library = &source[start..end];
    let mut found = None;
    for (start, end) in find_direct_child_blocks(library, "lib_symbols") {
        let block = &library[start..end];
        let node = parse_sexp(block)?;
        if node.head() == Some("symbol") && node.get(1).and_then(SexpNode::as_str) == Some(name) {
            ensure!(found.is_none(), "Duplicate library definition {name}");
            found = Some(block.to_owned());
        }
    }
    Ok(found)
}

fn root_relative_symbol_path(path: &str) -> Result<&str> {
    let (_, tail) = path[1..].split_once('/').context("Missing symbol path")?;
    Ok(&path[path.len() - tail.len() - 1..])
}

fn pcb_uuid_path(path: &str) -> Result<Vec<uuid::Uuid>> {
    path.strip_prefix('/')
        .context("PCB path must be absolute")?
        .split('/')
        .map(uuid::Uuid::parse_str)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Malformed footprint schematic UUID path")
}

fn move_pcb_paths(
    source: &str,
    mapping: &BTreeMap<String, String>,
    multi_root: bool,
    aliases: &BTreeMap<Vec<uuid::Uuid>, usize>,
) -> Result<(String, BTreeMap<String, String>)> {
    let tree = parse_sexp(source)?;
    ensure!(tree.head() == Some("kicad_pcb"), "Invalid project PCB");
    let paths = mapping
        .iter()
        .map(|(old, new)| Ok((pcb_uuid_path(old)?, pcb_uuid_path(new)?, new)))
        .collect::<Result<Vec<_>>>()?;
    let expected = copperpour_sexp::board::footprints(&tree).len() + tree.find_all("module").len();
    let mut footprints = 0;
    let mut seen = BTreeSet::new();
    let mut changed = BTreeMap::new();
    let mut edits = Vec::new();
    for (start, end) in find_direct_child_blocks(source, "kicad_pcb") {
        let block = &source[start..end];
        let footprint = parse_sexp(block)?;
        let Some(tag @ ("footprint" | "module")) = footprint.head() else {
            continue;
        };
        footprints += 1;
        let mut count = 0;
        for (from, to) in find_direct_child_blocks(block, tag) {
            let path = parse_sexp(&block[from..to])?;
            if path.head() != Some("path") {
                continue;
            }
            count += 1;
            ensure!(
                count == 1 && path.children().is_some_and(|c| c.len() == 2),
                "Malformed or duplicate footprint schematic path"
            );
            let old = path
                .get(1)
                .and_then(SexpNode::as_str)
                .context("Malformed footprint schematic path")?;
            if old.is_empty() {
                continue;
            }
            let parsed = pcb_uuid_path(old)?;
            let rootless = |path: &[uuid::Uuid]| path.get(1..) == Some(parsed.as_slice());
            let matches = |path: &[uuid::Uuid]| parsed.as_slice() == path || rootless(path);
            let matching = paths
                .iter()
                .enumerate()
                .filter(|(_, (from, _, _))| matches(from))
                .collect::<Vec<_>>();
            ensure!(matching.len() <= 1, "Ambiguous PCB path {old}");
            ensure!(
                !paths
                    .iter()
                    .any(|(from, new, _)| matches(new) && !(rootless(from) && rootless(new))),
                "PCB already contains a destination schematic path {old}"
            );
            if let Some((index, (full, new_full, destination))) = matching.first().copied() {
                ensure!(
                    seen.insert(index),
                    "Multiple PCB footprints claim schematic path {old}"
                );
                let new = if parsed == *full {
                    destination.as_str()
                } else {
                    if multi_root {
                        ensure!(
                            aliases.get(&parsed) == Some(&1)
                                && (rootless(new_full) || !aliases.contains_key(&new_full[1..])),
                            "Ambiguous root-relative PCB path {old} in a multi-root project"
                        );
                    }
                    root_relative_symbol_path(destination)?
                };
                if old != new {
                    edits.push(SexpEdit::replace(
                        start + from,
                        start + to,
                        format!("(path {})", quote(new)),
                    ));
                }
                // A root-to-root move can keep the root-relative path text
                // unchanged but still needs new sheet metadata.
                changed.insert(old.to_owned(), new.to_owned());
            }
        }
        ensure!(
            count == footprint.find_all("path").len(),
            "Cannot locate exact footprint path"
        );
    }
    ensure!(footprints == expected, "Cannot locate exact PCB footprints");
    Ok((apply_edits(source.to_owned(), edits), changed))
}

fn update_pcb_sheet_metadata(
    source: &str,
    targets: &BTreeSet<&str>,
    sheet: &PcbSheetMetadata,
) -> Result<String> {
    let mut edits = Vec::new();
    let mut seen = BTreeSet::new();
    for (start, end) in find_direct_child_blocks(source, "kicad_pcb") {
        let block = &source[start..end];
        let footprint = parse_sexp(block)?;
        let Some(tag @ ("footprint" | "module")) = footprint.head() else {
            continue;
        };
        let Some(path) = footprint
            .find_str("path")
            .filter(|path| targets.contains(path))
        else {
            continue;
        };
        ensure!(seen.insert(path.to_owned()), "Duplicate PCB path {path}");
        let mut fields = [
            ("sheetname", &sheet.name, false),
            ("sheetfile", &sheet.file, false),
        ];
        for (from, to) in find_direct_child_blocks(block, tag) {
            let node = parse_sexp(&block[from..to])?;
            if let Some((field, value, found)) = fields
                .iter_mut()
                .find(|(field, _, _)| node.head() == Some(*field))
            {
                ensure!(
                    !*found && node.children().is_some_and(|c| c.len() == 2),
                    "Malformed or duplicate PCB {field} at {path}"
                );
                *found = true;
                if node.get(1).and_then(SexpNode::as_str) != Some(value.as_str()) {
                    edits.push(SexpEdit::replace(
                        start + from,
                        start + to,
                        format!("({field} {})", quote(value)),
                    ));
                }
            }
        }
        let mut missing = String::new();
        for (field, value, found) in fields {
            if !found {
                ensure!(
                    footprint.find(field).is_none(),
                    "Cannot locate exact PCB {field}"
                );
                missing.push_str(&format!(" ({field} {})", quote(value)));
            }
        }
        if !missing.is_empty() {
            edits.push(SexpEdit::insert(end - 1, missing));
        }
    }
    ensure!(
        seen.len() == targets.len(),
        "Cannot locate candidate PCB footprint paths for sheet metadata"
    );
    Ok(apply_edits(source.to_owned(), edits))
}

fn move_pcb_net_names(
    source: &str,
    mapping: &BTreeMap<String, String>,
) -> Result<(String, BTreeMap<String, String>)> {
    use copperpour_sexp::net::{net_id, net_name};
    use copperpour_sexp::writer::find_block_starts;
    let tree = parse_sexp(source)?;
    let mut expected = 0;
    let mut renamed = BTreeMap::new();
    let mut pending = vec![(&tree, None)];
    while let Some((node, parent)) = pending.pop() {
        let name = match node.head() {
            Some("net") => net_name(node),
            Some("net_name" | "add_net") => node.get(1).and_then(SexpNode::as_str),
            _ => None,
        };
        if let Some(name) = name {
            ensure!(
                !mapping.values().any(|new| new == name),
                "PCB already contains destination net '{name}'"
            );
            if let Some(new) = mapping.get(name) {
                ensure!(
                    matches!(
                        (node.head(), parent),
                        (
                            Some("net"),
                            Some(
                                "kicad_pcb"
                                    | "pad"
                                    | "segment"
                                    | "arc"
                                    | "via"
                                    | "zone"
                                    | "gr_line"
                                    | "gr_rect"
                                    | "gr_circle"
                                    | "gr_arc"
                                    | "gr_poly"
                                    | "gr_curve"
                                    | "fp_line"
                                    | "fp_rect"
                                    | "fp_circle"
                                    | "fp_arc"
                                    | "fp_poly"
                                    | "fp_curve"
                            )
                        ) | (Some("net_name"), Some("zone"))
                            | (Some("add_net"), Some("net_class"))
                    ),
                    "Unsupported PCB net-name occurrence for {name:?} in {parent:?}"
                );
                expected += 1;
                renamed.insert(name.to_owned(), new.clone());
            }
        }
        if let Some(children) = node.children() {
            pending.extend(children.iter().map(|child| (child, node.head())));
        }
    }
    if renamed.is_empty() {
        return Ok((source.to_owned(), renamed));
    }
    // In the numeric format the code is the copper identity. Refuse
    // conflicting declarations or an affected name paired with another code.
    let mut codes = BTreeMap::new();
    let mut names = BTreeMap::new();
    for node in tree.find_all("net") {
        if let (Some(id), Some(name)) = (net_id(node), net_name(node)) {
            ensure!(
                codes.insert(id, name).is_none(),
                "Duplicate PCB net ID {id}"
            );
            ensure!(
                names.insert(name, id).is_none(),
                "Duplicate PCB net name {name:?}"
            );
        }
    }
    let mut pending = vec![&tree];
    while let Some(node) = pending.pop() {
        if node.head() != Some("kicad_pcb") {
            if let Some(net) = node.find("net") {
                if let Some(id) = net_id(net) {
                    let declared = codes.get(id).copied();
                    for name in [net_name(net), node.find_str("net_name")]
                        .into_iter()
                        .flatten()
                    {
                        if renamed.contains_key(name)
                            || declared.is_some_and(|n| renamed.contains_key(n))
                        {
                            ensure!(
                                declared == Some(name),
                                "Conflicting PCB name {name:?} for net ID {id}"
                            );
                        }
                    }
                }
            }
        }
        if let Some(children) = node.children() {
            pending.extend(children);
        }
    }
    let mut edits = Vec::new();
    for tag in ["net", "net_name", "add_net"] {
        for start in find_block_starts(source, tag) {
            let (_, end) =
                find_balanced_block(source, start).context("Malformed PCB net reference")?;
            let node = parse_sexp(&source[start..end])?;
            let name = if tag == "net" {
                net_name(&node)
            } else {
                node.get(1).and_then(SexpNode::as_str)
            };
            let Some(new) = name.and_then(|old| renamed.get(old)) else {
                continue;
            };
            let id = (tag == "net").then(|| net_id(&node)).flatten();
            let children = node.children().context("Malformed PCB net reference")?;
            ensure!(
                children.len() == 2 + usize::from(id.is_some())
                    && matches!(children.last(), Some(SexpNode::Str(_)))
                    && id.is_none_or(|id| id.parse::<u32>().is_ok_and(|n| n > 0)),
                "Unsupported PCB net encoding for {name:?}"
            );
            let replacement = if let Some(id) = id {
                format!("(net {id} {})", quote(new))
            } else {
                format!("({tag} {})", quote(new))
            };
            edits.push(SexpEdit::replace(start, end, replacement));
        }
    }
    ensure!(
        edits.len() == expected,
        "Cannot locate every PCB net-name occurrence"
    );
    Ok((apply_edits(source.to_owned(), edits), renamed))
}

fn optional_project_inputs(root: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    OPTIONAL_INPUTS
        .into_iter()
        .map(|name| root.with_file_name(name))
        .chain(std::iter::once(root.with_extension("kicad_dru")))
}

fn check_project_net_dependencies(
    source: &str,
    mapping: &BTreeMap<String, String>,
    pcb_present: bool,
) -> Result<()> {
    let project: Value = serde_json::from_str(source).context("Invalid project JSON")?;
    let Some(settings) = project.get("net_settings") else {
        return Ok(());
    };
    ensure!(
        settings.is_object(),
        "Project net_settings must be an object"
    );
    if let Some(assignments) = settings
        .get("netclass_assignments")
        .filter(|v| !v.is_null())
    {
        let assignments = assignments
            .as_object()
            .context("Project netclass_assignments must be an object")?;
        for (old, new) in mapping {
            ensure!(
                !assignments.contains_key(old),
                "The move would require updating saved project net assignments"
            );
            ensure!(
                !assignments.contains_key(new),
                "Destination net '{new}' would acquire an existing explicit netclass assignment"
            );
        }
    }
    if let Some(patterns) = settings.get("netclass_patterns").filter(|p| !p.is_null()) {
        let patterns = patterns
            .as_array()
            .context("Project netclass_patterns must be an array")?;
        for rule in patterns {
            let pattern = rule["pattern"]
                .as_str()
                .context("Invalid netclass pattern")?;
            ensure!(rule["netclass"].is_string(), "Invalid pattern netclass");
            if pattern == "*" {
                continue;
            }
            ensure!(
                pcb_present,
                "Cannot prove a net rename preserves saved project netclass pattern matches"
            );
            super::placement::check_netclass_rename_pattern(pattern, mapping)?;
        }
    }
    Ok(())
}

/// Refuse physical boundary connections and name-based local boundaries before
/// asking KiCad. Global labels and power nets are checked by the full netlist.
fn check_boundary(tree: &SexpNode, selected: &BTreeSet<String>, destination: bool) -> Result<()> {
    use copperpour_sexp::geometry::{point_on_segment, points_coincident};
    use copperpour_sexp::schematic::pin_endpoint;
    type Point = (f64, f64);
    struct Geometry {
        selected: bool,
        uuid: String,
        points: Vec<Point>,
        segments: Vec<(Point, Point)>,
    }
    let pins =
        super::resolved_placed_pins_by_reference(tree).context(super::UNRESOLVED_PIN_GEOMETRY)?;
    let mut geometry = Vec::new();
    let mut names: BTreeMap<String, Vec<(bool, bool)>> = BTreeMap::new();
    for node in tree.children().unwrap_or_default() {
        let kind = node.head().unwrap_or("");
        if !matches!(
            kind,
            "symbol"
                | "wire"
                | "junction"
                | "label"
                | "global_label"
                | "hierarchical_label"
                | "sheet"
                | "no_connect"
        ) {
            continue;
        }
        let uuid = node
            .find_str("uuid")
            .context("Electrical object lacks UUID")?
            .to_owned();
        let is_selected = selected.contains(&uuid);
        let mut g = Geometry {
            selected: is_selected,
            uuid: uuid.clone(),
            points: Vec::new(),
            segments: Vec::new(),
        };
        match kind {
            "symbol" => {
                let (_, placed) = pins
                    .iter()
                    .find(|(symbol, _)| symbol.uuid.as_deref() == Some(uuid.as_str()))
                    .context("Symbol pin geometry missing")?;
                g.points = placed
                    .iter()
                    .map(|(pin, transform)| pin_endpoint(pin, *transform))
                    .collect();
            }
            "wire" => {
                let pts = node.find("pts").context("Wire lacks pts")?.find_all("xy");
                ensure!(pts.len() == 2, "Only two-point wires are supported");
                let a = (
                    pts[0].get_f64(1).context("Invalid wire X")?,
                    pts[0].get_f64(2).context("Invalid wire Y")?,
                );
                let b = (
                    pts[1].get_f64(1).context("Invalid wire X")?,
                    pts[1].get_f64(2).context("Invalid wire Y")?,
                );
                g.points = vec![a, b];
                g.segments.push((a, b));
            }
            "sheet" => {
                for pin in node.find_all("pin") {
                    let at = pin.find("at").context("Sheet pin lacks at")?;
                    g.points.push((
                        at.get_f64(1).context("Invalid pin X")?,
                        at.get_f64(2).context("Invalid pin Y")?,
                    ));
                }
            }
            _ => {
                let at = node.find("at").context("Electrical object lacks at")?;
                g.points.push((
                    at.get_f64(1).context("Invalid X")?,
                    at.get_f64(2).context("Invalid Y")?,
                ));
            }
        }
        if matches!(kind, "label" | "hierarchical_label" | "global_label") {
            let name = node
                .get(1)
                .and_then(SexpNode::as_str)
                .context("Label lacks name")?;
            names
                .entry(name.into())
                .or_default()
                .push((is_selected, kind != "global_label"));
        }
        geometry.push(g);
    }
    for label in copperpour_sexp::schematic::extract_power_symbol_labels(tree) {
        let is_selected = label
            .uuid
            .as_ref()
            .is_some_and(|uuid| selected.contains(uuid));
        let instance = &pins
            .iter()
            .find(|(instance, _)| instance.uuid == label.uuid)
            .context("Cannot bind power symbol scope")?
            .0;
        let libraries = tree
            .find("lib_symbols")
            .context("Missing lib_symbols")?
            .find_all("symbol");
        let definition = copperpour_sexp::schematic::find_lib_symbol(&libraries, instance)
            .context("Cannot resolve power symbol scope")?;
        let local = definition
            .find("power")
            .and_then(|node| node.get(1))
            .and_then(SexpNode::as_str)
            == Some("local");
        names
            .entry(label.net)
            .or_default()
            .push((is_selected, local));
    }
    for (name, occurrences) in names {
        let crosses = occurrences.iter().any(|(is_selected, _)| *is_selected)
            && occurrences.iter().any(|(is_selected, _)| !*is_selected);
        let local = occurrences.iter().any(|(_, local)| *local);
        ensure!(!crosses || !local, "Sheet-local label or power name '{name}' crosses the {} selection boundary. Explicitly rewire or rename it before transfer", if destination {"destination"} else {"source"});
    }
    let touch = |point: Point, g: &Geometry| {
        g.points
            .iter()
            .any(|&(x, y)| points_coincident(point.0, point.1, x, y, 0.01))
            || g.segments
                .iter()
                .any(|&(a, b)| point_on_segment(point.0, point.1, a.0, a.1, b.0, b.1, 0.01))
    };
    for a in geometry.iter().filter(|g| g.selected) {
        for b in geometry.iter().filter(|g| !g.selected) {
            ensure!(!a.points.iter().any(|&p|touch(p,b)) && !b.points.iter().any(|&p|touch(p,a)),
                "Physical {} boundary connection between {} and {}. Select the complete connected geometry or explicitly rewire before transfer", if destination {"destination"} else {"source"},a.uuid,b.uuid);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PcbSheetMetadata {
    name: String,
    file: String,
}

#[derive(Debug)]
struct VerifiedTransfer {
    impacts: Vec<Value>,
    destination_sheet: Option<PcbSheetMetadata>,
    board_nets: Option<BoardNetEvidence>,
}

#[derive(Debug)]
struct BoardNetEvidence {
    renames: BTreeMap<String, (String, PinPartition)>,
    // Reference -> root-relative sheet and its symbol UUIDs.
    paths: BoardComponentPaths,
}

type BoardComponentPaths = BTreeMap<String, (Vec<uuid::Uuid>, BTreeSet<uuid::Uuid>)>;
type BoardNetExport = (NetNames, BoardComponentPaths);

fn pcb_netlist(source: &str) -> Result<BoardNetExport> {
    let tree = parse_sexp(source)?;
    ensure!(tree.head() == Some("export"), "Invalid native PCB netlist");
    let mut paths = BTreeMap::new();
    for comp in tree
        .find("components")
        .context("Native PCB netlist lacks components")?
        .find_all("comp")
    {
        let reference = comp
            .find_str("ref")
            .context("Missing PCB netlist reference")?;
        let timestamps = comp
            .find("sheetpath")
            .and_then(|s| s.find_str("tstamps"))
            .context("Missing native PCB sheet path")?;
        let path = if timestamps == "/" {
            Vec::new()
        } else {
            pcb_uuid_path(timestamps.trim_end_matches('/'))?
        };
        let stamps = comp
            .find_str("tstamps")
            .context("Missing native PCB symbol UUIDs")?
            .split_whitespace()
            .map(uuid::Uuid::parse_str)
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        ensure!(
            !stamps.is_empty() && paths.insert(reference.to_owned(), (path, stamps)).is_none(),
            "Ambiguous native PCB component {reference}"
        );
    }
    let mut names = BTreeMap::new();
    let mut used = BTreeSet::new();
    for net in tree
        .find("nets")
        .context("Native PCB netlist lacks nets")?
        .find_all("net")
    {
        let name = net
            .find_str("name")
            .context("Missing native PCB net name")?;
        let mut pins = BTreeSet::new();
        for node in net.find_all("node") {
            let reference = node
                .find_str("ref")
                .context("Missing native PCB pin reference")?;
            let pin = node
                .find_str("pin")
                .context("Missing native PCB pin number")?;
            ensure!(
                paths.contains_key(reference) && used.insert((reference, pin)),
                "Ambiguous native PCB pin"
            );
            pins.insert((reference.to_owned(), pin.to_owned()));
        }
        ensure!(
            !pins.is_empty() && names.insert(pins, name.to_owned()).is_none(),
            "Ambiguous native PCB net {name:?}"
        );
    }
    Ok((names, paths))
}

fn verify_pcb_pad_membership(
    source: &str,
    evidence: &BoardNetEvidence,
    renamed: &BTreeMap<String, String>,
    instances: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    use copperpour_sexp::net::{net_id, net_name};
    if renamed.is_empty() {
        return Ok(());
    }
    let tree = parse_sexp(source)?;
    let instance_paths = instances
        .keys()
        .map(|path| pcb_uuid_path(path))
        .collect::<Result<BTreeSet<_>>>()?;
    let codes = tree
        .find_all("net")
        .into_iter()
        .filter_map(|n| Some((net_id(n)?, net_name(n)?)))
        .collect::<BTreeMap<_, _>>();
    let mut witnessed = BTreeSet::new();
    for fp in copperpour_sexp::board::footprints(&tree)
        .into_iter()
        .chain(tree.find_all("module"))
    {
        for pad in fp.find_all("pad") {
            let Some(net) = pad.find("net") else {
                continue;
            };
            let Some(name) = net_name(net).or_else(|| codes.get(net_id(net)?).copied()) else {
                continue;
            };
            let Some((_, partition)) = evidence
                .renames
                .get(name)
                .filter(|_| renamed.contains_key(name))
            else {
                continue;
            };
            let reference = property(fp, "Reference")
                .or_else(|| {
                    fp.find_all("fp_text")
                        .into_iter()
                        .find(|t| t.get(1).and_then(SexpNode::as_str) == Some("reference"))
                        .and_then(|t| t.get(2))
                        .and_then(SexpNode::as_str)
                })
                .context("Affected PCB pad has no footprint reference")?;
            let pin = pad
                .get(1)
                .and_then(SexpNode::as_str)
                .context("Affected PCB pad has no number")?;
            let path = pcb_uuid_path(
                fp.find_str("path")
                    .context("Affected PCB pad has no schematic path")?,
            )?;
            let (symbol, sheet) = path
                .split_last()
                .context("Affected PCB pad has an empty path")?;
            ensure!(
                partition.contains(&(reference.to_owned(), pin.to_owned()))
                    && evidence
                        .paths
                        .get(reference)
                        .is_some_and(|(expected, stamps)| {
                            stamps.contains(symbol)
                                && (sheet == expected
                                    || (instance_paths.contains(sheet)
                                        && sheet.get(1..) == Some(expected.as_slice())))
                        }),
                "PCB net {name:?} does not match the verified schematic pin at {reference}.{pin}"
            );
            witnessed.insert(name);
        }
    }
    for old in renamed.keys() {
        ensure!(
            witnessed.contains(old.as_str()),
            "PCB net {old:?} has no verified schematic pad"
        );
    }
    Ok(())
}

fn pcb_destination_sheet(
    netlist: roxmltree::Node<'_, '_>,
    plan: &Plan,
) -> Result<PcbSheetMetadata> {
    let destination = plan.report["destination_sheet_instance"]
        .as_str()
        .context("Missing destination sheet identity")?;
    let suffix = destination[1..]
        .split_once('/')
        .map_or("", |(_, tail)| tail);
    let mut metadata: Option<PcbSheetMetadata> = None;
    for component in xml_child(netlist, "components")
        .context("Netlist lacks components")?
        .children()
    {
        if !component
            .attribute("ref")
            .is_some_and(|reference| plan.netlist_identities.contains_key(reference))
        {
            continue;
        }
        let sheet = xml_child(component, "sheetpath")
            .context("KiCad netlist lacks a moved component's sheet path")?;
        ensure!(
            sheet
                .attribute("tstamps")
                .is_some_and(|path| path.trim_matches('/') == suffix),
            "KiCad netlist puts a moved component on another sheet"
        );
        let name = sheet
            .attribute("names")
            .filter(|name| !name.is_empty())
            .context("KiCad netlist lacks the destination sheet name")?;
        let file = component
            .children()
            .filter(|node| {
                node.has_tag_name("property") && node.attribute("name") == Some("Sheetfile")
            })
            .filter_map(|node| node.attribute("value"))
            .next_back()
            .filter(|file| !file.is_empty())
            .context("KiCad netlist lacks the destination Sheetfile property")?;
        if let Some(prior) = &metadata {
            ensure!(
                prior.name == name && prior.file == file,
                "KiCad netlist disagrees on destination sheet metadata"
            );
        } else {
            metadata = Some(PcbSheetMetadata {
                name: name.to_owned(),
                file: file.to_owned(),
            });
        }
    }
    metadata.context("KiCad netlist lacks moved component sheet metadata")
}

async fn verify_with_kicad(plan: &Plan, cli_path: &str) -> Result<Option<VerifiedTransfer>> {
    if cli_path.trim().is_empty() {
        return Ok(None);
    }
    let mut netlists = Vec::new();
    let mut violations = Vec::new();
    let mut net_names = Vec::new();
    let mut destination_sheet = None;
    let mut pcb_exports = Vec::new();
    let expected_erc = plan.instances.keys().cloned().collect::<BTreeSet<_>>();
    for after in [false, true] {
        let identities = ErcIdentityIndex::build(plan, after)?;
        let snapshot = tempfile::tempdir()?;
        let directory = plan.root.parent().unwrap();
        for (path, doc) in &plan.documents {
            if Some(path) == plan.board.as_ref() {
                continue;
            }
            let target = snapshot.path().join(path.strip_prefix(directory)?);
            std::fs::create_dir_all(target.parent().unwrap())?;
            std::fs::write(target, if after { &doc.after } else { &doc.before })?;
        }
        let root = snapshot.path().join(plan.root.file_name().unwrap());
        let output = snapshot.path().join("transfer.xml");
        // The board-oriented export drops off-board pins. XML includes them
        // and records the effective KiCad netclass for each pin partition.
        if let Err(error) = cli::export_netlist(cli_path, &root, &output, "kicadxml").await {
            if !after
                && error
                    .downcast_ref::<cli::CliSpawnError>()
                    .is_some_and(|error| error.source.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(None);
            }
            return Err(error);
        }
        let netlist_source = std::fs::read_to_string(output)?;
        let document = roxmltree::Document::parse(&netlist_source)?;
        let netlist = document.root_element();
        verify_netlist_sheet_coverage(netlist, &plan.instances)?;
        if after
            && plan.report["pcb_path_mapping"]
                .as_object()
                .is_some_and(|map| !map.is_empty())
        {
            destination_sheet = Some(pcb_destination_sheet(netlist, plan)?);
        }
        let (partitions, names) = netlist_partitions(netlist)?;
        net_names.push(names);
        for (reference, uuids) in &plan.netlist_identities {
            ensure!(
                partitions
                    .0
                    .keys()
                    .any(|(observed, stamps)| observed == reference
                        && uuids
                            .iter()
                            .all(|uuid| stamps.split_whitespace().any(|stamp| stamp == uuid))),
                "KiCad netlist does not resolve moved component {reference} to its preserved UUIDs"
            );
        }
        netlists.push(partitions);
        if plan.board.is_some() {
            let output = snapshot.path().join("pcb.net");
            cli::export_netlist(cli_path, &root, &output, "kicadsexpr").await?;
            pcb_exports.push(pcb_netlist(&std::fs::read_to_string(output)?)?);
        }
        let report = cli::run_erc_strict(cli_path, &root, &expected_erc).await?;
        let mut counts = BTreeMap::new();
        for violation in report.violations {
            let mut ids = violation
                .items
                .iter()
                .map(|item| identities.identity(item))
                .collect::<Result<Vec<_>>>()?;
            ids.sort();
            *counts
                .entry((violation.rule, violation.severity, ids))
                .or_insert(0usize) += 1;
        }
        violations.push(counts);
    }
    ensure!(
        netlists[0] == netlists[1],
        "Before/after KiCad netlists differ in component identities, effective component settings, pin connectivity, or effective netclasses"
    );
    for (key, count) in &violations[1] {
        ensure!(
            *count <= *violations[0].get(key).unwrap_or(&0),
            "Transfer introduces an ERC violation: {} ({}, items {:?})",
            key.0,
            key.1,
            key.2
        );
    }
    let impacts = net_names[0].iter().filter_map(|(nodes,old)| {
        let new=net_names[1].get(nodes)?;
        (new != old).then(||json!({"kind":"net_name_changed","old_name":old,"new_name":new,"connectivity":"equivalent"}))
    }).collect();
    let board_nets = if let [before, after] = pcb_exports.as_slice() {
        ensure!(
            before.0.keys().eq(after.0.keys()),
            "Native PCB pin partitions changed"
        );
        let mut renames = BTreeMap::new();
        let mut destinations = BTreeSet::new();
        for (pins, old) in &before.0 {
            let new = &after.0[pins];
            if old != new {
                ensure!(
                    !old.is_empty()
                        && !new.is_empty()
                        && renames
                            .insert(old.clone(), (new.clone(), pins.clone()))
                            .is_none()
                        && destinations.insert(new),
                    "Native PCB net rename is not one-to-one"
                );
            }
        }
        Some(BoardNetEvidence {
            renames,
            paths: before.1.clone(),
        })
    } else {
        None
    };
    Ok(Some(VerifiedTransfer {
        impacts,
        destination_sheet,
        board_nets,
    }))
}

fn verify_netlist_sheet_coverage(
    tree: roxmltree::Node<'_, '_>,
    instances: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    let sheets = xml_child(tree, "design")
        .context("Netlist lacks the design section")?
        .children()
        .filter(|node| node.has_tag_name("sheet"));
    // Unlike ERC's uuid_path, the netlist omits each top-level UUID.
    // Include the source basename and keep multiplicity, even for empty sheets.
    let mut expected = instances
        .iter()
        .map(|(path, file)| {
            let (_, suffix) = path[1..].split_once('/').unwrap_or(("", ""));
            (
                suffix,
                file.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let mut observed = sheets
        .map(|sheet| {
            Ok((
                sheet
                    .attribute("tstamps")
                    .context("Netlist sheet lacks tstamps")?
                    .trim_matches('/'),
                xml_child(sheet, "title_block")
                    .and_then(|block| xml_child(block, "source"))
                    .and_then(|source| source.text())
                    .context("Netlist sheet lacks its source filename")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    expected.sort_unstable();
    observed.sort_unstable();
    ensure!(
        observed == expected,
        "KiCad netlist did not cover every project sheet instance (multi-root projects need KiCad 10.0.7 or newer)"
    );
    Ok(())
}

struct ErcPinIdentity {
    identity: String,
    reference: String,
    number: String,
    position: (f64, f64),
    repeated: bool,
}

struct ErcIdentityIndex {
    persisted: BTreeSet<String>,
    repeated: BTreeSet<String>,
    pins: Vec<ErcPinIdentity>,
}

impl ErcIdentityIndex {
    fn build(plan: &Plan, after: bool) -> Result<Self> {
        let mut index = Self {
            persisted: BTreeSet::new(),
            repeated: BTreeSet::new(),
            pins: Vec::new(),
        };
        for (path, document) in &plan.documents {
            if path.extension().is_none_or(|ext| ext != "kicad_sch") {
                continue;
            }
            let tree = parse_sexp(if after {
                &document.after
            } else {
                &document.before
            })?;
            let mut sheet_ids = BTreeSet::new();
            for child in tree.children().unwrap_or_default() {
                if child.head() != Some("lib_symbols") {
                    collect_uuids(child, &mut sheet_ids)?;
                }
            }
            if plan.instances.values().filter(|file| *file == path).count() > 1 {
                index.repeated.extend(sheet_ids.iter().cloned());
            }
            for id in sheet_ids {
                ensure!(
                    index.persisted.insert(id),
                    "Duplicate saved UUID in project sheets"
                );
            }
            let symbols = tree.find_all("symbol");
            let placed = super::resolved_placed_pins_by_reference(&tree)
                .context(super::UNRESOLVED_PIN_GEOMETRY)?;
            for (symbol, pins) in placed {
                let uuid = symbol
                    .uuid
                    .as_deref()
                    .context("Symbol lacks UUID for ERC comparison")?;
                let projects = symbols
                    .iter()
                    .find(|node| node.find_str("uuid") == Some(uuid))
                    .and_then(|node| node.find("instances"))
                    .map(|node| node.find_all("project"))
                    .unwrap_or_default();
                let repeated = projects.len() != 1 || projects[0].find_all("path").len() != 1;
                for (pin, transform) in pins {
                    index.pins.push(ErcPinIdentity {
                        identity: format!("symbol:{uuid}:pin:{}", pin.number),
                        reference: symbol.reference.clone(),
                        number: pin.number.clone(),
                        position: copperpour_sexp::schematic::pin_endpoint(&pin, transform),
                        repeated,
                    });
                }
            }
        }
        Ok(index)
    }

    fn identity(&self, item: &cli::ReportItem) -> Result<String> {
        if let Some(uuid) = &item.uuid {
            if self.persisted.contains(uuid) {
                ensure!(
                    !self.repeated.contains(uuid),
                    "Cannot uniquely bind ERC item {uuid} to one instance of a repeated sheet"
                );
                return Ok(format!("uuid:{uuid}"));
            }
        }
        // KiCad generates fresh SCH_PIN UUIDs while loading a symbol with no
        // saved pin UUID records. A before/after comparison must not mistake
        // those ephemeral IDs for new violations. Bind them to one observed
        // reference and pin endpoint, never to free text or coordinates alone.
        let pos = item
            .pos
            .as_ref()
            .context("ERC item has no persisted UUID or usable pin position")?;
        let candidates = self
            .pins
            .iter()
            .filter(|pin| {
                !pin.reference.is_empty()
                    && item.description.contains(&pin.reference)
                    && item
                        .description
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|token| token == pin.number)
                    && copperpour_sexp::geometry::points_coincident(
                        pos.x,
                        pos.y,
                        pin.position.0,
                        pin.position.1,
                        0.001,
                    )
            })
            .collect::<Vec<_>>();
        let identities = candidates
            .iter()
            .map(|pin| pin.identity.as_str())
            .collect::<BTreeSet<_>>();
        ensure!(identities.len()==1 && candidates.iter().all(|pin|!pin.repeated),
            "Cannot uniquely bind transient ERC item '{}' to one saved symbol pin; save/repair pin identities in KiCad before transfer",item.description);
        Ok(identities.first().unwrap().to_string())
    }
}

fn xml_child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(name))
}

type PinPartition = BTreeSet<(String, String)>;
type NetNames = BTreeMap<PinPartition, String>;
type NetlistPartitions = (BTreeMap<(String, String), Vec<String>>, NetNames);

fn netlist_partitions(tree: roxmltree::Node<'_, '_>) -> Result<(NetlistPartitions, NetNames)> {
    ensure!(tree.has_tag_name("export"), "Unexpected KiCad netlist root");
    let mut components = BTreeMap::new();
    for component in xml_child(tree, "components")
        .context("Netlist lacks components")?
        .children()
        .filter(roxmltree::Node::is_element)
    {
        ensure!(
            component.has_tag_name("comp"),
            "Unexpected netlist component record"
        );
        let reference = component
            .attribute("ref")
            .context("Netlist component lacks ref")?;
        let mut stamps = xml_child(component, "tstamps")
            .and_then(|node| node.text())
            .context("Netlist component lacks UUID")?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(!stamps.is_empty(), "Netlist component has no UUIDs");
        stamps.sort();
        // Sheet-level assembly settings can change effective component flags
        // even though the saved symbol and its instance records stay intact.
        let mut settings = component
            .children()
            .filter(|node| {
                node.has_tag_name("variants")
                    || node.has_tag_name("property")
                        && matches!(
                            node.attribute("name"),
                            Some(
                                "dnp"
                                    | "exclude_from_bom"
                                    | "exclude_from_board"
                                    | "exclude_from_pos_files"
                                    | "exclude_from_sim"
                            )
                        )
            })
            .map(|node| node.document().input_text()[node.range()].to_owned())
            .collect::<Vec<_>>();
        if let Some(classes) = xml_child(component, "component_classes") {
            for class in classes.children().filter(|node| node.has_tag_name("class")) {
                let name = class.text().context("Empty KiCad component class")?;
                settings.push(format!("component_class:{name}"));
            }
        }
        settings.sort_unstable();
        ensure!(
            components
                .insert((reference.into(), stamps.join(" ")), settings)
                .is_none(),
            "Duplicate component identity in netlist"
        );
    }
    let mut classes = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut seen_pins = BTreeSet::new();
    let references = components
        .keys()
        .map(|(reference, _): &(String, String)| reference.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        references.len() == components.len() && !references.contains(""),
        "Ambiguous or empty component reference in netlist"
    );
    for net in xml_child(tree, "nets")
        .context("Netlist lacks nets")?
        .children()
        .filter(roxmltree::Node::is_element)
    {
        ensure!(net.has_tag_name("net"), "Unexpected netlist net record");
        let mut nodes = BTreeSet::new();
        for node in net.children().filter(roxmltree::Node::is_element) {
            ensure!(node.has_tag_name("node"), "Unexpected netlist pin record");
            let reference = node.attribute("ref").context("Net node lacks ref")?;
            let pin = node.attribute("pin").context("Net node lacks pin")?;
            ensure!(
                references.contains(reference) && !pin.is_empty(),
                "KiCad net node refers to an unknown component or empty pin"
            );
            ensure!(
                seen_pins.insert((reference, pin)),
                "Repeated pin in KiCad netlist"
            );
            nodes.insert((reference.into(), pin.into()));
        }
        ensure!(!nodes.is_empty(), "KiCad net has no pin records");
        let class = net
            .attribute("class")
            .context("KiCad net lacks effective class")?;
        let name = net.attribute("name").context("KiCad net lacks name")?;
        classes.insert(nodes.clone(), class.to_owned());
        names.insert(nodes, name.to_owned());
    }
    Ok(((components, classes), names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::ToolContent;

    const ROOT_UUID: &str = "a73252f6-017c-453c-8321-f1201f92efed";
    const SHEET_UUID: &str = "269824f1-3d6b-4e8b-ac46-c5f8ca062c27";
    const RESISTOR: &str = "8b9b7750-3421-4b18-92a3-4bddb63e9056";
    const NATIVE: &str = include_str!("../../tests/fixtures/component_fields_kicad10.kicad_sch");

    fn fixture() -> (tempfile::TempDir, Request) {
        fixture_with_destination("logic.kicad_sch")
    }

    fn fixture_with_destination(destination: &str) -> (tempfile::TempDir, Request) {
        fixture_with_sheet_settings(destination, false)
    }

    fn fixture_with_sheet_settings(
        destination: &str,
        destination_dnp: bool,
    ) -> (tempfile::TempDir, Request) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("component_fields_kicad10.kicad_sch");
        // The units, all pin UUIDs, fields and library geometry are from the
        // KiCad 10-authored component-field fixture. Only add a destination.
        let sheet = copperpour_sexp::schematic::format_hierarchical_sheet(
            copperpour_sexp::schematic::HierarchicalSheetSpec {
                name: "Logic regulator",
                file: destination,
                x: 10.0,
                y: 10.0,
                width: 30.0,
                height: 20.0,
                project_name: "component_fields_kicad10",
                parent_instance_path: &format!("/{ROOT_UUID}"),
                page: "2",
            },
        );
        let tree = parse_sexp(&sheet).unwrap();
        let generated = tree.find_str("uuid").unwrap();
        let mut sheet = sheet.replace(generated, SHEET_UUID);
        if destination_dnp {
            assert_eq!(sheet.matches("(dnp no)").count(), 1);
            sheet = sheet.replace("(dnp no)", "(dnp yes)");
        }
        let close = NATIVE.rfind(')').unwrap();
        std::fs::write(
            &root,
            apply_edits(
                NATIVE.into(),
                vec![SexpEdit::insert(close, format!("\n{sheet}\n"))],
            ),
        )
        .unwrap();
        std::fs::write(
            root.with_extension("kicad_pro"),
            "{\"meta\":{\"filename\":\"component_fields_kicad10.kicad_pro\"}}",
        )
        .unwrap();
        let destination = dir.path().join(destination);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, super::super::blank_schematic_template()).unwrap();
        let request = Request {
            schematic: root,
            source_sheet_instance: format!("/{ROOT_UUID}"),
            destination_sheet_instance: format!("/{ROOT_UUID}/{SHEET_UUID}"),
            object_uuids: vec![RESISTOR.into()],
            offset_x_mm: -50.8,
            offset_y_mm: 25.4,
            dry_run: true,
            expected_plan_revision: None,
        };
        (dir, request)
    }

    const SECOND_ROOT: &str = "8e3b1c27-5d4a-4f60-a7c9-1e2d3c4b5a69";
    const FIRST_ROOT: &str = "b603aa20-aac9-4b43-91f9-885f100bae62";
    const ROOT_RESISTOR: &str = "75106e38-c2c4-464f-9641-606884966200";

    fn multi_root_fixture() -> (tempfile::TempDir, Request) {
        let dir = tempfile::tempdir().unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/top_level_sheets_kicad10");
        for name in [
            "toplevel.kicad_pro",
            "toplevel.kicad_sch",
            "child.kicad_sch",
            "second.kicad_sch",
        ] {
            std::fs::copy(fixtures.join(name), dir.path().join(name)).unwrap();
        }
        let pro = dir.path().join("toplevel.kicad_pro");
        let mut settings: Value = serde_json::from_slice(&std::fs::read(&pro).unwrap()).unwrap();
        settings["schematic"]["top_level_sheets"] = json!([
            {"filename": "toplevel.kicad_sch", "name": "First", "uuid": FIRST_ROOT},
            {"filename": "second.kicad_sch", "name": "Second", "uuid": SECOND_ROOT}
        ]);
        std::fs::write(pro, serde_json::to_vec(&settings).unwrap()).unwrap();
        let request = Request {
            schematic: dir.path().join("toplevel.kicad_sch"),
            source_sheet_instance: format!("/{FIRST_ROOT}"),
            destination_sheet_instance: format!("/{SECOND_ROOT}"),
            object_uuids: vec![ROOT_RESISTOR.into()],
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
            dry_run: true,
            expected_plan_revision: None,
        };
        (dir, request)
    }

    #[test]
    fn transfer_uses_declared_root_uuid_and_holds_sheets_outside_the_move() {
        let (dir, request) = multi_root_fixture();
        let before = snapshot(dir.path());
        let plan = Plan::prepare(&request).unwrap();
        let old_path = format!("/{FIRST_ROOT}/{ROOT_RESISTOR}");
        assert_eq!(
            plan.report["path_mapping"][old_path],
            format!("/{SECOND_ROOT}/{ROOT_RESISTOR}")
        );
        let instances = &plan.instances;
        assert_eq!(instances.len(), 3);
        let (path, child) = plan
            .documents
            .iter()
            .find(|(path, _)| path.ends_with("child.kicad_sch"))
            .unwrap();
        assert!(instances.values().any(|p| p == path));
        assert_eq!(child.after, child.before);
        assert_eq!(before, snapshot(dir.path()));
    }

    #[test]
    fn transfer_refuses_if_a_declared_root_is_missing() {
        let (dir, mut request) = multi_root_fixture();
        request.destination_sheet_instance =
            format!("/{FIRST_ROOT}/4e754337-e3f1-4d84-85ed-5ff5976e9393");
        std::fs::remove_file(dir.path().join("second.kicad_sch")).unwrap();
        let before = snapshot(dir.path());
        let error = Plan::prepare(&request).err().unwrap();
        assert!(error.to_string().contains("second.kicad_sch"), "{error:#}");
        assert_eq!(before, snapshot(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_refuses_incomplete_multi_root_netlist_and_erc_separately() {
        let (dir, request) = multi_root_fixture();
        let before = snapshot(dir.path());
        let control = tempfile::tempdir().unwrap();
        let child = "4e754337-e3f1-4d84-85ed-5ff5976e9393";
        let netlist = |include_second| {
            let second = if include_second {
                r#"<sheet tstamps="/"><title_block><source>second.kicad_sch</source></title_block></sheet>"#
            } else {
                ""
            };
            format!(
                r#"<export><design><sheet tstamps="/"><title_block><source>toplevel.kicad_sch</source></title_block></sheet>
                    <sheet tstamps="/{child}/"><title_block><source>child.kicad_sch</source></title_block></sheet>{second}</design>
                    <components><comp ref="R1"><tstamps>{ROOT_RESISTOR}</tstamps></comp></components><nets/></export>"#
            )
        };
        let erc = |include_second| {
            let mut sheets = vec![
                json!({"uuid_path":format!("/{FIRST_ROOT}"),"violations":[]}),
                json!({"uuid_path":format!("/{FIRST_ROOT}/{child}"),"violations":[]}),
            ];
            if include_second {
                sheets.push(json!({"uuid_path":format!("/{SECOND_ROOT}"),"violations":[]}));
            }
            json!({"sheets":sheets}).to_string()
        };
        let plan = Plan::prepare(&request).unwrap();
        let revision = plan.revision(&request);
        for (missing_netlist, missing_erc, finding, expected) in [
            (true, false, None, "KiCad netlist did not cover"),
            (false, false, Some(Value::Null), "has no 'violations' array"),
            (false, true, None, "KiCad ERC did not cover"),
            (
                false,
                false,
                Some(json!([])),
                "Malformed ERC violation identity",
            ),
            (
                false,
                false,
                Some(json!([{"uuid":ROOT_RESISTOR,"description":"R1"}])),
                "Transfer introduces an ERC violation",
            ),
        ] {
            let erc_before = erc(!missing_erc);
            let mut erc_after: Value = serde_json::from_str(&erc_before).unwrap();
            match finding {
                // KiCad did not report on the destination sheet at all.
                Some(Value::Null) => {
                    erc_after["sheets"][2]
                        .as_object_mut()
                        .unwrap()
                        .remove("violations");
                }
                Some(items) => {
                    erc_after["sheets"][2]["violations"] = json!([{
                        "type":"pin_not_connected","severity":"warning","items":items
                    }]);
                }
                None => {}
            }
            let operation = format!(
                "#!/bin/sh\nif [ \"$2\" = erc ]; then\nif grep -q '{ROOT_RESISTOR}' \"$(dirname \"$9\")/second.kicad_sch\"; then\ncat > \"$4\" <<'REPORT'\n{erc_after}\nREPORT\nelse\ncat > \"$4\" <<'REPORT'\n{erc_before}\nREPORT\nfi\nelse\ncat > \"$5\" <<'NETLIST'\n{}\nNETLIST\nfi\n",
                netlist(!missing_netlist),
            );
            let script =
                cli::test_support::write_script(control.path(), "coverage-cli", &operation, "");
            let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
                kicad_cli: script.to_str().unwrap().into(),
                ipc_address: String::new(),
                eager_toolsets: true,
                ..Default::default()
            })
            .await
            .unwrap();
            let response = handler
                .handle_message(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                        "name":OPERATION,"arguments":{
                            "schematic":request.schematic,
                            "source_sheet_instance":request.source_sheet_instance,
                            "destination_sheet_instance":request.destination_sheet_instance,
                            "object_uuids":request.object_uuids,
                            "offset_x_mm":request.offset_x_mm,
                            "offset_y_mm":request.offset_y_mm,
                            "dry_run":false,
                            "expected_plan_revision":revision
                        }
                    }}),
                )
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(response["isError"], true, "{response}");
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
            assert!(
                result["message"].as_str().unwrap().contains(expected),
                "{result}"
            );
            assert_eq!(snapshot(dir.path()), before);
        }
    }

    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                files.extend(snapshot(&path));
            } else if kind.is_symlink() {
                // A link is recorded by its target, never followed.
                let target = std::fs::read_link(&path).unwrap();
                files.insert(path, target.into_os_string().into_encoded_bytes());
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.insert(path, bytes);
            }
        }
        files
    }

    #[test]
    fn transfer_holds_changed_and_unchanged_inputs_in_one_transaction() {
        let (_dir, request) = multi_root_fixture();
        let plan = Plan::prepare(&request).unwrap();
        let transitions = plan.transitions();
        // The plan keys files by canonical path (`\\?\` on Windows, `/private` on macOS).
        let base = plan.root.parent().unwrap();
        for file in ["toplevel.kicad_sch", "second.kicad_sch"] {
            let path = base.join(file);
            let document = &plan.documents[&path];
            assert_ne!(document.before, document.after);
            assert!(transitions.contains(&FileTransition::replace(
                path,
                &document.before,
                &document.after
            )));
        }
        for file in ["child.kicad_sch", "toplevel.kicad_pro"] {
            let path = base.join(file);
            let before = &plan.documents[&path].before;
            assert!(transitions.contains(&FileTransition::guard(path, before)));
        }
        for file in [
            "fp-lib-table",
            "sym-lib-table",
            "toplevel.kicad_pcb",
            "toplevel.kicad_dru",
            "~toplevel.kicad_pro.lck",
        ] {
            assert!(transitions.contains(&FileTransition::guard_absent(base.join(file))));
        }
        assert_eq!(transitions.len(), 9);
    }

    #[test]
    fn transfer_plan_preserves_native_symbol_identity_fields_pins_and_unrelated_blocks() {
        let (dir, request) = fixture();
        let before = snapshot(dir.path());
        let plan = Plan::prepare(&request).unwrap();
        assert_eq!(before, snapshot(dir.path()), "planning is non-mutating");
        let root = &plan.documents[&plan.root];
        let destination = &plan.documents[&plan.root.with_file_name("logic.kicad_sch")];
        let old = parse_sexp(&root.before).unwrap();
        let new = parse_sexp(&destination.after).unwrap();
        let symbol = old
            .find_all("symbol")
            .into_iter()
            .find(|node| node.find_str("uuid") == Some(RESISTOR))
            .unwrap();
        let moved = new.find_all("symbol")[0];
        let mut old_ids = BTreeSet::new();
        let mut new_ids = BTreeSet::new();
        collect_uuids(symbol, &mut old_ids).unwrap();
        collect_uuids(moved, &mut new_ids).unwrap();
        assert_eq!(old_ids, new_ids);
        for field in [
            "Reference",
            "Value",
            "Footprint",
            "Datasheet",
            "Description",
        ] {
            assert_eq!(property(symbol, field), property(moved, field));
        }
        for field in ["unit", "dnp", "in_bom", "on_board", "exclude_from_sim"] {
            assert_eq!(symbol.find_str(field), moved.find_str(field));
        }
        let at = symbol.find("at").unwrap();
        let new_at = moved.find("at").unwrap();
        assert!((new_at.get_f64(1).unwrap() - at.get_f64(1).unwrap() + 50.8).abs() < 0.000001);
        assert!((new_at.get_f64(2).unwrap() - at.get_f64(2).unwrap() - 25.4).abs() < 0.000001);
        assert_eq!(
            moved
                .find("instances")
                .unwrap()
                .find("project")
                .unwrap()
                .find("path")
                .unwrap()
                .get(1)
                .and_then(SexpNode::as_str),
            Some(request.destination_sheet_instance.as_str())
        );
        let old_blocks = object_blocks(&root.before).unwrap();
        let new_blocks = object_blocks(&root.after).unwrap();
        assert!(!new_blocks.contains_key(RESISTOR));
        for (uuid, (start, end)) in old_blocks {
            if uuid != RESISTOR {
                let (new_start, new_end) = new_blocks[&uuid];
                assert_eq!(&root.before[start..end], &root.after[new_start..new_end]);
            }
        }
        assert_eq!(
            plan.report["path_mapping"][format!("/{ROOT_UUID}/{RESISTOR}")],
            format!("/{ROOT_UUID}/{SHEET_UUID}/{RESISTOR}")
        );
    }

    #[test]
    fn transfer_stages_project_relative_symbol_libraries() {
        let (dir, request) = fixture();
        let path = dir.path().canonicalize().unwrap().join("Device.kicad_sym");
        let library =
            include_str!("../../tests/fixtures/reference_prefix_kicad10/Device.kicad_sym");
        std::fs::write(&path, library).unwrap();
        std::fs::write(dir.path().join("sym-lib-table"),
            r#"(sym_lib_table (version 7) (lib (name "Device") (type "KiCad") (uri "${KIPRJMOD}/Device.kicad_sym") (options "") (descr "")))"#).unwrap();
        let before = snapshot(dir.path());
        let plan = Plan::prepare(&request).unwrap();
        let document = &plan.documents[&path];
        assert_eq!(document.before, library);
        assert_eq!(document.after, library);
        assert_eq!(before, snapshot(dir.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_refuses_library_inputs_it_cannot_snapshot() {
        let (dir, request) = fixture();
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: String::new(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let absolute = dir.path().join("Device.kicad_sym");
        for (table, head, uri, detail) in [
            (
                "fp-lib-table",
                "fp_lib_table",
                "${KIPRJMOD}/Device.pretty",
                "only .kicad_sym files are supported",
            ),
            (
                "sym-lib-table",
                "sym_lib_table",
                "${KIPRJMOD}/../Device.kicad_sym",
                "Cannot safely stage library",
            ),
            (
                "sym-lib-table",
                "sym_lib_table",
                absolute.to_str().unwrap(),
                "Use a project-relative URI",
            ),
        ] {
            let path = dir.path().join(table);
            std::fs::write(
                &path,
                format!("({head} (version 7) (lib (name Device) (type KiCad) (uri {uri:?})))"),
            )
            .unwrap();
            let before = snapshot(dir.path());
            let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":OPERATION,"arguments":{
                    "schematic":request.schematic,
                    "source_sheet_instance":request.source_sheet_instance,
                    "destination_sheet_instance":request.destination_sheet_instance,
                    "object_uuids":request.object_uuids,
                    "offset_x_mm":request.offset_x_mm,
                    "offset_y_mm":request.offset_y_mm
                }
            }});
            let response = handler.handle_message(call).await.unwrap().result.unwrap();
            assert_eq!(response["isError"], true, "{response}");
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
            assert!(
                result["message"].as_str().unwrap().contains(detail),
                "{result}"
            );
            assert_eq!(before, snapshot(dir.path()));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn transfer_refuses_netclass_directives_on_either_sheet() {
        let fixture_text = include_str!("../../tests/fixtures/rigid_block_move_kicad10.kicad_sch");
        let start = copperpour_sexp::writer::find_block_starts(fixture_text, "netclass_flag")[0];
        let (_, end) = find_balanced_block(fixture_text, start).unwrap();
        for (tag, in_source) in [
            ("netclass_flag", true),
            ("netclass_flag", false),
            ("directive_label", true),
            ("directive_label", false),
        ] {
            let (dir, request) = fixture();
            let path = if in_source {
                request.schematic.clone()
            } else {
                dir.path().join("logic.kicad_sch")
            };
            let original = std::fs::read_to_string(&path).unwrap();
            let end_of_root = original.rfind(')').unwrap();
            std::fs::write(
                &path,
                apply_edits(
                    original,
                    vec![SexpEdit::insert(
                        end_of_root,
                        format!(
                            "\n{}\n",
                            fixture_text[start..end].replacen(
                                "(netclass_flag",
                                &format!("({tag}"),
                                1
                            )
                        ),
                    )],
                ),
            )
            .unwrap();
            let before = snapshot(dir.path());
            let error = Plan::prepare(&request).err().unwrap();
            assert!(error.to_string().contains(tag), "{error:#}");
            assert_eq!(before, snapshot(dir.path()));
        }
    }

    #[tokio::test]
    async fn transfer_plans_footprint_paths_without_writing_the_board() {
        let (dir, request) = fixture();
        let board = include_str!("../../tests/fixtures/drc_ownership_j1.kicad_pcb");
        let fixture_path = "/dc16e7b1-abe3-4faa-bfec-023e07549c11";
        assert!(board.contains(fixture_path));
        let board_path = request.schematic.with_extension("kicad_pcb");
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: String::new(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        for (path, tag, refusal) in [
            (format!("/{RESISTOR}"), "footprint", None),
            (format!("/{ROOT_UUID}/{RESISTOR}"), "footprint", None),
            (format!("/{}", RESISTOR.to_uppercase()), "footprint", None),
            // KiCad 10 still reads the legacy block name.
            (format!("/{RESISTOR}"), "module", None),
            (
                format!("/{ROOT_UUID}/{SHEET_UUID}/{RESISTOR}"),
                "footprint",
                Some("already contains a destination schematic path"),
            ),
        ] {
            let saved = board
                .replace(fixture_path, &path)
                .replace("\t(footprint \"", &format!("\t({tag} \""));
            std::fs::write(&board_path, saved).unwrap();
            let before = snapshot(dir.path());
            let response = handler
                .handle_message(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                        "name":OPERATION,"arguments":{
                            "schematic":request.schematic,
                            "source_sheet_instance":request.source_sheet_instance,
                            "destination_sheet_instance":request.destination_sheet_instance,
                            "object_uuids":request.object_uuids,
                            "offset_x_mm":request.offset_x_mm,
                            "offset_y_mm":request.offset_y_mm
                        }
                    }}),
                )
                .await
                .unwrap()
                .result
                .unwrap();
            let text = response["content"][0]["text"].as_str().unwrap();
            if let Some(refusal) = refusal {
                assert_eq!(response["isError"], true, "{response}");
                assert!(text.contains(refusal), "{response}");
            } else {
                let plan: Value = serde_json::from_str(text).unwrap();
                let expected = if path.starts_with(&format!("/{ROOT_UUID}/")) {
                    format!("/{ROOT_UUID}/{SHEET_UUID}/{RESISTOR}")
                } else {
                    format!("/{SHEET_UUID}/{RESISTOR}")
                };
                assert_eq!(plan["pcb_path_mapping"][&path], expected);
                assert!(plan["pcb_net_name_mapping"].is_null());
            }
            assert_eq!(snapshot(dir.path()), before);
        }
        std::fs::write(&board_path, board).unwrap();
        let plan = Plan::prepare(&request).unwrap();
        assert_eq!(plan.report["affected_files"].as_array().unwrap().len(), 2);
        let document = &plan.documents[plan.board.as_ref().unwrap()];
        assert_eq!(document.before, document.after);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_refuses_unsupported_objects_and_hierarchies_without_writes() {
        const OTHER: &str = "4f0f6a51-0b7e-4c55-9d6a-2a9c1f3e7b10";
        let append = |path: &Path, block: &str| {
            let text = std::fs::read_to_string(path).unwrap();
            let close = text.rfind(')').unwrap();
            let edit = SexpEdit::insert(close, format!("\n{block}\n"));
            std::fs::write(path, apply_edits(text, vec![edit])).unwrap();
        };
        let replace = |path: &Path, from: &str, to: &str| {
            let text = std::fs::read_to_string(path).unwrap();
            assert_eq!(text.matches(from).count(), 1, "{from}");
            std::fs::write(path, text.replacen(from, to, 1)).unwrap();
        };
        let child_sheet = |file: &str| {
            copperpour_sexp::schematic::format_hierarchical_sheet(
                copperpour_sexp::schematic::HierarchicalSheetSpec {
                    name: "Nested",
                    file,
                    x: 10.0,
                    y: 10.0,
                    width: 20.0,
                    height: 10.0,
                    project_name: "component_fields_kicad10",
                    parent_instance_path: &format!("/{ROOT_UUID}/{SHEET_UUID}"),
                    page: "3",
                },
            )
        };
        let outside = tempfile::tempdir().unwrap();
        let bus = format!(
            "(bus (pts (xy 0 0) (xy 10 0)) (stroke (width 0) (type default)) (uuid \"{OTHER}\"))"
        );
        type Change<'a> = Box<dyn Fn(&Path, &mut Request) + 'a>;
        let cases: Vec<(&str, Change<'_>, &str)> = vec![
            (
                "bus on source",
                Box::new(|_, r| append(&r.schematic, &bus)),
                "unsupported 'bus'",
            ),
            (
                "bus on destination",
                Box::new(|d, _| append(&d.join("logic.kicad_sch"), &bus)),
                "unsupported 'bus'",
            ),
            (
                "instance table on source",
                Box::new(|_, r| append(&r.schematic, "(symbol_instances)")),
                "unsupported 'symbol_instances'",
            ),
            (
                "instance table on destination",
                Box::new(|d, _| append(&d.join("logic.kicad_sch"), "(symbol_instances)")),
                "unsupported 'symbol_instances'",
            ),
            (
                "hierarchical label",
                Box::new(|_, r| {
                    append(&r.schematic, &format!("(hierarchical_label \"H\" (shape input) (at 0 0 0) (effects (font (size 1.27 1.27))) (uuid \"{OTHER}\"))"));
                    r.object_uuids = vec![OTHER.into()];
                }),
                "unsupported type 'hierarchical_label'",
            ),
            (
                "sheet",
                Box::new(|_, r| r.object_uuids = vec![SHEET_UUID.into()]),
                "unsupported type 'sheet'",
            ),
            (
                "sheet pin",
                Box::new(|_, r| {
                    let anchor = format!("(uuid \"{SHEET_UUID}\")");
                    replace(&r.schematic, &anchor, &format!("{anchor} (pin \"P\" input (at 10 15 180) (effects (font (size 1.27 1.27))) (uuid \"{OTHER}\"))"));
                    r.object_uuids = vec![OTHER.into()];
                }),
                "exactly one top-level source object",
            ),
            (
                "derived library definition",
                Box::new(|_, r| {
                    replace(
                        &r.schematic,
                        "(symbol \"Device:R\"",
                        "(symbol \"Device:R\" (extends \"Device:X\")",
                    )
                }),
                "Derived library definitions",
            ),
            (
                "unannotated reference",
                Box::new(|_, r| {
                    replace(&r.schematic, "\"Reference\" \"R1\"", "\"Reference\" \"R?\"");
                    replace(&r.schematic, "(reference \"R1\")", "(reference \"R?\")");
                }),
                "Annotate symbol",
            ),
            (
                "instance table elsewhere in the hierarchy",
                Box::new(|d, _| {
                    let other = format!("{}(symbol_instances)\n)", {
                        let blank = super::super::blank_schematic_template();
                        blank[..blank.rfind(')').unwrap()].to_owned()
                    });
                    std::fs::write(d.join("other.kicad_sch"), other).unwrap();
                    append(&d.join("logic.kicad_sch"), &child_sheet("other.kicad_sch"));
                }),
                "unsupported 'symbol_instances'",
            ),
            (
                "wire to an unselected item on the source",
                Box::new(|_, r| {
                    append(
                        &r.schematic,
                        &format!("(no_connect (at 101.6 72.39) (uuid \"{OTHER}\"))"),
                    )
                }),
                "Physical source boundary",
            ),
            (
                "local label name crossing the source selection",
                Box::new(|_, r| {
                    const SECOND: &str = "0b8f5c1e-6a2d-4e3f-9c7b-5d1a2e3f4c5d";
                    for (uuid, at) in [(OTHER, "20 20 0"), (SECOND, "30 30 0")] {
                        append(&r.schematic, &format!("(label \"SCOPE\" (at {at}) (effects (font (size 1.27 1.27))) (uuid \"{uuid}\"))"));
                    }
                    r.object_uuids.push(OTHER.into());
                }),
                "crosses the source selection boundary",
            ),
            (
                "same source and destination file",
                Box::new(|_, r| r.destination_sheet_instance = r.source_sheet_instance.clone()),
                "Source and destination must be different schematic files",
            ),
            (
                "foreign project instance record",
                Box::new(|_, r| {
                    replace(
                        &r.schematic,
                        "\t\t\t(project \"component_fields_kicad10\"\n\t\t\t\t(path \"/a73252f6-017c-453c-8321-f1201f92efed\"\n\t\t\t\t\t(reference \"R1\")",
                        "\t\t\t(project \"another_project\"\n\t\t\t\t(path \"/a73252f6-017c-453c-8321-f1201f92efed\"\n\t\t\t\t\t(reference \"R1\")",
                    )
                }),
                "instance metadata disagrees with project",
            ),
            (
                "cyclic hierarchy",
                Box::new(|d, _| {
                    append(&d.join("logic.kicad_sch"), &child_sheet("logic.kicad_sch"))
                }),
                "Cyclic sheet hierarchy",
            ),
            (
                "sheet outside the project",
                Box::new(|d, _| {
                    let target = outside.path().join("outside.kicad_sch");
                    std::fs::write(&target, super::super::blank_schematic_template()).unwrap();
                    std::os::unix::fs::symlink(&target, d.join("outside.kicad_sch")).unwrap();
                    append(
                        &d.join("logic.kicad_sch"),
                        &child_sheet("outside.kicad_sch"),
                    );
                }),
                "escapes the root project directory",
            ),
        ];
        for (label, change, refusal) in cases {
            let (dir, mut request) = fixture();
            change(dir.path(), &mut request);
            let before = snapshot(dir.path());
            let error = Plan::prepare(&request)
                .err()
                .unwrap_or_else(|| panic!("{label}"));
            assert!(format!("{error:#}").contains(refusal), "{label}: {error:#}");
            assert_eq!(snapshot(dir.path()), before, "{label}");
        }
    }

    #[test]
    fn transfer_accepts_an_external_absolute_library_uri() {
        let external = tempfile::tempdir().unwrap();
        let library =
            include_str!("../../tests/fixtures/reference_prefix_kicad10/Device.kicad_sym");
        let path = external.path().join("Device.kicad_sym");
        std::fs::write(&path, library).unwrap();
        let (dir, request) = fixture();
        // The same file spelled through the project directory and back out.
        let dotted = dir
            .path()
            .join("..")
            .join(external.path().file_name().unwrap())
            .join("Device.kicad_sym");
        for uri in [&path, &dotted] {
            std::fs::write(
                dir.path().join("sym-lib-table"),
                format!(
                    "(sym_lib_table (version 7) (lib (name Device) (type KiCad) (uri {:?})))",
                    uri.to_str().unwrap()
                ),
            )
            .unwrap();
            let plan = Plan::prepare(&request).unwrap();
            assert!(
                plan.documents
                    .keys()
                    .all(|key| key.file_name() != Some("Device.kicad_sym".as_ref())),
                "external library must not be staged: {uri:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn transfer_refuses_an_in_project_library_named_through_an_alias() {
        let (dir, request) = fixture();
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("project");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        let library =
            include_str!("../../tests/fixtures/reference_prefix_kicad10/Device.kicad_sym");
        std::fs::write(dir.path().join("Device.kicad_sym"), library).unwrap();
        let uri = alias.join("Device.kicad_sym");
        std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name Device) (type KiCad) (uri {:?})))",
                uri.to_str().unwrap()
            ),
        )
        .unwrap();
        let before = snapshot(dir.path());
        let error = Plan::prepare(&request).err().unwrap();
        assert!(
            format!("{error:#}").contains("Use a project-relative URI"),
            "{error:#}"
        );
        assert_eq!(snapshot(dir.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_refuses_a_library_reached_through_a_project_link() {
        let (dir, request) = fixture();
        let shared = tempfile::tempdir().unwrap();
        let library =
            include_str!("../../tests/fixtures/reference_prefix_kicad10/Device.kicad_sym");
        std::fs::write(shared.path().join("Device.kicad_sym"), library).unwrap();
        let project = dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(shared.path(), project.join("vendor")).unwrap();
        let uri = project.join("vendor").join("Device.kicad_sym");
        std::fs::write(
            project.join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name Device) (type KiCad) (uri {:?})))",
                uri.to_str().unwrap()
            ),
        )
        .unwrap();
        let before = snapshot(dir.path());
        let error = Plan::prepare(&request).err().unwrap();
        assert!(
            format!("{error:#}").contains("Use a project-relative URI"),
            "{error:#}"
        );
        assert_eq!(snapshot(dir.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_refuses_a_dangling_library_link_that_may_point_into_the_project() {
        let (dir, request) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let link = outside.path().join("Device.kicad_sym");
        std::os::unix::fs::symlink(dir.path().join("Device.kicad_sym"), &link).unwrap();
        std::fs::write(
            dir.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table (version 7) (lib (name Device) (type KiCad) (uri {:?})))",
                link.to_str().unwrap()
            ),
        )
        .unwrap();
        let before = snapshot(dir.path());
        let error = Plan::prepare(&request).err().unwrap();
        assert!(
            format!("{error:#}").contains("Cannot tell whether library"),
            "{error:#}"
        );
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn transfer_moves_all_units_together_and_rejects_partial_unit_selection() {
        let (dir, mut request) = fixture();
        request.object_uuids = vec!["37dfbfbc-5926-4718-b154-59425486a365".into()];
        let before = snapshot(dir.path());
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("All units"));
        assert_eq!(before, snapshot(dir.path()));
        request.object_uuids.extend([
            "734a8165-fecd-4968-a9b8-6990edf43995".into(),
            "db4e5e3c-9adc-40eb-a748-d1a1a0578a45".into(),
        ]);
        let plan = Plan::prepare(&request).unwrap();
        let destination = plan.root.with_file_name("logic.kicad_sch");
        let moved = parse_sexp(&plan.documents[&destination].after).unwrap();
        assert_eq!(moved.find_all("symbol").len(), 3);
        assert_eq!(
            moved
                .find_all("symbol")
                .iter()
                .map(|node| node.find_str("unit").unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["1", "2", "3"])
        );
    }

    #[test]
    fn transfer_refuses_unknown_destination_duplicate_selection_and_repeated_sheet() {
        let (dir, mut request) = fixture();
        let before = snapshot(dir.path());
        let refusal = |request: &Request| format!("{:#}", Plan::prepare(request).err().unwrap());
        request.destination_sheet_instance = "/missing".into();
        assert!(refusal(&request).contains("Destination sheet instance does not exist"));
        request.destination_sheet_instance = format!("/{ROOT_UUID}/{SHEET_UUID}");
        request.object_uuids.push(RESISTOR.into());
        assert!(refusal(&request).contains("Select unique, nonempty object UUIDs"));
        request.object_uuids.pop();
        assert_eq!(before, snapshot(dir.path()));
        let source = std::fs::read_to_string(&request.schematic).unwrap();
        let tree = parse_sexp(&source).unwrap();
        let sheet = tree.find("sheet").unwrap().find_str("uuid").unwrap();
        let (start, end) = object_blocks(&source).unwrap()[sheet];
        let repeated =
            source[start..end].replace(SHEET_UUID, "933d214c-5586-4ed5-8eae-875b556d8cc1");
        let close = source.rfind(')').unwrap();
        std::fs::write(
            &request.schematic,
            apply_edits(source, vec![SexpEdit::insert(close, repeated)]),
        )
        .unwrap();
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("repeated instances"));
    }

    #[test]
    fn transfer_rejects_library_and_uuid_conflicts_without_mutation() {
        let (dir, request) = fixture();
        let dest = dir.path().join("logic.kicad_sch");
        let original = std::fs::read_to_string(&dest).unwrap();
        let (start, end) = find_direct_child_blocks(&original, "kicad_sch")
            .into_iter()
            .find(|&(start, end)| {
                parse_sexp(&original[start..end]).unwrap().head() == Some("lib_symbols")
            })
            .unwrap();
        let mut source = apply_edits(
            original.clone(),
            vec![SexpEdit::replace(
                start,
                end,
                "(lib_symbols (symbol \"Device:R\" (property \"Value\" \"conflict\")))",
            )],
        );
        std::fs::write(&dest, &source).unwrap();
        let before = snapshot(dir.path());
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("conflicts"));
        assert_eq!(before, snapshot(dir.path()));
        let close = original.rfind(')').unwrap();
        source = apply_edits(
            original,
            vec![SexpEdit::insert(
                close,
                format!("(text \"duplicate\" (at 0 0 0) (uuid \"{RESISTOR}\"))"),
            )],
        );
        std::fs::write(&dest, &source).unwrap();
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("UUID conflicts"));
    }

    #[test]
    fn transfer_boundary_guards_cover_midwire_pins_local_labels_ports_and_destination_overlap() {
        let selected = BTreeSet::from(["selected".into()]);
        for remainder in [
            "(wire (pts (xy 5 0) (xy 5 10)) (uuid other))",
            "(junction (at 5 0) (uuid other))",
            "(sheet (uuid other) (pin \"PORT\" input (at 5 0 0)))",
            "(no_connect (at 5 0) (uuid other))",
        ] {
            let tree=parse_sexp(&format!("(kicad_sch (lib_symbols) (wire (pts (xy 0 0) (xy 10 0)) (uuid selected)) {remainder})")).unwrap();
            for destination in [false, true] {
                let error = check_boundary(&tree, &selected, destination).unwrap_err();
                assert!(error.to_string().contains("Physical"), "{error:#}");
            }
        }
        let tree=parse_sexp("(kicad_sch (lib_symbols) (label \"SCOPE\" (at 0 0 0) (uuid selected)) (label \"SCOPE\" (at 50 50 0) (uuid other)))").unwrap();
        assert!(check_boundary(&tree, &selected, false)
            .err()
            .unwrap()
            .to_string()
            .contains("Sheet-local"));
        let tree=parse_sexp("(kicad_sch (lib_symbols) (global_label \"GND\" (at 0 0 0) (uuid selected)) (global_label \"GND\" (at 50 50 0) (uuid other)))").unwrap();
        check_boundary(&tree, &selected, false).unwrap();
    }

    #[test]
    fn transfer_translation_preserves_angles_relative_graphics_and_escaped_annotations() {
        let block = r#"(text_box "note \\\" (at 999 999)" (start 10 20) (end 30 40) (effects (font (size 1 1))) (uuid "text"))"#;
        let new = translate(block, 2.54, -1.27).unwrap();
        let tree = parse_sexp(&new).unwrap();
        assert_eq!(tree.find("start").unwrap().get_f64(1), Some(12.54));
        assert_eq!(tree.find("end").unwrap().get_f64(2), Some(38.73));
        assert_eq!(tree.get(1), parse_sexp(block).unwrap().get(1));
        let symbol=translate("(symbol (at 10 20 90) (property \"Reference\" \"R1\" (at 5 6 0)) (pin \"1\" (uuid \"pin\")))",5.0,10.0).unwrap();
        assert!(symbol.contains("(at 15 30 90)"));
        assert!(symbol.contains("(at 10 16 0)"));
        assert!(symbol.contains("(pin \"1\" (uuid \"pin\"))"));
    }

    #[test]
    fn transfer_netlist_comparison_detects_lost_or_merged_connections_but_ignores_generated_names()
    {
        let a = r#"<export><components><comp ref="R1"><tstamps>r1</tstamps></comp><comp ref="R2"><tstamps>r2</tstamps></comp></components><nets><net code="1" name="/Source/N" class="Default"><node ref="R1" pin="1"/><node ref="R2" pin="1"/></net></nets></export>"#;
        let b = a
            .replace("/Source/N", "/Destination/N")
            .replace("code=\"1\"", "code=\"8\"");
        assert_eq!(parse_netlist(a).unwrap(), parse_netlist(&b).unwrap());
        let split = a.replace(
            r#"<node ref="R2" pin="1"/>"#,
            r#"</net><net name="other" class="Default"><node ref="R2" pin="1"/>"#,
        );
        assert_ne!(parse_netlist(a).unwrap(), parse_netlist(&split).unwrap());
        assert!(parse_netlist("<export><nets/></export>").is_err());
        let excluded = a.replace(
            "</tstamps></comp>",
            r#"</tstamps><property name="dnp"/></comp>"#,
        );
        assert_ne!(parse_netlist(a).unwrap(), parse_netlist(&excluded).unwrap());
        let classified = a.replace(
            "<tstamps>r1</tstamps>",
            "<tstamps>r1</tstamps><component_classes><class>Startup</class></component_classes>",
        );
        assert_ne!(
            parse_netlist(a).unwrap(),
            parse_netlist(&classified).unwrap()
        );
        assert_ne!(
            parse_netlist(a).unwrap(),
            parse_netlist(&b.replace("Default", "Fast")).unwrap()
        );
    }

    fn parse_netlist(source: &str) -> Result<NetlistPartitions> {
        let doc = roxmltree::Document::parse(source)?;
        Ok(netlist_partitions(doc.root_element())?.0)
    }

    #[test]
    fn transfer_pcb_rename_preserves_copper_bytes_and_refuses_destination_collision() {
        let before = r#"(kicad_pcb (version 20260306)
            (footprint "R" (uuid "footprint") (pad "1" (net "/source/LIMIT")))
            (segment (uuid "track") (start 1 2) (end 3 4) (width 0.25) (net "/source/LIMIT"))
            (arc (uuid "arc") (net "/source/LIMIT"))
            (via (uuid "via") (net "/source/LIMIT"))
            (zone (uuid "zone") (net "/source/LIMIT"))
            (gr_text "/source/LIMIT" (at 10 20)))"#;
        let mapping = BTreeMap::from([("/source/LIMIT".into(), "/destination/LIMIT".into())]);
        let (after, reported) = move_pcb_net_names(before, &mapping).unwrap();
        assert_eq!(reported, mapping);
        assert_eq!(
            after,
            before.replace("(net \"/source/LIMIT\")", "(net \"/destination/LIMIT\")")
        );
        let conflicting = before.replace("(net \"/source/LIMIT\")", "(net \"/destination/LIMIT\")");
        assert!(move_pcb_net_names(&conflicting, &mapping)
            .unwrap_err()
            .to_string()
            .contains("destination net"));
        let legacy = r#"(kicad_pcb (net 7 "/source/LIMIT") (segment (net 7)) (zone (net 7) (net_name "/source/LIMIT")))"#;
        assert_eq!(
            move_pcb_net_names(legacy, &mapping).unwrap().0,
            legacy.replace("/source/LIMIT", "/destination/LIMIT")
        );
        let harmless = before.replace("(net \"/source/LIMIT\")", "(net \"unrelated\")");
        assert_eq!(
            move_pcb_net_names(&harmless, &mapping).unwrap(),
            (harmless, BTreeMap::new())
        );
        let unsupported = before.replace("(segment", "(future_copper");
        assert!(move_pcb_net_names(&unsupported, &mapping)
            .unwrap_err()
            .to_string()
            .contains("Unsupported PCB net-name occurrence"));
        let inconsistent = legacy.replace("(zone (net 7)", "(zone (net 9)");
        assert!(move_pcb_net_names(&inconsistent, &mapping)
            .unwrap_err()
            .to_string()
            .contains("Conflicting PCB name"));
    }

    #[test]
    fn transfer_pcb_refuses_a_same_named_net_on_the_wrong_pad_or_path() {
        let old = "/SIG";
        let new = "/Logic/SIG";
        let native = format!(
            "(export (components (comp (ref \"R1\") (sheetpath (tstamps \"/\")) \
             (tstamps \"{RESISTOR}\"))) (nets (net (name \"{old}\") \
             (node (ref \"R1\") (pin \"1\")))))"
        );
        let (nets, paths) = pcb_netlist(&native).unwrap();
        let evidence = BoardNetEvidence {
            renames: BTreeMap::from([(old.into(), (new.into(), nets.into_keys().next().unwrap()))]),
            paths,
        };
        let renamed = BTreeMap::from([(old.into(), new.into())]);
        let instances = BTreeMap::from([(format!("/{ROOT_UUID}"), PathBuf::new())]);
        let board = |path: &str, pin: &str| {
            format!(
                "(kicad_pcb (footprint \"R\" (path \"{path}\") \
                 (property \"Reference\" \"R1\") (pad \"{pin}\" (net \"{old}\"))))"
            )
        };
        for path in [format!("/{RESISTOR}"), format!("/{ROOT_UUID}/{RESISTOR}")] {
            assert!(
                verify_pcb_pad_membership(&board(&path, "1"), &evidence, &renamed, &instances)
                    .is_ok()
            );
            assert!(
                verify_pcb_pad_membership(&board(&path, "2"), &evidence, &renamed, &instances)
                    .is_err()
            );
        }
        let foreign = format!("/{SHEET_UUID}/{RESISTOR}");
        assert!(
            verify_pcb_pad_membership(&board(&foreign, "1"), &evidence, &renamed, &instances)
                .is_err()
        );
    }

    #[test]
    fn transfer_pcb_paths_refuse_duplicate_aliases_and_ambiguous_multi_root_paths() {
        let full = format!("/{ROOT_UUID}/{RESISTOR}");
        let short = format!("/{RESISTOR}");
        let destination = format!("/{ROOT_UUID}/{SHEET_UUID}/{RESISTOR}");
        let mapping = BTreeMap::from([(full.clone(), destination)]);
        let board = |paths: &[&str]| {
            format!(
                "(kicad_pcb {})",
                paths
                    .iter()
                    .map(|path| format!("(footprint \"resistor\" (path {}))", quote(path)))
                    .collect::<String>()
            )
        };
        assert!(
            move_pcb_paths(&board(&[&full, &short]), &mapping, false, &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("Multiple PCB footprints")
        );
        let aliases = BTreeMap::from([(pcb_uuid_path(&short).unwrap(), 2)]);
        assert!(move_pcb_paths(&board(&[&short]), &mapping, true, &aliases)
            .unwrap_err()
            .to_string()
            .contains("Ambiguous root-relative PCB path"));
    }

    #[test]
    fn transfer_keeps_no_connects_before_symbols_and_symbols_before_the_footer() {
        let (_dir, mut request) = fixture();
        let mut source = copperpour_schematic_editor::Schematic::load(&request.schematic).unwrap();
        let uuid = source.add_no_connect(101.6, 72.39).uuid.clone();
        source.overwrite().unwrap();
        request.object_uuids.push(uuid);
        let plan = Plan::prepare(&request).unwrap();
        let after = &plan.documents[&plan.root.with_file_name("logic.kicad_sch")].after;
        let tree = parse_sexp(after).unwrap();
        let kinds = tree
            .children()
            .unwrap()
            .iter()
            .filter_map(SexpNode::head)
            .collect::<Vec<_>>();
        let symbol = kinds.iter().position(|kind| *kind == "symbol").unwrap();
        assert!(kinds.iter().position(|kind| *kind == "no_connect").unwrap() < symbol);
        if let Some(footer) = kinds.iter().position(|kind| *kind == "embedded_fonts") {
            assert!(symbol < footer);
        }
    }

    #[test]
    fn transfer_netclass_guard_refuses_explicit_assignments() {
        let source = json!({"unrelated":{"title":"keep"},"net_settings":{
            "netclass_patterns":[{"pattern":"*","netclass":"Default"}],
            "netclass_assignments":{"/source/LIMIT":["Fast"]}
        }})
        .to_string();
        let mapping = BTreeMap::from([("/source/LIMIT".into(), "/destination/LIMIT".into())]);
        assert!(check_project_net_dependencies(&source, &mapping, true)
            .unwrap_err()
            .to_string()
            .contains("updating saved project net assignments"));
        let harmless = source.replace("\"/source/LIMIT\"", "\"/unrelated/LIMIT\"");
        assert!(check_project_net_dependencies(&harmless, &mapping, true).is_ok());
        let collision = json!({"net_settings":{"netclass_assignments":{
            "/destination/LIMIT":["Slow"]
        }}})
        .to_string();
        assert!(check_project_net_dependencies(&collision, &mapping, true).is_err());
    }

    #[test]
    fn transfer_netclass_guard_refuses_different_patterns_for_the_same_class() {
        let source = json!({"net_settings":{"netclass_patterns":[
            {"pattern":"/source/*","netclass":"Fast"},
            {"pattern":"/destination/*","netclass":"Fast"}
        ]}})
        .to_string();
        let mapping = BTreeMap::from([("/source/LIMIT".into(), "/destination/LIMIT".into())]);
        assert!(check_project_net_dependencies(&source, &mapping, true)
            .unwrap_err()
            .to_string()
            .contains("changes the match of netclass pattern"));
        let equivalent = source
            .replace("/source/*", "/.*/LIMIT")
            .replace("/destination/*", ".*LIMIT");
        assert!(check_project_net_dependencies(&equivalent, &mapping, true).is_ok());
        let wildcard = equivalent.replace(".*LIMIT", "*/LIMIT");
        assert!(check_project_net_dependencies(&wildcard, &mapping, true).is_ok());
        let lookahead = equivalent.replace(".*LIMIT", "(?=LIMIT)");
        assert!(check_project_net_dependencies(&lookahead, &mapping, true)
            .unwrap_err()
            .to_string()
            .contains("unsupported netclass pattern"));
        let alternation = equivalent.replace(".*LIMIT", "/source/LIM|/other/LIMIT");
        assert!(check_project_net_dependencies(&alternation, &mapping, true)
            .unwrap_err()
            .to_string()
            .contains("unsupported netclass pattern"));
    }

    #[test]
    fn transfer_refuses_to_change_inherited_simulation_exclusion() {
        let (dir, request) = fixture();
        let original = std::fs::read_to_string(&request.schematic).unwrap();
        let start = copperpour_sexp::writer::find_block_starts(&original, "sheet")[0];
        let (_, end) = find_balanced_block(&original, start).unwrap();
        let sheet = &original[start..end];
        assert!(sheet.contains("(exclude_from_sim no)"));
        let modified = format!(
            "{}{}{}",
            &original[..start],
            sheet.replacen("(exclude_from_sim no)", "(exclude_from_sim yes)", 1),
            &original[end..]
        );
        std::fs::write(&request.schematic, modified).unwrap();
        let before = snapshot(dir.path());
        let error = Plan::prepare(&request).err().unwrap();
        assert!(error.to_string().contains("inherited simulation exclusion"));
        assert_eq!(snapshot(dir.path()), before);
    }

    #[test]
    fn transfer_nested_destination_round_trip_preserves_persisted_symbol_and_pin_ids() {
        let (dir, mut request) = fixture();
        let middle = dir.path().join("logic.kicad_sch");
        let mut schematic = copperpour_schematic_editor::Schematic::load(&middle).unwrap();
        let sheet = copperpour_schematic_editor::Sheet::new(
            "Destination",
            "leaf.kicad_sch",
            10.0,
            10.0,
            30.0,
            20.0,
        );
        let uuid = sheet.uuid.clone();
        schematic.add_sheet(sheet);
        schematic.overwrite().unwrap();
        std::fs::write(
            dir.path().join("leaf.kicad_sch"),
            super::super::blank_schematic_template(),
        )
        .unwrap();
        request.destination_sheet_instance.push('/');
        request.destination_sheet_instance.push_str(&uuid);
        let plan = Plan::prepare(&request).unwrap();
        copperpour_sexp::commit_file_transaction(dir.path(), plan.transitions()).unwrap();
        let leaf = copperpour_schematic_editor::Schematic::load(dir.path().join("leaf.kicad_sch"))
            .unwrap();
        assert_eq!(leaf.symbols.len(), 1);
        assert_eq!(leaf.symbols.iter().next().unwrap().uuid, RESISTOR);
        // A normal editor save must retain all identity and instance metadata.
        leaf.overwrite().unwrap();
        std::mem::swap(
            &mut request.source_sheet_instance,
            &mut request.destination_sheet_instance,
        );
        request.offset_x_mm = 50.8;
        request.offset_y_mm = -25.4;
        let reverse = Plan::prepare(&request).unwrap();
        let root = &reverse.documents[&reverse.root];
        let tree = parse_sexp(&root.after).unwrap();
        let resistor = tree
            .find_all("symbol")
            .into_iter()
            .find(|symbol| symbol.find_str("uuid") == Some(RESISTOR))
            .unwrap();
        assert_eq!(
            resistor.find("pin").unwrap().find_str("uuid"),
            Some("2e601eb3-6f37-4903-9f86-b1aa7e0196b4")
        );
        assert_eq!(resistor.find("at").unwrap().get_f64(1), Some(101.6));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_served_dispatch_dry_run_and_apply_report_observed_persistence() {
        assert_served_transfer(Served::Apply).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_served_dispatch_writes_board_paths_sheet_metadata_and_routing_nets() {
        assert_served_transfer(Served::Pcb).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_pcb_recovery_preserves_a_divergent_rules_guard_then_resumes() {
        assert_served_transfer(Served::PcbInterrupt).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_skips_rule_conditions_without_a_pcb_net_rename() {
        assert_served_transfer(Served::RulesWithoutPcb).await;
        assert_served_transfer(Served::PcbPathsOnly).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_recovery_refuses_changed_guards_after_a_partial_write() {
        assert_served_transfer(Served::Interrupt).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_apply_preserves_a_save_made_after_the_journal() {
        assert_served_transfer(Served::LateSave).await;
    }

    #[test]
    fn transfer_treats_only_an_absent_ipc_endpoint_as_a_closed_board() {
        use super::super::live_board::LiveBoard;
        use copperpour_ipc::UnreachableReason;
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("project.kicad_pcb");
        std::fs::write(&board, "(kicad_pcb)").unwrap();
        let state = |reason| LiveBoard::<()>::NeverReached {
            reason,
            message: String::new(),
            candidates: Vec::new(),
        };
        let ambiguous = LiveBoard::<()>::NeverReached {
            reason: UnreachableReason::NotConfigured,
            message: String::new(),
            candidates: vec![
                "ipc:///tmp/kicad/api.sock".into(),
                "ipc:///tmp/kicad/api-2.sock".into(),
            ],
        };
        let refusal = body(&board_state_refusal(&ambiguous, &board).unwrap());
        assert_eq!(refusal["error"]["kind"], "plan_blocked", "{refusal}");
        for reason in [
            UnreachableReason::NotConfigured,
            UnreachableReason::NoListener,
        ] {
            assert!(board_state_refusal(&state(reason), &board).is_none());
        }
        for reason in [
            UnreachableReason::AccessDenied,
            UnreachableReason::HandshakeFailed,
            UnreachableReason::TransportError,
        ] {
            let refusal = body(&board_state_refusal(&state(reason), &board).unwrap());
            assert_eq!(refusal["error"]["kind"], "plan_blocked", "{reason:?}");
        }
    }

    #[test]
    fn transfer_netlist_reader_refuses_unknown_or_incomplete_records() {
        let netlist = |components: &str, nets: &str| {
            format!(
                "<export><components><comp ref=\"R1\"><tstamps>{RESISTOR}</tstamps></comp>{components}</components><nets>{nets}</nets></export>"
            )
        };
        let net = r#"<net name="A" class="Default"><node ref="R1" pin="1"/></net>"#;
        let read = |xml: String| {
            let document = roxmltree::Document::parse(&xml).unwrap();
            netlist_partitions(document.root_element()).map(|_| ())
        };
        read(netlist("", net)).unwrap();
        for (components, nets, refusal) in [
            ("<unit/>", net, "Unexpected netlist component record"),
            ("", &format!("{net}<bus/>"), "Unexpected netlist net record"),
            (
                "",
                r#"<net name="A" class="Default"><pin/></net>"#,
                "Unexpected netlist pin record",
            ),
            (
                "",
                r#"<net name="A" class="Default"/>"#,
                "KiCad net has no pin records",
            ),
            (
                "",
                r#"<net name="A" class="Default"><node ref="R9" pin="1"/></net>"#,
                "unknown component",
            ),
        ] {
            let error = read(netlist(components, nets)).unwrap_err();
            assert!(error.to_string().contains(refusal), "{refusal}: {error:#}");
        }
    }

    #[test]
    fn transfer_reports_each_transaction_conflict_phase() {
        use copperpour_sexp::{SexpError, TransactionConflictKind as Phase, TransactionFailure};
        let target = PathBuf::from("/project/logic.kicad_sch");
        let journal = PathBuf::from("/project/.copperpour-transaction-old.json");
        let failure = |kind, retained: bool| TransactionFailure {
            error: SexpError::TransactionConflict {
                path: target.clone(),
                journal: journal.clone(),
                kind,
                reason: "changed".into(),
            },
            journal: retained.then(|| journal.clone().into_boxed_path()),
        };
        let shown_journal = copperpour_sexp::paths::display_path(&journal);
        let before = body(&transaction_failure_result(failure(
            Phase::BeforeImage,
            false,
        )));
        assert_eq!(before["error"]["kind"], "conflict", "{before}");
        assert!(!before.to_string().contains(&shown_journal), "{before}");
        let recovery = body(&transaction_failure_result(failure(Phase::Recovery, false)));
        assert_eq!(recovery["error"]["kind"], "conflict", "{recovery}");
        assert_eq!(
            recovery["error"]["paths"],
            json!([shown_journal]),
            "{recovery}"
        );
        for retained in [false, true] {
            let after = body(&transaction_failure_result(failure(
                Phase::AfterImage,
                retained,
            )));
            assert_eq!(
                after["error"]["kind"], "mutation_outcome_uncertain",
                "{after}"
            );
            assert!(after["message"].as_str().unwrap().contains(&shown_journal));
        }
    }

    #[test]
    fn transfer_reports_uncertainty_when_completed_transaction_no_longer_matches() {
        let (dir, request) = fixture();
        let plan = Plan::prepare(&request).unwrap();
        let commit =
            copperpour_sexp::try_commit_file_transaction(dir.path(), plan.transitions()).unwrap();
        assert!(copperpour_sexp::inspect_file_transactions(dir.path())
            .unwrap()
            .is_empty());
        let destination = dir.path().join("logic.kicad_sch");
        let mut newer = std::fs::read(&destination).unwrap();
        newer.push(b'\n');
        std::fs::write(&destination, newer).unwrap();
        let changed = snapshot(dir.path());

        let result = body(&report_committed_transfer(plan, &commit.id));
        assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
        assert!(result["message"].as_str().unwrap().contains(&commit.id));
        assert!(result.get("transaction_id").is_none(), "{result}");
        assert_eq!(snapshot(dir.path()), changed);
    }

    #[cfg(unix)]
    #[derive(PartialEq)]
    enum Served {
        Apply,
        Interrupt,
        LateSave,
        Pcb,
        PcbInterrupt,
        RulesWithoutPcb,
        PcbPathsOnly,
    }

    #[cfg(unix)]
    async fn assert_served_transfer(mode: Served) {
        let (dir, request) = fixture();
        let control = tempfile::tempdir().unwrap();
        let board = request.schematic.with_extension("kicad_pcb");
        if matches!(
            mode,
            Served::Pcb | Served::PcbInterrupt | Served::PcbPathsOnly
        ) {
            let saved =
                include_str!("../../tests/fixtures/repeat_layout_routing_kicad10.kicad_pcb");
            let tree = parse_sexp(saved).unwrap();
            let first = copperpour_sexp::board::footprints(&tree)[0].clone();
            let first_path = first.find_str("path").unwrap();
            let mut edits = Vec::new();
            for (start, end) in find_direct_child_blocks(saved, "kicad_pcb") {
                let block = &saved[start..end];
                let node = parse_sexp(block).unwrap();
                if node.head() == Some("footprint") {
                    let changed = if node.find_str("path") == Some(first_path) {
                        block
                            .replace(
                                &format!("(path {})", quote(first_path)),
                                &format!("(path \"/{RESISTOR}\")"),
                            )
                            .replace(
                                "(property \"Reference\" \"R2\"",
                                "(property \"Reference\" \"R1\"",
                            )
                    } else {
                        block.replace("(net \"/Channel 1/MID\")", "(net \"\")")
                    };
                    edits.push(SexpEdit::replace(start, end, changed));
                }
            }
            std::fs::write(&board, apply_edits(saved.to_owned(), edits)).unwrap();
            if mode == Served::Pcb {
                std::fs::write(
                    board.with_extension("kicad_dru"),
                    "(version 1)\n# A.NetName in this comment is not a condition\n\
                     (rule \"example\" (condition \"A.Type == 'Track'\") (constraint clearance (min 0.2mm)))\n",
                )
                .unwrap();
            }
        }
        let rules = board.with_extension("kicad_dru");
        if matches!(mode, Served::RulesWithoutPcb | Served::PcbPathsOnly) {
            std::fs::write(
                &rules,
                "(version 1)\n(rule \"example\" (condition \"A.NetName == '/Channel 1/MID'\"))\n",
            )
            .unwrap();
        }
        // This mock checks dispatch/transaction/readback, NOT KiCad semantics.
        let report = json!({"kicad_version":"10.0.7","sheets":[
            {"uuid_path":request.source_sheet_instance,"violations":[]},
            {"uuid_path":request.destination_sheet_instance,"violations":[]}
        ]});
        let netlist = format!(
            r#"<export><design><sheet tstamps="/"><title_block><source>component_fields_kicad10.kicad_sch</source></title_block></sheet>
                <sheet name="/Logic regulator/" tstamps="/{SHEET_UUID}/"><title_block><source>logic.kicad_sch</source></title_block></sheet></design>
                <components><comp ref="R1"><tstamps>{RESISTOR}</tstamps><sheetpath names="/Logic regulator/" tstamps="/{SHEET_UUID}/"/><property name="Sheetfile" value="logic.kicad_sch"/></comp></components><nets><net name="$net" class="Default"><node ref="R1" pin="1"/></net></nets></export>"#
        );
        let old_net = if mode == Served::PcbPathsOnly {
            "/not-on-the-board"
        } else {
            "/Channel 1/MID"
        };
        let command = format!(
            "#!/bin/sh\ntrigger=\"$(dirname \"$0\")/late-input\"\nif [ -f \"$trigger\" ]; then printf 'late input' > \"$(cat \"$trigger\")\" || exit 1; fi\nif [ \"$2\" = erc ]; then\ncat > \"$4\" <<'ERC'\n{report}\nERC\nelse\nnet='{old_net}'\nsheet=/\nif grep -q '{RESISTOR}' \"$(dirname \"$8\")/logic.kicad_sch\"; then net='/Logic regulator/MID'; sheet='/{SHEET_UUID}/'; fi\nif [ \"$7\" = kicadsexpr ]; then\ncat > \"$5\" <<PCB\n(export (components (comp (ref \"R1\") (sheetpath (tstamps \"$sheet\")) (tstamps \"{RESISTOR}\"))) (nets (net (name \"$net\") (node (ref \"R1\") (pin \"1\")))))\nPCB\nelse\ncat > \"$5\" <<NETLIST\n{netlist}\nNETLIST\nfi\nfi\n"
        );
        let script =
            cli::test_support::write_script(control.path(), "fake-transfer-cli", &command, "");
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: script.to_str().unwrap().into(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let call = |dry_run: bool, revision: Option<&str>| {
            let mut arguments = json!({
                "schematic":request.schematic,"source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,"object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,"offset_y_mm":request.offset_y_mm,"dry_run":dry_run
            });
            if let Some(revision) = revision {
                arguments["expected_plan_revision"] = json!(revision);
            }
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"move_schematic_objects_between_sheets", "arguments":arguments
            }})
        };
        let kind = |response: &Value| -> Value {
            let body: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            body["error"]["kind"].clone()
        };
        let before = snapshot(dir.path());
        let planned = handler
            .handle_message(call(true, None))
            .await
            .unwrap()
            .result
            .unwrap();
        let planned: Value =
            serde_json::from_str(planned["content"][0]["text"].as_str().unwrap()).unwrap();
        let revision = planned["plan_revision"].as_str().unwrap().to_owned();
        if matches!(mode, Served::RulesWithoutPcb | Served::PcbPathsOnly) {
            assert_eq!(planned["pcb_net_name_mapping"], json!({}));
            assert_eq!(planned["connectivity_impacts"][0]["old_name"], old_net);
            assert_eq!(snapshot(dir.path()), before);
            let response = handler
                .handle_message(call(false, Some(&revision)))
                .await
                .unwrap()
                .result
                .unwrap();
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["status"], "moved", "{result}");
            assert_eq!(std::fs::read(&rules).unwrap(), before[&rules]);
            if mode == Served::PcbPathsOnly {
                let saved = std::fs::read_to_string(&board).unwrap();
                assert!(saved.contains(&format!("(path \"/{SHEET_UUID}/{RESISTOR}\")")));
                assert!(saved.contains("(net \"/Channel 1/MID\")"));
            } else {
                assert!(!board.exists());
            }
            return;
        }
        if matches!(mode, Served::Pcb | Served::PcbInterrupt) {
            assert_eq!(before, snapshot(dir.path()));
            assert_eq!(
                planned["pcb_path_mapping"],
                json!({format!("/{RESISTOR}"): format!("/{SHEET_UUID}/{RESISTOR}")})
            );
            assert_eq!(
                planned["pcb_net_name_mapping"],
                json!({"/Channel 1/MID": "/Logic regulator/MID"})
            );
            if mode == Served::PcbInterrupt {
                let destination = dir.path().join("logic.kicad_sch");
                copperpour_sexp::writer::test_hooks::lock_before_replace(
                    &destination,
                    "late editor lock",
                );
                let response = handler
                    .handle_message(call(false, Some(&revision)))
                    .await
                    .unwrap()
                    .result
                    .unwrap();
                assert_eq!(kind(&response), "mutation_outcome_uncertain", "{response}");
                let mut active = copperpour_sexp::inspect_file_transactions(dir.path()).unwrap();
                assert_eq!(active.len(), 1);
                let transaction = active.pop().unwrap();
                assert_ne!(std::fs::read(&board).unwrap(), before[&board]);
                assert_eq!(std::fs::read(&destination).unwrap(), before[&destination]);
                let lock = copperpour_sexp::writer::kicad_editor_lock_path(&destination).unwrap();
                std::fs::remove_file(lock).unwrap();
                let rules = board.with_extension("kicad_dru");
                std::fs::write(&rules, "(version 1)\n").unwrap();
                let divergent = snapshot(dir.path());
                assert!(
                    copperpour_sexp::recover_file_transaction(dir.path(), &transaction.id).is_err()
                );
                assert_eq!(snapshot(dir.path()), divergent);
                std::fs::remove_file(rules).unwrap();
                copperpour_sexp::recover_file_transaction(dir.path(), &transaction.id).unwrap();
                assert_ne!(std::fs::read(&destination).unwrap(), before[&destination]);
                assert!(copperpour_sexp::inspect_file_transactions(dir.path())
                    .unwrap()
                    .is_empty());
            } else {
                let rules = board.with_extension("kicad_dru");
                std::fs::write(&rules, "(version 1)\n").unwrap();
                let changed = snapshot(dir.path());
                let stale = handler
                    .handle_message(call(false, Some(&revision)))
                    .await
                    .unwrap()
                    .result
                    .unwrap();
                assert_eq!(kind(&stale), "stale_target", "{stale}");
                assert_eq!(snapshot(dir.path()), changed);
                std::fs::write(&rules, &before[&rules]).unwrap();
                let response = handler
                    .handle_message(call(false, Some(&revision)))
                    .await
                    .unwrap()
                    .result
                    .unwrap();
                let result: Value =
                    serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(result["status"], "moved", "{result}");
                assert_eq!(std::fs::read(&rules).unwrap(), before[&rules]);
            }
            let saved = std::fs::read_to_string(&board).unwrap();
            let tree = parse_sexp(&saved).unwrap();
            let footprint = copperpour_sexp::board::footprints(&tree)[0];
            assert_eq!(footprint.find_str("sheetname"), Some("/Logic regulator/"));
            assert_eq!(footprint.find_str("sheetfile"), Some("logic.kicad_sch"));
            let original = std::str::from_utf8(&before[&board]).unwrap();
            let old_tree = parse_sexp(original).unwrap();
            let old_fp = copperpour_sexp::board::footprints(&old_tree)[0];
            let expected = original
                .replacen(
                    &format!("(path \"/{RESISTOR}\")"),
                    &format!("(path \"/{SHEET_UUID}/{RESISTOR}\")"),
                    1,
                )
                .replacen(
                    &format!(
                        "(sheetname {})",
                        quote(old_fp.find_str("sheetname").unwrap())
                    ),
                    "(sheetname \"/Logic regulator/\")",
                    1,
                )
                .replacen(
                    &format!(
                        "(sheetfile {})",
                        quote(old_fp.find_str("sheetfile").unwrap())
                    ),
                    "(sheetfile \"logic.kicad_sch\")",
                    1,
                )
                .replace("(net \"/Channel 1/MID\")", "(net \"/Logic regulator/MID\")");
            assert_eq!(saved, expected);
            return;
        }
        if mode == Served::LateSave {
            assert_late_save_is_preserved(
                &handler,
                call(false, Some(&revision)),
                dir.path(),
                &request,
                before,
            )
            .await;
            return;
        }
        if mode == Served::Interrupt {
            assert_interrupted_transfer_recovers(
                &handler,
                call(false, Some(&revision)),
                dir.path(),
                &request,
                before,
            )
            .await;
            return;
        }
        let trigger = control.path().join("late-input");
        for (extension, expected_kind, detail) in [
            (
                "kicad_pcb",
                "plan_blocked",
                "PCB appeared during verification",
            ),
            ("kicad_dru", "stale_target", "was not applied"),
        ] {
            let appeared = request.schematic.with_extension(extension);
            std::fs::write(&trigger, appeared.as_os_str().as_encoded_bytes()).unwrap();
            let response = handler
                .handle_message(call(false, Some(&revision)))
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(kind(&response), expected_kind, "{response}");
            assert!(
                response["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains(detail),
                "{response}"
            );
            let mut expected = before.clone();
            expected.insert(appeared.clone(), b"late input".to_vec());
            assert_eq!(expected, snapshot(dir.path()));
            std::fs::remove_file(appeared).unwrap();
        }
        std::fs::remove_file(trigger).unwrap();
        let without_cli = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: String::new(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let unverified = without_cli
            .handle_message(call(true, None))
            .await
            .unwrap()
            .result
            .unwrap();
        let unverified: Value =
            serde_json::from_str(unverified["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(unverified["verification"]["status"], "not_run");
        let refused = handler
            .handle_message(call(false, unverified["plan_revision"].as_str()))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(kind(&refused), "stale_target", "{refused}");
        assert_eq!(before, snapshot(dir.path()));
        for (revision, expected) in [(None, "invalid_argument"), (Some("stale"), "stale_target")] {
            let refused = handler
                .handle_message(call(false, revision))
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(refused["isError"], true, "{refused}");
            assert_eq!(kind(&refused), expected, "{refused}");
            assert_eq!(before, snapshot(dir.path()));
        }
        let destination = dir.path().join("logic.kicad_sch");
        let mut schematic = copperpour_schematic_editor::Schematic::load(&destination).unwrap();
        schematic.add_no_connect(50.8, 97.79);
        schematic.overwrite().unwrap();
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("Physical destination boundary"));
        let changed = snapshot(dir.path());
        let refused = handler
            .handle_message(call(false, Some(&revision)))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(kind(&refused), "stale_target", "{refused}");
        assert_eq!(changed, snapshot(dir.path()));
        std::fs::write(&destination, &before[&destination]).unwrap();
        for dry_run in [true, false] {
            let response = handler
                .handle_message(call(dry_run, (!dry_run).then_some(revision.as_str())))
                .await
                .unwrap()
                .result
                .unwrap();
            assert_ne!(response["isError"], true, "{response}");
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["outcome"]["status"], "complete");
            assert_eq!(result["plan_revision"], revision.as_str());
            if dry_run {
                assert_eq!(result["status"], "planned");
                assert_eq!(before, snapshot(dir.path()));
            } else {
                assert_eq!(result["status"], "moved");
                assert_ne!(before, snapshot(dir.path()));
                let root =
                    copperpour_schematic_editor::Schematic::load(&request.schematic).unwrap();
                assert!(root.symbols.iter().all(|symbol| symbol.uuid != RESISTOR));
                let destination = copperpour_schematic_editor::Schematic::load(
                    dir.path().join("logic.kicad_sch"),
                )
                .unwrap();
                assert_eq!(destination.symbols.iter().next().unwrap().uuid, RESISTOR);
            }
        }
    }

    #[cfg(unix)]
    async fn assert_late_save_is_preserved(
        handler: &crate::mcp::handler::McpHandler,
        call: Value,
        dir: &Path,
        request: &Request,
        before: BTreeMap<PathBuf, Vec<u8>>,
    ) {
        use copperpour_sexp::{
            inspect_file_transactions, recover_file_transaction, TransactionTargetState,
        };
        let destination = dir.join("logic.kicad_sch");
        let late = b"late save during apply".to_vec();
        // The writer publishes the source first; the save lands on the
        // destination after the journal is durable, before its rename.
        copperpour_sexp::writer::test_hooks::save_before_final_check(
            &destination,
            String::from_utf8(late.clone()).unwrap(),
        );
        let response = handler.handle_message(call).await.unwrap().result.unwrap();
        let result: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            result["error"]["kind"], "mutation_outcome_uncertain",
            "{result}"
        );
        assert_eq!(
            result["error"]["path"],
            copperpour_sexp::paths::display_path(&std::fs::canonicalize(&destination).unwrap())
        );
        assert_eq!(std::fs::read(&destination).unwrap(), late);
        assert_ne!(
            std::fs::read(&request.schematic).unwrap(),
            before[&request.schematic]
        );
        let active = inspect_file_transactions(dir).unwrap();
        assert_eq!(active.len(), 1);
        assert!(active[0].targets.iter().any(|target| {
            target.path == Path::new("logic.kicad_sch")
                && target.state == TransactionTargetState::Divergent
        }));
        let retained = snapshot(dir);
        assert!(recover_file_transaction(dir, &active[0].id).is_err());
        assert_eq!(snapshot(dir), retained);
    }

    #[cfg(unix)]
    async fn assert_interrupted_transfer_recovers(
        handler: &crate::mcp::handler::McpHandler,
        call: Value,
        dir: &Path,
        request: &Request,
        before: BTreeMap<PathBuf, Vec<u8>>,
    ) {
        use copperpour_sexp::{
            inspect_file_transactions, recover_file_transaction, TransactionTargetState,
        };
        let destination = dir.join("logic.kicad_sch");
        let lock = copperpour_sexp::writer::kicad_editor_lock_path(&destination).unwrap();
        // The writer publishes the source first. Stop the destination rename
        // after the real journal is durable, without a timing race.
        copperpour_sexp::writer::test_hooks::lock_before_replace(&destination, "late editor lock");
        let response = handler
            .handle_message(call.clone())
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(response["isError"], true, "{response}");
        let result: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            result["error"]["kind"], "mutation_outcome_uncertain",
            "{result}"
        );
        assert_eq!(result["outcome"]["status"], "uncertain", "{result}");
        let active = inspect_file_transactions(dir).unwrap();
        assert_eq!(active.len(), 1);
        let transaction = &active[0];
        assert_eq!(
            result["error"]["path"],
            copperpour_sexp::paths::display_path(&std::fs::canonicalize(&destination).unwrap())
        );
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains(&copperpour_sexp::paths::display_path(&transaction.journal)));
        for (path, state) in [
            (&request.schematic, TransactionTargetState::Applied),
            (&destination, TransactionTargetState::Pending),
        ] {
            assert!(transaction.targets.iter().any(|target| {
                target.path == Path::new(path.file_name().unwrap()) && target.state == state
            }));
        }
        assert_eq!(std::fs::read(&destination).unwrap(), before[&destination]);
        let published_source = std::fs::read(&request.schematic).unwrap();
        assert_ne!(published_source, before[&request.schematic]);

        std::fs::remove_file(lock).unwrap();
        let pending = snapshot(dir);
        for dry_run in [true, false] {
            let mut retry = call.clone();
            retry["params"]["arguments"]["dry_run"] = json!(dry_run);
            let response = handler.handle_message(retry).await.unwrap().result.unwrap();
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"]["kind"], "conflict", "{result}");
            assert_eq!(
                result["error"]["paths"],
                json!([copperpour_sexp::paths::display_path(&transaction.journal)])
            );
            assert_eq!(snapshot(dir), pending);
        }
        let board = request.schematic.with_extension("kicad_pcb");
        std::fs::write(&board, "new board blocks recovery").unwrap();
        let interrupted = snapshot(dir);
        assert!(recover_file_transaction(dir, &transaction.id).is_err());
        assert_eq!(snapshot(dir), interrupted);

        std::fs::remove_file(board).unwrap();
        let project_lock = copperpour_sexp::writer::kicad_project_lock_path(
            &request.schematic.with_extension("kicad_pro"),
        )
        .unwrap();
        std::fs::write(&project_lock, "late project lock").unwrap();
        let interrupted = snapshot(dir);
        assert!(recover_file_transaction(dir, &transaction.id).is_err());
        assert_eq!(snapshot(dir), interrupted);
        std::fs::remove_file(project_lock).unwrap();

        let project = request.schematic.with_extension("kicad_pro");
        let mut changed_project = before[&project].clone();
        changed_project.push(b'\n');
        std::fs::write(&project, changed_project).unwrap();
        let interrupted = snapshot(dir);
        assert!(recover_file_transaction(dir, &transaction.id).is_err());
        assert_eq!(snapshot(dir), interrupted);
        std::fs::write(&project, &before[&project]).unwrap();

        // A save after recovery's last guard check must survive.
        let interrupted = snapshot(dir);
        let late = b"late save during recovery".to_vec();
        copperpour_sexp::writer::test_hooks::save_before_final_check(
            &destination,
            String::from_utf8(late.clone()).unwrap(),
        );
        assert!(recover_file_transaction(dir, &transaction.id).is_err());
        let mut expected = interrupted.clone();
        expected.insert(destination.clone(), late);
        assert_eq!(snapshot(dir), expected);
        assert_eq!(inspect_file_transactions(dir).unwrap().len(), 1);
        std::fs::write(&destination, &before[&destination]).unwrap();

        assert_eq!(
            recover_file_transaction(dir, &transaction.id)
                .unwrap()
                .completed_files,
            1
        );
        assert!(inspect_file_transactions(dir).unwrap().is_empty());
        let after = snapshot(dir);
        assert!(after.keys().eq(before.keys()));
        assert_eq!(after[&request.schematic], published_source);
        let destination = parse_sexp(std::str::from_utf8(&after[&destination]).unwrap()).unwrap();
        assert!(destination
            .find_all("symbol")
            .into_iter()
            .any(|symbol| symbol.find_str("uuid") == Some(RESISTOR)));
        assert_eq!(after[&project], before[&project]);
    }

    #[tokio::test]
    async fn transfer_editor_locks_refuse_dry_run_and_apply_without_writes() {
        let (dir, request) = fixture();
        let project_lock = copperpour_sexp::writer::kicad_project_lock_path(
            &request.schematic.with_extension("kicad_pro"),
        )
        .unwrap();
        let sheet_lock =
            copperpour_sexp::writer::kicad_editor_lock_path(&dir.path().join("logic.kicad_sch"))
                .unwrap();
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: String::new(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        for (lock, kind) in [
            (project_lock, "unsafe_file_fallback"),
            (sheet_lock, "plan_blocked"),
        ] {
            std::fs::write(&lock, "test-owned editor lock").unwrap();
            let before = snapshot(dir.path());
            for dry_run in [true, false] {
                let call = json!({
                    "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                        "name":"move_schematic_objects_between_sheets","arguments":{
                            "schematic":request.schematic,
                            "source_sheet_instance":request.source_sheet_instance,
                            "destination_sheet_instance":request.destination_sheet_instance,
                            "object_uuids":request.object_uuids,
                            "offset_x_mm":request.offset_x_mm,"offset_y_mm":request.offset_y_mm,
                            "dry_run":dry_run,
                            // Reaches the lock check on apply; it refuses first.
                            "expected_plan_revision":"unchecked"
                        }
                    }
                });
                let response = handler.handle_message(call).await.unwrap().result.unwrap();
                assert_eq!(response["isError"], true, "{response}");
                let body: Value =
                    serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(body["error"]["kind"], kind, "{body}");
                if kind == "unsafe_file_fallback" {
                    assert_eq!(body["error"]["reason"], "kicad_project_lock_present");
                } else {
                    assert!(
                        body["message"]
                            .as_str()
                            .unwrap()
                            .contains("~logic.kicad_sch.lck"),
                        "{body}"
                    );
                }
                assert_eq!(before, snapshot(dir.path()));
            }
            std::fs::remove_file(lock).unwrap();
        }
    }

    #[test]
    fn transfer_erc_comparison_binds_transient_pin_ids_and_rejects_unknown_or_ambiguous_items() {
        let (_dir, request) = fixture();
        let plan = Plan::prepare(&request).unwrap();
        let before = ErcIdentityIndex::build(&plan, false).unwrap();
        let after = ErcIdentityIndex::build(&plan, true).unwrap();
        let item: cli::ReportItem = serde_json::from_value(json!({
            "description":"Symbol R1 [R], Pin 1 [~ Passive Line]", "uuid":"fresh-before",
            "pos":{"x":101.6,"y":72.39}
        }))
        .unwrap();
        let mut moved = item.clone();
        moved.uuid = Some("fresh-after".into());
        moved.pos.as_mut().unwrap().x -= 50.8;
        moved.pos.as_mut().unwrap().y += 25.4;
        assert_eq!(
            before.identity(&item).unwrap(),
            after.identity(&moved).unwrap()
        );
        moved.description = "Unidentified object".into();
        assert!(after.identity(&moved).is_err());
        let mut repeated = ErcIdentityIndex::build(&plan, false).unwrap();
        repeated.repeated.insert(RESISTOR.into());
        let mut saved = item.clone();
        saved.uuid = Some(RESISTOR.into());
        assert!(repeated.identity(&saved).is_err());
        let mut ambiguous = before;
        ambiguous.pins.push(ErcPinIdentity {
            identity: "different-symbol:1".into(),
            reference: "R1".into(),
            number: "1".into(),
            position: (101.6, 72.39),
            repeated: false,
        });
        assert!(ambiguous.identity(&item).is_err());
    }

    fn body(result: &CallToolResult) -> Value {
        let ToolContent::Text { text } = &result.content[0] else {
            panic!("text result")
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn transfer_group_membership_and_local_global_scope_collisions_are_explicit_refusals() {
        let (dir, request) = fixture();
        let source = std::fs::read_to_string(&request.schematic).unwrap();
        let close = source.rfind(')').unwrap();
        std::fs::write(
            &request.schematic,
            apply_edits(
                source,
                vec![SexpEdit::insert(
                    close,
                    format!("(group \"startup\" (id \"group-id\") (members \"{RESISTOR}\"))"),
                )],
            ),
        )
        .unwrap();
        let before = snapshot(dir.path());
        assert!(Plan::prepare(&request)
            .err()
            .unwrap()
            .to_string()
            .contains("Ungroup"));
        assert_eq!(before, snapshot(dir.path()));
        let tree=parse_sexp("(kicad_sch (lib_symbols) (label \"GND\" (at 0 0 0) (uuid selected)) (global_label \"GND\" (at 50 50 0) (uuid other)))").unwrap();
        assert!(check_boundary(&tree, &BTreeSet::from(["selected".into()]), true).is_err());
    }

    #[tokio::test]
    async fn transfer_served_dispatch_plans_without_cli_but_refuses_apply() {
        assert_missing_cli_plan(String::new()).await;
    }

    #[tokio::test]
    async fn transfer_refuses_a_custom_rules_directory_without_writes() {
        assert_non_regular_rules_refused(|rules| {
            std::fs::create_dir(rules).unwrap();
            std::fs::write(rules.join("keep.txt"), b"keep these bytes").unwrap();
        })
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_refuses_a_custom_rules_symlink_without_writes() {
        assert_non_regular_rules_refused(|rules| {
            let target = rules.with_file_name("linked-rules.kicad_dru");
            std::fs::write(&target, b"(version 1)\n").unwrap();
            std::os::unix::fs::symlink(target, rules).unwrap();
        })
        .await;
    }

    async fn assert_non_regular_rules_refused(set_up_rules: impl FnOnce(&Path)) {
        let (dir, request) = fixture();
        let rules = std::fs::canonicalize(&request.schematic)
            .unwrap()
            .with_extension("kicad_dru");
        set_up_rules(&rules);
        let before = snapshot(dir.path());
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: String::new(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"move_schematic_objects_between_sheets", "arguments":{
                "schematic":request.schematic,"source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,"object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,"offset_y_mm":request.offset_y_mm,
                "dry_run":true
            }
        }});
        let response = handler.handle_message(call).await.unwrap().result.unwrap();
        assert_eq!(snapshot(dir.path()), before, "{response}");
        assert!(copperpour_sexp::inspect_file_transactions(dir.path())
            .unwrap()
            .is_empty());
        assert_eq!(response["isError"], true, "{response}");
        let result: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
        let message = result["message"].as_str().unwrap();
        assert!(
            message.contains(&format!(
                "Custom rules '{}' must be a regular file",
                rules.display()
            )),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_refuses_dependent_project_net_rename_without_writes() {
        let (dir, request) = fixture();
        std::fs::write(
            request.schematic.with_extension("kicad_pro"),
            json!({"net_settings":{"netclass_assignments":{"/SIG":["Startup"]}}}).to_string(),
        )
        .unwrap();
        let control = tempfile::tempdir().unwrap();
        // Exercise the tool's decision with equal pin partitions but different
        // names, selecting the report from the actual staged schematic.
        let erc = json!({"sheets":[
            {"uuid_path":request.source_sheet_instance,"violations":[]},
            {"uuid_path":request.destination_sheet_instance,"violations":[]}
        ]});
        let cli = format!(
            r#"#!/bin/sh
if [ "$2" = erc ]; then
    cat > "$4" <<'ERC'
{erc}
ERC
else
    name=/SIG
    sheet=/
    if grep -q '{RESISTOR}' "$(dirname "$8")/logic.kicad_sch"; then
        name="/Logic regulator/SIG"
        sheet="/{SHEET_UUID}/"
    fi
    if [ "$7" = kicadsexpr ]; then
        cat > "$5" <<PCB
(export (components (comp (ref "R1") (sheetpath (tstamps "$sheet")) (tstamps "{RESISTOR}"))) (nets (net (name "$name") (node (ref "R1") (pin "1")))))
PCB
        exit
    fi
    cat > "$5" <<NETLIST
<export><design><sheet tstamps="/"><title_block><source>component_fields_kicad10.kicad_sch</source></title_block></sheet>
    <sheet tstamps="/{SHEET_UUID}/"><title_block><source>logic.kicad_sch</source></title_block></sheet></design>
    <components><comp ref="R1"><tstamps>{RESISTOR}</tstamps><sheetpath names="/Logic regulator/" tstamps="/{SHEET_UUID}/"/><property name="Sheetfile" value="logic.kicad_sch"/></comp></components>
    <nets><net name="$name" class="Default"><node ref="R1" pin="1"/></net></nets></export>
NETLIST
fi
"#,
        );
        let script = cli::test_support::write_script(control.path(), "dependent-net-cli", &cli, "");
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: script.to_str().unwrap().into(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let mut call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":OPERATION,"arguments":{
                "schematic":request.schematic,
                "source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,
                "object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,
                "offset_y_mm":request.offset_y_mm
            }
        }});
        for (dry_run, with_rules, expected) in [
            (true, false, "updating saved project net assignments"),
            (false, false, "updating saved project net assignments"),
            (false, true, "custom rules"),
        ] {
            if with_rules {
                std::fs::write(request.schematic.with_extension("kicad_pro"), "{}").unwrap();
                std::fs::write(
                    request.schematic.with_extension("kicad_pcb"),
                    format!("(kicad_pcb (footprint \"R\" (property \"Reference\" \"R1\") (path \"/{RESISTOR}\") (pad \"1\" (net \"/SIG\"))) (segment (net \"/SIG\")))"),
                )
                .unwrap();
                std::fs::write(
                    request.schematic.with_extension("kicad_dru"),
                    "(version 1)\n(rule \"example\" (condition \"A.(NetName) == '/SIG'\"))\n",
                )
                .unwrap();
            }
            let before = snapshot(dir.path());
            let revision = Plan::prepare(&request).unwrap().revision(&request);
            call["params"]["arguments"]["dry_run"] = json!(dry_run);
            if !dry_run {
                call["params"]["arguments"]["expected_plan_revision"] = json!(revision);
            }
            let response = handler
                .handle_message(call.clone())
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(response["isError"], true, "{response}");
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
            assert!(
                result["message"].as_str().unwrap().contains(expected),
                "{result}"
            );
            assert_eq!(before, snapshot(dir.path()));
        }
    }

    #[tokio::test]
    async fn transfer_served_dispatch_plans_when_configured_cli_is_missing() {
        let missing = tempfile::tempdir()
            .unwrap()
            .path()
            .join("missing-kicad-cli");
        assert_missing_cli_plan(missing.to_str().unwrap().into()).await;
    }

    async fn assert_missing_cli_plan(kicad_cli: String) {
        let (dir, request) = fixture();
        let before = snapshot(dir.path());
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli,
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let mut call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"move_schematic_objects_between_sheets", "arguments":{
                "schematic":request.schematic,"source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,"object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,"offset_y_mm":request.offset_y_mm
            }
        }});
        let response = handler
            .handle_message(call.clone())
            .await
            .unwrap()
            .result
            .unwrap();
        assert_ne!(response["isError"], true, "{response}");
        let report: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(report["verification"]["status"], "not_run");
        assert!(report["connectivity_impacts"].is_null());
        assert_eq!(report["affected_files"].as_array().unwrap().len(), 2);
        assert_eq!(before, snapshot(dir.path()));
        call["params"]["arguments"]["dry_run"] = json!(false);
        call["params"]["arguments"]["expected_plan_revision"] = report["plan_revision"].clone();
        let response = handler.handle_message(call).await.unwrap().result.unwrap();
        assert_eq!(response["isError"], true, "{response}");
        assert!(
            response["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("kicad-cli is required"),
            "{response}"
        );
        assert_eq!(before, snapshot(dir.path()));
        let error = blocked("boundary rewiring required");
        assert_eq!(body(&error)["error"]["kind"], "plan_blocked");
    }

    #[test]
    fn arguments_are_read_by_name_and_refused_when_unknown_missing_or_mistyped() {
        let valid = json!({
            "schematic": "/project/root.kicad_sch",
            "source_sheet_instance": format!("/{ROOT_UUID}"),
            "destination_sheet_instance": format!("/{ROOT_UUID}/{SHEET_UUID}"),
            "object_uuids": [RESISTOR],
            "offset_x_mm": 1.27,
            "offset_y_mm": -2.54
        });
        let request = Request::from_args(&valid).unwrap();
        assert!(request.dry_run);
        assert_eq!(request.expected_plan_revision, None);
        assert_eq!(request.object_uuids, vec![RESISTOR.to_owned()]);
        assert_eq!((request.offset_x_mm, request.offset_y_mm), (1.27, -2.54));

        let field = |args: Value| {
            let refusal = Request::from_args(&args).unwrap_err();
            body(&refusal)["error"]["field"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let mut unknown = valid.clone();
        unknown["offset_mm"] = json!(1);
        assert_eq!(field(unknown), "offset_mm");
        let mut mistyped = valid.clone();
        mistyped["dry_run"] = json!("false");
        assert_eq!(field(mistyped), "dry_run");
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("object_uuids");
        assert_eq!(field(missing), "object_uuids");
        for (name, value) in [
            ("object_uuids", json!(["not-a-uuid"])),
            ("source_sheet_instance", json!("a")),
            (
                "destination_sheet_instance",
                json!(format!("/{ROOT_UUID}/sheet")),
            ),
            ("offset_x_mm", json!(1e9)),
            ("schematic", json!("/project/root.kicad_pro")),
        ] {
            let mut malformed = valid.clone();
            malformed[name] = value;
            assert_eq!(field(malformed), name);
        }
    }

    /// Opt-in native acceptance: never treat absence of KiCad as a pass.
    #[tokio::test]
    #[ignore = "requires KiCad 10 kicad-cli; run with COPPERPOUR_TEST_KICAD_CLI configured"]
    async fn transfer_native_netlist_and_erc_acceptance() {
        let (dir, request) = fixture();
        assert_native_served_transfer(dir.path(), &request, "logic.kicad_sch", RESISTOR).await;
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.7 or newer; run with COPPERPOUR_TEST_KICAD_CLI configured"]
    async fn transfer_native_multi_root_netlist_and_erc_acceptance() {
        let (dir, request) = multi_root_fixture();
        assert_native_served_transfer(dir.path(), &request, "second.kicad_sch", ROOT_RESISTOR)
            .await;
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.7 or newer; run with COPPERPOUR_TEST_KICAD_CLI configured"]
    async fn transfer_native_subdirectory_sheet_netlist_and_erc_acceptance() {
        let destination = "nested/logic.kicad_sch";
        let (dir, request) = fixture_with_destination(destination);
        assert_native_served_transfer(dir.path(), &request, destination, RESISTOR).await;
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.8; run with COPPERPOUR_TEST_KICAD_CLI configured"]
    async fn transfer_native_refuses_inherited_dnp_without_writes() {
        let (dir, request) = fixture_with_sheet_settings("logic.kicad_sch", true);
        let cli = std::env::var("COPPERPOUR_TEST_KICAD_CLI")
            .expect("COPPERPOUR_TEST_KICAD_CLI is required");
        let plan = Plan::prepare(&request).unwrap();
        let before = snapshot(dir.path());
        let error = verify_with_kicad(&plan, &cli).await.unwrap_err();
        assert!(
            error.to_string().contains("effective component settings"),
            "{error:#}"
        );
        assert_eq!(snapshot(dir.path()), before);
    }

    fn add_transfer_signal(request: &mut Request, signal: &str) {
        use copperpour_sexp::schematic::{format_net_label, format_wire};

        let wire = format_wire(101.6, 72.39, 114.3, 72.39);
        let label = format_net_label(signal, 114.3, 72.39, 0.0);
        for block in [&wire, &label] {
            let node = parse_sexp(block).unwrap();
            request
                .object_uuids
                .push(node.find_str("uuid").unwrap().to_owned());
        }
        let original = std::fs::read_to_string(&request.schematic).unwrap();
        let end = original.rfind(')').unwrap();
        std::fs::write(
            &request.schematic,
            apply_edits(
                original,
                vec![SexpEdit::insert(end, format!("\n{wire}\n{label}\n"))],
            ),
        )
        .unwrap();
    }

    fn native_routed_board(
        request: &Request,
        symbol_uuid: &str,
        path: &str,
        seed_signal: bool,
    ) -> PathBuf {
        let python = std::env::var("COPPERPOUR_TEST_PCBNEW_PYTHON")
            .expect("COPPERPOUR_TEST_PCBNEW_PYTHON must name Python with pcbnew installed");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/repeat_layout_routing_kicad10.kicad_pcb");
        let board = request.schematic.with_extension("kicad_pcb");
        // SaveBoard can save project settings as well. Author the board in
        // isolation so it cannot replace the schematic fixture's root list.
        let staging = tempfile::tempdir().unwrap();
        let staged_board = staging.path().join(board.file_name().unwrap());
        let script = r#"
import pcbnew as p
import sys
b = p.LoadBoard(sys.argv[1])
fp = next(fp for fp in b.GetFootprints() if fp.GetReference() == 'R1')
fp.SetPath(p.KIID_PATH(sys.argv[3]))
if sys.argv[4] == 'yes':
    name = '/Channel 1/MID'
    target = p.NETINFO_ITEM(b, '/A{slash}B')
    b.Add(target)
    connected = list(b.GetTracks())
    assert any(item.GetNetname() == name for item in connected)
    for item in connected:
        if item.GetNetname() == name:
            item.SetNet(target)
    fp.FindPadByNumber('1').SetNet(target)
    graphic = p.PCB_SHAPE(b)
    graphic.SetShape(p.SHAPE_T_SEGMENT)
    graphic.SetStart(p.VECTOR2I(p.FromMM(75), p.FromMM(75)))
    graphic.SetEnd(p.VECTOR2I(p.FromMM(76), p.FromMM(75)))
    graphic.SetWidth(p.FromMM(0.25))
    graphic.SetLayer(p.F_Cu)
    graphic.SetNet(target)
    b.Add(graphic)
assert p.SaveBoard(sys.argv[2], b)
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .arg(fixture)
            .arg(&staged_board)
            .arg(format!("{path}/{symbol_uuid}"))
            .arg(if seed_signal { "yes" } else { "no" })
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::copy(staged_board, &board).unwrap();
        board
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.8 CLI and pcbnew Python"]
    async fn transfer_native_routed_board_net_and_path_acceptance() {
        let (dir, mut request) = fixture_with_destination("nested/logic.kicad_sch");
        add_transfer_signal(&mut request, "A/B");
        let board = native_routed_board(&request, RESISTOR, "", true);
        let before = std::fs::read_to_string(&board).unwrap();
        assert!(before.contains("(net \"/A{slash}B\")"));
        assert_native_served_transfer(dir.path(), &request, "nested/logic.kicad_sch", RESISTOR)
            .await;
        let after = std::fs::read_to_string(&board).unwrap();
        let old = format!("/{RESISTOR}");
        let new = format!("/{SHEET_UUID}/{RESISTOR}");
        let tree = parse_sexp(&after).unwrap();
        let moved = copperpour_sexp::board::footprints(&tree)
            .into_iter()
            .find(|fp| fp.find_str("path") == Some(&new))
            .unwrap();
        assert_eq!(moved.find_str("sheetfile"), Some("nested/logic.kicad_sch"));
        assert_eq!(moved.find_str("sheetname"), Some("/Logic regulator/"));
        assert_board_changes(
            &before,
            &after,
            &old,
            &new,
            Some("/Logic regulator/A{slash}B"),
        );
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.8 CLI and pcbnew Python"]
    async fn transfer_native_routed_board_declared_multi_root_acceptance() {
        for rooted in [false, true] {
            let (dir, request) = multi_root_fixture();
            let source = if rooted {
                request.source_sheet_instance.as_str()
            } else {
                ""
            };
            let destination = if rooted {
                request.destination_sheet_instance.as_str()
            } else {
                ""
            };
            let board = native_routed_board(&request, ROOT_RESISTOR, source, false);
            let before = std::fs::read_to_string(&board).unwrap();
            assert_native_served_transfer(dir.path(), &request, "second.kicad_sch", ROOT_RESISTOR)
                .await;
            let after = std::fs::read_to_string(&board).unwrap();
            let old = format!("{source}/{ROOT_RESISTOR}");
            let new = format!("{destination}/{ROOT_RESISTOR}");
            assert_board_changes(&before, &after, &old, &new, None);
        }
    }

    fn assert_board_changes(before: &str, after: &str, old: &str, new: &str, net: Option<&str>) {
        let parsed = parse_sexp(after).unwrap();
        let footprint = copperpour_sexp::board::footprints(&parsed)
            .into_iter()
            .find(|fp| fp.find_str("path") == Some(new))
            .unwrap();
        let prior = parse_sexp(before).unwrap();
        let old_fp = copperpour_sexp::board::footprints(&prior)
            .into_iter()
            .find(|fp| fp.find_str("path") == Some(old))
            .unwrap();
        assert_eq!(footprint.find("uuid"), old_fp.find("uuid"));
        assert_eq!(footprint.find("at"), old_fp.find("at"));
        if let Some(net) = net {
            let old_graphic = prior
                .find_all("gr_line")
                .into_iter()
                .find(|shape| shape.find_str("net") == Some("/A{slash}B"))
                .unwrap();
            let new_graphic = parsed
                .find_all("gr_line")
                .into_iter()
                .find(|shape| shape.find_str("net") == Some(net))
                .unwrap();
            for field in ["uuid", "start", "end", "width", "layer"] {
                assert_eq!(old_graphic.find(field), new_graphic.find(field));
            }
        }
        assert_ne!(
            footprint.find_str("sheetname"),
            old_fp.find_str("sheetname")
        );
        assert_ne!(
            footprint.find_str("sheetfile"),
            old_fp.find_str("sheetfile")
        );
        let python = std::env::var("COPPERPOUR_TEST_PCBNEW_PYTHON").unwrap();
        let owned = tempfile::tempdir().unwrap();
        let path = owned.path().join("after.kicad_pcb");
        std::fs::write(&path, after).unwrap();
        let output = std::process::Command::new(python)
            .args([
                "-c",
                r#"
import pcbnew as p
import sys
b = p.LoadBoard(sys.argv[1])
fp = next(fp for fp in b.GetFootprints() if fp.GetReference() == 'R1')
assert fp.GetPath().AsString() == sys.argv[2]
if sys.argv[3]:
    assert b.FindNet(sys.argv[3])
    code = b.FindNet(sys.argv[3]).GetNetCode()
    tracks = list(b.GetTracks())
    assert any(t.GetNetCode() == code for t in tracks), \
        (sys.argv[3], code, sorted({(t.GetNetCode(), t.GetNetname()) for t in tracks}))
    assert any(pad.GetNetCode() == code for pad in fp.Pads()), \
        [(pad.GetNumber(), pad.GetNetCode(), pad.GetNetname()) for pad in fp.Pads()]
"#,
            ])
            .arg(path)
            .arg(new)
            .arg(net.unwrap_or(""))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "requires KiCad 10.0.8; run with COPPERPOUR_TEST_KICAD_CLI configured"]
    async fn transfer_native_refuses_changed_effective_netclass_without_writes() {
        let (dir, mut request) = fixture();
        add_transfer_signal(&mut request, "TransferSignal");
        let stock: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/netclass_diff_pair_kicad10.kicad_pro"
        ))
        .unwrap();
        let mut settings = json!({"net_settings":stock["net_settings"]});
        let project = request.schematic.with_extension("kicad_pro");
        let cli = std::env::var("COPPERPOUR_TEST_KICAD_CLI")
            .expect("COPPERPOUR_TEST_KICAD_CLI is required");
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: cli,
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":OPERATION,"arguments":{
                "schematic":request.schematic,
                "source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,
                "object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,
                "offset_y_mm":request.offset_y_mm
            }
        }});
        for changes_class in [false, true] {
            settings["net_settings"]["netclass_patterns"] = if changes_class {
                json!([{"netclass":"USB","pattern":"/TransferSignal"}])
            } else {
                json!([])
            };
            std::fs::write(&project, serde_json::to_vec(&settings).unwrap()).unwrap();
            let before = snapshot(dir.path());
            let response = handler
                .handle_message(call.clone())
                .await
                .unwrap()
                .result
                .unwrap();
            let result: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            if changes_class {
                assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
                assert!(
                    result["message"]
                        .as_str()
                        .unwrap()
                        .contains("or effective netclasses"),
                    "{result}"
                );
            } else {
                assert_eq!(result["verification"]["status"], "passed", "{result}");
                assert!(
                    result["connectivity_impacts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["old_name"] == "/TransferSignal"),
                    "{result}"
                );
            }
            assert_eq!(snapshot(dir.path()), before);
        }
    }

    async fn assert_native_served_transfer(
        dir: &Path,
        request: &Request,
        destination: &str,
        symbol_uuid: &str,
    ) {
        let cli = std::env::var("COPPERPOUR_TEST_KICAD_CLI")
            .expect("COPPERPOUR_TEST_KICAD_CLI is required");
        let before = snapshot(dir);
        let exports = tempfile::tempdir().unwrap();
        let netlist = exports.path().join("observed.net");
        // Native export may create a .kicad_prl. Keep independent verification
        // from touching the project whose byte-for-byte changes we assert.
        let export = |files: &BTreeMap<PathBuf, Vec<u8>>| {
            let copy = tempfile::tempdir().unwrap();
            for (path, bytes) in files {
                let target = copy.path().join(path.strip_prefix(dir).unwrap());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, bytes).unwrap();
            }
            copy
        };
        let initial = export(&before);
        let entry = request.schematic.file_name().unwrap();
        cli::export_netlist(&cli, &initial.path().join(entry), &netlist, "kicadxml")
            .await
            .unwrap();
        let prior = parse_netlist(&std::fs::read_to_string(&netlist).unwrap()).unwrap();
        let handler = crate::mcp::handler::McpHandler::new(super::super::ServerConfig {
            kicad_cli: cli.clone(),
            ipc_address: String::new(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let mut call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":OPERATION,"arguments":{
                "schematic":request.schematic,
                "source_sheet_instance":request.source_sheet_instance,
                "destination_sheet_instance":request.destination_sheet_instance,
                "object_uuids":request.object_uuids,
                "offset_x_mm":request.offset_x_mm,
                "offset_y_mm":request.offset_y_mm
            }
        }});
        let decode = |result: Value| -> Value {
            assert_ne!(result["isError"], true, "{result}");
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
        };
        let dry_run = decode(
            handler
                .handle_message(call.clone())
                .await
                .unwrap()
                .result
                .unwrap(),
        );
        assert_eq!(dry_run["verification"]["status"], "passed");
        assert_eq!(before, snapshot(dir));
        call["params"]["arguments"]["dry_run"] = json!(false);
        call["params"]["arguments"]["expected_plan_revision"] = dry_run["plan_revision"].clone();
        let applied = decode(handler.handle_message(call).await.unwrap().result.unwrap());
        assert_eq!(applied["status"], "moved");
        let after = snapshot(dir);
        let changed = before
            .keys()
            .filter(|path| before[*path] != after[*path])
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        let mut expected = BTreeSet::from([request.schematic.clone(), dir.join(destination)]);
        let board = request.schematic.with_extension("kicad_pcb");
        if before.contains_key(&board) {
            expected.insert(board);
        }
        assert_eq!(changed, expected);
        let source = copperpour_schematic_editor::Schematic::load(&request.schematic).unwrap();
        let dest = copperpour_schematic_editor::Schematic::load(dir.join(destination)).unwrap();
        assert!(source.symbols.iter().all(|s| s.uuid != symbol_uuid));
        let moved = dest.symbols.iter().find(|s| s.uuid == symbol_uuid).unwrap();
        assert_eq!(
            moved.instances()[0].path.as_deref(),
            Some(request.destination_sheet_instance.as_str())
        );
        std::fs::remove_file(&netlist).unwrap();
        let saved = export(&after);
        cli::export_netlist(&cli, &saved.path().join(entry), &netlist, "kicadxml")
            .await
            .unwrap();
        let persisted = parse_netlist(&std::fs::read_to_string(&netlist).unwrap()).unwrap();
        assert_eq!(prior, persisted);
    }
}
