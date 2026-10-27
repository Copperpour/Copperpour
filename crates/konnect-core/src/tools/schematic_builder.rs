//! SchematicBuilder — Structured writer for .kicad_sch files.
//!
//! KiCAD 10's parser requires elements in a specific order. This builder
//! parses an existing schematic into sections, allows adding elements to
//! the correct section, and serializes back with guaranteed valid ordering.
//!
//! Element order (enforced by this builder):
//!   1. Header (version, generator, uuid, paper, title_block)
//!   2. lib_symbols (library symbol definitions)
//!   3. Junctions, no_connects
//!   4. Wires, buses, bus_entries
//!   5. Text annotations
//!   6. Labels (net_label, global_label, hierarchical_label)
//!   7. Symbol instances (ALWAYS LAST)

use konnect_sexp::{
    parse_sexp,
    writer::{
        find_balanced_block, find_block_starts, find_direct_child_blocks, read_consistent,
        write_atomic_if_unchanged, write_new_atomic,
    },
    SexpError,
};
use std::path::{Path, PathBuf};

/// Structured representation of a .kicad_sch file.
/// Each section holds raw S-expression strings that are written in order.
pub struct SchematicBuilder {
    source_revision: Option<(PathBuf, String)>,
    lib_symbol_extras: Vec<String>,
    preserved_before_symbols: Vec<String>,
    preserved_after_symbols: Vec<String>,
    /// Everything before lib_symbols: version, generator, uuid, paper, title_block
    pub header: String,
    /// Contents inside (lib_symbols ...) — each entry is a complete (symbol "Lib:Name" ...) block
    pub lib_symbols: Vec<String>,
    /// Junction dots
    pub junctions: Vec<String>,
    /// No-connect flags
    pub no_connects: Vec<String>,
    /// Wire segments
    pub wires: Vec<String>,
    /// Bus segments
    pub buses: Vec<String>,
    /// Bus entry points
    pub bus_entries: Vec<String>,
    /// Text annotations
    pub texts: Vec<String>,
    /// Net labels (net_label, global_label, hierarchical_label)
    pub labels: Vec<String>,
    /// Symbol instances — ALWAYS serialized last
    pub symbols: Vec<String>,
}

impl Default for SchematicBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SchematicBuilder {
    /// Create an empty schematic with KiCAD 10 header.
    pub fn new() -> Self {
        let uuid = konnect_sexp::writer::new_uuid();
        SchematicBuilder {
            source_revision: None,
            lib_symbol_extras: Vec::new(),
            preserved_before_symbols: Vec::new(),
            preserved_after_symbols: Vec::new(),
            header: format!(
                "(kicad_sch\n\t(version 20250610)\n\t(generator \"copperpour\")\n\t(generator_version \"10.0\")\n\t(uuid \"{}\")\n\t(paper \"A4\")",
                uuid
            ),
            lib_symbols: Vec::new(),
            junctions: Vec::new(),
            no_connects: Vec::new(),
            wires: Vec::new(),
            buses: Vec::new(),
            bus_entries: Vec::new(),
            texts: Vec::new(),
            labels: Vec::new(),
            symbols: Vec::new(),
        }
    }

