//! A JLCPCB order link (#208): the board's Gerbers, BOM and CPL uploaded the
//! way EasyEDA Pro's "Order PCB" uploads them, so that JLCPCB's quote page
//! opens with the board and its assembly files already loaded.
//!
//! The upload goes to EasyEDA's order endpoint, which is not documented and
//! takes no login. It answers with a JLCPCB link to the uploaded files, as
//! `{"success":true,"code":0,"result":{"url":"https://jlcpcb.com/quote/eda?eadLink=2&uuid=…","id":"…"}}`
//! (seen 2026-11-06). The link asks for a JLCPCB login, then opens the quote
//! page with the board loaded and the BOM and CPL in its PCBA steps.
//! Copperpour names itself in the User-Agent and sends no browser headers.
//!
//! The BOM and CPL are EasyEDA's: UTF-16LE with a byte-order mark, separated
//! by tabs. JLCPCB's parser does not read the UTF-8 CSVs of
//! `export_manufacturing_package` from this endpoint.

use anyhow::{bail, Context};
use std::collections::BTreeMap;

/// Where EasyEDA Pro posts an order.
pub(crate) const EASYEDA_ORDER_BASE: &str = "https://pro.easyeda.com";

/// The BOM columns the order tool asks kicad-cli for, and the labels it
/// reads them back by.
pub(crate) const BOM_FIELDS: &str = "Reference,Value,Footprint,MPN,Manufacturer,LCSC,${QUANTITY}";
pub(crate) const BOM_LABELS: &str = "Designator,Value,Footprint,MPN,Manufacturer,LCSC,Quantity";
pub(crate) const BOM_GROUP_BY: &str = "Value,Footprint,MPN,Manufacturer,LCSC";

/// `text` as UTF-16 little-endian, after the byte-order mark `FF FE`.
pub(crate) fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// One BOM row: a group of identical parts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BomLine {
    pub designators: Vec<String>,
    pub value: String,
    pub footprint: String,
    pub mpn: String,
    pub manufacturer: String,
    pub lcsc: String,
}

/// The rows of a BOM kicad-cli wrote with [`BOM_FIELDS`] and [`BOM_LABELS`].
pub(crate) fn bom_lines(source: &str) -> anyhow::Result<Vec<BomLine>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(source.as_bytes());
    let headers = reader.headers()?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .with_context(|| format!("the order BOM has no '{name}' column"))
    };
    let (designator, value, footprint, mpn, manufacturer, lcsc) = (
        column("Designator")?,
        column("Value")?,
        column("Footprint")?,
        column("MPN")?,
        column("Manufacturer")?,
        column("LCSC")?,
    );
    let mut lines = Vec::new();
    for row in reader.records() {
        let row = row?;
        let cell = |index: usize| row.get(index).unwrap_or("").trim().to_owned();
        let designators: Vec<String> = cell(designator)
            .split(',')
            .map(str::trim)
            .filter(|reference| !reference.is_empty())
            .map(str::to_owned)
            .collect();
        if designators.is_empty() {
            continue;
        }
        lines.push(BomLine {
            designators,
            value: cell(value),
            footprint: without_library(&cell(footprint)).to_owned(),
            mpn: cell(mpn),
            manufacturer: cell(manufacturer),
            lcsc: cell(lcsc),
        });
    }
    if lines.is_empty() {
        bail!("the order BOM lists no parts");
    }
    Ok(lines)
}

/// EasyEDA's BOM: tab-separated, unquoted, `\n` line ends.
pub(crate) fn easyeda_bom(lines: &[BomLine]) -> String {
    let mut text = String::from(
        "No.\tQuantity\tComment\tDesignator\tFootprint\tValue\tManufacturer Part\tManufacturer\tSupplier Part\tSupplier\n",
    );
    for (index, line) in lines.iter().enumerate() {
        let supplier = if line.lcsc.is_empty() { "" } else { "LCSC" };
        let cells = [
            (index + 1).to_string(),
            line.designators.len().to_string(),
            line.value.clone(),
            line.designators.join(","),
            line.footprint.clone(),
            line.value.clone(),
            line.mpn.clone(),
            line.manufacturer.clone(),
            line.lcsc.clone(),
            supplier.to_owned(),
        ];
        text.push_str(
            &cells
                .iter()
                .map(|cell| unquoted_cell(cell))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        text.push('\n');
    }
    text
}

/// One placed part, in board coordinates with Y up, as the CPL gives them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CplLine {
    pub designator: String,
    pub value: String,
    pub footprint: String,
    pub mid_mm: (f64, f64),
    pub reference_mm: (f64, f64),
    pub pad_mm: (f64, f64),
    pub pins: usize,
    pub bottom: bool,
    pub rotation_degrees: f64,
    pub smd: bool,
}

