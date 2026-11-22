use super::*;

pub(super) fn test_ctx() -> ToolContext {
    ToolContext::new(
        ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: None,
            auto_load_toolsets: false,
            eager_toolsets: false,
        },
        Arc::new(ToolRouter::new()),
    )
}

/// A lib-table in the exact shape KiCad writes it: CRLF-terminated and
/// TAB-indented.
///
/// Field values are escaped the way `quote_lib_table_string` writes them,
/// because a tempdir path is interpolated here and on Windows it contains
/// backslashes: an unescaped `…\template-fp-lib-table` reads back with a
/// TAB, so the entry pointed at a file that does not exist.
fn kicad_style_table(kind: &str, entries: &[(&str, &str, &str)]) -> String {
    let body: String = entries
        .iter()
        .map(|(nick, ty, uri)| {
            let (nick, ty, uri) = (
                escape_library_string(nick),
                escape_library_string(ty),
                escape_library_string(uri),
            );
            format!(
                "\t(lib (name \"{nick}\") (type \"{ty}\") (uri \"{uri}\") (options \"\") (descr \"\"))\r\n"
            )
        })
        .collect();
    format!("({kind}\r\n\t(version 7)\r\n{body})\r\n")
}

/// Serializes tests that set KICAD10_FOOTPRINT_DIR (process-wide env), the
/// way `sch_components`' `SYMBOL_DIR_ENV` does for the symbol equivalent.
static FOOTPRINT_DIR_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point `KICAD10_FOOTPRINT_DIR` at `dir` for as long as the returned guard
/// lives.
///
/// Rust runs tests in threads of one process, so two tests setting this to
/// their own tempdir would race. Holding the lock serializes them, and
/// restoring the previous value keeps a developer's real KiCad environment
/// intact for whatever runs next.
fn footprint_dir_env(dir: &Path) -> FootprintDirEnv {
    let guard = FOOTPRINT_DIR_ENV.lock().unwrap_or_else(|e| e.into_inner());
    // var_os, not var: a value this process cannot decode as UTF-8 is still
    // one the developer set, and `var` would report it as absent, leaving
    // the restore to silently delete it.
    let previous = std::env::var_os("KICAD10_FOOTPRINT_DIR");
    std::env::set_var("KICAD10_FOOTPRINT_DIR", dir);
    FootprintDirEnv {
        _guard: guard,
        previous,
    }
}

struct FootprintDirEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Drop for FootprintDirEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => std::env::set_var("KICAD10_FOOTPRINT_DIR", v),
            None => std::env::remove_var("KICAD10_FOOTPRINT_DIR"),
        }
    }
}

#[tokio::test]
async fn list_footprint_libraries_reads_a_table_kicad_wrote() {
    // End-to-end regression for the user-visible symptom: on a stock KiCad
    // 10 install every library listing returned {"count": 0}, which left
    // place_component unable to resolve any Library:Footprint id. Drive the
    // real handler with a table in the exact shape KiCad writes.
    let tmp = tempfile::tempdir().unwrap();
    let pretty = tmp.path().join("MyParts.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    let table = kicad_style_table(
        "fp_lib_table",
        &[("MyParts", "KiCad", &pretty.to_string_lossy())],
    );
    assert!(
        !table.contains("\n  (lib "),
        "fixture must be in KiCad's tab format, not the old needle's"
    );
    std::fs::write(tmp.path().join("fp-lib-table"), table).unwrap();

    let args = json!({
        "project": tmp.path().join("board.kicad_pro").to_string_lossy(),
        "scope": "project",
    });
    let res = handle_list_footprint_libraries(&args, &test_ctx())
        .await
        .unwrap();
    assert!(!res.is_error, "handler errored: {:?}", res.content);

    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(out["count"], 1, "library not found: {out}");
    assert_eq!(out["libraries"][0]["nickname"], "MyParts");
    assert_eq!(
        out["libraries"][0]["path"].as_str().map(PathBuf::from),
        Some(pretty),
        "the resolved directory should be reported alongside the raw uri"
    );
}

