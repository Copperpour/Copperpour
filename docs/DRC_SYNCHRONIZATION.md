# DRC source synchronization ([#58](https://github.com/Copperpour/Copperpour/issues/58))

Both DRC tools share `tools::drc`, which owns source selection and execution
ordering. Their existing summary/filter formats remain separate. `cli::run_drc`
continues to own KiCad report parsing, parity and UUID ownership enrichment.

```json
{"board":"/absolute/path/clock.kicad_pcb","sync_live_board":true,"refill_zones":true}
```

Finish placement/routing/deletion calls before this request. Do not launch more
mutations or saves concurrently: this is a synchronization barrier, not an
editor-wide transaction/lock. The same exact-target IPC binding is reobserved
before each operation; another open board is never substituted.

| Case | Behavior / evidence |
|---|---|
| Options omitted | Saved-file CLI DRC; no IPC/save; synchronization and refill false. |
| `refill_zones` only | CLI fills for analysis; `zone_refill_source: kicad_cli`; live state is not saved. |
| `sync_live_board` | Exact target -> save -> native snapshot/persisted comparison -> CLI -> saved-source stability check. |
| Both options | Exact target -> refill -> read-only readiness barrier -> save -> verification -> CLI. One refill and one save, no mutation retries. |
| Wrong/ambiguous/stale target before mutation | Existing structured target error; no other document touched and no CLI invocation. |
| Unavailable/rejected initial IPC observation | `editor_unavailable`; no mutation and no CLI invocation. |
| Failed refill/save or unprovable persistence | `mutation_outcome_uncertain`; stop before CLI; inspect/reconcile editor and file before retry. |
| CLI failure after save or source changed during DRC | No accepted DRC result; structured uncertainty with recovery instructions. Already applied work is not replayed. |
| Wrong-typed/unknown options | Served schema validation rejects before the handler; no write. |
| Optional report output | Canonical source-board aliases and obvious directory/non-directory-ancestor destinations are refused before mutation. Reports publish through atomic sibling-file replacement, preserving the source board behind distinct hard-link paths. Later publication failure preserves the completed `source_evidence` receipt and board path, and tells the caller not to repeat refill/save. |
| Standalone refill | Exact target -> refill -> readiness; success does not mean saved. Failure is an MCP error, not a success containing `success:false`. |

KiCad explicitly acknowledges `RefillZones` immediately and returns `AS_BUSY`
to subsequent requests while filling. Only that typed status is retried (read
requests, never refill/save). The readiness polling budget is 60 seconds, with
50 ms between busy responses; a pending transport receive retains the client's
existing 30-second per-request timeout. Other errors fail immediately.

Native snapshot comparison uses parsed S-expressions, not byte formatting or
requested values echoed as proof. Only direct footprint `version`, `generator`
and `generator_version` metadata are normalized: KiCad's IPC serializer includes
standalone-footprint metadata which the saved board omits. Board version and all
geometry, connectivity, model and UUID fields remain checked. The saved bytes are
also checked after CLI.
This detects a changed final source, but cannot prove the absence of transient
concurrent changes; callers must finish their mutations first. General live/file
authority and the shared outcome-envelope migration remain tracked separately
in [#58](https://github.com/Copperpour/Copperpour/issues/58) and
[#16](https://github.com/Copperpour/Copperpour/issues/16); this synchronization does
not close
those larger contracts.

## Evidence

Unit tests in `tools::drc::tests` use the served `tools/call` dispatch, the shared
lifecycle-owned NNG mock, native KiCad fixtures, and a CLI stand-in which copies
the input it actually sees. They cover both DRC surfaces, exact two-board
binding, busy completion, failed save, unproven persistence, file-only control,
standalone refill and a readiness deadline without replay.

The ignored live regression requires a **disposable copy**, already open in
KiCad, with a named net. It creates/saves a via, deletes it without saving,
proves file-only DRC still reports its UUID, adds an unfilled native copper zone,
and proves both synchronized tools remove the UUID and compute actual copper
after refill/save. The fixture needs a line-based board outline and named-net
copper inside that outline. Set `KONNECT_LIVE_KICAD_BOARD`, `KICAD_API_SOCKET`,
and `KONNECT_TEST_KICAD_CLI`, then run:

```text
cargo test -p konnect-core --locked --lib live_drc_drops_deleted_via_uuid_after_refill_and_save -- --ignored --nocapture --test-threads=1
```

Never point that test at a tracked fixture or working project. Tests do not own
the GUI; the operator must close the disposable KiCad instance afterwards.

Local evidence: Windows, KiCad 10.0.6, disposable native J1 ownership fixture:
the live stale-UUID regression passed for both DRC entry points, including IPC
refill completion and verified save. This does not establish macOS/Linux live
compatibility; those observations remain community validation debt.
