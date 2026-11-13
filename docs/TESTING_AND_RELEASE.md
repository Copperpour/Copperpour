# Testing And Release

The pull-request baseline is defined in `CONTRIBUTING.md`. Run the same commands
locally when the required platform dependencies are available:

```bash
cargo test --workspace --locked --lib --tests
cargo test --workspace --locked --doc
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

`protoc` and its well-known type includes are required by
`crates/copperpour-ipc/build.rs`.

## Coverage Map

| Area | Source of tests |
|---|---|
| MCP protocol, CLI, and asset contracts | `crates/copperpour/tests` and `crates/copperpour/src` unit tests |
| Router and toolset invariants | `crates/copperpour-core/src/router` tests |
| Dispatch and required-argument behavior | `crates/copperpour-core/src/mcp/handler.rs` tests |
| Domain handlers and evidence rules | tests beside modules in `crates/copperpour-core/src/tools` |
| S-expression parsing and atomic writes | `crates/copperpour-sexp` tests |
| Typed schematic model | `crates/copperpour-schematic-editor` tests |
| IPC builders, transport, and client behavior | `crates/copperpour-ipc` tests |
| Real KiCad behavior | ignored live tests and local CI's real-KiCad job (`e2e-kicad.yml`'s steps) |
| Viewer engine and browser service | `crates/copperpour-viewer` engine and HTTP/SSE tests, included in workspace checks |
| Python plugin | `plugin/tests` and the plugin CI job |
| PCM package | `packaging/validate-pcm.py` and packaging CI jobs |

Run the engine tests without GUI prerequisites using
`cargo test -p copperpour-viewer --locked`. Workspace commands compile the
embedded frontend assets along with the engine/service, but do not execute their
JavaScript. Browser visual/CSP execution checks and native KiCad-renderer
smoke tests are separate evidence; HTTP header tests do not substitute for them.

## Evidence-Focused Regression Tests

When a tool returns a count, success state, or verdict, test the evidence behind
that field. The v0.7 reference cases include:

- complete DRC category parsing in `tools/cli.rs`;
- DRC-backed review/readiness decisions in `tools/design_review.rs` and
  `tools/manufacturing.rs`;
- footprint-type discrimination and post-commit read-back in
  `tools/pcb_sync.rs` and `copperpour-ipc/src/builders.rs`;
- closed-board placement and flip refusal cases in `tools/pcb_components.rs`.

Use real KiCad-generated fixtures for formats KiCad owns. For an IPC path, unit
tests should prove request construction and failure classification; an ignored
live test or the end-to-end workflow should prove behavior that depends on a
running editor.

IPC doubles in `copperpour-core` must use the shared `test_support::MockIpcServer`.
The returned guard owns an already-listening, process-unique `inproc://`
endpoint and joins its worker when dropped. Retain that guard for the whole
test. Do not discover a free TCP port by binding it, dropping the listener, and
then asking NNG to bind the same port: another process can claim the port in
between, making otherwise deterministic tests fail with `AddressInUse`.

`copperpour-ipc`'s integration tests cannot reach that type; they use
`tests/mock_kicad`, the same guard. Neither has a receive timeout, so a double
keeps answering however long a loaded runner idles before its first request.

The Specctra import undo boundary has a manual live gate because KiCad IPC can
create a named commit but cannot invoke the editor's Undo action. Open a
disposable copy of
`crates/copperpour-core/tests/fixtures/specctra_two_resistors_locked.kicad_pcb`,
set `COPPERPOUR_LIVE_SPECCTRA_BOARD`, `KICAD_API_SOCKET`, and (when it is not on
`PATH`) `KICAD_CLI_PATH`, set `FREEROUTING_JAR` to the local Freerouting 2.3.0
JAR, then run:

```text
cargo test -p copperpour-core --locked one_undo_restores_the_exact_pre_import_board_snapshot -- --ignored --nocapture
```

When the test prints `LIVE_UNDO_READY`, press Ctrl+Z once in PCB Editor. The
test passes only when the exact pre-import IPC snapshot returns.