#[tokio::test]
async fn list_footprint_libraries_expands_a_nested_table_of_env_var_uris() {
    // The two things that kept KiCad's ~155 bundled libraries invisible even
    // once the table parsed: a `(type "Table")` indirection, and entries
    // addressed as ${KICAD10_FOOTPRINT_DIR}/Foo.pretty.
    let tmp = tempfile::tempdir().unwrap();
    let shipped = tmp.path().join("share");
    let pretty = shipped.join("Resistor_SMD.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    let _env = footprint_dir_env(&shipped);

    let nested = tmp.path().join("template-fp-lib-table");
    std::fs::write(
        &nested,
        kicad_style_table(
            "fp_lib_table",
            &[(
                "Resistor_SMD",
                "KiCad",
                "${KICAD10_FOOTPRINT_DIR}/Resistor_SMD.pretty",
            )],
        ),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("fp-lib-table"),
        kicad_style_table(
            "fp_lib_table",
            &[("KiCad", "Table", &nested.to_string_lossy())],
        ),
    )
    .unwrap();

    let args = json!({
        "project": tmp.path().join("board.kicad_pro").to_string_lossy(),
        "scope": "project",
    });
    let res = handle_list_footprint_libraries(&args, &test_ctx())
        .await
        .unwrap();
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();

    assert_eq!(out["count"], 1, "nested table not expanded: {out}");
    assert_eq!(out["libraries"][0]["nickname"], "Resistor_SMD");
    assert_eq!(
        out["libraries"][0]["path"].as_str().map(PathBuf::from),
        Some(pretty),
        "env-var URI should resolve to a real directory"
    );
}

#[test]
fn parse_lib_table_reads_kicad10_crlf_tab_format() {
    // Regression: parse_lib_table hard-coded the needle `\n  (lib ` (LF +
    // exactly 2 spaces). KiCad writes these tables CRLF-terminated and
    // TAB-indented, so the needle never matched and every library listing
    // came back empty — which in turn made footprint placement unable to
    // resolve any `Library:Footprint` id.
    let content = kicad_style_table(
        "fp_lib_table",
        &[
            ("OpenDongle", "KiCad", "/tmp/OpenDongle"),
            ("wch-antenna", "KiCad", "/tmp/wch.pretty"),
        ],
    );
    assert!(
        !content.contains("\n  (lib "),
        "fixture must not contain the old LF/2-space needle"
    );

    let libs = parse_lib_table(&content);
    assert_eq!(libs.len(), 2, "parsed: {libs:?}");
    assert_eq!(libs[0]["nickname"], "OpenDongle");
    assert_eq!(libs[1]["uri"], "/tmp/wch.pretty");
}

#[test]
fn parse_lib_table_still_reads_two_space_indentation() {
    // konnect's own writer emits two-space indentation; both must work.
    let content = "(fp_lib_table\n  (version 7)\n  (lib (name \"Local\") (type \"KiCad\") (uri \"/tmp/local.pretty\") (options \"\") (descr \"\"))\n)\n";
    let libs = parse_lib_table(content);
    assert_eq!(libs.len(), 1);
    assert_eq!(libs[0]["nickname"], "Local");
}

#[test]
fn parse_lib_table_keeps_an_escaped_quote_in_a_description() {
    // Regression: fields were scraped with a substring scan that stopped at
    // the first raw `"`, so an escaped quote truncated the value — a descr
    // of `0.1" pitch` came back as `0.1\`. The parser decodes the escape.
    let content = "(fp_lib_table\n\t(lib (name \"Headers\") (type \"KiCad\") \
         (uri \"/tmp/h.pretty\") (options \"\") (descr \"0.1\\\" pitch headers\"))\n)\n";

    let libs = parse_lib_table(content);
    assert_eq!(libs.len(), 1, "parsed: {libs:?}");
    assert_eq!(libs[0]["description"], "0.1\" pitch headers");
    assert_eq!(libs[0]["uri"], "/tmp/h.pretty");
}

#[test]
fn parse_lib_table_reads_a_field_split_across_lines() {
    // The scan required the literal `(name "` — one space, quote next. A
    // table written with the value on its own line matched nothing and the
    // entry came back with every field empty.
    let content = concat!(
        "(fp_lib_table\n",
        "  (lib\n",
        "    (name\n",
        "      \"Split\")\n",
        "    (type \"KiCad\")\n",
        "    (uri \"/tmp/split.pretty\"))\n",
        ")\n",
    );

    let libs = parse_lib_table(content);
    assert_eq!(libs.len(), 1, "parsed: {libs:?}");
    assert_eq!(libs[0]["nickname"], "Split");
    assert_eq!(libs[0]["uri"], "/tmp/split.pretty");
}

#[test]
fn parse_lib_table_reads_an_escaped_windows_uri() {
    // A Windows URI is a string full of escape introducers, and the one
    // that bites is a path component starting with `t`: written escaped —
    // which is how KiCad and `quote_lib_table_string` write it — it must
    // come back as a backslash and a `t`, not a TAB.
    let content = concat!(
        "(fp_lib_table\r\n",
        "\t(lib (name \"Parts\") (type \"Table\")",
        " (uri \"C:\\\\Users\\\\me\\\\template-fp-lib-table\")",
        " (options \"\") (descr \"\"))\r\n",
        ")\r\n",
    );

    let libs = parse_lib_table(content);
    assert_eq!(libs.len(), 1, "parsed: {libs:?}");
    assert_eq!(libs[0]["uri"], r"C:\Users\me\template-fp-lib-table");
}

#[test]
fn parse_lib_table_skips_only_the_entry_that_does_not_parse() {
    // Entries are located textually so one unbalanced block cannot discard
    // the rest of the table, which a whole-file parse would.
    let content = concat!(
        "(fp_lib_table\n",
        "  (lib (name \"Good\") (type \"KiCad\") (uri \"/tmp/good.pretty\"))\n",
        "  (lib (name \"Bad\" (type \"KiCad\")\n",
        ")\n",
    );

    let libs = parse_lib_table(content);
    assert_eq!(libs.len(), 1, "parsed: {libs:?}");
    assert_eq!(libs[0]["nickname"], "Good");
}

#[test]
fn flatten_lib_table_follows_nested_table_entries() {
    // KiCad 10's default global table does not copy the ~155 bundled
    // libraries; it holds one `(type "Table")` entry pointing at the
    // template table that KiCad ships. Treating that as a library makes
    // every bundled library invisible.
    let tmp = tempfile::tempdir().unwrap();
    let leaf_dir = tmp.path().join("Resistor_SMD.pretty");
    std::fs::create_dir_all(&leaf_dir).unwrap();

    let nested = tmp.path().join("template-fp-lib-table");
    std::fs::write(
        &nested,
        kicad_style_table(
            "fp_lib_table",
            &[("Resistor_SMD", "KiCad", &leaf_dir.to_string_lossy())],
        ),
    )
    .unwrap();

    let root = kicad_style_table(
        "fp_lib_table",
        &[("KiCad", "Table", &nested.to_string_lossy())],
    );

    let libs = flatten_lib_table(&root, 0, None);
    assert_eq!(libs.len(), 1, "nested table not followed: {libs:?}");
    assert_eq!(libs[0]["nickname"], "Resistor_SMD");
    assert_eq!(
        libs[0]["path"].as_str().map(PathBuf::from),
        Some(leaf_dir),
        "resolved path missing"
    );
}

#[test]
fn flatten_lib_table_stops_at_a_self_referencing_table() {
    // A table that points at itself must not recurse forever.
    let tmp = tempfile::tempdir().unwrap();
    let table = tmp.path().join("fp-lib-table");
    std::fs::write(
        &table,
        kicad_style_table(
            "fp_lib_table",
            &[("Loop", "Table", &table.to_string_lossy())],
        ),
    )
    .unwrap();

    let content = std::fs::read_to_string(&table).unwrap();
    assert!(flatten_lib_table(&content, 0, None).is_empty());
}

#[test]
fn is_lib_id_separates_library_ids_from_paths() {
    assert!(is_lib_id("Resistor_SMD:R_0402"));
    assert!(is_lib_id("MyParts:Weird:Name")); // only the first colon splits

    // Paths, by separator.
    assert!(!is_lib_id(r"C:\KiCad\R.kicad_mod"));
    assert!(!is_lib_id("/usr/share/kicad/R.kicad_mod"));
    assert!(!is_lib_id("Resistor_SMD.pretty/R.kicad_mod"));
    // No colon at all.
    assert!(!is_lib_id("R_0402.kicad_mod"));
}

#[test]
fn a_windows_drive_relative_path_is_not_a_library_id() {
    // `C:R.kicad_mod` means R.kicad_mod in drive C's current directory. It
    // has a colon and no separator, so it is shaped exactly like a lib id;
    // the one-letter prefix is what gives it away.
    assert!(!is_lib_id("C:R_0402.kicad_mod"));
    assert!(!is_lib_id("d:board.kicad_mod"));
    // Two letters is a nickname again — no drive is named "Ab".
    assert!(is_lib_id("Ab:R_0402"));
}

#[test]
fn an_absent_lib_table_is_not_an_error() {
    // Every caller checks both the global and project tables, and a project
    // without its own is the normal case.
    let tmp = tempfile::tempdir().unwrap();
    let absent = tmp.path().join("fp-lib-table");
    assert_eq!(read_lib_table_checked(&absent), Ok(Vec::new()));
}

#[test]
fn an_unreadable_lib_table_is_an_error_not_an_empty_list() {
    // Reading a directory as a file fails with something other than
    // NotFound on every platform, which is the case that must not be
    // folded into "0 libraries" — that is the symptom of the very bug this
    // module fixes.
    let tmp = tempfile::tempdir().unwrap();
    let dir_as_table = tmp.path().join("fp-lib-table");
    std::fs::create_dir(&dir_as_table).unwrap();

    let err = read_lib_table_checked(&dir_as_table)
        .expect_err("a table that exists but cannot be read must be reported");
    assert!(err.contains("fp-lib-table"), "must name the table: {err}");
}

#[tokio::test]
async fn list_footprint_libraries_reports_an_unreadable_table() {
    // The handler-level half: this used to surface a read error via `?`
    // before the table read was centralised, and must still.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("fp-lib-table")).unwrap();

    let args = json!({
        "project": tmp.path().join("board.kicad_pro").to_string_lossy(),
        "scope": "project",
    });
    let res = handle_list_footprint_libraries(&args, &test_ctx())
        .await
        .unwrap();
    assert!(
        res.is_error,
        "an unreadable table must not report zero libraries: {:?}",
        res.content
    );
}

#[test]
fn a_missing_footprint_path_names_itself() {
    // Without the existence check the caller's read fails with a bare
    // "os error 2" that never mentions the file, so the message is the
    // point of the test.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope.kicad_mod");
    let err = resolve_footprint_path(&missing.to_string_lossy(), None)
        .expect_err("a nonexistent path must not resolve");
    assert!(err.contains("nope.kicad_mod"), "must name the file: {err}");
    assert!(
        err.contains("Library:Footprint"),
        "should say what the alternative is: {err}"
    );
}

#[test]
fn a_directory_is_not_a_footprint() {
    // is_file, not exists — a .pretty directory would otherwise resolve and
    // fail confusingly at read time.
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_footprint_path(&tmp.path().to_string_lossy(), None).is_err());
}

#[test]
fn an_existing_footprint_path_resolves_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("R_0805.kicad_mod");
    std::fs::write(&file, "(footprint \"R_0805\")").unwrap();
    assert_eq!(
        resolve_footprint_path(&file.to_string_lossy(), None).unwrap(),
        file
    );
}

#[test]
fn a_project_registered_library_resolves() {
    // register_footprint_library writes to the project fp-lib-table by
    // default, so a global-only lookup could not see anything it
    // registered — the default workflow resolved to "library not found".
    let tmp = tempfile::tempdir().unwrap();
    let pretty = tmp.path().join("MyProjLib.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    std::fs::write(pretty.join("Foo.kicad_mod"), "(footprint \"Foo\")").unwrap();
    std::fs::write(
        tmp.path().join("fp-lib-table"),
        kicad_style_table(
            "fp_lib_table",
            &[("MyProjLib", "KiCad", &pretty.to_string_lossy())],
        ),
    )
    .unwrap();

    assert_eq!(
        resolve_footprint_path("MyProjLib:Foo", Some(tmp.path())).unwrap(),
        pretty.join("Foo.kicad_mod")
    );
    // Without the project dir it is invisible, which is the bug.
    assert!(resolve_footprint_path("MyProjLib:Foo", None).is_err());
}

#[test]
fn an_unregistered_nickname_falls_back_to_the_conventional_pretty_dir() {
    // A stock install whose global table is missing or unreadable can
    // still serve Resistor_SMD:R_0402 from <libdir>/Resistor_SMD.pretty.
    let tmp = tempfile::tempdir().unwrap();
    let pretty = tmp.path().join("Fallback_Lib.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    std::fs::write(pretty.join("R_1.kicad_mod"), "(footprint \"R_1\")").unwrap();
    let _env = footprint_dir_env(tmp.path());

    assert_eq!(
        resolve_footprint_path("Fallback_Lib:R_1", None).unwrap(),
        pretty.join("R_1.kicad_mod")
    );
}

#[test]
fn a_missing_library_error_names_the_nickname_and_attempted_locations() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = footprint_dir_env(tmp.path());
    let err = resolve_footprint_path("NoSuchLib:R_1", Some(tmp.path()))
        .expect_err("an unknown nickname must not resolve");
    assert!(err.contains("NoSuchLib"), "must name the library: {err}");
    assert!(
        err.contains("libraries known"),
        "should count the known libraries: {err}"
    );
    assert!(
        err.contains("NoSuchLib.pretty"),
        "should list the attempted fallback location: {err}"
    );
}

#[test]
fn expand_lib_uri_expands_a_kicad_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let pretty = tmp.path().join("Resistor_SMD.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    let _env = footprint_dir_env(tmp.path());

    assert_eq!(
        expand_lib_uri("${KICAD10_FOOTPRINT_DIR}/Resistor_SMD.pretty", None),
        Some(pretty)
    );
    assert_eq!(
        expand_lib_uri("/plain/path", None),
        Some(PathBuf::from("/plain/path")),
        "a non-variable URI must pass through untouched"
    );
}

static TEMPLATE_DIR_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TemplateDirEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Drop for TemplateDirEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => std::env::set_var("KICAD10_TEMPLATE_DIR", v),
            None => std::env::remove_var("KICAD10_TEMPLATE_DIR"),
        }
    }
}

