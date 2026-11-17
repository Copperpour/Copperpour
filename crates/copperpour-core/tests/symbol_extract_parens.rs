//! #572: symbols with unbalanced parentheses or escaped quotes inside strings
//! must resolve, embed cleanly into `lib_symbols`, and suggest only top-level
//! symbols (never `_0_1`/`_1_1` unit sub-symbols).
use copperpour_core::{mcp::handler::McpHandler, tools::ServerConfig};
use serde_json::{json, Value};

const TEST_SYM_LIB: &str = "\
(kicad_symbol_lib
\t(version 20241209)
\t(generator \"kicad_symbol_editor\")
\t(symbol \"Conn_02x20_Row_Letter_Last\"
\t\t(property \"Reference\" \"J\" (at 0 0 0) (effects (font (size 1.27 1.27))))
\t\t(property \"Value\" \"Conn_02x20_Row_Letter_Last\" (at 0 2.54 0) (effects (font (size 1.27 1.27))))
\t\t(property \"Footprint\" \"\" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
\t\t(property \"Datasheet\" \"~\" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
\t\t(property \"Description\" \"Generic connector, double row, 02x20, row letter last pin numbering scheme (pin 1a, 1b, ..., 20b)), script generated (quote \\\")\\\")\" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
\t\t(symbol \"Conn_02x20_Row_Letter_Last_0_1\"
\t\t\t(rectangle (start -2.54 -2.54) (end 2.54 2.54))
\t\t)
\t\t(symbol \"Conn_02x20_Row_Letter_Last_1_1\"
\t\t\t(pin passive line (at -5.08 0 0) (length 2.54)
\t\t\t\t(name \"Pin_1a\" (effects (font (size 1.27 1.27))))
\t\t\t\t(number \"1a\" (effects (font (size 1.27 1.27))))
\t\t\t)
\t\t)
\t)
\t(symbol \"STM32H503CBTx\"
\t\t(property \"Reference\" \"U\" (at 0 0 0) (effects (font (size 1.27 1.27))))
\t\t(property \"Value\" \"STM32H503CBTx\" (at 0 2.54 0) (effects (font (size 1.27 1.27))))
\t\t(property \"Footprint\" \"\" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
\t\t(property \"Datasheet\" \"~\" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
\t\t(symbol \"STM32H503CBTx_0_1\"
\t\t\t(pin bidirectional line (at -5.08 0 0) (length 2.54)
\t\t\t\t(name \"PA13(JTMS\" (effects (font (size 1.27 1.27))))
\t\t\t\t(number \"34\" (effects (font (size 1.27 1.27))))
\t\t\t)
\t\t)
\t)
)
";

async fn handler() -> McpHandler {
    McpHandler::new(ServerConfig {
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn call_tool(h: &McpHandler, tool: &str, args: Value) -> (bool, String) {
    let response = h
        .handle_message(json!({
            "jsonrpc": "2.0",
            "id": 572,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": args
            }
        }))
        .await
        .unwrap();
    let result = response.result.expect("JSON-RPC success");
    let is_error = result["isError"] == true;
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (is_error, text)
}

#[tokio::test]
async fn add_schematic_component_handles_unbalanced_parens_in_strings_and_suggestions() {
    let h = handler().await;
    let dir = tempfile::tempdir().unwrap();
    let sch_path = dir.path().join("board.kicad_sch");
    let sym_path = dir.path().join("TestLib.kicad_sym");
    std::fs::write(&sym_path, TEST_SYM_LIB).unwrap();
    std::fs::write(
        dir.path().join("sym-lib-table"),
        format!(
            "(sym_lib_table\n  (version 7)\n  (lib (name \"TestLib\")(type \"KiCad\")(uri \"{}\")(options \"\")(descr \"\"))\n)\n",
            sym_path.display()
        ),
    )
    .unwrap();

    let (err, out) = call_tool(
        &h,
        "create_schematic",
        json!({
            "path": sch_path
        }),
    )
    .await;
    assert!(!err, "create_schematic failed: {out}");

    // 1. Place a symbol with an unbalanced '(' in a pin name.
    let (err, out) = call_tool(
        &h,
        "add_schematic_component",
        json!({
            "schematic": sch_path,
            "lib_id": "TestLib:STM32H503CBTx",
            "reference": "U1",
            "x": 100.0,
            "y": 100.0
        }),
    )
    .await;
    assert!(!err, "first add_schematic_component failed: {out}");

    // 2. Replace a component with another symbol (whose Description has an
    //    unbalanced ')' and '\"' escape) via replace_component, which
    //    calls ensure_lib_symbol_in_schematic: it must find the real closing
    //    paren of lib_symbols rather than running past it due to U1's
    //    unbalanced '('.
    let (err, out) = call_tool(
        &h,
        "add_schematic_component",
        json!({
            "schematic": sch_path,
            "lib_id": "TestLib:STM32H503CBTx",
            "reference": "U2",
            "x": 150.0,
            "y": 100.0
        }),
    )
    .await;
    assert!(!err, "second add_schematic_component failed: {out}");

    let (err, out) = call_tool(
        &h,
        "replace_component",
        json!({
            "schematic": sch_path,
            "reference": "U2",
            "new_lib_id": "TestLib:Conn_02x20_Row_Letter_Last"
        }),
    )
    .await;
    assert!(!err, "replace_component failed: {out}");

    // Verify the resulting schematic parses cleanly and both symbols are direct
    // children of lib_symbols.
    let content = std::fs::read_to_string(&sch_path).unwrap();
    copperpour_sexp::parser::parse_sexp(&content).expect("schematic must parse as valid sexp");
    let lib_children = copperpour_sexp::writer::find_direct_child_blocks(&content, "lib_symbols");
    assert_eq!(
        lib_children.len(),
        2,
        "both symbols must be direct children of lib_symbols:\n{content}"
    );

    // 3. Misspelled name must suggest the top-level symbol and never its
    //    _0_1/_1_1 unit sub-symbols.
    let (err, out) = call_tool(
        &h,
        "add_schematic_component",
        json!({
            "schematic": sch_path,
            "lib_id": "TestLib:Conn_02x20_Row_Letter",
            "reference": "J2",
            "x": 200.0,
            "y": 100.0
        }),
    )
    .await;
    assert!(err, "misspelled symbol must error");
    assert!(
        out.contains("TestLib:Conn_02x20_Row_Letter_Last"),
        "expected top-level suggestion, got: {out}"
    );
    assert!(
        !out.contains("_0_1") && !out.contains("_1_1"),
        "unit sub-symbols must not appear in suggestions: {out}"
    );
}