## Zone deletion live check

Open a disposable board with at least two copper zones in KiCad and enable its
IPC API. Set `COPPERPOUR_ZONE_TEST_BOARD` to that copy and
`COPPERPOUR_ZONE_TEST_IPC_ADDRESS` to the endpoint serving it, then run:

```text
cargo test -p copperpour-core --locked live_zone_delete_on_configured_disposable_board --lib -- --ignored --nocapture
```

This deletes one observed zone, waits through KiCad's asynchronous refill, and
requires stable live readback with the target absent, other zones preserved,
and remaining copper reported filled. It does not invoke Undo. On KiCad 10.0.5,
refill and deletion produce separate undo entries: undo the refill, then the
deletion. Verify that the pre-delete board state returns, save the disposable
board, and check native readback. The Windows acceptance run also compared the
complete pre-delete and post-Undo IPC snapshot hashes and independently checked
saved zones, footprints/pads, tracks/vias, and drawing identities with pcbnew.

## CI And Live Validation

CI runs on a maintainer's machine, not on GitHub Actions
([GOVERNANCE.md](../GOVERNANCE.md#where-ci-runs)). It runs `.github/workflows/ci.yml`'s
jobs on every PR: the Rust workspace, formatting, clippy, documentation tests,
viewer, plugin, Nix, and PCM validation. The real-KiCad job runs
`e2e-kicad.yml`'s steps against KiCad 10.0.5 in a Windows VM. It is not a per-PR
check, because it takes over half an hour. Ask a maintainer to run it on a pull
request when KiCad-facing behavior needs that evidence. The `run:e2e-kicad`
label no longer triggers anything.

Before creating a release tag, run `cargo xtask collect-api-migrations
<version>` and `cargo xtask fix-doc-counts`, and commit the result. The first
moves the notes in `docs/api-migrations/` into `docs/API_MIGRATIONS.md`; the
second rewrites the tool counts in the docs, which PRs leave alone. Then a
maintainer runs local CI's release job against the exact candidate ref, with a
synthetic `0.0.0` version. It runs the `doc-counts` check, which fails on a
stale count, then builds the Linux x86_64 and Windows release binaries and PCM
packages, validates both packages and runs the real-KiCad job. It publishes
nothing.

For the tag, the same job builds the release artifacts from the tagged commit.
The macOS ones come from GitHub: pushing the tag runs `macos.yml`, which runs
Check & Test on macOS and builds the `x86_64-apple-darwin` and `aarch64-apple-darwin` archives and the universal
macOS PCM package, and validates the package. It publishes nothing. Download
its artifacts with `gh run download <run-id>` next to local CI's, merge the
three PCM zips with `packaging/merge-pcm.py`, run
`packaging/pcm-submission-metadata.py` on the merged zip, and publish all of
them in one release. A failure in any build, on either side, or in the
real-KiCad job means the release is not cut, so no partial set of assets is
published. To check the macOS build before tagging, run `gh workflow run
macos.yml --ref <candidate>` beside local CI's dry run. Nothing else runs
macOS: PRs and `main` are not tested on it. Linux aarch64, which `release.yml` built on
GitHub, is not built.

In the PR description, list every command run and explicitly name checks skipped
because they require KiCad, another operating system, credentials, or release
infrastructure.

## Documentation

For a public behavior change, inspect README, `DEV.md`, `tool-directory.md`,
these developer maps, and bundled guidance under
`crates/copperpour/assets/skills` and `assets/agents`. Tool-count locations and
their enforcement are defined by `CONTRIBUTING.md` and
`crates/copperpour/tests/doc_tool_counts.rs`; link to the authoritative catalogue
instead of copying totals into new documents.

Every behavioral claim in these maps should name its source module. When the
module changes, a contributor can find the claim by searching for the old path.
Describe unimplemented future behavior explicitly as design intent rather than
current capability.

## Packaging And Release

Build the server with `cargo build --release -p copperpour`. Build the viewer
separately when it is in scope. Changes to plugin files, binary layout, metadata,
icons, or release scripts require PCM assembly and
`packaging/validate-pcm.py` validation, which also applies KiCad's addon
guidelines; its tests run with `python -m unittest discover -s packaging/tests`.
Keep tool and toolset counts out of the package descriptions, since they change
every release. The template's version `status` is `testing` until KiCad's
addon repository has listed a release; then set it to `stable`.

`packaging/build-pcm.ps1` and `build-pcm.sh` enumerate the files staged into the
PCM archive. Developer documentation stays in the repository and is not added
to release zips.

KiCad's official addon repository lists a package from a metadata file whose
versions carry `download_sha256` and `download_url`, values that exist only
once the zips are built. `packaging/metadata.json` therefore stays a template
with one bare version entry; do not stamp release hashes into it. The release
workflow merges the three PCM zips with `packaging/merge-pcm.py`, runs
`packaging/pcm-submission-metadata.py` on the merged zip, and attaches it and
`copperpour-pcm-v<version>-metadata.json` to the GitHub release. The metadata
has one version entry for all three platforms, because the repository rejects
a version number listed twice, and says `"license": "open-source"`, because the
repository also checks against the v1 schema, which has no AGPL entry. To list a
release, copy its `versions` entries into `packages/<identifier>/metadata.json`
in a merge request to <https://gitlab.com/kicad/addons/metadata>.

## Browser viewer and standalone retirement

`open_schematic_viewer` now uses the browser service. Core tests exercise its served
MCP schema, canonical-root reuse, saved-data HTTP response and context teardown,
plus opener success/failure/timeout and startup recovery. Run
`cargo test -p copperpour-core --locked tools::viewer` and the workspace gates.
Also check `--no-default-features`: this loopback service is available offline.

The standalone `schematic-viewer` command, Tauri crate, packaging and separate
CI job have been removed with the owner’s explicit waiver of the planned grace
period; see [the migration](api-migrations/retire-standalone-viewer.md). The
workspace build embeds the browser viewer in the server. Linux interactive feedback is available;
Windows/macOS browser launch and interactive behavior need platform verification.

## Server guidance delivery

Run `cargo test -p copperpour-core --locked guidance` for served resource/tool
success and refusal cases, embedded asset inventory, and instructions with/without
a drift probe. `cargo test -p copperpour --locked --test protocol_stdio guidance`
checks the real binary: guidance works without `init`, no native skill files are
installed, and the one-time drift notice does not suppress rules on later starts.
Resource-not-found and malformed-parameter errors remain JSON-RPC errors; bad
`get_guidance` inputs remain structured tool errors. Local profile edits are not
served as resources. The normal workspace gates cover catalog/installer sharing.

## Native/Rust Specctra selection (#72)

`cargo test -p copperpour-core --locked --lib specctra` covers the shared model,
existing parser/adoption corpus and exporter selection. The native-profile mode
matrix uses a child process with an isolated bridge directory, an authenticated
HTTP bridge double serving KiCad-authored DSNs and a bound IPC double. It checks
all modes, unavailable/invalid native exports, Rust fallback, shared safety
refusals and post-export source changes. Model identities are compared against
the historical Rust-DSN-derived identities on existing fixtures.

The circular-cutout and partial-layer through-hole (#484) fixtures are
generated/exported by KiCad and routed by Freerouting. Their manifest-bound SES
apply tests are served with an IPC double and native-import board oracles; created
geometry/layers/nets (and vias) are compared with those oracles. The default tests
use a DRC stub only for control-flow evidence. For real KiCad DRC on the resulting
candidates, run
`cargo test -p copperpour-core --locked --lib native_profile_circle_manifest_apply_with_native_drc -- --ignored --nocapture`
and
`cargo test -p copperpour-core --locked --lib native_profile_partial_th_manifest_apply_with_native_drc -- --ignored --nocapture`.
These do not claim a real GUI IPC transaction; native bridge/UI observations
on other platforms remain separate evidence. Fixture provenance is recorded
beside each board.