fn template_dir_env(dir: &Path) -> TemplateDirEnv {
    let guard = TEMPLATE_DIR_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("KICAD10_TEMPLATE_DIR");
    std::env::set_var("KICAD10_TEMPLATE_DIR", dir);
    TemplateDirEnv {
        _guard: guard,
        previous,
    }
}

#[test]
fn expand_lib_uri_expands_template_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sym_table = tmp.path().join("sym-lib-table");
    std::fs::write(&sym_table, "(sym_lib_table)\n").unwrap();
    let _env = template_dir_env(tmp.path());

    assert_eq!(
        expand_lib_uri("${KICAD10_TEMPLATE_DIR}/sym-lib-table", None),
        Some(sym_table)
    );
}

#[test]
fn kiprjmod_resolves_against_the_tables_own_directory() {
    // The default register_footprint_library scope is "project", which
    // writes ${KIPRJMOD}/… entries — the common case, not an edge (#61
    // repro case 1 was exactly this).
    let tmp = tempfile::tempdir().unwrap();
    let pretty = tmp.path().join("MyParts.pretty");
    std::fs::create_dir_all(&pretty).unwrap();
    let table = tmp.path().join("fp-lib-table");
    std::fs::write(
        &table,
        "(fp_lib_table\n\t(version 7)\n\t(lib (name \"MyParts\") (type \"KiCad\") (uri \"${KIPRJMOD}/MyParts.pretty\") (options \"\") (descr \"\"))\n)\n",
    )
    .unwrap();

    let libs = read_lib_table_checked(&table).unwrap();
    assert_eq!(libs.len(), 1);
    assert_eq!(
        libs[0]["path"].as_str().map(PathBuf::from),
        Some(pretty),
        "a project-scoped ${{KIPRJMOD}} URI must resolve via the table's directory"
    );

    // Without a project context (direct call, no table), it must not
    // resolve rather than guess.
    assert_eq!(expand_lib_uri("${KIPRJMOD}/MyParts.pretty", None), None);
}

#[test]
fn table_root_element_matches_the_table_kind() {
    // Credit: PR #54 — the scaffold was hardcoded to fp_lib_table, so
    // registering a symbol library on a machine with no global
    // sym-lib-table wrote a file KiCad rejects.
    assert_eq!(
        table_root_element(Path::new("sym-lib-table")),
        "sym_lib_table"
    );
    assert_eq!(
        table_root_element(Path::new("C:/proj/sym-lib-table")),
        "sym_lib_table"
    );
    assert_eq!(
        table_root_element(Path::new("fp-lib-table")),
        "fp_lib_table"
    );
}

#[tokio::test]
async fn registering_a_symbol_library_scaffolds_a_sym_root() {
    let tmp = tempfile::tempdir().unwrap();
    let table = tmp.path().join("sym-lib-table");
    let repaired = register_in_lib_table(&table, "MySyms", "${KIPRJMOD}/my.kicad_sym", "KiCad")
        .await
        .unwrap();
    assert!(!repaired, "a fresh scaffold has nothing to repair");
    let content = std::fs::read_to_string(&table).unwrap();
    assert!(
        content.starts_with("(sym_lib_table"),
        "scaffold root must match the table kind, got: {content}"
    );
    assert!(content.contains("\"MySyms\""));
}

/// A sym-lib-table an older build wrote with an `(fp_lib_table` root is
/// rejected wholesale by KiCad ("has type FOOTPRINT but expected SYMBOL;
/// skipping"). #54 stopped new files being written that way; without a
/// repair the existing ones stay broken through every later registration.
const BROKEN_SYM_TABLE: &str = "(fp_lib_table\n  (version 7)\n  (lib (name \"Old\") (type \"KiCad\") (uri \"/abs/old.kicad_sym\") (options \"\") (descr \"\"))\n)\n";

#[tokio::test]
async fn an_existing_sym_table_with_an_fp_root_is_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let table = tmp.path().join("sym-lib-table");
    std::fs::write(&table, BROKEN_SYM_TABLE).unwrap();

    let repaired = register_in_lib_table(&table, "MySyms", "/abs/my.kicad_sym", "KiCad")
        .await
        .unwrap();

    assert!(
        repaired,
        "the wrong root must be reported, not fixed mutely"
    );
    let content = std::fs::read_to_string(&table).unwrap();
    assert!(
        content.starts_with("(sym_lib_table"),
        "root must be repaired, got: {content}"
    );
    assert!(!content.contains("fp_lib_table"), "{content}");
    assert!(
        content.contains("\"Old\""),
        "existing entry lost: {content}"
    );
    assert!(content.contains("\"MySyms\""), "{content}");
}

/// The idempotent "already registered" early return used to skip the write
/// entirely, so re-registering the same nickname left the bad root in place.
#[tokio::test]
async fn a_repair_still_lands_when_the_nickname_is_already_registered() {
    let tmp = tempfile::tempdir().unwrap();
    let table = tmp.path().join("sym-lib-table");
    std::fs::write(&table, BROKEN_SYM_TABLE).unwrap();

    let repaired = register_in_lib_table(&table, "Old", "/abs/old.kicad_sym", "KiCad")
        .await
        .unwrap();

    assert!(repaired);
    let content = std::fs::read_to_string(&table).unwrap();
    assert!(content.starts_with("(sym_lib_table"), "{content}");
    assert_eq!(
        content.matches("(lib ").count(),
        1,
        "an already-registered nickname must not be duplicated: {content}"
    );
}

#[test]
fn register_footprint_library_schema_exposes_opt_in_replacement() {
    let registration = tools()
        .into_iter()
        .find(|tool| tool.name == "register_footprint_library")
        .expect("library must expose register_footprint_library");
    let replace = &registration.input_schema["properties"]["replace_existing"];

    assert_eq!(replace["type"], "boolean");
    assert_eq!(replace["default"], false);
    assert_eq!(
        registration.input_schema["required"],
        json!(["library_path", "nickname"])
    );
}