/// The CPL's parts: position, side and rotation from the JLCPCB CPL, after
/// its rotation corrections; value and footprint from KiCad's position file;
/// anchor, pad 1, pads and SMD from the native snapshot.
pub(crate) fn cpl_lines(
    jlcpcb_cpl: &[u8],
    kicad_positions: &str,
    midpoints: &BTreeMap<String, super::midpoints::Midpoint>,
) -> anyhow::Result<Vec<CplLine>> {
    let mut kicad = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(kicad_positions.as_bytes());
    let headers = kicad.headers()?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .with_context(|| format!("KiCad position CSV is missing '{name}'"))
    };
    let (reference, value, package) = (column("Ref")?, column("Val")?, column("Package")?);
    let mut parts = BTreeMap::new();
    for row in kicad.records() {
        let row = row?;
        let get = |index: usize| row.get(index).unwrap_or("").to_owned();
        parts.insert(get(reference), (get(value), get(package)));
    }

    let mut cpl = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(jlcpcb_cpl);
    let mut lines = Vec::new();
    for row in cpl.records() {
        let row = row?;
        let field = |index: usize, name: &str| {
            row.get(index)
                .with_context(|| format!("JLCPCB CPL row is missing {name}"))
        };
        let number = |index: usize, name: &str| -> anyhow::Result<f64> {
            let text = field(index, name)?;
            text.parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .with_context(|| format!("JLCPCB CPL {name} '{text}' is not a number"))
        };
        let designator = field(0, "Designator")?.to_owned();
        let mid_mm = (number(1, "Mid X")?, number(2, "Mid Y")?);
        let bottom = match field(3, "Layer")? {
            "top" => false,
            "bottom" => true,
            other => bail!("JLCPCB CPL layer '{other}' of {designator} is neither top nor bottom"),
        };
        let rotation_degrees = number(4, "Rotation")?;
        let (value, footprint) = parts
            .get(&designator)
            .with_context(|| format!("KiCad's position file has no {designator}"))?;
        let native = midpoints
            .get(&designator)
            .with_context(|| format!("no native geometry for {designator}"))?;
        // The snapshot is in KiCad's board space, Y down; the CPL's Y is up.
        let reference_mm = (native.anchor_x_mm, -native.anchor_y_mm);
        let pad_mm = native.pin1_mm.map_or(mid_mm, |(x, y)| (x, -y));
        lines.push(CplLine {
            designator,
            value: value.clone(),
            footprint: without_library(footprint).to_owned(),
            mid_mm,
            reference_mm,
            pad_mm,
            pins: native.pad_count,
            bottom,
            rotation_degrees,
            smd: native.smd,
        });
    }
    if lines.is_empty() {
        bail!("the CPL lists no parts");
    }
    Ok(lines)
}

