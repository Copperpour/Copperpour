# Tool reliability contract

Apply this contract to tool calls and to new or changed tool behavior.
See the migration inventory for tracked exceptions and repository governance
for merge requirements.

## For callers: establish what happened

1. Bind the requested document and identify the source of each relevant result:
   live editor, saved file, or a stated mixture. Await state-changing prerequisites
   before dependent checks; issuing save and DRC concurrently does not order them.
2. Inspect the complete response, including errors, warnings, per-item results and
   coverage. A successful transport or `isError: false` alone does not establish
   that every requested item was applied or every required check ran.
3. Verify the actual outcome using readback and the relevant independent evidence
   (for example netlist, ERC/DRC, or rendered inspection). Compare with intent;
   echoing the request is not evidence that the operation applied it.
4. Classify incomplete work before continuing. If a response lacks outcome or
   source information, inspect through available tools or report that uncertainty.
   Use the fields the installed version actually provides; the categories below
   are meanings, not a newly guaranteed response schema.
5. Recover according to observed state. Retry only unapplied items of a partial
   batch. After a timeout or uncertain write, reload and inspect before deciding
   what to repeat. A refusal that proves nothing changed permits correcting the
   request and retrying. An error after a write does not prove rollback.

| Outcome | Required interpretation |
|---|---|
| Complete | The requested operation and relevant verification completed. |
| Partial | Identify applied, failed and unattempted items or checks; completion remains incomplete. |
| Refused/failed before mutation | State the cause and evidence that nothing was applied. |
| Uncertain after possible mutation | Preserve the target and diagnostic; inspect before retrying. |
| Check unavailable | Report missing evidence; do not substitute zero findings or a pass. |

Separate execution from engineering findings: a completed DRC run can report
violations, and a valid search can return no matches. Both differ from a check
that could not run or a result that could not be decoded. Required unavailable
evidence makes a design verdict incomplete; it does not automatically block an
unrelated contribution.

## For contributors: describe the changed behavior

For a PR changing tool behavior, complete the behavioral table in the PR template.
Use short answers for the affected tools; mark a row not applicable with a reason.
Pure documentation or mechanical changes need no invented runtime test matrix.

- **Inputs:** defaults apply to omitted options or explicitly documented null.
  Reject malformed present values with a structured error naming the field.
  Validate ranges before narrowing conversions. Treat integral JSON numbers
  consistently across equivalent arguments; never silently truncate fractions.
- **State:** identify the exact target, prerequisites and live/saved data sources.
  Disclose fallback and mixed-source results. Reuse existing target, transaction,
  readback and error helpers; introduce a shared helper when it gives a repeated
  invariant one owner.
- **Mutation:** preflight what can be checked before writing. Preserve unrelated
  objects and content. State atomic or partial batch semantics explicitly;
  preserve compatibility or document the migration. Report any applied work
  when a later step fails.
- **Evidence:** derive completion fields from observed results. Unsupported or
  malformed evidence is not a valid empty result. Preserve useful error context
  and stable error kinds for caller recovery.
- **Recovery:** distinguish refusal from uncertain persistence. Give the caller
  enough identity and applied-item information to avoid repeating successful work.

Review newly introduced suppression at boundaries: ignored persistence results,
error-to-default conversions, discarded row errors, unchecked casts, and requested
values presented as proof. These are review signals, not blanket bans on
`let _ =`, `unwrap_or_default`, or `filter_map`. Optional lookups and cleanup
can legitimately continue; explain the fallback and expose material degradation.
Do not claim all-or-nothing behavior when the backend cannot provide it.

## Bounded migration inventory

The [machine-checked inventory](reliability-legacy-inventory.json) lists tools
using the shared `outcome` envelope and remaining exceptions with their scope
and removal criteria. Reuse the shared outcome type for focused migrations with
their own compatibility and failure evidence.

## Proportionate evidence

Provide an ordinary success case and the failures material to the changed
behavior. Mutation coverage includes unchanged state on refusal and evidence
that a successful operation actually applied its intended changes. Test a
post-write failure when it is a credible path, including truthful recovery.

Contract tests exercise the served dispatch as well as direct internal paths
where applicable. Use KiCad-authored fixtures for design parsing and mutation.
Expected results must be independent of the implementation under test: call
readback and check relevant external semantics, not just the writer's calculation.

A new guard is pinned by a test that fails without it. The reviewer checks
this by neutralizing the guard ([GOVERNANCE.md](../GOVERNANCE.md#evidence));
authors need not run or record negative controls. Every operating system on
every PR is not required. Hosted CI and material safety evidence remain
mandatory. Name secondary environments not observed and
use the existing validation-debt process.

## Adoption and enforcement

These are review requirements for new and changed behavior. Fixing one tool
does not require migrating every other handler first.

The dispatch compiles, caches, and enforces every advertised tool schema before
the handler runs. Handlers still own domain rules and checked direct-call paths.
CI checks that adopted tools serve the envelope through JSON-RPC `tools/call`
and that each legacy entry names existing paths, rationale, tracking issues, and
removal criteria. Lower the ceiling when removing a legacy entry. Adding an
entry requires an explicit, reviewed guard change.

Contributors may use any AI or none. This contract specifies outcomes, not
model selection.

## Maintainer references

- [Governance](../GOVERNANCE.md)
- [Contributing](../CONTRIBUTING.md)
- [Branch workflow](BRANCH_AND_PULL_REQUEST_WORKFLOW.md)

The installer embeds this contract as `references/reliability-contract.md`
for offline use. Repository links refer to current maintainer policy.