    /// Parse an existing .kicad_sch file into structured sections.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = read_consistent(path)?;
        let mut builder = Self::parse(&content)?;
        builder.source_revision = Some((path.to_path_buf(), content));
        Ok(builder)
    }

    /// Parse schematic content into structured sections.
    pub fn parse(content: &str) -> anyhow::Result<Self> {
        let mut builder = SchematicBuilder {
            source_revision: None,
            lib_symbol_extras: Vec::new(),
            preserved_before_symbols: Vec::new(),
            preserved_after_symbols: Vec::new(),
            header: String::new(),
            lib_symbols: Vec::new(),
            junctions: Vec::new(),
            no_connects: Vec::new(),
            wires: Vec::new(),
            buses: Vec::new(),
            bus_entries: Vec::new(),
            texts: Vec::new(),
            labels: Vec::new(),
            symbols: Vec::new(),
        };

        let root_start = find_block_starts(content, "kicad_sch")
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing kicad_sch root"))?;
        let (root_start, root_end) = find_balanced_block(content, root_start)
            .ok_or_else(|| anyhow::anyhow!("unbalanced kicad_sch root"))?;
        if !content[..root_start].trim().is_empty() || !content[root_end..].trim().is_empty() {
            anyhow::bail!("schematic must contain exactly one kicad_sch root");
        }
        let root = parse_sexp(&content[root_start..root_end])?;
        if root.head() != Some("kicad_sch") {
            anyhow::bail!("schematic root is not kicad_sch");
        }

        let direct = find_direct_child_blocks(content, "kicad_sch");
        let tagged = direct
            .iter()
            .map(|&(start, end)| {
                let node = parse_sexp(&content[start..end])?;
                let tag = node
                    .head()
                    .ok_or_else(|| anyhow::anyhow!("top-level item at byte {start} has no tag"))?;
                Ok((start, end, tag.to_owned()))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let lib_ranges = tagged
            .iter()
            .filter(|(_, _, tag)| tag == "lib_symbols")
            .collect::<Vec<_>>();
        if lib_ranges.len() > 1 {
            anyhow::bail!("schematic has more than one top-level lib_symbols block");
        }

        const HEADER_TAGS: &[&str] = &[
            "version",
            "generator",
            "generator_version",
            "uuid",
            "paper",
            "title_block",
        ];
        let first_body = tagged
            .iter()
            .find(|(_, _, tag)| !HEADER_TAGS.contains(&tag.as_str()));
        if let Some(body) = first_body {
            if tagged
                .iter()
                .any(|(start, _, tag)| *start > body.0 && HEADER_TAGS.contains(&tag.as_str()))
            {
                anyhow::bail!("schematic header item appears after a body item");
            }
        }
        if let Some(lib) = lib_ranges.first() {
            if first_body.is_some_and(|body| body.0 != lib.0) {
                anyhow::bail!("lib_symbols must precede schematic body items");
            }
        }
        let header_end = lib_ranges
            .first()
            .map(|range| range.0)
            .or_else(|| first_body.map(|range| range.0))
            .unwrap_or(root_end - 1);
        builder.header = content[..header_end].trim_end().to_owned();

        let body_start = if let Some((start, end, _)) = lib_ranges.first().copied() {
            let lib_source = &content[*start..*end];
            for (child_start, child_end) in find_direct_child_blocks(lib_source, "lib_symbols") {
                let child = &lib_source[child_start..child_end];
                let node = parse_sexp(child)?;
                if node.head() == Some("symbol") {
                    builder.lib_symbols.push(child.to_owned());
                } else {
                    builder.lib_symbol_extras.push(child.to_owned());
                }
            }
            *end
        } else {
            header_end
        };

        let mut seen_symbol = false;
        for (start, end, tag) in tagged {
            if start < body_start || tag == "lib_symbols" || HEADER_TAGS.contains(&tag.as_str()) {
                continue;
            }
            let block = content[start..end].to_owned();
            match tag.as_str() {
                "junction" => builder.junctions.push(block),
                "no_connect" => builder.no_connects.push(block),
                "wire" => builder.wires.push(block),
                "bus" => builder.buses.push(block),
                "bus_entry" => builder.bus_entries.push(block),
                "text" | "text_box" => builder.texts.push(block),
                "net_label" | "global_label" | "hierarchical_label" | "label" => {
                    builder.labels.push(block)
                }
                "symbol" => {
                    seen_symbol = true;
                    builder.symbols.push(block);
                }
                "sheet_instances" | "symbol_instances" | "embedded_fonts" => {
                    builder.preserved_after_symbols.push(block)
                }
                _ if seen_symbol => builder.preserved_after_symbols.push(block),
                _ => builder.preserved_before_symbols.push(block),
            }
        }

        Ok(builder)
    }

    /// Add a lib_symbol definition (if not already present).
    pub fn add_lib_symbol(&mut self, definition: &str) {
        // Check if already present by matching the symbol name
        if let Some(name_start) = definition.find("(symbol \"") {
            let after = &definition[name_start + 9..];
            if let Some(name_end) = after.find('"') {
                let name = &after[..name_end];
                if self
                    .lib_symbols
                    .iter()
                    .any(|s| s.contains(&format!("(symbol \"{}\"", name)))
                {
                    return; // Already present
                }
            }
        }
        self.lib_symbols.push(definition.to_string());
    }

    /// Add a wire segment.
    pub fn add_wire(&mut self, sexp: &str) {
        self.wires.push(sexp.trim().to_string());
    }

    /// Add a junction.
    pub fn add_junction(&mut self, sexp: &str) {
        self.junctions.push(sexp.trim().to_string());
    }

    /// Add a no-connect flag.
    pub fn add_no_connect(&mut self, sexp: &str) {
        self.no_connects.push(sexp.trim().to_string());
    }

    /// Add a label (net_label, global_label, hierarchical_label).
    pub fn add_label(&mut self, sexp: &str) {
        self.labels.push(sexp.trim().to_string());
    }

    /// Add a text annotation.
    pub fn add_text(&mut self, sexp: &str) {
        self.texts.push(sexp.trim().to_string());
    }

    /// Add a symbol instance (always serialized last).
    pub fn add_symbol(&mut self, sexp: &str) {
        self.symbols.push(sexp.trim().to_string());
    }

    /// Serialize to a valid .kicad_sch string with correct element ordering.
    /// (Deliberately an inherent method — this is a file serialization, not a
    /// human-readable Display.)
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let mut out = String::new();

        // Header
        out.push_str(&self.header);
        out.push('\n');

        // lib_symbols
        out.push_str("\t(lib_symbols\n");
        for sym in &self.lib_symbols {
            out.push_str("\t\t");
            out.push_str(sym);
            out.push('\n');
        }
        for item in &self.lib_symbol_extras {
            out.push_str("\t\t");
            out.push_str(item);
            out.push('\n');
        }
        out.push_str("\t)\n");

        // Junctions
        for item in &self.junctions {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // No-connects
        for item in &self.no_connects {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Wires
        for item in &self.wires {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Buses
        for item in &self.buses {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Bus entries
        for item in &self.bus_entries {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Text
        for item in &self.texts {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Labels
        for item in &self.labels {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        for item in &self.preserved_before_symbols {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Symbols — ALWAYS LAST
        for item in &self.symbols {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        for item in &self.preserved_after_symbols {
            out.push_str("  ");
            out.push_str(item);
            out.push('\n');
        }

        // Close the root kicad_sch
        out.push_str(")\n");

        out
    }

    /// Save a loaded document with its exact revision precondition, or create a
    /// new destination without replacing anything already present.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let content = self.to_string();
        Self::parse(&content)?;
        if let Some((source_path, expected)) = &self.source_revision {
            if source_path == path {
                write_atomic_if_unchanged(path, expected, &content)?;
                return verify_saved_schematic(path, &content);
            }
        }
        write_new_atomic(path, &content)?;
        verify_saved_schematic(path, &content)
    }
}

fn verify_saved_schematic(path: &Path, expected: &str) -> anyhow::Result<()> {
    let observed = read_consistent(path)?;
    if observed != expected {
        return Err(SexpError::Conflict {
            path: path.to_path_buf(),
        }
        .into());
    }
    SchematicBuilder::parse(&observed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KICAD_STRUCTURAL_FIXTURE: &str =
        include_str!("../../tests/fixtures/structural_scans_kicad10.kicad_sch");

    fn hierarchical_fixture() -> String {
        r#"(kicad_sch
	(version 20250610)
	(generator "eeschema")
	(generator_version "10.0")
	(uuid "root-id")
	(paper "A4")
	(title_block (title "Controller (rev A)"))
	(lib_symbols
		(symbol "Amplifier:DUAL"
			(property "Value" "Dual (op amp)")
		)
		(future_library_metadata (payload "quoted ) text"))
	)
	(junction (at 10 10) (uuid "j1"))
	(wire (pts (xy 10 10) (xy 20 10)) (uuid "w1"))
	(text "note (do not split)" (at 5 5 0) (uuid "t1"))
	(label "SIG" (at 20 10 0) (uuid "l1"))
	(future_item (payload "unknown (direct) child") (uuid "future-1"))
	(symbol
		(lib_id "Amplifier:DUAL")
		(at 30 30 0)
		(unit 1)
		(property "Reference" "U1")
		(uuid "u1-unit-1")
		(instances (project "root" (path "/sheet-a/u1" (reference "U1") (unit 1))))
	)
	(symbol
		(lib_id "Amplifier:DUAL")
		(at 40 30 0)
		(unit 2)
		(property "Reference" "U1")
		(uuid "u1-unit-2")
		(instances (project "root" (path "/sheet-a/u1" (reference "U1") (unit 2))))
	)
	(sheet
		(at 60 40)
		(size 25 20)
		(property "Sheetname" "Child")
		(property "Sheetfile" "child.kicad_sch")
		(uuid "sheet-a")
	)
	(future_footer (payload "keep me") (uuid "future-footer"))
	(sheet_instances (path "/" (page "1")) (path "/sheet-a" (page "2")))
	(symbol_instances
		(path "/sheet-a/u1" (reference "U1") (unit 1) (value "DUAL"))
	)
	(embedded_fonts no)
)
"#
        .replace('\n', "\r\n")
    }

    #[test]
    fn empty_builder_produces_valid_structure() {
        let builder = SchematicBuilder::new();
        let output = builder.to_string();
        assert!(output.starts_with("(kicad_sch"));
        assert!(output.contains("(version 20250610)"));
        assert!(output.contains("(generator \"copperpour\")"), "{output}");
        assert!(output.contains("(lib_symbols"));
        assert!(output.ends_with(")\n"));
    }

    #[test]
    fn new_builder_save_refuses_to_replace_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.kicad_sch");
        std::fs::write(&path, "keep me").unwrap();

        let error = SchematicBuilder::new().save(&path).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<konnect_sexp::SexpError>(),
            Some(konnect_sexp::SexpError::Io(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me");
    }

    #[test]
    fn elements_are_ordered_correctly() {
        let mut builder = SchematicBuilder::new();
        // Add in wrong order — builder should serialize in correct order
        builder.add_symbol("(symbol (lib_id \"Device:R\") (at 100 100 0) (uuid \"sym1\"))");
        builder
            .add_wire("(wire (pts (xy 100 90) (xy 100 100)) (stroke (width 0)) (uuid \"wire1\"))");
        builder.add_label("(net_label \"VCC\" (at 100 85 0) (uuid \"label1\"))");
        builder.add_junction("(junction (at 100 90) (uuid \"junc1\"))");

        let output = builder.to_string();

        // Verify order: junction < wire < label < symbol
        let junc_pos = output.find("(junction").unwrap();
        let wire_pos = output.find("(wire").unwrap();
        let label_pos = output.find("(net_label").unwrap();
        let sym_pos = output.find("(symbol").unwrap();

        assert!(junc_pos < wire_pos, "junction should come before wire");
        assert!(wire_pos < label_pos, "wire should come before label");
        assert!(label_pos < sym_pos, "label should come before symbol");
    }

    #[test]
    fn parse_and_reserialize_preserves_elements() {
        let input = r#"(kicad_sch
	(version 20250610)
	(generator "konnect")
	(generator_version "10.0")
	(paper "A4")
	(lib_symbols
	)
  (wire (pts (xy 100 90) (xy 100 100)) (stroke (width 0) (type default)) (uuid "w1"))
  (net_label "VCC" (at 100 85 0) (effects (font (size 1.27 1.27))) (uuid "l1"))
  (symbol
    (lib_id "Device:R")
    (at 100 100 0)
    (uuid "s1")
    (property "Reference" "R1" (at 100 96 0) (effects (font (size 1.27 1.27))))
    (instances (project "" (path "/" (reference "R1") (unit 1))))
  )
)
"#;

        let builder = SchematicBuilder::parse(input).unwrap();
        assert_eq!(builder.wires.len(), 1);
        assert_eq!(builder.labels.len(), 1);
        assert_eq!(builder.symbols.len(), 1);

        let output = builder.to_string();
        assert!(output.contains("(wire"));
        assert!(output.contains("(net_label"));
        assert!(output.contains("(symbol"));
    }

    #[test]
    fn kicad_authored_sections_reload_after_builder_reserialization() {
        let builder = SchematicBuilder::parse(KICAD_STRUCTURAL_FIXTURE).unwrap();
        assert_eq!(builder.lib_symbols.len(), 1);
        assert_eq!(builder.junctions.len(), 3);
        assert_eq!(builder.no_connects.len(), 1);
        assert_eq!(builder.wires.len(), 7);
        assert_eq!(builder.buses.len(), 2);
        assert_eq!(builder.labels.len(), 1);
        assert_eq!(builder.symbols.len(), 2);

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("structural_scans.kicad_sch");
        std::fs::write(&path, KICAD_STRUCTURAL_FIXTURE).unwrap();
        let loaded = SchematicBuilder::from_file(&path).unwrap();
        loaded.save(&path).unwrap();

        let observed = std::fs::read_to_string(path).unwrap();
        let reparsed = SchematicBuilder::parse(&observed).unwrap();
        assert_eq!(reparsed.wires.len(), 7);
        assert_eq!(reparsed.symbols.len(), 2);
        for identity in [
            "11111111-2222-4333-8444-555555555555",
            "5bb775af-80e4-4b9f-b739-822ba959ac32",
            "C25804",
        ] {
            assert!(observed.contains(identity), "missing {identity}");
        }
        parse_sexp(&observed).expect("builder output from KiCad fixture must reload");
    }

    #[test]
    fn tab_crlf_hierarchy_multi_unit_and_unknown_children_round_trip_semantically() {
        let input = hierarchical_fixture();
        let builder = SchematicBuilder::parse(&input).unwrap();
        assert_eq!(builder.lib_symbols.len(), 1);
        assert_eq!(builder.wires.len(), 1);
        assert_eq!(builder.texts.len(), 1);
        assert_eq!(builder.symbols.len(), 2);
        assert_eq!(builder.preserved_before_symbols.len(), 1);
        assert_eq!(builder.preserved_after_symbols.len(), 5);
        assert_eq!(builder.lib_symbol_extras.len(), 1);

        let output = builder.to_string();
        assert_eq!(parse_sexp(&output).unwrap(), parse_sexp(&input).unwrap());
        for identity in [
            "u1-unit-1",
            "u1-unit-2",
            "sheet-a",
            "/sheet-a/u1",
            "future-1",
            "future-footer",
            "future_library_metadata",
        ] {
            assert!(output.contains(identity), "missing {identity}");
        }
    }

    #[test]
    fn malformed_or_duplicate_structural_roots_are_refused() {
        assert!(SchematicBuilder::parse("(kicad_sch (version 1)").is_err());
        assert!(
            SchematicBuilder::parse("(kicad_sch (version 1) (lib_symbols) (lib_symbols))").is_err()
        );
        assert!(
            SchematicBuilder::parse("(kicad_sch (version 1)) (kicad_sch (version 2))").is_err()
        );
        assert!(SchematicBuilder::parse(
            "(kicad_sch (lib_symbols) (wire (start 0 0) (end 1 1)) (paper \"A4\"))"
        )
        .is_err());
    }

    #[test]
    fn loaded_builder_refuses_a_stale_source_without_overwriting_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stale.kicad_sch");
        let original = hierarchical_fixture();
        std::fs::write(&path, &original).unwrap();
        let mut builder = SchematicBuilder::from_file(&path).unwrap();
        builder.add_wire("(wire (pts (xy 1 1) (xy 2 2)) (uuid \"new-wire\"))");

        let newer = original.replace("Controller (rev A)", "Controller (rev B)");
        std::fs::write(&path, &newer).unwrap();
        let error = builder.save(&path).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<SexpError>(),
            Some(SexpError::Conflict { .. })
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), newer);
    }

    #[test]
    fn loaded_builder_cannot_overwrite_a_different_document() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.kicad_sch");
        let other = directory.path().join("other.kicad_sch");
        let original = hierarchical_fixture();
        std::fs::write(&source, &original).unwrap();
        std::fs::write(&other, "keep other").unwrap();
        let builder = SchematicBuilder::from_file(&source).unwrap();

        let error = builder.save(&other).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<SexpError>(),
            Some(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read_to_string(other).unwrap(), "keep other");
        assert_eq!(std::fs::read_to_string(source).unwrap(), original);
    }

    #[test]
    fn saved_result_is_reparsed_and_contains_new_items() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("observed.kicad_sch");
        let mut builder = SchematicBuilder::new();
        builder.add_wire("(wire (pts (xy 1 1) (xy 2 2)) (uuid \"observed-wire\"))");

        builder.save(&path).unwrap();

        let observed = std::fs::read_to_string(path).unwrap();
        assert!(observed.contains("observed-wire"));
        assert_eq!(SchematicBuilder::parse(&observed).unwrap().wires.len(), 1);
    }
}