/// EasyEDA's CPL: tab-separated, `\r\n` line ends, the header unquoted and
/// every data cell quoted, coordinates in mm with the unit.
pub(crate) fn easyeda_cpl(lines: &[CplLine]) -> String {
    let mut text = String::from(
        "Designator\tDevice\tFootprint\tMid X\tMid Y\tRef X\tRef Y\tPad X\tPad Y\tPins\tLayer\tRotation\tSMD\tComment\tName\r\n",
    );
    for line in lines {
        let cells = [
            line.designator.clone(),
            line.value.clone(),
            line.footprint.clone(),
            millimetres(line.mid_mm.0),
            millimetres(line.mid_mm.1),
            millimetres(line.reference_mm.0),
            millimetres(line.reference_mm.1),
            millimetres(line.pad_mm.0),
            millimetres(line.pad_mm.1),
            line.pins.to_string(),
            if line.bottom { "B" } else { "T" }.to_owned(),
            decimal(line.rotation_degrees),
            if line.smd { "Yes" } else { "No" }.to_owned(),
            line.value.clone(),
            line.value.clone(),
        ];
        text.push_str(
            &cells
                .iter()
                .map(|cell| format!("\"{}\"", unquoted_cell(cell).replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        text.push_str("\r\n");
    }
    text
}

/// What the quote page needs besides the files.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderOptions {
    pub gerber_name: String,
    pub layers: usize,
    pub width_mm: f64,
    pub length_mm: f64,
    pub quantity: u64,
    pub thickness_mm: f64,
    pub color: String,
    pub surface_finish: String,
    pub copper_weight: String,
}

/// The files of an order. Without assembly there is no BOM or CPL.
pub(crate) struct OrderFiles {
    pub gerber_zip: Vec<u8>,
    pub assembly: Option<(Vec<u8>, Vec<u8>)>,
}

/// The `multipart/form-data` body EasyEDA Pro posts: the four files, each
/// named `blob`, then the order's fields as text. The CPL goes twice, as
/// `coordinate` and as `pickAndPlace`.
pub(crate) fn multipart_body(
    files: &OrderFiles,
    options: &OrderOptions,
    boundary: &str,
) -> Vec<u8> {
    let mut body = Vec::new();
    let mut file = |name: &str, content_type: &str, bytes: &[u8]| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"blob\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    };
    file("gerber", "application/zip", &files.gerber_zip);
    if let Some((bom, cpl)) = &files.assembly {
        file("bom", "text/csv", bom);
        file("coordinate", "text/csv", cpl);
        file("pickAndPlace", "text/csv", cpl);
    }
    let fields = [
        ("gerber_name", options.gerber_name.clone()),
        ("edaActivity", "null".to_owned()),
        ("layer", options.layers.to_string()),
        ("width", decimal(options.width_mm)),
        ("length", decimal(options.length_mm)),
        ("qty", options.quantity.to_string()),
        ("thickness", decimal(options.thickness_mm)),
        ("color", options.color.clone()),
        ("surface_finish", options.surface_finish.clone()),
        ("copper_weight", options.copper_weight.clone()),
    ];
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// The endpoint's answer to an upload: JLCPCB's link to the uploaded files,
/// and the upload's id.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OrderLink {
    pub url: String,
    pub id: Option<String>,
}

/// The link in the endpoint's answer. Only an https link to jlcpcb.com is
/// taken: the answer comes from a third party, and the link is handed on.
pub(crate) fn order_link(body: &str) -> anyhow::Result<OrderLink> {
    let json: serde_json::Value = serde_json::from_str(body)
        .with_context(|| format!("the order endpoint did not answer JSON: {}", excerpt(body)))?;
    if json["success"] == false {
        bail!("the order endpoint refused the upload: {}", excerpt(body));
    }
    let url = json["result"]["url"]
        .as_str()
        .with_context(|| format!("the order endpoint's answer has no link: {}", excerpt(body)))?;
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("the order endpoint's link '{url}' is not a URL"))?;
    let on_jlcpcb = parsed
        .host_str()
        .is_some_and(|host| host == "jlcpcb.com" || host.ends_with(".jlcpcb.com"));
    if parsed.scheme() != "https" || !on_jlcpcb {
        bail!("the order endpoint's link '{url}' is not an https link to jlcpcb.com");
    }
    Ok(OrderLink {
        url: url.to_owned(),
        id: json["result"]["id"].as_str().map(str::to_owned),
    })
}

/// The Gerber and drill files, zipped flat, in name order.
pub(crate) fn zip_files(files: &[std::path::PathBuf]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by_key(|path| path.file_name().map(|name| name.to_owned()));
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for path in sorted {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("{} has no file name", path.display()))?;
        writer.start_file(name, options)?;
        writer
            .write_all(&std::fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
    }
    Ok(writer.finish()?.into_inner())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UploadOutcome {
    NotSent,
    Sent,
    Uncertain,
}

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub(crate) struct UploadFailure {
    pub outcome: UploadOutcome,
    pub http_status: Option<u16>,
    reason: String,
}

impl UploadFailure {
    fn before_send(error: reqwest::Error) -> Self {
        Self {
            outcome: UploadOutcome::NotSent,
            http_status: None,
            reason: format!(
                "could not prepare the order upload: {:#}",
                anyhow::Error::new(error)
            ),
        }
    }

    fn transport(error: reqwest::Error) -> Self {
        let outcome = if error.is_connect() || error.is_builder() {
            UploadOutcome::NotSent
        } else {
            UploadOutcome::Uncertain
        };
        Self {
            outcome,
            http_status: None,
            reason: format!(
                "the order upload request failed: {:#}",
                anyhow::Error::new(error)
            ),
        }
    }

    fn response(status: reqwest::StatusCode, reason: String) -> Self {
        Self {
            outcome: UploadOutcome::Sent,
            http_status: Some(status.as_u16()),
            reason,
        }
    }

    pub fn uploaded(&self) -> Option<bool> {
        match self.outcome {
            UploadOutcome::NotSent => Some(false),
            UploadOutcome::Sent => Some(true),
            UploadOutcome::Uncertain => None,
        }
    }

    pub fn recovery(&self) -> &'static str {
        match self.outcome {
            UploadOutcome::NotSent => "Correct the failure before retrying the upload.",
            UploadOutcome::Sent => "The files were sent. Keep order_id and inspect the response before starting another upload.",
            UploadOutcome::Uncertain => "The files may already have reached EasyEDA. Do not retry automatically; keep order_id and inspect the previous upload outcome first.",
        }
    }
}

/// Posts the order and preserves what a failure proves about transmission.
pub(crate) async fn upload(
    base: &str,
    order_id: &str,
    boundary: &str,
    body: Vec<u8>,
) -> Result<String, UploadFailure> {
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "Copperpour/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/Copperpour/Copperpour)"
        ))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(180))
        // Observe the first HTTP response. Following a redirect could send
        // the files again and turn a later connection failure into a false
        // claim that the original request was never sent.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(UploadFailure::before_send)?;
    let request = client
        .post(format!("{base}/api/pcb_order/{order_id}/save"))
        .header("Accept", "*/*")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .build()
        .map_err(UploadFailure::before_send)?;
    let response = client
        .execute(request)
        .await
        .map_err(UploadFailure::transport)?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        UploadFailure::response(
            status,
            format!("the order endpoint answered {status}, but its response body could not be read: {:#}", anyhow::Error::new(error)),
        )
    })?;
    if !status.is_success() {
        return Err(UploadFailure::response(
            status,
            format!("the order endpoint answered {status}: {}", excerpt(&text)),
        ));
    }
    Ok(text)
}