/// #211: registering a nickname that already exists silently reported
/// success while changing nothing, so a stale project URI could not be
/// corrected and the caller could not tell. The footprint half gained a
/// reported state and `replace_existing` in #205; the symbol half kept
/// the old always-`true` shape, which left the API asymmetric.
#[tokio::test]
async fn register_symbol_library_reports_that_it_changed_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    let table = project.parent().unwrap().join("sym-lib-table");
    let original = "(sym_lib_table
  (version 7)
  (lib (name \"Parts\") (type \"KiCad\") (uri \"C:/stale/Parts.kicad_sym\") (options \"keep=1\") (descr \"keep me\"))
)
";
    std::fs::write(&table, original).unwrap();

    let result = handle_register_symbol_library(
        &json!({
            "library_path": tmp.path().join("lib").join("Parts.kicad_sym"),
            "nickname": "Parts",
            "project": project
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(
        output["state"], "unchanged",
        "a no-op must say so rather than reporting a bare success: {output}"
    );
    assert_eq!(std::fs::read_to_string(&table).unwrap(), original);
}

#[tokio::test]
async fn register_symbol_library_reports_a_new_entry_as_inserted() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();

    let result = handle_register_symbol_library(
        &json!({
            "library_path": tmp.path().join("lib").join("Parts.kicad_sym"),
            "nickname": "Parts",
            "project": project
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["state"], "inserted", "{output}");
}

/// The point of the issue: a stale URI must be correctable, and the entry's
/// own options/descr must survive the correction.
#[tokio::test]
async fn register_symbol_library_replaces_a_stale_uri_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    std::fs::write(&project, "{}").unwrap();
    let library = tmp.path().join("test").join("Parts.kicad_sym");
    std::fs::write(&library, "(kicad_symbol_lib)").unwrap();
    let table = project.parent().unwrap().join("sym-lib-table");
    std::fs::write(
        &table,
        "(sym_lib_table
  (version 7)
  (lib (name \"Parts\") (type \"KiCad\") (uri \"C:/stale/Parts.kicad_sym\") (options \"keep=1\") (descr \"keep me\"))
)
",
    )
    .unwrap();

    let result = handle_register_symbol_library(
        &json!({
            "library_path": library,
            "nickname": "Parts",
            "project": project,
            "replace_existing": true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["state"], "updated", "{output}");
    let written = std::fs::read_to_string(&table).unwrap();
    assert!(
        !written.contains("C:/stale"),
        "stale URI must be gone:
{written}"
    );
    assert!(
        written.contains("keep=1") && written.contains("keep me"),
        "the entry's own metadata must survive:
{written}"
    );
}

#[tokio::test]
async fn register_symbol_library_rejects_a_non_boolean_replace_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    let result = handle_register_symbol_library(
        &json!({
            "library_path": tmp.path().join("lib").join("Parts.kicad_sym"),
            "nickname": "Parts",
            "project": project,
            "replace_existing": "yes"
        }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(result.is_error, "{:?}", result.content);
}

#[tokio::test]
async fn register_footprint_library_keeps_existing_uri_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    let table = project.parent().unwrap().join("fp-lib-table");
    let original = "(fp_lib_table\n  (version 7)\n  (lib (name \"Parts\") (type \"KiCad\") (uri \"C:/stale/Parts.pretty\") (options \"keep=1\") (descr \"keep me\"))\n)\n";
    std::fs::write(&table, original).unwrap();

    let result = handle_register_footprint_library(
        &json!({
            "library_path": tmp.path().join("lib").join("Parts.pretty"),
            "nickname": "Parts",
            "project": project
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["state"], "unchanged");
    assert_eq!(std::fs::read_to_string(table).unwrap(), original);
}

#[tokio::test]
async fn register_footprint_library_replaces_uri_portably_and_preserves_entry_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    let project = tmp.path().join("test").join("board.kicad_pro");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    std::fs::write(&project, "{}").unwrap();
    let library = tmp.path().join("lib").join("Parts.pretty");
    std::fs::create_dir_all(&library).unwrap();
    let table = project.parent().unwrap().join("fp-lib-table");
    let original = "(fp_lib_table\n\t(version 7)\n\t(lib (name \"Other\") (type \"KiCad\") (uri \"${KIPRJMOD}/other.pretty\") (options \"\") (descr \"other\"))\n\t(lib (name \"Parts\") (type \"KiCad\") (uri \"C:/stale/Parts.pretty\") (options \"keep=1\") (descr \"keep me\"))\n)\n";
    std::fs::write(&table, original).unwrap();

    let result = handle_register_footprint_library(
        &json!({
            "library_path": library,
            "nickname": "Parts",
            "project": project,
            "replace_existing": true
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["state"], "updated");
    assert_eq!(output["uri"], "${KIPRJMOD}/../lib/Parts.pretty");

    let updated = std::fs::read_to_string(table).unwrap();
    assert!(updated.starts_with("(fp_lib_table\n\t(version 7)"));
    assert!(
        updated.contains("(name \"Other\") (type \"KiCad\") (uri \"${KIPRJMOD}/other.pretty\")")
    );
    assert!(updated.contains("(name \"Parts\") (type \"KiCad\") (uri \"${KIPRJMOD}/../lib/Parts.pretty\") (options \"keep=1\") (descr \"keep me\")"));
    assert!(!updated.contains("C:/stale"));
}

/// The whole reported failure, end to end: two projects side by side under
/// one parent, the same nickname registered in each. Before, both resolved
/// to a stray table in the parent, so the second call reported `unchanged`
/// against the first project's entry and neither project ever saw one.
#[tokio::test]
async fn registering_by_project_directory_keeps_two_projects_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let alpha = tmp.path().join("alpha");
    let beta = tmp.path().join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    let library = tmp.path().join("shared.kicad_sym");
    std::fs::write(&library, "(kicad_symbol_lib)\n").unwrap();

    let register = |project: std::path::PathBuf| {
        let library = library.clone();
        async move {
            handle_register_symbol_library(
                &json!({
                    "library_path": library,
                    "nickname": "Shared",
                    "project": project
                }),
                &test_ctx(),
            )
            .await
            .unwrap()
        }
    };

    let first = register(alpha.clone()).await;
    let second = register(beta.clone()).await;

    assert!(!first.is_error, "{:?}", first.content);
    assert!(!second.is_error, "{:?}", second.content);
    let first: serde_json::Value = serde_json::from_str(&result_text(&first)).unwrap();
    let second: serde_json::Value = serde_json::from_str(&result_text(&second)).unwrap();
    assert_eq!(first["state"], "inserted");
    assert_eq!(
        second["state"], "inserted",
        "the second project must get its own entry, not `unchanged`"
    );

    assert!(
        alpha.join("sym-lib-table").exists(),
        "alpha must carry its own table"
    );
    assert!(
        beta.join("sym-lib-table").exists(),
        "beta must carry its own table"
    );
    assert!(
        !tmp.path().join("sym-lib-table").exists(),
        "nothing may be written above the projects"
    );
}

/// A project argument that names nothing is refused instead of resolving to
/// its parent and reporting success there.
#[tokio::test]
async fn registering_against_a_missing_project_refuses_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("parts.kicad_sym");
    std::fs::write(&library, "(kicad_symbol_lib)\n").unwrap();

    let result = handle_register_symbol_library(
        &json!({
            "library_path": library,
            "nickname": "Parts",
            "project": tmp.path().join("no-such-project")
        }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(result.is_error, "{:?}", result.content);
    assert!(!tmp.path().join("sym-lib-table").exists());
}

#[tokio::test]
async fn register_footprint_library_rejects_duplicate_nicknames_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("board.kicad_pro");
    let table = tmp.path().join("fp-lib-table");
    let duplicate = "(fp_lib_table\n  (version 7)\n  (lib (name \"Parts\") (type \"KiCad\") (uri \"/one\") (options \"\") (descr \"\"))\n  (lib (name \"Parts\") (type \"KiCad\") (uri \"/two\") (options \"\") (descr \"\"))\n)\n";
    std::fs::write(&table, duplicate).unwrap();

    let result = handle_register_footprint_library(
        &json!({
            "library_path": tmp.path().join("Parts.pretty"),
            "nickname": "Parts",
            "project": project,
            "replace_existing": true
        }),
        &test_ctx(),
    )
    .await
    .expect("a malformed table is a tool error, not a transport error");

    assert!(result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(output["error"]["kind"], "invalid_argument");
    assert_eq!(output["error"]["field"], "nickname");
    assert_eq!(std::fs::read_to_string(table).unwrap(), duplicate);
}

#[test]
fn stale_library_table_source_is_rejected_without_overwriting_newer_content() {
    let tmp = tempfile::tempdir().unwrap();
    let table = tmp.path().join("fp-lib-table");
    let original = "(fp_lib_table\n  (version 7)\n)\n";
    std::fs::write(&table, original).unwrap();
    let prepared = "(fp_lib_table\n  (version 7)\n  (lib (name \"Parts\") (type \"KiCad\") (uri \"/parts\") (options \"\") (descr \"\"))\n)\n";
    let newer = "(fp_lib_table\n  (version 7)\n  (lib (name \"Other\") (type \"KiCad\") (uri \"/other\") (options \"\") (descr \"\"))\n)\n";
    std::fs::write(&table, newer).unwrap();

    let error = persist_lib_table_registration(&table, original, prepared)
        .expect_err("a stale expected table must conflict");
    assert!(matches!(error, copperpour_sexp::SexpError::Conflict { .. }));
    assert_eq!(std::fs::read_to_string(table).unwrap(), newer);
}

#[test]
fn a_correct_root_and_a_mention_inside_a_descr_are_left_alone() {
    let ok = "(fp_lib_table\n  (version 7)\n)\n".to_string();
    assert_eq!(
        repair_table_root(ok.clone(), "fp_lib_table"),
        (ok, false),
        "a correct table must not be rewritten"
    );

    // The wrong token appears, but not as the root element.
    let nested = "(sym_lib_table\n  (lib (name \"X\") (descr \"copied from fp_lib_table\"))\n)\n"
        .to_string();
    assert_eq!(
        repair_table_root(nested.clone(), "sym_lib_table"),
        (nested, false),
        "only the root element may be rewritten"
    );
}

#[tokio::test]
async fn bounded_catalogue_search_proves_an_extra_match_across_libraries() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reference_prefix_kicad10/Device.kicad_sym");
    let content = std::fs::read_to_string(&fixture).unwrap();
    let names = top_level_symbol_names(&content).unwrap();
    assert_eq!(names, ["R"]);
    // Each library supplies exactly one match. Lookahead must cross the
    // library boundary to distinguish exactly full from truncated.
    let query = "r".to_string();
    let per_library = 1;
    let entries = vec![
        ("first".into(), fixture.display().to_string()),
        ("second".into(), fixture.display().to_string()),
    ];
    for (limit, shown, truncated) in [
        (0, 0, true),
        (per_library, per_library, true),
        (per_library * 2, per_library * 2, false),
    ] {
        let hits = search_symbol_entries(entries.clone(), &query, limit).await;
        let body: serde_json::Value =
            serde_json::from_str(&result_text(&search_response(&query, &hits, limit))).unwrap();
        assert_eq!(body["shown"], shown);
        assert_eq!(body["count"], shown);
        assert_eq!(body["truncated"], truncated);
        assert_eq!(body["results"].as_array().unwrap().len(), shown);
        if shown > 0 {
            assert_eq!(body["results"][0]["library"], "first");
        }
    }
    let hits = search_symbol_entries(entries, "no-such-symbol-issue16", 0).await;
    assert!(hits.is_empty());
    let body: serde_json::Value =
        serde_json::from_str(&result_text(&search_response("none", &hits, 0))).unwrap();
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn bounded_footprint_search_proves_an_extra_match_across_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let native =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket_kicad10.kicad_mod");
    let mut libraries = Vec::new();
    for name in ["first", "second"] {
        let folder = dir.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        std::fs::copy(&native, folder.join("Match.kicad_mod")).unwrap();
        libraries.push(json!({"nickname": name, "path": folder}));
    }
    for (limit, shown, truncated) in [(0, 0, true), (1, 1, true), (2, 2, false), (3, 2, false)] {
        let hits = search_footprint_entries(libraries.clone(), "match", limit).await;
        let body: serde_json::Value =
            serde_json::from_str(&result_text(&search_response("Match", &hits, limit))).unwrap();
        assert_eq!(body["shown"], shown);
        assert_eq!(body["count"], shown);
        assert_eq!(body["truncated"], truncated);
        assert_eq!(body["results"].as_array().unwrap().len(), shown);
    }
    assert!(search_footprint_entries(libraries, "absent", 0)
        .await
        .is_empty());
}

#[tokio::test]
async fn bounded_catalogue_listing_is_served_with_full_count_and_sample() {
    let handler = crate::mcp::handler::McpHandler::new(test_ctx().config.clone())
        .await
        .unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/symbol_fields_kicad10.kicad_sym");
    let names = ["FIELDS_BARE", "FIELDS_FULL"];
    handler
        .handle_message(json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
        "params":{"name":"load_toolset", "arguments":{"name":"library"}}}))
        .await;
    for (limit, shown) in [
        (json!(0), 0),
        (json!(1.0), 1),
        (json!(names.len()), names.len()),
    ] {
        let response = handler.handle_message(json!({"jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"list_symbols_in_library", "arguments":{"library_path":fixture,"limit":limit}}})).await.unwrap().result.unwrap();
        assert_eq!(response["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["count"], names.len());
        assert_eq!(body["shown"], shown);
        assert_eq!(body["truncated"], shown < names.len());
        assert_eq!(body["symbols"], json!(&names[..shown]));
    }
}

#[test]
fn bounded_tool_schemas_refuse_negative_limits_and_accept_integral_floats() {
    for (tool, args) in [
        ("run_drc", json!({"board":"missing.kicad_pcb"})),
        ("get_drc_violations", json!({"board":"missing.kicad_pcb"})),
        (
            "list_symbols_in_library",
            json!({"library_path":"missing.kicad_sym"}),
        ),
        ("search_symbols", json!({"query":"R"})),
        ("search_footprints", json!({"query":"R"})),
    ] {
        let def = tools()
            .into_iter()
            .chain(super::super::verification::tools())
            .chain(super::super::pcb_export::tools())
            .find(|def| def.name == tool)
            .unwrap();
        for (limit, valid) in [
            (json!(-1), false),
            (json!(1.5), false),
            (json!(0), true),
            (json!(1.0), true),
        ] {
            let mut args = args.clone();
            args["limit"] = limit;
            assert_eq!(def.input_validator.is_valid(&args), valid, "{tool}: {args}");
        }
    }
}

#[tokio::test]
async fn bounded_tools_refuse_invalid_limits_through_served_dispatch() {
    let mut config = test_ctx().config.clone();
    config.auto_load_toolsets = true;
    let handler = crate::mcp::handler::McpHandler::new(config).await.unwrap();
    for (tool, args) in [
        ("run_drc", json!({"board":"missing.kicad_pcb"})),
        ("get_drc_violations", json!({"board":"missing.kicad_pcb"})),
        (
            "list_symbols_in_library",
            json!({"library_path":"missing.kicad_sym"}),
        ),
        ("search_symbols", json!({"query":"R"})),
        ("search_footprints", json!({"query":"R"})),
    ] {
        for limit in [json!(-1), json!(1.5), json!("1"), json!(null), json!(true)] {
            let mut args = args.clone();
            args["limit"] = limit;
            let response = handler
                .handle_message(json!({"jsonrpc":"2.0", "id":3, "method":"tools/call",
                "params":{"name":tool, "arguments":args}}))
                .await
                .unwrap()
                .result
                .unwrap();
            assert_eq!(response["isError"], true, "{tool}: {response}");
            let body: serde_json::Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["error"]["kind"], "invalid_argument", "{tool}: {body}");
            assert_eq!(body["error"]["field"], "limit", "{tool}: {body}");
        }
    }
}

#[tokio::test]
async fn list_symbols_parses_kicad10_crlf_tab_format() {
    // Regression: konnect 0.2.0 hard-coded the needle `\n  (symbol "` (LF +
    // exactly 2 spaces) and so returned 0 symbols for every real KiCad
    // library. On disk those files are CRLF-terminated and TAB-indented
    // (KiCad 10, format version 20251024), so the needle never matched.
    // Build a fixture in that exact on-disk shape and confirm we now find
    // the top-level symbols and skip the nested `_N_M` sub-units.
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("kicad10.kicad_sym");
    let unit = |name: &str| {
        format!("\t(symbol \"{name}\"\r\n\t\t(symbol \"{name}_0_1\"\r\n\t\t)\r\n\t)\r\n")
    };
    let content = format!(
        "(kicad_symbol_lib\r\n\t(version 20251024)\r\n\t(generator \"kicad_symbol_editor\")\r\n{}{})\r\n",
        unit("R_ohm"),
        unit("LED"),
    );
    // Sanity: the fixture really is CRLF + TAB and lacks the old needle.
    assert!(content.contains("\r\n"));
    assert!(
        !content.contains("\n  (symbol \""),
        "fixture must not contain the old LF/2-space needle"
    );
    std::fs::write(&lib, content).unwrap();

    let args = json!({ "library_path": lib.to_string_lossy() });
    let res = handle_list_symbols_in_library(&args, &test_ctx())
        .await
        .unwrap();
    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let text = match res.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    let out: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        out["count"], 2,
        "expected 2 top-level symbols (R_ohm, LED), got: {text}"
    );
    let names: Vec<String> = serde_json::from_value(out["symbols"].clone()).unwrap();
    assert!(names.contains(&"R_ohm".to_string()), "names={names:?}");
    assert!(names.contains(&"LED".to_string()), "names={names:?}");
    assert!(
        !names.iter().any(|n| n.ends_with("_0_1")),
        "sub-units must not leak into the listing: {names:?}"
    );
}

pub(super) fn result_text(res: &CallToolResult) -> String {
    match res.content.first() {
        Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

/// Build a temp "project dir" containing a `sym-lib-table` that references a
/// single `.kicad_sym` library, returning the project dir path. The URI is
/// absolute (not `${KICAD_*}`) so it resolves without KiCad env vars.
fn write_project_sym_lib(tmp: &tempfile::TempDir, nick: &str, lib_body: &str) -> PathBuf {
    let lib_file = tmp.path().join(format!("{nick}.kicad_sym"));
    std::fs::write(&lib_file, lib_body).unwrap();
    let uri = lib_file.to_string_lossy().replace('\\', "/");
    let table = format!(
        "(sym_lib_table\n  (lib (name \"{nick}\") (type \"Normal\") (uri \"{uri}\") (options \"\") (descr \"\"))\n)\n",
    );
    std::fs::write(tmp.path().join("sym-lib-table"), table).unwrap();
    tmp.path().to_path_buf()
}

#[tokio::test]
async fn get_symbol_info_parses_kicad10_pins_and_props() {
    // Regression: get_symbol_info hard-coded `  (symbol "NAME"` / `\n    (pin `
    // string searches and only consulted the GLOBAL table, so it returned
    // "not found" for every real KiCad 10 symbol (CRLF + TAB files) and could
    // never resolve project libraries. Fixture is a KiCad-10-shaped (CRLF +
    // TAB) library resolved via a project sym-lib-table; we expect pins +
    // properties read from the tree, with the nested _1_1 unit's pins
    // collected recursively.
    let tmp = tempfile::tempdir().unwrap();
    let body = concat!(
        "(kicad_symbol_lib\r\n",
        "\t(version 20251024)\r\n",
        "\t(generator \"kicad_symbol_editor\")\r\n",
        "\t(symbol \"T1\"\r\n",
        "\t\t(property \"Reference\" \"Q\" (at 0 5.08 0))\r\n",
        "\t\t(property \"Value\" \"T1\" (at 0 -5.08 0))\r\n",
        "\t\t(symbol \"T1_1_1\"\r\n",
        "\t\t\t(pin input line (at -5.08 2.54 0) (length 2.54) (name \"G\") (number \"1\"))\r\n",
        "\t\t\t(pin output line (at 5.08 0 180) (length 2.54) (name \"S\") (number \"3\"))\r\n",
        "\t\t)\r\n",
        "\t)\r\n",
        ")\r\n",
    );
    let proj = write_project_sym_lib(&tmp, "testlib", body);

    let args = json!({
        "lib_id": "testlib:T1",
        "project_dir": proj.to_string_lossy(),
    });
    let res = handle_get_symbol_info(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(out["pin_count"], 2, "full result: {out}");
    let numbers: Vec<&str> = out["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["number"].as_str().unwrap_or(""))
        .collect();
    assert!(numbers.contains(&"1"), "pins: {out}");
    assert!(numbers.contains(&"3"), "pins: {out}");
    let g_pin = out["pins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["number"] == "1")
        .unwrap();
    assert_eq!(g_pin["type"], "input", "{g_pin}");
    assert_eq!(g_pin["name"], "G", "{g_pin}");
    assert_eq!(out["properties"]["Reference"], "Q", "{out}");
    assert_eq!(out["properties"]["Value"], "T1", "{out}");
}

const EXTENDS_DERIVED_LIB: &str = "\
(kicad_symbol_lib
  (version 20251024)
  (symbol \"Base\"
    (symbol \"Base_1_1\"
      (pin input line (at -5.08 2.54 0) (length 2.54) (name \"G\") (number \"1\"))
      (pin output line (at 5.08 0 180) (length 2.54) (name \"S\") (number \"3\"))
    )
  )
  (symbol \"Derived\"
    (extends \"Base\")
    (property \"Reference\" \"U\" (at 0 5.08 0))
    (property \"Value\" \"Derived\" (at 0 -5.08 0))
  )
)
";

#[test]
fn resolve_symbol_pins_inherits_from_base() {
    let root = parse_sexp(EXTENDS_DERIVED_LIB).unwrap();
    let derived = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("Derived"))
        .unwrap();
    let pins = resolve_symbol_pins(&root, derived);
    let numbers: Vec<&str> = pins
        .iter()
        .map(|p| p.find_str("number").unwrap_or(""))
        .collect();
    assert_eq!(
        pins.len(),
        2,
        "derived symbol should inherit base pins: {numbers:?}"
    );
    assert!(numbers.contains(&"1"), "{numbers:?}");
    assert!(numbers.contains(&"3"), "{numbers:?}");
}

#[tokio::test]
async fn get_symbol_info_resolves_extends_pins() {
    // Derived symbol (extends Base) has no own pins; get_symbol_info must
    // follow the extends chain and report the base's pins.
    let tmp = tempfile::tempdir().unwrap();
    let proj = write_project_sym_lib(&tmp, "testlib", EXTENDS_DERIVED_LIB);
    let args = json!({
        "lib_id": "testlib:Derived",
        "project_dir": proj.to_string_lossy(),
    });
    let res = handle_get_symbol_info(&args, &test_ctx()).await.unwrap();
    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(
        out["pin_count"], 2,
        "derived symbol should inherit 2 base pins: {out}"
    );
    let numbers: Vec<&str> = out["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["number"].as_str().unwrap_or(""))
        .collect();
    assert!(numbers.contains(&"1"), "pins: {out}");
    assert!(numbers.contains(&"3"), "pins: {out}");
    assert_eq!(out["properties"]["Reference"], "U", "{out}");
}

/// A `.kicad_sym` file no table registers is read by `library_path` and
/// `symbol_name`, with the pins and properties `lib_id` gives for the same
/// file through a table, extends chain included; the answer names the
/// file instead of a library nickname.
#[tokio::test]
async fn get_symbol_info_reads_an_unregistered_library_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = write_project_sym_lib(&tmp, "testlib", EXTENDS_DERIVED_LIB);
    let registered = handle_get_symbol_info(
        &json!({ "lib_id": "testlib:Derived", "project_dir": proj.to_string_lossy() }),
        &test_ctx(),
    )
    .await
    .unwrap();
    let registered: serde_json::Value = serde_json::from_str(&result_text(&registered)).unwrap();

    let unregistered = tempfile::tempdir().unwrap();
    let path = unregistered.path().join("loose.kicad_sym");
    std::fs::write(&path, EXTENDS_DERIVED_LIB).unwrap();
    let res = handle_get_symbol_info(
        &json!({ "library_path": path.to_string_lossy(), "symbol_name": "Derived" }),
        &test_ctx(),
    )
    .await
    .unwrap();

    assert!(!res.is_error, "handler errored: {:?}", res.content);
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(out["library_path"], path.display().to_string(), "{out}");
    assert_eq!(out["name"], "Derived", "{out}");
    assert!(out.get("lib_id").is_none(), "{out}");
    assert!(out.get("library").is_none(), "{out}");
    for key in ["pin_count", "pins", "properties"] {
        assert_eq!(out[key], registered[key], "{key}: {out}");
    }
    assert_eq!(out["pin_count"], 2, "{out}");
}

/// The symbol is named one way, by `lib_id` or by `library_path` with
/// `symbol_name`, and a path must be an existing `.kicad_sym` holding it.
#[tokio::test]
async fn get_symbol_info_refuses_a_mixed_or_unusable_library_path() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("loose.kicad_sym");
    std::fs::write(&path, EXTENDS_DERIVED_LIB).unwrap();
    let path = path.to_string_lossy().to_string();
    let missing = tmp
        .path()
        .join("missing.kicad_sym")
        .to_string_lossy()
        .to_string();
    let wrong_type = tmp
        .path()
        .join("loose.kicad_mod")
        .to_string_lossy()
        .to_string();
    for (args, kind, field) in [
        (
            json!({ "lib_id": "Device:R", "library_path": path, "symbol_name": "Derived" }),
            "invalid_argument",
            Some("library_path"),
        ),
        (
            json!({ "library_path": path }),
            "invalid_argument",
            Some("symbol_name"),
        ),
        (
            json!({ "lib_id": "Device:R", "symbol_name": "R" }),
            "invalid_argument",
            Some("symbol_name"),
        ),
        (
            json!({ "library_path": wrong_type, "symbol_name": "Derived" }),
            "invalid_argument",
            Some("library_path"),
        ),
        (
            json!({ "library_path": missing, "symbol_name": "Derived" }),
            "file_not_found",
            None,
        ),
        (
            json!({ "library_path": path, "symbol_name": "Derived", "project_dir": "." }),
            "invalid_argument",
            Some("project_dir"),
        ),
    ] {
        let res = handle_get_symbol_info(&args, &test_ctx()).await.unwrap();
        assert!(res.is_error, "{args}");
        let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
        assert_eq!(out["error"]["kind"], kind, "{args}: {out}");
        if let Some(field) = field {
            assert_eq!(out["error"]["field"], field, "{args}: {out}");
        }
    }

    let res = handle_get_symbol_info(
        &json!({ "library_path": path, "symbol_name": "Nope" }),
        &test_ctx(),
    )
    .await
    .unwrap();
    assert!(res.is_error);
    assert!(
        result_text(&res).contains("Symbol 'Nope' not found in library"),
        "{}",
        result_text(&res)
    );
}

#[test]
fn resolve_symbol_pins_follows_multilevel_chain() {
    let src = "\
(kicad_symbol_lib
  (symbol \"C\"
    (symbol \"C_1_1\"
      (pin passive line (at 0 5.08 0) (length 2.54) (name \"C1\") (number \"1\"))
    )
  )
  (symbol \"B\" (extends \"C\"))
  (symbol \"A\" (extends \"B\"))
)";
    let root = parse_sexp(src).unwrap();
    let a = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("A"))
        .unwrap();
    let pins = resolve_symbol_pins(&root, a);
    let numbers: Vec<&str> = pins
        .iter()
        .map(|p| p.find_str("number").unwrap_or(""))
        .collect();
    assert_eq!(numbers, vec!["1"], "A→B→C should resolve to C's pin");
}

#[test]
fn resolve_symbol_pins_handles_cycle() {
    let src = "\
(kicad_symbol_lib
  (symbol \"A\"
    (extends \"B\")
    (symbol \"A_1_1\"
      (pin passive line (at 0 5.08 0) (length 2.54) (name \"A1\") (number \"1\"))
    )
  )
  (symbol \"B\"
    (extends \"A\")
    (symbol \"B_1_1\"
      (pin passive line (at 0 -5.08 0) (length 2.54) (name \"B2\") (number \"2\"))
    )
  )
)";
    let root = parse_sexp(src).unwrap();
    let a = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("A"))
        .unwrap();
    let pins = resolve_symbol_pins(&root, a);
    let numbers: Vec<&str> = pins
        .iter()
        .map(|p| p.find_str("number").unwrap_or(""))
        .collect();
    // Terminates (no hang); collects A's pin "1" then B's pin "2".
    assert!(numbers.contains(&"1"), "{numbers:?}");
    assert!(numbers.contains(&"2"), "{numbers:?}");
}

#[test]
fn resolve_symbol_pins_missing_base_falls_back() {
    let src = "\
(kicad_symbol_lib
  (symbol \"Orphan\"
    (extends \"NoSuch\")
    (symbol \"Orphan_1_1\"
      (pin passive line (at 0 5.08 0) (length 2.54) (name \"P\") (number \"7\"))
    )
  )
)";
    let root = parse_sexp(src).unwrap();
    let orphan = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("Orphan"))
        .unwrap();
    let pins = resolve_symbol_pins(&root, orphan);
    let numbers: Vec<&str> = pins
        .iter()
        .map(|p| p.find_str("number").unwrap_or(""))
        .collect();
    // Missing base: walk stops, returns Orphan's own pin (no panic).
    assert_eq!(numbers, vec!["7"]);
}

#[test]
fn resolve_symbol_pins_derived_shadows_base() {
    let src = "\
(kicad_symbol_lib
  (symbol \"Base\"
    (symbol \"Base_1_1\"
      (pin input line (at 0 5.08 0) (length 2.54) (name \"BASE_G\") (number \"1\"))
    )
  )
  (symbol \"Derived\"
    (extends \"Base\")
    (symbol \"Derived_1_1\"
      (pin output line (at 0 -5.08 0) (length 2.54) (name \"DERIVED_G\") (number \"1\"))
    )
  )
)";
    let root = parse_sexp(src).unwrap();
    let derived = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some("Derived"))
        .unwrap();
    let pins = resolve_symbol_pins(&root, derived);
    // Derived's own pin "1" shadows base's pin "1": one pin, derived's name.
    assert_eq!(pins.len(), 1, "{pins:?}");
    assert_eq!(pins[0].find_str("name"), Some("DERIVED_G"));
    assert_eq!(pins[0].find_str("number"), Some("1"));
}

#[tokio::test]
async fn search_lib_symbols_matches_underscore_names_and_skips_units() {
    // Pure check of the per-library matcher factored out of search_symbols:
    // top-level symbols with underscores must be returned verbatim, and the
    // nested _0_1 unit sub-symbols must not leak into results.
    let body = concat!(
        "(kicad_symbol_lib\r\n\t(version 20251024)\r\n",
        "\t(symbol \"FOO_BAR\"\r\n\t\t(symbol \"FOO_BAR_0_1\")\r\n\t)\r\n",
        "\t(symbol \"LED\"\r\n\t\t(symbol \"LED_0_1\")\r\n\t)\r\n",
        ")\r\n",
    );
    let results = search_lib_symbols("projlib", body, "foo");
    let names: Vec<&str> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"FOO_BAR"), "names={names:?}");
    assert_eq!(results[0]["library"], "projlib");
    assert_eq!(results[0]["id"], "projlib:FOO_BAR");
    assert!(
        !names.iter().any(|n| n.ends_with("_0_1")),
        "sub-units leaked: {names:?}"
    );
}

mod symbol_source_tests {
    use super::super::*;
    use copperpour_schematic_editor::library::SymbolLibrarySource;

    /// The bug this fixes: a library registered in the project table was
    /// invisible to placement, which only scanned the KiCad install dirs.
    #[test]
    fn project_table_entry_is_offered_before_the_install_dirs() {
        let proj = tempfile::tempdir().unwrap();
        let lib = proj.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let file = lib.join("vendor-parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        std::fs::write(
            proj.path().join("sym-lib-table"),
            format!(
                "(sym_lib_table\n  (version 7)\n  (lib (name \"MyParts\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\n)\n",
                file.display()
            ),
        )
        .unwrap();

        let src = KiCadSymbolSource::new(Some(proj.path().to_path_buf()));
        let candidates = src.candidates("MyParts");

        assert_eq!(
            candidates.first(),
            Some(&file),
            "the project table entry must be tried first, got {candidates:?}"
        );
    }

    /// `${KIPRJMOD}` resolves against the table's own directory, not the env.
    #[test]
    fn kiprjmod_uri_resolves_against_the_project_dir() {
        let proj = tempfile::tempdir().unwrap();
        let lib = proj.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let file = lib.join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        std::fs::write(
            proj.path().join("sym-lib-table"),
            "(sym_lib_table\n  (version 7)\n  (lib (name \"Proj\") (type \"KiCad\") (uri \"${KIPRJMOD}/lib/parts.kicad_sym\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();

        let src = KiCadSymbolSource::new(Some(proj.path().to_path_buf()));
        assert!(
            src.candidates("Proj").contains(&file),
            "${{KIPRJMOD}} must expand to the project dir"
        );
    }

    /// A stock install keeps working with no table entry at all. Points
    /// KICAD10_SYMBOL_DIR at a tempdir rather than asserting against whatever
    /// KiCad is installed — CI has none, so the fallback would have no
    /// directory to derive from and the assertion would be vacuous at best.
    #[test]
    fn unregistered_nickname_falls_back_to_the_conventional_layout() {
        let _env = crate::tools::KICAD_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let install = tempfile::tempdir().unwrap();
        std::env::set_var("KICAD10_SYMBOL_DIR", install.path());

        let src = KiCadSymbolSource::new(None);
        let candidates = src.candidates("Device");
        assert!(
            candidates.contains(&install.path().join("Device.kicad_symdir")),
            "expected the symdir fallback, got {candidates:?}"
        );
        assert!(
            candidates.contains(&install.path().join("Device.kicad_sym")),
            "expected the single-file fallback, got {candidates:?}"
        );
    }

    /// Reported on #136: a sheet under `<proj>/sheets/` saw no project library,
    /// because the table was looked for beside the sheet rather than at the
    /// project root. `add_hierarchical_sheet` accepts such a `sheet_file`, so
    /// this is reachable through Copperpour alone.
    #[test]
    fn a_sheet_in_a_subdirectory_resolves_against_the_project_table() {
        let proj = tempfile::tempdir().unwrap();
        let file = proj.path().join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();
        let (_, child) = crate::tools::schematic_target_tests::native_deep_project(proj.path());
        std::fs::write(
            proj.path().join("sym-lib-table"),
            "(sym_lib_table\n  (version 7)\n  (lib (name \"MyLib\") (type \"KiCad\") (uri \"${KIPRJMOD}/parts.kicad_sym\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();

        let candidates = KiCadSymbolSource::for_file(&child)
            .unwrap()
            .candidates("MyLib");
        assert!(
            candidates.contains(&file),
            "a sub-sheet must see the project table at the root, got {candidates:?}"
        );
    }

    /// The fallback the walk must not break: a schematic belonging to no
    /// project still resolves against the tables sitting beside it.
    #[test]
    fn a_schematic_with_no_project_falls_back_to_its_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();
        std::fs::write(
            dir.path().join("sym-lib-table"),
            "(sym_lib_table\n  (version 7)\n  (lib (name \"Loose\") (type \"KiCad\") (uri \"${KIPRJMOD}/parts.kicad_sym\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();
        let sch = dir.path().join("loose.kicad_sch");
        std::fs::write(&sch, "(kicad_sch)\n").unwrap();

        let candidates = KiCadSymbolSource::for_file(&sch)
            .unwrap()
            .candidates("Loose");
        assert!(
            candidates.contains(&file),
            "a projectless schematic must still use the table beside it, got {candidates:?}"
        );
    }

    /// The walk is unbounded, so an unrelated `.kicad_pro` in any ancestor used
    /// to capture the search and send every lookup to the wrong `KIPRJMOD`. A
    /// project nested inside another project's folder is the realistic case;
    /// the one that actually bit was a stray `.kicad_pro` in the system temp
    /// directory, which quietly defeated the hermetic fixtures of two tests —
    /// they passed on CI, where temp is clean, and failed on any machine that
    /// had accumulated one.
    ///
    /// A table beside the file is the more specific statement and must win.
    #[test]
    fn a_table_beside_the_file_beats_an_unrelated_project_further_up() {
        let outer = tempfile::tempdir().unwrap();
        // An unrelated project sitting above — the interloper.
        std::fs::write(outer.path().join("Unrelated.kicad_pro"), "{}\n").unwrap();
        std::fs::write(
            outer.path().join("sym-lib-table"),
            "(sym_lib_table\n  (version 7)\n  (lib (name \"Shared\") (type \"KiCad\") (uri \"${KIPRJMOD}/wrong.kicad_sym\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();

        let inner = outer.path().join("nested");
        std::fs::create_dir(&inner).unwrap();
        let want = inner.join("right.kicad_sym");
        std::fs::write(&want, "(kicad_symbol_lib)\n").unwrap();
        std::fs::write(
            inner.join("sym-lib-table"),
            "(sym_lib_table\n  (version 7)\n  (lib (name \"Shared\") (type \"KiCad\") (uri \"${KIPRJMOD}/right.kicad_sym\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();
        let sch = inner.join("nested.kicad_sch");
        std::fs::write(&sch, "(kicad_sch)\n").unwrap();

        let candidates = KiCadSymbolSource::for_file(&sch)
            .unwrap()
            .candidates("Shared");
        assert!(
            candidates.contains(&want),
            "the table beside the schematic must win, got {candidates:?}"
        );
        assert!(
            !candidates.contains(&outer.path().join("wrong.kicad_sym")),
            "the outer project's table must not be consulted, got {candidates:?}"
        );
    }

    /// `Path::new("board.kicad_sch").parent()` is `Some("")`, not `None`. The
    /// walk must leave that as-is so a bare relative path keeps resolving
    /// against the working directory. The ancestors are scanned from a fresh
    /// directory, not cargo's cwd, whose ancestors may hold a project (#558).
    #[test]
    fn a_bare_relative_schematic_keeps_an_empty_project_dir() {
        let cwd = tempfile::tempdir().unwrap();
        let cwd_path = cwd.path().to_path_buf();
        assert_eq!(
            project_root_for_from(Path::new("board.kicad_sch"), || Ok(cwd_path)).unwrap(),
            Some(PathBuf::new())
        );
    }

    /// A project above the working directory is a candidate owner of a bare
    /// relative schematic; unable to prove it owns it, the walk refuses.
    #[test]
    fn a_project_above_the_working_directory_makes_a_bare_relative_schematic_conflict() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("x.kicad_pro"), "{}").unwrap();
        let cwd_path = outer.path().join("checkout");
        std::fs::create_dir(&cwd_path).unwrap();
        let result = project_root_for_from(Path::new("board.kicad_sch"), || Ok(cwd_path));
        assert!(
            matches!(
                result,
                Err(crate::tools::SchematicTargetError::ProjectConflict { .. })
            ),
            "got {result:?}"
        );
    }

    #[test]
    fn project_scoped_registration_writes_a_portable_uri() {
        let proj = tempfile::tempdir().unwrap();
        let lib = proj.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let file = lib.join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        assert_eq!(
            project_relative_uri(&file, proj.path()),
            "${KIPRJMOD}/lib/parts.kicad_sym"
        );
    }

    /// `Path::new("board.kicad_pro").parent()` is `Some("")`, and
    /// `strip_prefix("")` returns the whole path — which would emit
    /// `${KIPRJMOD}//abs/path`, resolving nowhere on read.
    #[test]
    fn an_empty_table_dir_does_not_produce_a_rooted_kiprjmod_uri() {
        let other = tempfile::tempdir().unwrap();
        let file = other.path().join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        let uri = project_relative_uri(&file, Path::new(""));
        assert!(
            !uri.contains("KIPRJMOD"),
            "empty project dir must fall back to an absolute uri: {uri}"
        );
        assert_eq!(uri, portable_uri(&file));
    }

    /// A library outside the project keeps an absolute URI, but still written
    /// with forward slashes — a backslash URI does not survive the project
    /// being opened on Linux or macOS.
    #[test]
    fn a_library_outside_the_project_stays_absolute_with_forward_slashes() {
        let proj = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let file = other.path().join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        let uri = project_relative_uri(&file, proj.path());
        assert!(!uri.contains("KIPRJMOD"), "outside the project: {uri}");
        assert!(!uri.contains('\\'), "uri must be slash-separated: {uri}");
    }

    /// Global scope is never relativized, and never carries the Windows
    /// verbatim prefix or backslashes.
    #[test]
    fn global_scope_writes_a_plain_forward_slash_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("parts.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        let (table, uri) = lib_table_target(
            "global",
            None,
            &file,
            Some(PathBuf::from("/cfg/sym-lib-table")),
            "sym-lib-table",
        )
        .expect("global scope needs no project");

        assert_eq!(table, PathBuf::from("/cfg/sym-lib-table"));
        assert!(!uri.contains("KIPRJMOD"), "global is never relative: {uri}");
        assert!(!uri.starts_with(r"\\?\"), "verbatim prefix leaked: {uri}");
        assert!(!uri.contains('\\'), "uri must be slash-separated: {uri}");
    }

    /// Registering a library before it exists on disk is normal — the file is
    /// often created by the very next call. `canonicalize` fails for a path
    /// that is not there yet, so without a lexical fallback the URI silently
    /// came out absolute and the project stopped being portable.
    #[test]
    fn a_library_not_yet_on_disk_still_gets_a_portable_uri() {
        let proj = tempfile::tempdir().unwrap();
        let file = proj.path().join("lib").join("not-created-yet.kicad_sym");

        assert_eq!(
            project_relative_uri(&file, proj.path()),
            "${KIPRJMOD}/lib/not-created-yet.kicad_sym"
        );
    }

    /// Both registrars share one target helper, so footprints get the same
    /// portable URI and the same empty-parent guard as symbols.
    /// The report: passing the project *directory* resolved one level above it,
    /// so the table landed where KiCad never looks and the call still said
    /// `inserted`.
    #[test]
    fn a_project_directory_resolves_to_its_own_table() {
        let parent = tempfile::tempdir().unwrap();
        let proj = parent.path().join("board");
        std::fs::create_dir_all(proj.join("lib")).unwrap();
        let sym = proj.join("lib/parts.kicad_sym");
        std::fs::write(&sym, "(kicad_symbol_lib)\n").unwrap();

        let (table, uri) = lib_table_target(
            "project",
            proj.to_str(),
            &sym,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("a project directory is a usable project argument");

        assert_eq!(table, proj.join("sym-lib-table"));
        assert_ne!(
            table,
            parent.path().join("sym-lib-table"),
            "the table must not land above the project"
        );
        assert_eq!(uri, "${KIPRJMOD}/lib/parts.kicad_sym");
    }

    /// Both spellings of the same project must name the same table, or a caller
    /// gets a different answer depending on which one it happened to pass.
    #[test]
    fn a_project_file_and_its_directory_agree() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join("lib")).unwrap();
        let sym = proj.path().join("lib/parts.kicad_sym");
        std::fs::write(&sym, "(kicad_symbol_lib)\n").unwrap();
        let proj_file = proj.path().join("board.kicad_pro");
        std::fs::write(&proj_file, "{}\n").unwrap();

        let from_file = lib_table_target(
            "project",
            proj_file.to_str(),
            &sym,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("project file");
        let from_dir = lib_table_target(
            "project",
            proj.path().to_str(),
            &sym,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("project directory");

        assert_eq!(from_file, from_dir);
    }

    /// The follow-on failure in the report: with both projects resolving to the
    /// same stray table, registering a nickname for the second one came back
    /// `unchanged` because the handler was reading the first one's entry.
    #[test]
    fn two_projects_under_one_parent_get_separate_tables() {
        let parent = tempfile::tempdir().unwrap();
        let one = parent.path().join("alpha");
        let two = parent.path().join("beta");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let lib = parent.path().join("shared.kicad_sym");
        std::fs::write(&lib, "(kicad_symbol_lib)\n").unwrap();

        let (table_one, _) = lib_table_target(
            "project",
            one.to_str(),
            &lib,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("first project");
        let (table_two, _) = lib_table_target(
            "project",
            two.to_str(),
            &lib,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("second project");

        assert_ne!(
            table_one, table_two,
            "two projects must not share one table"
        );
        assert_eq!(table_one, one.join("sym-lib-table"));
        assert_eq!(table_two, two.join("sym-lib-table"));
    }

    /// A path that is neither a directory nor recognisable as a file is refused
    /// rather than resolved to its parent — guessing is what produced the stray
    /// table in the first place.
    #[test]
    fn an_unusable_project_argument_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("no-such-project");
        let lib = parent.path().join("parts.kicad_sym");
        std::fs::write(&lib, "(kicad_symbol_lib)\n").unwrap();

        let result = lib_table_target(
            "project",
            missing.to_str(),
            &lib,
            global_sym_lib_table(),
            "sym-lib-table",
        );

        assert!(
            result.is_err(),
            "a path that names nothing must not resolve"
        );
        assert!(
            !parent.path().join("sym-lib-table").exists(),
            "nothing may be written above the project"
        );
    }

    /// A `.kicad_pro` that does not exist yet still resolves — the project file
    /// is often written by the very next call.
    #[test]
    fn a_project_file_not_yet_on_disk_still_resolves() {
        let proj = tempfile::tempdir().unwrap();
        let lib = proj.path().join("parts.kicad_sym");
        std::fs::write(&lib, "(kicad_symbol_lib)\n").unwrap();
        let not_yet = proj.path().join("board.kicad_pro");

        let (table, _) = lib_table_target(
            "project",
            not_yet.to_str(),
            &lib,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("a .kicad_pro path resolves whether or not it exists yet");

        assert_eq!(table, proj.path().join("sym-lib-table"));
    }

    /// The footprint registrar shares `lib_table_target`, so the directory form
    /// has to reach it too.
    #[test]
    fn the_footprint_registrar_takes_a_project_directory_as_well() {
        let parent = tempfile::tempdir().unwrap();
        let proj = parent.path().join("board");
        std::fs::create_dir_all(proj.join("lib/parts.pretty")).unwrap();
        let fp = proj.join("lib/parts.pretty");

        let (table, uri) = lib_table_target(
            "project",
            proj.to_str(),
            &fp,
            global_fp_lib_table(),
            "fp-lib-table",
        )
        .expect("footprint target");

        assert_eq!(table, proj.join("fp-lib-table"));
        assert_eq!(uri, "${KIPRJMOD}/lib/parts.pretty");
    }

    #[test]
    fn both_registrars_agree_on_the_project_table_target() {
        let proj = tempfile::tempdir().unwrap();
        let lib = proj.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let sym = lib.join("parts.kicad_sym");
        std::fs::write(&sym, "(kicad_symbol_lib)\n").unwrap();
        let fp = lib.join("parts.pretty");
        std::fs::create_dir_all(&fp).unwrap();

        let proj_file = proj.path().join("board.kicad_pro");
        let proj_arg = proj_file.to_str();

        let (sym_table, sym_uri) = lib_table_target(
            "project",
            proj_arg,
            &sym,
            global_sym_lib_table(),
            "sym-lib-table",
        )
        .expect("symbol target");
        let (fp_table, fp_uri) = lib_table_target(
            "project",
            proj_arg,
            &fp,
            global_fp_lib_table(),
            "fp-lib-table",
        )
        .expect("footprint target");

        assert_eq!(sym_uri, "${KIPRJMOD}/lib/parts.kicad_sym");
        assert_eq!(fp_uri, "${KIPRJMOD}/lib/parts.pretty");
        assert_eq!(sym_table, proj.path().join("sym-lib-table"));
        assert_eq!(fp_table, proj.path().join("fp-lib-table"));
    }

    #[test]
    fn a_library_outside_the_project_keeps_its_absolute_uri() {
        let proj = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let file = other.path().join("shared.kicad_sym");
        std::fs::write(&file, "(kicad_symbol_lib)\n").unwrap();

        let uri = project_relative_uri(&file, proj.path());
        assert!(
            !uri.contains("KIPRJMOD"),
            "nothing portable to say about an out-of-project library: {uri}"
        );
    }
}