/// The order endpoint's base: tests point it at a local server.
#[cfg(not(test))]
pub(crate) fn order_base(_args: &serde_json::Value) -> &str {
    EASYEDA_ORDER_BASE
}

#[cfg(test)]
pub(crate) fn order_base(args: &serde_json::Value) -> &str {
    args["_order_base_url"]
        .as_str()
        .unwrap_or(EASYEDA_ORDER_BASE)
}

fn without_library(footprint: &str) -> &str {
    footprint.rsplit(':').next().unwrap_or(footprint)
}

/// A cell can't hold the separator or a line end.
fn unquoted_cell(cell: &str) -> String {
    cell.replace(['\t', '\r', '\n'], " ")
}

fn millimetres(value: f64) -> String {
    format!("{}mm", decimal(value))
}

/// At most four decimals, without trailing zeros: `1.6`, `-31.434`, `90`.
fn decimal(value: f64) -> String {
    let text = format!("{:.4}", value);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn excerpt(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(300) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(bytes: &[u8]) -> String {
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "UTF-16LE byte-order mark");
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn text_is_utf16le_after_a_byte_order_mark() {
        let bytes = utf16le_with_bom("R1\tΩ");
        assert_eq!(bytes, [0xFF, 0xFE, b'R', 0, b'1', 0, b'\t', 0, 0xA9, 0x03]);
        assert_eq!(decode(&bytes), "R1\tΩ");
    }

    #[test]
    fn the_bom_is_easyedas_unquoted_tab_separated_table() {
        let source = "\"Designator\",\"Value\",\"Footprint\",\"MPN\",\"Manufacturer\",\"LCSC\",\"Quantity\"\n\
                      \"R1,R2\",\"10k\",\"Resistor_SMD:R_0402_1005Metric\",\"RC0402\",\"Yageo\",\"C25744\",\"2\"\n\
                      \"J1\",\"PH 2-pin\",\"Connector_JST:JST_PH_B2B\",\"\",\"\",\"\",\"1\"\n";
        let lines = bom_lines(source).unwrap();
        assert_eq!(lines[0].designators, ["R1", "R2"]);
        assert_eq!(lines[0].footprint, "R_0402_1005Metric");
        assert_eq!(
            easyeda_bom(&lines),
            "No.\tQuantity\tComment\tDesignator\tFootprint\tValue\tManufacturer Part\tManufacturer\tSupplier Part\tSupplier\n\
             1\t2\t10k\tR1,R2\tR_0402_1005Metric\t10k\tRC0402\tYageo\tC25744\tLCSC\n\
             2\t1\tPH 2-pin\tJ1\tJST_PH_B2B\tPH 2-pin\t\t\t\t\n"
        );
        assert!(
            bom_lines("\"Designator\",\"Value\"\n").is_err(),
            "missing columns"
        );
    }

    fn midpoint(
        designator: &str,
        anchor: (f64, f64),
        pin1: Option<(f64, f64)>,
    ) -> super::super::midpoints::Midpoint {
        super::super::midpoints::Midpoint {
            designator: designator.into(),
            anchor_x_mm: anchor.0,
            anchor_y_mm: anchor.1,
            midpoint_x_mm: anchor.0,
            midpoint_y_mm: anchor.1,
            algorithm: "kicad_native_board_pad_boxes_union_center",
            pad_count: 2,
            pin1_mm: pin1,
            smd: pin1.is_some(),
        }
    }

    /// Position, side and rotation come from the corrected CPL, the anchor and
    /// pad 1 from the snapshot with Y turned up; a part without pad 1 gets
    /// its midpoint.
    #[test]
    fn the_cpl_is_easyedas_quoted_table_in_millimetres() {
        let jlcpcb = b"Designator,Mid X,Mid Y,Layer,Rotation\nR1,10.500000,-20.250000,top,270.000000\nJ1,30.000000,-40.000000,bottom,180.000000\n";
        let kicad = "Ref,Val,Package,PosX,PosY,Rot,Side\nR1,10k,Resistor_SMD:R_0402,10.5,-20.25,0,top\nJ1,\"PH, 2\",JST_PH,30,-40,0,bottom\n";
        let natives = BTreeMap::from([
            (
                "R1".to_owned(),
                midpoint("R1", (10.0, 20.25), Some((9.52, 20.25))),
            ),
            ("J1".to_owned(), midpoint("J1", (31.0, 40.0), None)),
        ]);
        let lines = cpl_lines(jlcpcb, kicad, &natives).unwrap();
        assert_eq!(lines[0].reference_mm, (10.0, -20.25));
        assert_eq!(lines[0].pad_mm, (9.52, -20.25));
        assert_eq!(lines[1].pad_mm, (30.0, -40.0));
        assert_eq!(
            easyeda_cpl(&lines),
            "Designator\tDevice\tFootprint\tMid X\tMid Y\tRef X\tRef Y\tPad X\tPad Y\tPins\tLayer\tRotation\tSMD\tComment\tName\r\n\
             \"R1\"\t\"10k\"\t\"R_0402\"\t\"10.5mm\"\t\"-20.25mm\"\t\"10mm\"\t\"-20.25mm\"\t\"9.52mm\"\t\"-20.25mm\"\t\"2\"\t\"T\"\t\"270\"\t\"Yes\"\t\"10k\"\t\"10k\"\r\n\
             \"J1\"\t\"PH, 2\"\t\"JST_PH\"\t\"30mm\"\t\"-40mm\"\t\"31mm\"\t\"-40mm\"\t\"30mm\"\t\"-40mm\"\t\"2\"\t\"B\"\t\"180\"\t\"No\"\t\"PH, 2\"\t\"PH, 2\"\r\n"
        );
        let unknown = b"Designator,Mid X,Mid Y,Layer,Rotation\nU9,1,2,top,0\n";
        assert!(
            cpl_lines(unknown, kicad, &natives).is_err(),
            "a part KiCad didn't list"
        );
    }

    fn options() -> OrderOptions {
        OrderOptions {
            gerber_name: "board_20261106.zip".into(),
            layers: 2,
            width_mm: 49.5,
            length_mm: 59.5,
            quantity: 5,
            thickness_mm: 1.6,
            color: "Green".into(),
            surface_finish: "HASL(with lead)".into(),
            copper_weight: "1".into(),
        }
    }

    #[test]
    fn the_form_carries_four_blobs_and_ten_fields_in_easyedas_order() {
        let files = OrderFiles {
            gerber_zip: b"ZIP".to_vec(),
            assembly: Some((b"BOM".to_vec(), b"CPL".to_vec())),
        };
        let body = String::from_utf8(multipart_body(&files, &options(), "b0undary")).unwrap();
        let names: Vec<&str> = body
            .split("form-data; name=\"")
            .skip(1)
            .map(|part| part.split('"').next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "gerber",
                "bom",
                "coordinate",
                "pickAndPlace",
                "gerber_name",
                "edaActivity",
                "layer",
                "width",
                "length",
                "qty",
                "thickness",
                "color",
                "surface_finish",
                "copper_weight"
            ]
        );
        assert!(body.contains(
            "name=\"gerber\"; filename=\"blob\"\r\nContent-Type: application/zip\r\n\r\nZIP\r\n"
        ));
        assert_eq!(
            body.matches("filename=\"blob\"\r\nContent-Type: text/csv\r\n\r\nCPL\r\n")
                .count(),
            2
        );
        assert!(body.contains("name=\"width\"\r\n\r\n49.5\r\n"));
        assert!(body.contains("name=\"thickness\"\r\n\r\n1.6\r\n"));
        assert!(body.contains("name=\"edaActivity\"\r\n\r\nnull\r\n"));
        assert!(body.ends_with("--b0undary--\r\n"));

        let pcb_only = OrderFiles {
            gerber_zip: b"ZIP".to_vec(),
            assembly: None,
        };
        let body = String::from_utf8(multipart_body(&pcb_only, &options(), "b")).unwrap();
        assert!(!body.contains("name=\"bom\"") && !body.contains("pickAndPlace"));
    }

    /// The endpoint's answer as it came on 2026-11-06, with the id replaced:
    /// a link, slashes escaped, and the upload's id.
    const ANSWER: &str = r#"{"success":true,"code":0,"result":{"url":"https:\/\/jlcpcb.com\/quote\/eda?eadLink=2&uuid=0123456789abcdef0123456789abcdef&from=eda-pro&edaActivity=null","id":"0123456789abcdef0123456789abcdef"}}"#;

    #[test]
    fn the_answer_gives_the_link_as_it_is() {
        assert_eq!(
            order_link(ANSWER).unwrap(),
            OrderLink {
                url: "https://jlcpcb.com/quote/eda?eadLink=2&uuid=0123456789abcdef0123456789abcdef&from=eda-pro&edaActivity=null".into(),
                id: Some("0123456789abcdef0123456789abcdef".into()),
            }
        );
        let refused = order_link(r#"{"success":false,"code":500,"message":"bad gerber"}"#)
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("refused") && refused.contains("bad gerber"),
            "{refused}"
        );
        let no_link = order_link(r#"{"success":true,"result":{}}"#)
            .unwrap_err()
            .to_string();
        assert!(no_link.contains("no link"), "{no_link}");
        assert!(order_link("<html>")
            .unwrap_err()
            .to_string()
            .contains("did not answer JSON"));
        for elsewhere in [
            "http://jlcpcb.com/quote/eda?uuid=1",
            "https://jlcpcb.com.example.net/quote",
            "https://example.com/?jlcpcb.com",
        ] {
            let answer = json!({ "success": true, "result": { "url": elsewhere } }).to_string();
            assert!(order_link(&answer).is_err(), "{elsewhere}");
        }
        let subdomain = json!({ "success": true, "result": { "url": "https://cart.jlcpcb.com/quote?fileId=1" } });
        assert!(order_link(&subdomain.to_string()).is_ok());
    }

    #[test]
    fn the_gerber_zip_holds_each_file_flat_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<_> = ["b-F_Cu.gtl", "a-PTH.drl"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                std::fs::write(&path, name.as_bytes()).unwrap();
                path
            })
            .collect();
        let bytes = zip_files(&files).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        assert_eq!(names, ["a-PTH.drl", "b-F_Cu.gtl"]);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("b-F_Cu.gtl").unwrap(), &mut text)
            .unwrap();
        assert_eq!(text, "b-F_Cu.gtl");
    }

    #[test]
    fn decimals_drop_trailing_zeros() {
        assert_eq!(decimal(1.6), "1.6");
        assert_eq!(decimal(90.0), "90");
        assert_eq!(decimal(-31.43400001), "-31.434");
        assert_eq!(decimal(-0.00001), "0");
    }

    #[tokio::test]
    async fn an_invalid_request_is_proven_not_sent() {
        let failure = upload("not a URL", "id", "boundary", vec![])
            .await
            .unwrap_err();
        assert_eq!(failure.outcome, UploadOutcome::NotSent);
        assert_eq!(failure.uploaded(), Some(false));
        assert_eq!(failure.http_status, None);
    }

    #[tokio::test]
    async fn a_truncated_response_preserves_the_observed_http_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&buf[..n]);
                if request.ends_with(b"payload") {
                    break;
                }
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
        });
        let failure = upload(&base, "id", "boundary", b"payload".to_vec())
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(failure.outcome, UploadOutcome::Sent);
        assert_eq!(failure.uploaded(), Some(true));
        assert_eq!(failure.http_status, Some(200));
        assert!(failure.to_string().contains("response body"));
    }

    /// The upload names Copperpour, posts the form to the order's path, and
    /// returns the answer; a refusal keeps the endpoint's status and words.
    #[tokio::test]
    async fn the_upload_posts_the_form_and_returns_the_answer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for status in [
                "200 OK",
                "500 Internal Server Error",
                "307 Temporary Redirect",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if rest.len() >= length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let body = if status.starts_with("200") {
                    ANSWER
                } else {
                    "busy"
                };
                let location = if status.starts_with("307") {
                    format!("Location: {redirect}\r\n")
                } else {
                    String::new()
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                requests.push(String::from_utf8_lossy(&request).into_owned());
            }
            requests
        });
        let answer = upload(&base, "0123abcd", "bnd", b"--bnd--\r\n".to_vec())
            .await
            .unwrap();
        assert_eq!(answer, ANSWER);
        let refused = upload(&base, "0123abcd", "bnd", b"--bnd--\r\n".to_vec())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("500") && refused.contains("busy"),
            "{refused}"
        );
        let redirected = upload(&base, "0123abcd", "bnd", b"--bnd--\r\n".to_vec())
            .await
            .unwrap_err();
        assert_eq!(redirected.outcome, UploadOutcome::Sent);
        assert_eq!(redirected.http_status, Some(307));
        let requests = server.await.unwrap();
        let first = requests[0].to_ascii_lowercase();
        assert!(
            first.starts_with("post /api/pcb_order/0123abcd/save http/1.1"),
            "{first}"
        );
        assert!(first.contains("user-agent: copperpour/"), "{first}");
        assert!(
            first.contains("content-type: multipart/form-data; boundary=bnd"),
            "{first}"
        );
        assert!(
            !first.contains("origin:") && !first.contains("referer:"),
            "{first}"
        );
    }
}
