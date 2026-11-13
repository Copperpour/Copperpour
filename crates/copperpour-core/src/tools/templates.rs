//! `templates` toolset — Reference circuit library for validated subcircuit templates.
//!
//! Templates are JSON files stored in `~/.copperpour/templates/` (user) and
//! shipped as embedded defaults. Claude retrieves a template and adapts it to the
//! user's project — this prevents hallucinating component values.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{get_path, require_str, ToolContext, ToolDef};
use serde_json::json;
use std::path::PathBuf;
use tracing::{debug, info, warn};

// ─── Embedded starter templates ──────────────────────────────────────────────

fn builtin_templates() -> Vec<serde_json::Value> {
    vec![
        json!({
            "id": "usb_c_5v_sink",
            "name": "USB-C Power Sink (5V default)",
            "description": "USB Type-C receptacle with CC resistors for 5V default power and ESD protection on D+/D-.",
            "category": "connectivity/usb",
            "tags": ["usb-c", "power", "5v", "sink"],
            "components": [
                {"ref_prefix": "J", "lib_id": "Connector:USB_C_Receptacle_USB2.0_16P", "value": "USB_C", "notes": "USB-C receptacle, 16-pin USB 2.0"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "5.1k", "quantity": 2, "package": "0402", "notes": "CC1 and CC2 pull-down — required for 5V default current"},
                {"ref_prefix": "D", "lib_id": "Device:D_TVS", "value": "PRTR5V0U2X", "quantity": 1, "notes": "ESD protection on D+/D-"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "VBUS decoupling"}
            ],
            "connections": [
                {"from": "J.VBUS", "to_net": "VUSB", "notes": "5V power from USB host"},
                {"from": "J.CC1", "via": "R1", "to_net": "GND", "notes": "5.1k pull-down identifies as UFP sink"},
                {"from": "J.CC2", "via": "R2", "to_net": "GND", "notes": "5.1k pull-down"},
                {"from": "J.D+", "to_net": "USB_DP", "notes": "USB data positive"},
                {"from": "J.D-", "to_net": "USB_DN", "notes": "USB data negative"},
                {"from": "J.GND", "to_net": "GND", "notes": "Ground"}
            ],
            "design_notes": "CC resistor value is critical: 5.1k ±1% for default 5V/900mA. For USB 2.0 only, connect D+/D- directly to MCU. For USB 3.x, route TX/RX as controlled impedance pairs.",
            "references": ["USB Type-C Spec Rev 2.0, Table 4-25"]
        }),
        json!({
            "id": "ldo_3v3",
            "name": "3.3V LDO Regulator",
            "description": "Low-dropout 3.3V regulator with input/output capacitors. Generic topology, adapt MPN to your needs.",
            "category": "power/regulator",
            "tags": ["ldo", "3v3", "regulator", "power"],
            "components": [
                {"ref_prefix": "U", "lib_id": "Regulator_Linear:AP2112K-3.3", "value": "AP2112K-3.3", "notes": "3.3V 600mA LDO, stable with 1uF ceramic capacitors"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "1uF", "quantity": 1, "package": "0402", "notes": "C1: input capacitor — ceramic X5R or X7R"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "1uF", "quantity": 1, "package": "0402", "notes": "C2: output capacitor — ceramic X5R or X7R"}
            ],
            "connections": [
                {"from": "U.VIN", "to_net": "VIN", "notes": "Input voltage, 6V max (6.5V absolute)"},
                {"from": "U.EN", "to_net": "VIN", "notes": "EN has an internal 3M pull-down: left open, the regulator stays off"},
                {"from": "U.VOUT", "to_net": "VCC_3V3", "notes": "3.3V regulated output"},
                {"from": "U.GND", "to_net": "GND", "notes": "Ground — ensure low-impedance path"},
                {"from": "C1.1", "to_net": "VIN", "notes": "Input capacitor at the VIN pin"},
                {"from": "C1.2", "to_net": "GND"},
                {"from": "C2.1", "to_net": "VCC_3V3", "notes": "Output capacitor at the VOUT pin"},
                {"from": "C2.2", "to_net": "GND"}
            ],
            "design_notes": "C1 and C2 in the connections are the first and second capacitor in the component list; apply_template numbers them from the schematic's next free reference. AP2112K-3.3 is stable with 1uF ceramic capacitors on input and output; place both within 5mm of the pins. Dropout is 250mV typical / 400mV max at 600mA, so a 5V rail keeps regulating down to USB's 4.4V minimum. Pin 4 is NC. The absolute maximum input is 6.5V: on a hot-plugged USB VBUS input, cable inductance rings with small input capacitance and can overshoot that — clamp it with a TVS or choose a regulator rated for a higher input (e.g. LP38693, 10V). AMS1117-class LDOs are a poor default here: ~1.1V dropout, and they need an output capacitor with controlled ESR (tantalum) rather than ceramic.",
            "references": ["Diodes AP2112 datasheet DS39724 Rev. 2-2"]
        }),
        json!({
            "id": "stm32_minimal",
            "name": "STM32 Minimal System",
            "description": "STM32 MCU with HSE crystal, decoupling caps, reset circuit, boot-mode pull-downs and SWD debug header.",
            "category": "mcu/stm32",
            "tags": ["stm32", "mcu", "minimal", "crystal", "swd"],
            "components": [
                {"ref_prefix": "U", "lib_id": "MCU_ST_STM32F4:STM32F411CEUx", "value": "STM32F411CEU6", "notes": "48-pin UFQFPN"},
                {"ref_prefix": "Y", "lib_id": "Device:Crystal", "value": "8MHz", "quantity": 1, "notes": "HSE crystal — STM32F411 accepts 4-26MHz"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "20pF", "quantity": 2, "package": "0402", "notes": "C1, C2: crystal load caps — calculate from the crystal's CL"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 4, "package": "0402", "notes": "C3-C5: one per VDD pin (3); C6: VDDA"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "4.7uF", "quantity": 1, "package": "0603", "notes": "C7: bulk decoupling on VDD"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "4.7uF", "quantity": 1, "package": "0603", "notes": "C8: VCAP1 — internal regulator, ESR < 1 ohm"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "10k", "quantity": 1, "package": "0402", "notes": "R1: NRST pull-up"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "C9: NRST filter cap to GND"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "10k", "quantity": 2, "package": "0402", "notes": "R2: BOOT0 pull-down; R3: PB2 (BOOT1) pull-down"},
                {"ref_prefix": "J", "lib_id": "Connector:Conn_ARM_JTAG_SWD_10", "value": "SWD", "notes": "10-pin Cortex debug header"}
            ],
            "connections": [
                {"from": "U.VDD", "to_net": "VCC_3V3", "notes": "All three VDD pins to 3.3V"},
                {"from": "U.VDDA", "to_net": "VCC_3V3", "notes": "Analog supply — add ferrite bead for sensitive analog work"},
                {"from": "U.VBAT", "to_net": "VCC_3V3", "notes": "No backup battery: tie VBAT to VDD"},
                {"from": "U.VSS", "to_net": "GND", "notes": "All VSS pins to ground"},
                {"from": "U.VSSA", "to_net": "GND", "notes": "Analog ground"},
                {"from": "U.VCAP1", "to_net": "VCAP1", "notes": "Internal regulator output — only C8 on this net"},
                {"from": "C8.1", "to_net": "VCAP1"},
                {"from": "C8.2", "to_net": "GND"},
                {"from": "U.NRST", "to_net": "NRST", "notes": "Reset with 10k pull-up + 100nF cap"},
                {"from": "R1.1", "to_net": "NRST"},
                {"from": "R1.2", "to_net": "VCC_3V3"},
                {"from": "C9.1", "to_net": "NRST"},
                {"from": "C9.2", "to_net": "GND"},
                {"from": "U.BOOT0", "to_net": "BOOT0", "notes": "Low: boot from flash. Pull high to enter the system bootloader"},
                {"from": "R2.1", "to_net": "BOOT0"},
                {"from": "R2.2", "to_net": "GND"},
                {"from": "U.PB2", "to_net": "BOOT1", "notes": "PB2 is BOOT1: it must be low for the system bootloader"},
                {"from": "R3.1", "to_net": "BOOT1"},
                {"from": "R3.2", "to_net": "GND"},
                {"from": "U.PH0", "to_net": "HSE_IN", "notes": "PH0 = RCC_OSC_IN, crystal input"},
                {"from": "Y.1", "to_net": "HSE_IN"},
                {"from": "C1.1", "to_net": "HSE_IN"},
                {"from": "C1.2", "to_net": "GND"},
                {"from": "U.PH1", "to_net": "HSE_OUT", "notes": "PH1 = RCC_OSC_OUT, crystal output"},
                {"from": "Y.2", "to_net": "HSE_OUT"},
                {"from": "C2.1", "to_net": "HSE_OUT"},
                {"from": "C2.2", "to_net": "GND"},
                {"from": "C3.1", "to_net": "VCC_3V3", "notes": "C3-C5 each at one VDD pin"},
                {"from": "C3.2", "to_net": "GND"},
                {"from": "C4.1", "to_net": "VCC_3V3"},
                {"from": "C4.2", "to_net": "GND"},
                {"from": "C5.1", "to_net": "VCC_3V3"},
                {"from": "C5.2", "to_net": "GND"},
                {"from": "C6.1", "to_net": "VCC_3V3", "notes": "At the VDDA pin"},
                {"from": "C6.2", "to_net": "GND"},
                {"from": "C7.1", "to_net": "VCC_3V3"},
                {"from": "C7.2", "to_net": "GND"},
                {"from": "U.PA13", "to_net": "SWDIO", "notes": "PA13 = SYS_JTMS-SWDIO"},
                {"from": "J.SWDIO/TMS", "to_net": "SWDIO"},
                {"from": "U.PA14", "to_net": "SWCLK", "notes": "PA14 = SYS_JTCK-SWCLK"},
                {"from": "J.SWCLK/TCK", "to_net": "SWCLK"},
                {"from": "J.~{RESET}", "to_net": "NRST", "notes": "Lets the debugger reset the MCU"},
                {"from": "J.VTref", "to_net": "VCC_3V3", "notes": "Target voltage reference for the probe"},
                {"from": "J.GND", "to_net": "GND"},
                {"from": "J.GNDDetect", "to_net": "GND"}
            ],
            "design_notes": "C1-C9 and R1-R3 in the connections count each prefix's parts in component-list order; apply_template numbers them from the schematic's next free reference. Crystal load cap formula: CL = (C1*C2)/(C1+C2) + Cstray; ST suggests 10pF as a rough estimate of pin plus board capacitance. Place all decoupling caps within 3mm of their pin. VCAP1: STM32F411 has a single VCAP pin, which needs 4.7uF with ESR < 1 ohm; parts with two VCAP pins (e.g. STM32F407) use 2.2uF on each — check your variant. SWO (PB3) can be routed to the header's SWO/TDO pin for trace output.",
            "references": ["ST DS10314: STM32F411xC/xE datasheet", "AN4488: Getting started with STM32F4 MCU hardware development"]
        }),
        json!({
            "id": "i2c_pullups",
            "name": "I2C Bus Pull-ups",
            "description": "Standard I2C pull-up resistors for SDA and SCL lines.",
            "category": "connectivity/i2c",
            "tags": ["i2c", "pull-up", "bus"],
            "components": [
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "4.7k", "quantity": 2, "package": "0402", "notes": "SDA and SCL pull-ups. Use 2.2k for fast-mode (400kHz), 1k for fast-mode plus (1MHz)"}
            ],
            "connections": [
                {"from": "R1.1", "to_net": "SDA", "notes": "I2C data line"},
                {"from": "R1.2", "to_net": "VCC_3V3", "notes": "Pull to I2C bus voltage"},
                {"from": "R2.1", "to_net": "SCL", "notes": "I2C clock line"},
                {"from": "R2.2", "to_net": "VCC_3V3", "notes": "Pull to I2C bus voltage"}
            ],
            "design_notes": "One set of pull-ups per I2C bus — do NOT add pull-ups on every device. Value depends on bus speed and capacitance. 4.7k is safe for standard mode (100kHz) with <400pF bus capacitance.",
            "references": ["NXP UM10204: I2C-bus specification"]
        }),
        json!({
            "id": "led_indicator",
            "name": "LED Indicator Circuit",
            "description": "Simple LED with current-limiting resistor, driven by a GPIO pin.",
            "category": "misc/led",
            "tags": ["led", "indicator", "gpio"],
            "components": [
                {"ref_prefix": "D", "lib_id": "Device:LED", "value": "LED_Green", "quantity": 1, "package": "0603", "notes": "Standard indicator LED"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "1k", "quantity": 1, "package": "0402", "notes": "Current limiter: R = (Vcc - Vf) / If. For 3.3V, green Vf≈2.1V, If=1.2mA → 1k"}
            ],
            "connections": [
                {"from": "GPIO", "to": "R1.1", "notes": "GPIO output drives LED through resistor"},
                {"from": "R1.2", "to": "D1.A", "notes": "Resistor to LED anode"},
                {"from": "D1.K", "to_net": "GND", "notes": "LED cathode to ground"}
            ],
            "design_notes": "R = (VCC - Vf) / If. For 3.3V GPIO: green (Vf=2.1V) → 1k gives 1.2mA. Red (Vf=1.8V) → 680R gives 2.2mA. Bright LEDs may only need 0.5mA. Check your LED's datasheet for Vf and recommended If.",
            "references": []
        }),
        json!({
            "id": "buck_converter",
            "name": "Buck Converter (Step-Down)",
            "description": "Synchronous buck converter with input/output caps, inductor, and feedback resistors. Generic topology — adapt MPN and passives to your voltage/current needs.",
            "category": "power/switching",
            "tags": ["buck", "step-down", "switching", "regulator", "power"],
            "components": [
                {"ref_prefix": "U", "lib_id": "Regulator_Switching:TPS563200", "value": "TPS563200DDCR", "notes": "3A sync buck, 4.5-17V input. Substitute: AP63356, MP2315, SY8089"},
                {"ref_prefix": "L", "lib_id": "Device:L", "value": "4.7uH", "quantity": 1, "notes": "Inductor — check datasheet for recommended value and saturation current > Iout*1.3"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "10uF", "quantity": 2, "package": "0805", "notes": "Input capacitors — X5R/X7R ceramic, voltage rating > Vin*1.5"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "22uF", "quantity": 2, "package": "0805", "notes": "Output capacitors — low ESR ceramic"},
                {"ref_prefix": "C", "lib_id": "Device:C", "value": "100nF", "quantity": 1, "package": "0402", "notes": "Boot cap"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "100k", "quantity": 1, "package": "0402", "notes": "Feedback upper resistor — adjust for target Vout"},
                {"ref_prefix": "R", "lib_id": "Device:R", "value": "49.9k", "quantity": 1, "package": "0402", "notes": "Feedback lower resistor — Vout = Vref * (1 + Rtop/Rbot)"}
            ],
            "connections": [
                {"from": "U.VIN", "to_net": "VIN", "notes": "Input power"},
                {"from": "U.SW", "to": "L1.1", "notes": "Switch node to inductor"},
                {"from": "L1.2", "to_net": "VOUT", "notes": "Inductor output"},
                {"from": "U.VFB", "via": "voltage divider R_top/R_bot", "to_net": "VOUT", "notes": "Feedback voltage divider"},
                {"from": "U.VBST", "notes": "Bootstrap cap from VBST to SW"},
                {"from": "U.GND", "to_net": "GND", "notes": "Power ground — kelvin sense to output cap GND"}
            ],
            "design_notes": "Layout is critical: keep input caps close to VIN/GND pins, keep SW trace short and wide (high di/dt), keep feedback divider close to FB pin away from SW node. Ground plane under inductor improves EMI. Calculate passives from datasheet — do NOT guess values.",
            "references": ["TI SLVA477: Application Note for TPS563200"]
        }),
    ]
}

// ─── Template storage paths ──────────────────────────────────────────────────

fn user_templates_dir() -> Option<PathBuf> {
    crate::app_dirs::user_data_dir().map(|dir| dir.join("templates"))
}

/// Load all templates: builtins + any user-created ones from disk.
async fn load_all_templates() -> Vec<serde_json::Value> {
    let mut templates = builtin_templates();

    if let Some(user_dir) = user_templates_dir().filter(|dir| dir.is_dir()) {
        if let Ok(mut rd) = tokio::fs::read_dir(&user_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    match tokio::fs::read_to_string(&path).await {
                        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                            Ok(tmpl) => {
                                debug!(path = %path.display(), "Loaded user template");
                                templates.push(tmpl);
                            }
                            Err(e) => {
                                warn!(path = %path.display(), error = %e, "Failed to parse user template")
                            }
                        },
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "Failed to read user template")
                        }
                    }
                }
            }
        }
    }

    templates
}

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "search_templates",
            "Search the reference circuit template library. Returns matching templates for \
             common subcircuits (USB-C, LDO, buck converter, MCU minimal system, I2C pull-ups, etc.). \
             Use these instead of designing from scratch — templates have verified component values.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search string: 'usb-c', '3.3v regulator', 'stm32', 'i2c', 'led', 'buck converter', etc."
                    },
                    "category": {
                        "type": "string",
                        "description": "Filter by category: 'power', 'connectivity', 'mcu', 'misc' (optional)"
                    }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_templates(args, ctx).await }
        ),
        tool!(
            "get_template",
            "Get full details for a reference circuit template including all components, \
             connections, and design notes. Use the template ID from search_templates.",
            json!({
                "type": "object",
                "properties": {
                    "template_id": { "type": "string", "description": "Template ID (e.g. 'usb_c_5v_sink', 'ldo_3v3', 'stm32_minimal')" }
                },
                "required": ["template_id"]
            }),
            |args, ctx| async move { handle_get_template(args, ctx).await }
        ),
        tool!(
            "apply_template",
            "Instantiate a reference circuit template into the current schematic. Places components \
             in a column, then wires resolvable connection-map entries with short labelled stubs. \
             net_mappings renames template nets. Inspect connections_made, connections_unresolved \
             and unconnected_pins; unresolved pins still need wiring or no-connect flags. \
             connections_to_wire is a deprecated copy of the map, not outstanding work. \
             Placement and each net are separate writes; do not reapply after a partial failure.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "template_id": { "type": "string", "description": "Template ID to instantiate" },
                    "position_x": { "type": "number", "description": "X position to place the subcircuit (mm)", "default": 100.0 },
                    "position_y": { "type": "number", "description": "Y position to place the subcircuit (mm)", "default": 100.0 },
                    "net_mappings": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Map template net names to your project's net names. E.g. {\"VUSB\": \"VCC_5V\", \"GND\": \"GND\"}"
                    },
                    "ref_start": {
                        "type": "integer",
                        "description": "Starting reference number (e.g. 10 → R10, C10, U10). Auto-detected if omitted."
                    }
                },
                "required": ["schematic", "template_id"]
            }),
            |args, ctx| async move { handle_apply_template(args, ctx).await }
        ),
        tool!(
            "list_template_categories",
            "List all available template categories and the number of templates in each.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            |args, ctx| async move { handle_list_categories(args, ctx).await }
        ),
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_search_templates(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let query = match require_str(args, "query") {
        Ok(v) => v.to_lowercase(),
        Err(e) => return Ok(e),
    };
    let category_filter = args["category"].as_str().map(|s| s.to_lowercase());

    info!(query = %query, category = ?category_filter, "Searching templates");

    let templates = load_all_templates().await;
    let mut results = Vec::new();

    for tmpl in &templates {
        let id = tmpl["id"].as_str().unwrap_or("");
        let name = tmpl["name"].as_str().unwrap_or("");
        let desc = tmpl["description"].as_str().unwrap_or("");
        let category = tmpl["category"].as_str().unwrap_or("");
        let tags: Vec<&str> = tmpl["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // Category filter
        if let Some(ref cat) = category_filter {
            if !category.to_lowercase().contains(cat) {
                continue;
            }
        }

        // Search across name, description, tags, category
        let haystack =
            format!("{} {} {} {} {}", id, name, desc, category, tags.join(" ")).to_lowercase();

        let matches = query.split_whitespace().all(|word| haystack.contains(word));

        if matches {
            let component_count: usize =
                tmpl["components"].as_array().map(|a| a.len()).unwrap_or(0);
            results.push(json!({
                "id": id,
                "name": name,
                "description": desc,
                "category": category,
                "tags": tags,
                "component_count": component_count
            }));
        }
    }

    debug!(query = %query, results = results.len(), "Template search complete");

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "query": query,
            "count": results.len(),
            "templates": results
        }))
        .unwrap(),
    ))
}

async fn handle_get_template(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let template_id = match require_str(args, "template_id") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    info!(template_id = %template_id, "Loading template");

    let templates = load_all_templates().await;
    let tmpl = templates
        .iter()
        .find(|t| t["id"].as_str() == Some(&template_id));

    match tmpl {
        Some(t) => Ok(CallToolResult::text(serde_json::to_string(t).unwrap())),
        None => {
            warn!(template_id = %template_id, "Template not found");
            Ok(CallToolResult::error(format!(
                "Template '{}' not found. Use search_templates to find available templates.",
                template_id
            )))
        }
    }
}

async fn handle_apply_template(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let template_id = match require_str(args, "template_id") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let base_x = args["position_x"].as_f64().unwrap_or(100.0);
    let base_y = args["position_y"].as_f64().unwrap_or(100.0);
    let mut net_mappings = std::collections::HashMap::new();
    if let Some(value) = args.get("net_mappings") {
        let Some(mappings) = value.as_object() else {
            return Ok(crate::tools::invalid_arg(
                "net_mappings",
                "must be an object of nonempty net names",
            ));
        };
        for (name, value) in mappings {
            let Some(net) = value.as_str().filter(|net| !net.trim().is_empty()) else {
                return Ok(crate::tools::invalid_arg(
                    "net_mappings",
                    "every destination must be a nonempty net name",
                ));
            };
            net_mappings.insert(name.clone(), net.to_string());
        }
    }

    info!(
        template_id = %template_id,
        schematic = %sch_path.display(),
        position = ?(base_x, base_y),
        net_mappings = ?net_mappings,
        "Applying template"
    );

    let templates = load_all_templates().await;
    let tmpl = match templates
        .iter()
        .find(|t| t["id"].as_str() == Some(&template_id))
    {
        Some(t) => t.clone(),
        None => {
            warn!(template_id = %template_id, "Template not found for apply");
            return Ok(CallToolResult::error(format!(
                "Template '{}' not found",
                template_id
            )));
        }
    };

    // Templates are ordinary symbol placements. Load and validate the same
    // instance context used by the single and batch placement tools rather
    // than hand-writing a second, incomplete interpretation of KiCad's
    // instance metadata (#609).
    let mut sch = match copperpour_schematic_editor::Schematic::load(&sch_path) {
        Ok(schematic) => schematic,
        Err(error) => return Ok(CallToolResult::error(error.to_string())),
    };
    let context = match crate::tools::sheet_instance_context(&sch_path, &mut sch) {
        Ok(context) => context,
        Err(error) => return Ok(error.into_tool_result()),
    };
    if let Err(error) = crate::tools::validate_sheet_instance_state(&sch_path, &sch, &context) {
        return Ok(error.into_tool_result());
    }
    let source = match crate::tools::library::KiCadSymbolSource::for_file(&sch_path) {
        Ok(source) => source,
        Err(error) => return Ok(error.into_tool_result()),
    };

    // Determine starting reference numbers by scanning existing components
    let ref_start = args["ref_start"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or_else(|| find_next_ref_number(&sch.to_source()));

    let (_, before_tree) = copperpour_sexp::schematic::read_schematic(&sch_path)?;
    let existing_nets: std::collections::HashSet<String> =
        copperpour_sexp::schematic::extract_all_net_labels(&before_tree)
            .into_iter()
            .map(|label| label.net)
            .collect();

    let components = tmpl["components"].as_array().cloned().unwrap_or_default();
    let mut placed = Vec::new();
    let mut placed_targets = Vec::new();
    let mut ref_counters: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // Place components in a column layout
    let spacing_y = 15.24; // mm, on the 1.27 mm schematic grid
    let mut previous_bottom: Option<f64> = None;
    for comp in components.iter() {
        let ref_prefix = comp["ref_prefix"].as_str().unwrap_or("U");
        let lib_id = comp["lib_id"].as_str().unwrap_or("Device:R");
        let value = comp["value"].as_str().unwrap_or("");
        let quantity = comp["quantity"].as_u64().unwrap_or(1) as usize;
        let notes = comp["notes"].as_str().unwrap_or("");

        for _q in 0..quantity {
            let counter = ref_counters
                .entry(ref_prefix.to_string())
                .or_insert(ref_start);
            let reference = format!("{}{}", ref_prefix, counter);
            *counter += 1;

            let x = base_x;
            let y = base_y + (placed.len() as f64) * spacing_y;
            let placement = match crate::tools::sch_components::place_one_component(
                &mut sch,
                &context.instance_paths,
                &context.project_name,
                lib_id,
                x,
                y,
                0.0,
                None,
                Some(&reference),
                Some(value),
                None,
                1,
                &source,
                crate::tools::sch_components::ComponentPlacementExtras::default(),
            ) {
                Ok(placement) => placement,
                Err(error) => return Ok(error),
            };
            // Leave room for stubs even after a tall IC. A fixed pitch put
            // the following passive inside the IC's drawing (#89).
            let tree = copperpour_sexp::parse_sexp(&sch.to_source())?;
            let instances = copperpour_sexp::schematic::extract_symbol_instances(&tree);
            let libraries = tree
                .find("lib_symbols")
                .map(|node| node.find_all("symbol"))
                .unwrap_or_default();
            let bounds = instances
                .iter()
                .find(|instance| instance.uuid.as_deref() == Some(placement.uuid.as_str()))
                .and_then(|instance| {
                    copperpour_sexp::schematic::find_lib_symbol(&libraries, instance).and_then(
                        |symbol| {
                            copperpour_sexp::schematic::symbol_bounds_for_instance(symbol, instance)
                        },
                    )
                });
            let (x, mut y) = sch
                .symbols
                .iter()
                .find(|symbol| symbol.uuid == placement.uuid)
                .expect("just placed symbol")
                .position();
            if let Some(bounds) = bounds {
                let shift = previous_bottom.map_or(0.0, |bottom| {
                    ((bottom + 10.16 - bounds.min_y).max(0.0) / 1.27).ceil() * 1.27
                });
                if shift > 0.0 {
                    let symbol = sch
                        .symbols
                        .iter_mut()
                        .find(|symbol| symbol.uuid == placement.uuid)
                        .expect("just placed symbol");
                    symbol.translate(0.0, shift);
                    y = symbol.at.y;
                }
                previous_bottom = Some(bounds.max_y + shift);
            }
            placed_targets.push((placement.uuid, reference.clone()));

            placed.push(json!({
                "reference": reference,
                "lib_id": lib_id,
                "value": value,
                "x": x, "y": y,
                "notes": notes
            }));

            debug!(reference = %reference, lib_id = %lib_id, value = %value, "Placed template component");
        }
    }

    // The editor's overwrite is an atomic compare-and-swap against the exact
    // revision loaded above. All placements are prepared in memory, so a
    // preflight or library failure leaves the schematic byte-for-byte intact.
    if let Err(error) = sch.overwrite() {
        if let Some(refusal) = crate::tools::unwritten_schematic_refusal("apply_template", &error) {
            return Ok(refusal);
        }
        return Ok(crate::tools::mutation_outcome_uncertain(
            &sch_path,
            "apply_template",
            format!("schematic persistence failed: {error}"),
        ));
    }
    let committed = match copperpour_schematic_editor::Schematic::load(&sch_path) {
        Ok(schematic) => schematic,
        Err(error) => {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "apply_template",
                format!("saved schematic could not be reloaded: {error}"),
            ))
        }
    };
    if let Err(error) = crate::tools::validate_sheet_instance_state(&sch_path, &committed, &context)
    {
        return Ok(crate::tools::mutation_outcome_uncertain(
            &sch_path,
            "apply_template",
            format!("saved template instance validation failed: {error}"),
        ));
    }
    for ((uuid, reference), placed) in placed_targets.iter().zip(&mut placed) {
        let Some(symbol) = committed
            .symbols
            .iter()
            .find(|symbol| symbol.uuid == *uuid && symbol.reference() == Some(reference))
        else {
            return Ok(crate::tools::mutation_outcome_uncertain(
                &sch_path,
                "apply_template",
                format!("saved template readback did not contain {reference} with UUID {uuid}"),
            ));
        };
        let (x, y) = symbol.position();
        placed["x"] = json!(x);
        placed["y"] = json!(y);
    }

    info!(
        template_id = %template_id,
        components_placed = placed.len(),
        "Template applied successfully"
    );

    // Wire what the connection map describes (#89): each pin it names gets
    // a stub and a label for its net, drawn as connect_to_net draws one.
    let wiring = match wire_template_connections(
        &sch_path,
        &tmpl,
        &net_mappings,
        ref_start,
        &existing_nets,
        ctx,
    )
    .await
    {
        Ok(wiring) => wiring,
        Err(error) => {
            // Preserve placed identities so recovery never duplicates the template.
            let references = template_references(&tmpl, ref_start);
            return Ok(crate::tools::mutation_outcome_uncertain(
                    &sch_path,
                    "apply_template",
                    format!(
                        "the parts were placed ({}), with template references placed as {}, but \
                         wiring them failed: {error:#}. Do not apply the template again; finish the \
                         remaining nets with batch_connect_to_net on the placed references.",
                        serde_json::to_string(&placed).unwrap_or_default(),
                        serde_json::to_string(&references).unwrap_or_default()
                    ),
                ));
        }
    };
    // Retain the released guide field while callers migrate to observed results.
    let connections = tmpl["connections"].as_array().cloned().unwrap_or_default();
    let mapped_connections: Vec<serde_json::Value> = connections
        .iter()
        .map(|conn| {
            let mut c = conn.clone();
            let original_net = c["to_net"].as_str().map(String::from);
            if let Some(net) = original_net {
                if let Some(mapped) = net_mappings.get(&net) {
                    c["to_net"] = json!(mapped);
                    c["mapped_from"] = json!(net);
                }
            }
            c
        })
        .collect();

    let mut response = json!({
        "template": template_id,
        "components_placed": placed,
        "connections_to_wire": mapped_connections,
        "connections_made": wiring.made,
        "connections_unresolved": wiring.unresolved,
        "unconnected_pins": wiring.unconnected,
        "joined_existing_nets": wiring.joined_existing,
        "design_notes": tmpl["design_notes"],
        "next_steps": "The reported connections were wired. Inspect unresolved entries and unconnected pins; do not wire the deprecated connections_to_wire guide again.",
    });
    if !response["connections_unresolved"]
        .as_array()
        .is_some_and(Vec::is_empty)
        || !response["unconnected_pins"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        response["next_steps"] = json!(
            "Wire the pins in unconnected_pins that the design needs, mark the rest no-connect, \
             and resolve the entries in connections_unresolved, with connect_to_net or \
             connect_pins."
        );
    }
    Ok(CallToolResult::text(serde_json::to_string(&response)?))
}

/// What [`wire_template_connections`] did: each map entry it wired, with the
/// net and placed pins, each it could not, with why, and the template's
/// placed pins no entry reached.
struct TemplateWiring {
    made: Vec<serde_json::Value>,
    unresolved: Vec<serde_json::Value>,
    unconnected: Vec<String>,
    /// Template nets the caller did not map that already exist in the
    /// schematic, so the template's parts were joined to them.
    joined_existing: Vec<String>,
}

/// A placed pin: its symbol's reference and the pin number.
type TemplatePin = (String, String);

/// Template-local references to placed ones. The template's k-th part with
/// prefix `R` is `R{k}` in its connection map and `R{ref_start + k - 1}` on
/// the sheet, matching the placement loop. A bare prefix (`J`) names the
/// template's only part with that prefix.
fn template_references(
    tmpl: &serde_json::Value,
    ref_start: usize,
) -> std::collections::HashMap<String, String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut references = std::collections::HashMap::new();
    for comp in tmpl["components"].as_array().into_iter().flatten() {
        let prefix = comp["ref_prefix"].as_str().unwrap_or("U");
        for _ in 0..comp["quantity"].as_u64().unwrap_or(1) {
            let k = counts.entry(prefix.to_string()).or_insert(0);
            *k += 1;
            references.insert(
                format!("{prefix}{k}"),
                format!("{prefix}{}", ref_start + *k - 1),
            );
        }
    }
    for (prefix, count) in counts {
        if count == 1 {
            references.insert(prefix.clone(), format!("{prefix}{ref_start}"));
        }
    }
    references
}

/// The placed pins a map endpoint such as `R1.2`, `J.VBUS` or `U.VDD` names:
/// the pin with that number, or else every pin with that name, as in "all VDD
/// pins to 3.3V".
fn resolve_template_endpoint(
    endpoint: &str,
    references: &std::collections::HashMap<String, String>,
    pins: &std::collections::HashMap<String, Vec<(String, String)>>,
) -> Result<Vec<TemplatePin>, String> {
    let (local, pin) = endpoint
        .split_once('.')
        .ok_or_else(|| format!("'{endpoint}' names no pin"))?;
    let reference = references
        .get(local)
        .ok_or_else(|| format!("the template has no part '{local}'"))?;
    let symbol_pins = pins
        .get(reference)
        .ok_or_else(|| format!("{reference} has no resolvable pins on the sheet"))?;
    if symbol_pins.iter().any(|(number, _)| number == pin) {
        return Ok(vec![(reference.clone(), pin.to_string())]);
    }
    let named: Vec<TemplatePin> = symbol_pins
        .iter()
        .filter(|(_, name)| name == pin)
        .map(|(number, _)| (reference.clone(), number.clone()))
        .collect();
    if named.is_empty() {
        Err(format!("{reference} has no pin numbered or named '{pin}'"))
    } else {
        Ok(named)
    }
}

/// Connect the placed template parts as its connection map says.
///
/// Entries take four shapes: `from` a pin `to_net` a net; `from` a pin `via` a
/// two-pin part (pin 1 on the `from` side, pin 2 on `to_net`); `from` a pin
/// `to` another pin; and `from` an outside signal (no `.`) `to` a pin. Nets go
/// through `net_mappings`; a net between two template pins is named after
/// the first pin number, such as `J1_A5`. Each net is drawn with
/// [`handle_batch_connect_to_net`](crate::tools::sch_batch::handle_batch_connect_to_net):
/// a 2.54 mm stub and a label per pin.
async fn wire_template_connections(
    sch_path: &std::path::Path,
    tmpl: &serde_json::Value,
    net_mappings: &std::collections::HashMap<String, String>,
    ref_start: usize,
    existing_nets: &std::collections::HashSet<String>,
    ctx: &ToolContext,
) -> anyhow::Result<TemplateWiring> {
    use std::collections::{BTreeMap, HashMap, HashSet};

    let references = template_references(tmpl, ref_start);
    let placed: HashSet<&String> = references.values().collect();
    let (_, tree) = copperpour_sexp::schematic::read_schematic(sch_path)?;
    let mut pins: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (instance, instance_pins) in crate::tools::placed_pins_by_reference(&tree) {
        if placed.contains(&instance.reference) {
            pins.entry(instance.reference.clone()).or_default().extend(
                instance_pins
                    .iter()
                    .map(|(pin, _)| (pin.number.clone(), pin.name.clone())),
            );
        }
    }
    let mapped = |net: &str| {
        net_mappings
            .get(net)
            .cloned()
            .unwrap_or_else(|| net.to_string())
    };
    // A library may repeat a pin name on distinct physical pins (e.g. USB D+).
    // Only the reference and pin number identify the endpoint constraint.
    let local_net = |(reference, number): &TemplatePin| format!("{reference}_{number}");
    let endpoint = |e: &str| resolve_template_endpoint(e, &references, &pins);

    let mut nets: BTreeMap<String, Vec<TemplatePin>> = BTreeMap::new();
    let mut made = Vec::new();
    let mut unresolved = Vec::new();
    for conn in tmpl["connections"].as_array().into_iter().flatten() {
        let Some(from) = conn["from"].as_str().filter(|from| !from.trim().is_empty()) else {
            unresolved.push(json!({"connection":conn,"reason":"connection field 'from' must be a nonempty string"}));
            continue;
        };
        if let Some(field) = ["to", "to_net", "via"].into_iter().find(|field| {
            conn.get(*field)
                .is_some_and(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
        }) {
            unresolved.push(json!({"connection":conn,"reason":format!("connection field '{field}' must be a nonempty string")}));
            continue;
        }
        let groups: Result<Vec<(String, Vec<TemplatePin>)>, String> = (|| {
            match (
            conn["to_net"].as_str(),
            conn["to"].as_str(),
            conn["via"].as_str(),
        ) {
            (Some(to_net), None, None) => Ok(vec![(mapped(to_net), endpoint(from)?)]),
            (Some(to_net), None, Some(via)) => {
                let start = endpoint(from)?;
                let inner = local_net(&start[0]);
                let mut first = start;
                first.extend(endpoint(&format!("{via}.1"))?);
                Ok(vec![
                    (inner, first),
                    (mapped(to_net), endpoint(&format!("{via}.2"))?),
                ])
            }
            (None, Some(to), None) if from.contains('.') => {
                let mut start = endpoint(from)?;
                let inner = local_net(&start[0]);
                start.extend(endpoint(to)?);
                Ok(vec![(inner, start)])
            }
            (None, Some(to), None) => Ok(vec![(mapped(from), endpoint(to)?)]),
            _ => Err("connection fields 'to', 'to_net' and 'via' must describe exactly one supported route".to_string()),
        }
        })();
        match groups {
            Ok(groups) => {
                for (net, net_pins) in groups {
                    made.push(json!({
                        "connection": conn,
                        "net": net,
                        "pins": net_pins.iter().map(|(r, n)| format!("{r}.{n}")).collect::<Vec<_>>(),
                    }));
                    nets.entry(net).or_default().extend(net_pins);
                }
            }
            Err(reason) => unresolved.push(json!({ "connection": conn, "reason": reason })),
        }
    }

    // Pin-pair entries describe one logical net even when their generated names
    // differ. A connected generated group takes an explicit name when present.
    let named: HashSet<String> = made
        .iter()
        .filter_map(|entry| template_connection_net(&entry["connection"]).map(&mapped))
        .collect();
    let generated = nets
        .keys()
        .filter(|net| !named.contains(*net))
        .cloned()
        .collect();
    normalize_template_nets(&mut nets, &mut made, generated);

    // One pin may have at most one planned net. Name order makes conflict
    // reporting deterministic; no connection is claimed until batch readback.
    let mut owner: HashMap<TemplatePin, String> = HashMap::new();
    for (net, net_pins) in &mut nets {
        net_pins.retain(|pin| match owner.get(pin) {
            Some(first) if first != net => {
                unresolved.push(json!({
                    "reason": format!(
                        "pin {}.{} is requested by both net '{first}' and net '{net}'; skipped \
                         '{net}' because one pin cannot carry two nets",
                        pin.0, pin.1
                    )
                }));
                false
            }
            _ => {
                owner.insert(pin.clone(), net.clone());
                true
            }
        });
    }
    let mut wired: HashMap<TemplatePin, String> = HashMap::new();
    let mut wiring_errors: Vec<String> = Vec::new();
    let mut written_nets: Vec<String> = Vec::new();
    for (net, net_pins) in &mut nets {
        net_pins.sort();
        net_pins.dedup();
        if net_pins.is_empty() {
            continue;
        }
        let args = json!({
            "schematic": sch_path.display().to_string(),
            "net": net,
            "pins": net_pins
                .iter()
                .map(|(reference, number)| json!({ "reference": reference, "pin_number": number }))
                .collect::<Vec<_>>(),
            "stub_length": 2.54,
            "direction": "auto",
            "label_type": "net_label",
        });
        let done = |written: &[String]| {
            if written.is_empty() {
                "no net was written".to_string()
            } else {
                format!("nets already written: {}", written.join(", "))
            }
        };
        let result = match crate::tools::sch_batch::handle_batch_connect_to_net(&args, ctx).await {
            Ok(result) => result,
            Err(error) => {
                anyhow::bail!(
                    "connecting net '{net}' failed ({}): {error:#}",
                    done(&written_nets)
                )
            }
        };
        let text = crate::tools::sch_batch::error_text(&result);
        if result.is_error {
            unresolved.push(json!({"net": net, "reason": text}));
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(&text)?;
        // Count only nets the batch reports as applied.
        if body["added"]
            .as_array()
            .is_some_and(|added| !added.is_empty())
        {
            written_nets.push(net.clone());
        }
        for added in body["added"].as_array().into_iter().flatten() {
            if let (Some(reference), Some(pin)) =
                (added["reference"].as_str(), added["pin"].as_str())
            {
                wired.insert((reference.to_string(), pin.to_string()), net.clone());
            }
        }
        for error in body["errors"].as_array().into_iter().flatten() {
            if let Some(error) = error.as_str() {
                wiring_errors.push(format!("{net}: {error}"));
            }
        }
    }
    verify_template_wiring(sch_path, &wired).map_err(|error| {
        anyhow::anyhow!(
            "{error:#}; nets already written: {}",
            written_nets.join(", ")
        )
    })?;
    let mut unconnected: Vec<String> = pins
        .iter()
        .flat_map(|(reference, symbol_pins)| {
            symbol_pins
                .iter()
                .filter(|(number, _)| !wired.contains_key(&(reference.clone(), number.clone())))
                .map(move |(number, _)| format!("{reference}.{number}"))
        })
        .collect();
    unconnected.sort();
    unconnected.dedup();
    for error in wiring_errors {
        unresolved.push(json!({ "reason": error }));
    }
    // `connections_made` lists only the pins the wiring connected; the pins of
    // an entry that were not connected are named beside them.
    for entry in &mut made {
        let pins: Vec<String> = entry["pins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|pin| pin.as_str().map(str::to_string))
            .collect();
        let (connected, missing): (Vec<String>, Vec<String>) = pins.into_iter().partition(|pin| {
            pin.split_once('.').is_some_and(|(r, n)| {
                wired
                    .get(&(r.to_string(), n.to_string()))
                    .is_some_and(|net| entry["net"].as_str() == Some(net.as_str()))
            })
        });
        entry["pins"] = json!(connected);
        if !missing.is_empty() {
            entry["not_connected"] = json!(missing);
        }
    }
    // Disclose only joins established by saved readback, including generated
    // names. An applied explicit mapping acknowledges its destination name.
    let acknowledged: HashSet<String> = made
        .iter()
        .filter(|entry| {
            entry["pins"]
                .as_array()
                .is_some_and(|pins| !pins.is_empty())
        })
        .filter_map(|entry| {
            let original = template_connection_net(&entry["connection"])?;
            let destination = net_mappings.get(original)?;
            (entry["net"].as_str() == Some(destination.as_str())).then(|| destination.clone())
        })
        .collect();
    let joined_existing = wired
        .values()
        .filter(|net| existing_nets.contains(*net) && !acknowledged.contains(*net))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(TemplateWiring {
        made,
        unresolved,
        unconnected,
        joined_existing,
    })
}

/// The explicit net named by a connection, before caller mappings.
fn template_connection_net(connection: &serde_json::Value) -> Option<&str> {
    connection["to_net"].as_str().or_else(|| {
        let from = connection["from"].as_str()?;
        (connection["to"].is_string() && !from.contains('.')).then_some(from)
    })
}

/// Merge groups linked through shared pins without turning generated names into
/// competing electrical constraints. Distinct explicit names stay separate for
/// the conflict check; otherwise prefer an explicit name, then name order.
fn normalize_template_nets(
    nets: &mut std::collections::BTreeMap<String, Vec<TemplatePin>>,
    made: &mut [serde_json::Value],
    mut generated: std::collections::BTreeSet<String>,
) {
    while let Some((from, to)) = generated.iter().find_map(|from| {
        let pins = nets.get(from)?;
        nets.iter()
            .filter(|(to, other)| {
                *to != from
                    && (!generated.contains(*to) || *to < from)
                    && other.iter().any(|pin| pins.contains(pin))
            })
            .min_by_key(|(to, _)| (generated.contains(*to), *to))
            .map(|(to, _)| (from.clone(), to.clone()))
    }) {
        let pins = nets.remove(&from).expect("planned generated net");
        nets.get_mut(&to).expect("planned target net").extend(pins);
        generated.remove(&from);
        for entry in made.iter_mut() {
            if entry["net"].as_str() == Some(from.as_str()) {
                entry["net"] = json!(to);
            }
        }
    }
}

/// Confirm the requested named label and each claimed pin share a saved net root.
/// Compare identities because KiCad may report another, higher-priority alias.
fn verify_template_wiring(
    path: &std::path::Path,
    wired: &std::collections::HashMap<TemplatePin, String>,
) -> anyhow::Result<()> {
    use copperpour_sexp::schematic::{extract_all_net_labels, extract_wires};
    let (_, tree) = copperpour_sexp::schematic::read_schematic(path)?;
    let labels = extract_all_net_labels(&tree);
    let mut graph =
        crate::tools::sch_connectivity::net_graph_for(&tree, &extract_wires(&tree), &labels);
    let placed = crate::tools::PlacedPins::new(&tree);
    for ((reference, number), net) in wired {
        let pin = placed
            .resolve(reference, number)
            .map_err(|error| anyhow::anyhow!(error.describe()))?;
        let (x, y) = pin.endpoint();
        let root = graph.root_at(x, y);
        anyhow::ensure!(
            labels
                .iter()
                .any(|label| label.net == *net && graph.root_at(label.x, label.y) == root),
            "saved readback did not connect {reference}.{number} to '{net}'"
        );
    }
    Ok(())
}

async fn handle_list_categories(
    _args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let templates = load_all_templates().await;
    let mut categories: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for tmpl in &templates {
        let cat = tmpl["category"]
            .as_str()
            .unwrap_or("uncategorized")
            .to_string();
        *categories.entry(cat).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = categories.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "categories": sorted.iter().map(|(cat, count)| json!({"category": cat, "count": count})).collect::<Vec<_>>(),
            "total_templates": templates.len()
        }))
        .unwrap(),
    ))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Find the next available reference number by scanning existing references in the schematic.
fn find_next_ref_number(content: &str) -> usize {
    let mut max_ref = 0usize;
    let mut pos = 0;
    while let Some(ref_pos) = content[pos..].find("(reference \"") {
        let abs = pos + ref_pos + 12;
        if let Some(end) = content[abs..].find('"') {
            let reference = &content[abs..abs + end];
            // Extract the numeric suffix
            let num_str: String = reference
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if let Ok(num) = num_str.parse::<usize>() {
                if num > max_ref {
                    max_ref = num;
                }
            }
        }
        pos = abs + 1;
    }
    max_ref + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::{ServerConfig, KICAD_ENV_LOCK};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    /// Runs `test` again in a child process whose `HOME` and `APPDATA` are a
    /// fresh directory, and returns whether this is that child. Setting them
    /// in this process moved every parallel test's lock directory into a
    /// directory that was then deleted (#160).
    fn in_isolated_home(test: &str) -> bool {
        const CHILD: &str = "COPPERPOUR_TEST_TEMPLATE_HOME_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        // The child inherits the `KICAD*_DIR` variables other tests set.
        let _guard = KICAD_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, "1")
            .env("APPDATA", home.path())
            .env("HOME", home.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    fn test_ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext::new(
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
        ))
    }

    /// Run only inside [`in_isolated_home`], since it writes a user template.
    fn template_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let template_dir = user_templates_dir().unwrap();
        std::fs::create_dir_all(&template_dir).unwrap();
        std::fs::write(
            template_dir.join("one_resistor.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "test_one_resistor",
                "name": "One resistor",
                "description": "Hermetic placement fixture",
                "category": "test",
                "components": [
                    {"ref_prefix": "R", "lib_id": "Device:R", "value": "4.7k"}
                ],
                "connections": [],
                "design_notes": "test"
            }))
            .unwrap(),
        )
        .unwrap();

        // A real KiCad schematic fixture, including a fully embedded
        // Device:R definition, makes this independent of a KiCad installation.
        let schematic = dir.path().join("derived_lib_name.kicad_sch");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/derived_lib_name.kicad_sch"
            ),
            &schematic,
        )
        .unwrap();
        std::fs::write(dir.path().join("derived_lib_name.kicad_pro"), "{}\n").unwrap();

        (dir, schematic)
    }

    async fn call_registered(
        definitions: Vec<ToolDef>,
        name: &str,
        args: serde_json::Value,
        ctx: Arc<ToolContext>,
    ) -> CallToolResult {
        let tool = definitions
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("registered tool");
        (tool.handler)(&args, ctx).await.unwrap()
    }

    /// KiCad 10.0.8-resaved output of the installed server's I2C template.
    /// See tests/fixtures/template_wiring_kicad10.README.md for the oracle.
    fn wiring_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        std::fs::create_dir_all(user_templates_dir().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let schematic = dir.path().join("pullups.kicad_sch");
        for (source, destination) in [
            ("template_wiring_kicad10.kicad_sch", "pullups.kicad_sch"),
            ("template_wiring_kicad10.kicad_pro", "pullups.kicad_pro"),
        ] {
            std::fs::copy(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures")
                    .join(source),
                dir.path().join(destination),
            )
            .unwrap();
        }
        (dir, schematic)
    }

    #[tokio::test]
    async fn apply_template_leaves_malformed_or_unsupported_routes_unresolved() {
        if !in_isolated_home("tools::templates::tests::apply_template_leaves_malformed_or_unsupported_routes_unresolved") { return; }
        for invalid in [
            json!({"from":"R1.1","to":"R1.2","via":"R2"}),
            json!({"from":"R1.1","to_net":"BAD","via":123}),
            json!({"from":"R1.1","to":"R1.2","to_net":"BAD"}),
            json!({"from":"R1.1","to_net":" "}),
            json!({"from":" ","to":"R1.1"}),
            json!({"to":"R1.1"}),
            json!({"from":"R1.1","to":"R1.2","to_net":"BAD","via":"R1"}),
            json!({"from":"R1.1","to":123,"to_net":"BAD"}),
            json!({"from":"R1.1","to_net":123,"to":"R1.2"}),
            json!({"from":"R1.1","to_net":"BAD","via":""}),
            json!({"from":"CTRL","to":"R1.1","via":"R1"}),
        ] {
            let (_dir, schematic) = wiring_fixture();
            std::fs::write(user_templates_dir().unwrap().join("invalid_routes.json"), serde_json::to_vec(&json!({"id":"test_invalid_routes","components":[{"ref_prefix":"R","lib_id":"Device:R","value":"1k"}],"connections":[invalid,{"from":"R1.2","to_net":"GOOD","notes":"unrelated metadata is allowed"}]})).unwrap()).unwrap();
            let result = handle_apply_template(&json!({"schematic":schematic,"template_id":"test_invalid_routes","position_x":180.34,"position_y":101.6}), &test_ctx()).await.unwrap();
            assert!(!result.is_error, "{invalid}: {result:?}");
            let body: serde_json::Value =
                serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
            assert_eq!(
                body["connections_unresolved"].as_array().unwrap().len(),
                1,
                "{invalid}: {body}"
            );
            assert_eq!(body["connections_unresolved"][0]["connection"], invalid);
            assert_eq!(
                body["unconnected_pins"],
                json!(["R3.1"]),
                "{invalid}: {body}"
            );
            assert_eq!(
                body["connections_made"].as_array().unwrap().len(),
                1,
                "{invalid}: {body}"
            );
            assert_eq!(body["connections_made"][0]["net"], "GOOD");
            assert_eq!(body["connections_made"][0]["pins"], json!(["R3.2"]));
            let (_, tree) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
            let labels = copperpour_sexp::schematic::extract_all_net_labels(&tree);
            let wires = copperpour_sexp::schematic::extract_wires(&tree);
            assert_eq!(labels.len(), 1, "{invalid}");
            assert_eq!(labels[0].net, "GOOD");
            assert_eq!(
                wires.len(),
                1,
                "only the valid entry may add a wire: {invalid}"
            );
            let mut graph = crate::tools::sch_connectivity::net_graph_for(&tree, &wires, &labels);
            let pins = crate::tools::PlacedPins::new(&tree);
            let (x, y) = pins
                .resolve("R3", "1")
                .unwrap_or_else(|_| panic!("missing pin"))
                .endpoint();
            assert_eq!(
                graph.net_at(x, y),
                None,
                "invalid routing must not short the resistor: {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn apply_template_keeps_distinct_numbers_with_the_same_pin_name_separate() {
        if !in_isolated_home("tools::templates::tests::apply_template_keeps_distinct_numbers_with_the_same_pin_name_separate") { return; }
        std::fs::create_dir_all(user_templates_dir().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx();
        let created = call_registered(
            crate::tools::project::tools(),
            "create_project",
            json!({"path":dir.path(),"name":"power_rows_kicad10"}),
            ctx.clone(),
        )
        .await;
        assert!(!created.is_error, "{created:?}");
        let schematic = dir.path().join("power_rows_kicad10.kicad_sch");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/power_rows_kicad10.kicad_sch"),
            &schematic,
        )
        .unwrap();
        // KiCad's fixture oracle: distinct VDD pins 24/36 are at different points.
        std::fs::write(
            user_templates_dir().unwrap().join("repeated_names.json"),
            serde_json::to_vec(&json!({"id":"test_repeated_names","components":[
            {"ref_prefix":"U","lib_id":"MCU_ST_STM32F1:STM32F103C8Tx","value":"STM32F103C8Tx"},
            {"ref_prefix":"R","lib_id":"Device:R","value":"1k","quantity":2}
        ],"connections":[{"from":"U.24","to":"R1.1"},{"from":"U.36","to":"R2.1"}]}))
            .unwrap(),
        )
        .unwrap();
        let result = handle_apply_template(&json!({"schematic":schematic,"template_id":"test_repeated_names","position_x":230.0,"position_y":101.6}), &ctx).await.unwrap();
        assert!(!result.is_error, "{result:?}");
        let body: serde_json::Value =
            serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
        assert_eq!(body["connections_unresolved"], json!([]), "{body}");
        assert_eq!(body["connections_made"][0]["net"], "U9_24");
        assert_eq!(body["connections_made"][1]["net"], "U9_36");
        let (_, tree) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
        let labels = copperpour_sexp::schematic::extract_all_net_labels(&tree);
        let mut graph = crate::tools::sch_connectivity::net_graph_for(
            &tree,
            &copperpour_sexp::schematic::extract_wires(&tree),
            &labels,
        );
        let pins = crate::tools::PlacedPins::new(&tree);
        let a = pins
            .resolve("U9", "24")
            .unwrap_or_else(|_| panic!("missing VDD 24"))
            .endpoint();
        let b = pins
            .resolve("U9", "36")
            .unwrap_or_else(|_| panic!("missing VDD 36"))
            .endpoint();
        assert_ne!(
            graph.root_at(a.0, a.1),
            graph.root_at(b.0, b.1),
            "a repeated library name must not invent a connection"
        );
    }

    #[tokio::test]
    async fn apply_template_merges_shared_pin_chains_and_an_explicit_net() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_merges_shared_pin_chains_and_an_explicit_net",
        ) {
            return;
        }
        let (_dir, schematic) = wiring_fixture();
        let (_, before) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
        let original: Vec<_> = before.find_all("symbol").into_iter().cloned().collect();
        let mut template = json!({"id":"test_shared_chain", "components":[{"ref_prefix":"R", "lib_id":"Device:R", "value":"1k", "quantity":3}], "connections":[
            {"from":"R1.1", "to":"R2.1"},
            {"from":"R2.1", "to":"R3.1"},
            {"from":"R1.2", "to_net":"GND"},
            {"from":"R2.2", "to_net":"GND"},
            {"from":"R3.2", "to_net":"GND"}
        ]});
        // The same constraints must work with either an internal or an explicitly named net.
        for (index, expected) in ["R3_1", "SENSE_BUS"].into_iter().enumerate() {
            if index == 1 {
                template["connections"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"from":"R3.1", "to_net":"SENSE"}));
            }
            std::fs::write(
                user_templates_dir().unwrap().join("shared_chain.json"),
                serde_json::to_vec(&template).unwrap(),
            )
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("pullups.kicad_sch");
            std::fs::copy(&schematic, &path).unwrap();
            std::fs::copy(
                schematic.with_extension("kicad_pro"),
                path.with_extension("kicad_pro"),
            )
            .unwrap();
            let result = handle_apply_template(&json!({"schematic":path, "template_id":"test_shared_chain", "position_x":180.34, "position_y":101.6, "net_mappings":{"SENSE":"SENSE_BUS"}}), &test_ctx()).await.unwrap();
            assert!(!result.is_error, "{result:?}");
            let body: serde_json::Value =
                serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
            assert_eq!(body["connections_unresolved"], json!([]), "{body}");
            assert_eq!(body["unconnected_pins"], json!([]), "{body}");
            assert_eq!(body["connections_made"][0]["net"], expected);
            assert_eq!(body["connections_made"][1]["net"], expected);
            verify_template_wiring(
                &path,
                &std::collections::HashMap::from([
                    (("R3".into(), "1".into()), expected.into()),
                    (("R4".into(), "1".into()), expected.into()),
                    (("R5".into(), "1".into()), expected.into()),
                ]),
            )
            .unwrap();
            let (_, after) = copperpour_sexp::schematic::read_schematic(&path).unwrap();
            for symbol in &original {
                assert!(
                    after.find_all("symbol").contains(&symbol),
                    "existing R1/R2 changed"
                );
            }
        }
    }

    #[tokio::test]
    async fn apply_template_reports_only_observed_existing_net_joins() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_reports_only_observed_existing_net_joins",
        ) {
            return;
        }
        let (_dir, schematic) = wiring_fixture();
        let ctx = test_ctx();
        for (reference, net) in [("R1", "SDA"), ("R2", "R3_1")] {
            let result = call_registered(crate::tools::sch_batch::tools(), "batch_connect_to_net", json!({"schematic":schematic, "net":net, "pins":[{"reference":reference,"pin_number":"1"}]}), ctx.clone()).await;
            assert!(!result.is_error, "{result:?}");
        }
        std::fs::write(user_templates_dir().unwrap().join("observed_joins.json"), serde_json::to_vec(&json!({"id":"test_observed_joins", "components":[{"ref_prefix":"R","lib_id":"Device:R","value":"1k","quantity":2}], "connections":[
            {"from":"R1.1","to":"R2.1"},
            {"from":"Q.1","to_net":"SDA"}
        ]})).unwrap()).unwrap();
        let result = handle_apply_template(&json!({"schematic":schematic,"template_id":"test_observed_joins","position_x":180.34,"position_y":101.6}), &ctx).await.unwrap();
        assert!(!result.is_error, "{result:?}");
        let body: serde_json::Value =
            serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
        assert_eq!(
            body["joined_existing_nets"],
            json!(["R3_1"]),
            "SDA was never joined; the generated name was"
        );
        assert_eq!(body["connections_unresolved"].as_array().unwrap().len(), 1);
        verify_template_wiring(
            &schematic,
            &std::collections::HashMap::from([
                (("R3".into(), "1".into()), "R3_1".into()),
                (("R4".into(), "1".into()), "R3_1".into()),
            ]),
        )
        .unwrap();
    }

    #[test]
    fn template_endpoints_resolve_numbers_before_names_and_all_named_pins() {
        use std::collections::HashMap;
        let references = HashMap::from([("U".into(), "U7".into())]);
        let pins = HashMap::from([(
            "U7".into(),
            vec![
                ("1".into(), "VDD".into()),
                ("2".into(), "VDD".into()),
                ("3".into(), "1".into()),
            ],
        )]);
        assert_eq!(
            resolve_template_endpoint("U.1", &references, &pins).unwrap(),
            vec![("U7".into(), "1".into())]
        );
        assert_eq!(
            resolve_template_endpoint("U.VDD", &references, &pins).unwrap(),
            vec![("U7".into(), "1".into()), ("U7".into(), "2".into())]
        );
        for endpoint in ["SIGNAL", "Q.1", "U.ABSENT"] {
            assert!(resolve_template_endpoint(endpoint, &references, &pins).is_err());
        }
    }

    #[tokio::test]
    async fn apply_template_wires_via_and_pin_pairs_without_claiming_a_conflicting_net() {
        if !in_isolated_home("tools::templates::tests::apply_template_wires_via_and_pin_pairs_without_claiming_a_conflicting_net") { return; }
        let (_dir, schematic) = wiring_fixture();
        std::fs::write(user_templates_dir().unwrap().join("chain.json"), serde_json::to_vec(&json!({
            "id":"test_chain", "components":[{"ref_prefix":"R", "lib_id":"Device:R", "value":"1k", "quantity":3}],
            "connections":[
                {"from":"GPIO", "to":"R1.1"},
                {"from":"R1.2", "to":"R2.1"},
                {"from":"R2.2", "via":"R3", "to_net":"GND"},
                {"from":"R1.1", "to_net":"WRONG"}
            ]
        })).unwrap()).unwrap();
        let result = handle_apply_template(&json!({"schematic":schematic, "template_id":"test_chain", "position_x":180.34, "position_y":101.6, "net_mappings":{"GPIO":"INPUT"}}), &test_ctx()).await.unwrap();
        assert!(!result.is_error, "{result:?}");
        let body: serde_json::Value =
            serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
        assert_eq!(body["unconnected_pins"], json!([]), "{body}");
        assert_eq!(
            body["connections_unresolved"].as_array().unwrap().len(),
            1,
            "{body}"
        );
        assert!(
            body["connections_unresolved"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("requested by both net 'INPUT' and net 'WRONG'"),
            "{body}"
        );
        let made = body["connections_made"].as_array().unwrap();
        assert_eq!(made[0]["net"], "INPUT");
        assert_eq!(made[0]["pins"], json!(["R3.1"]));
        assert_eq!(made[1]["pins"], json!(["R3.2", "R4.1"]));
        assert_eq!(made[2]["pins"], json!(["R4.2", "R5.1"]));
        assert_eq!(made[3]["pins"], json!(["R5.2"]));
        assert_eq!(
            made[4]["pins"],
            json!([]),
            "a pin on INPUT must not be reported on WRONG"
        );
        assert_eq!(made[4]["not_connected"], json!(["R3.1"]));
        let (_, tree) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
        let labels = copperpour_sexp::schematic::extract_all_net_labels(&tree);
        assert!(!labels.iter().any(|label| label.net == "WRONG"));
        // Reject both a nonexistent label and an existing label on another net.
        for net in ["WRONG", "GND"] {
            let wrong = std::collections::HashMap::from([(("R3".into(), "1".into()), net.into())]);
            assert!(verify_template_wiring(&schematic, &wrong).is_err(), "{net}");
        }
    }

    #[tokio::test]
    async fn apply_template_reports_implicit_existing_net_joins() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_reports_implicit_existing_net_joins",
        ) {
            return;
        }
        let (_dir, schematic) = wiring_fixture();
        let ctx = test_ctx();
        let connected = call_registered(crate::tools::sch_batch::tools(), "batch_connect_to_net",
            json!({"schematic":schematic, "net":"SDA", "pins":[{"reference":"R1", "pin_number":"1"}], "stub_length":2.54}), ctx.clone()).await;
        assert!(!connected.is_error, "{connected:?}");
        // A global alias wins KiCad's reported name without changing net identity.
        let alias = call_registered(crate::tools::sch_batch::tools(), "batch_connect_to_net",
            json!({"schematic":schematic, "net":"BUS", "pins":[{"reference":"R1", "pin_number":"1"}], "label_type":"global_label"}), ctx.clone()).await;
        assert!(!alias.is_error, "{alias:?}");
        verify_template_wiring(
            &schematic,
            &std::collections::HashMap::from([(("R1".into(), "1".into()), "SDA".into())]),
        )
        .unwrap();
        let result = handle_apply_template(&json!({"schematic":schematic, "template_id":"i2c_pullups", "position_x":180.34, "position_y":101.6}), &ctx).await.unwrap();
        assert!(!result.is_error, "{result:?}");
        let body: serde_json::Value =
            serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
        assert_eq!(body["joined_existing_nets"], json!(["SDA"]));
        assert_eq!(body["connections_unresolved"], json!([]));
        assert_eq!(body["unconnected_pins"], json!([]));
        assert!(body["connections_to_wire"].is_array());
        assert!(body["next_steps"].is_string());
        let (_, tree) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
        let labels = copperpour_sexp::schematic::extract_all_net_labels(&tree);
        let mut graph = crate::tools::sch_connectivity::net_graph_for(
            &tree,
            &copperpour_sexp::schematic::extract_wires(&tree),
            &labels,
        );
        let pins = crate::tools::PlacedPins::new(&tree);
        let mut root = |reference: &str, number: &str| {
            let (x, y) = pins
                .resolve(reference, number)
                .unwrap_or_else(|_| panic!("missing pin"))
                .endpoint();
            graph.root_at(x, y)
        };
        assert_eq!(root("R1", "1"), root("R3", "1"));
        assert_ne!(root("R1", "2"), root("R3", "1"));
        let mapped = handle_apply_template(&json!({"schematic":schematic, "template_id":"i2c_pullups", "position_x":230.0, "position_y":101.6, "net_mappings":{"SDA":"SDA", "SCL":"SCL", "VCC_3V3":"VCC_3V3"}}), &ctx).await.unwrap();
        assert!(!mapped.is_error, "{mapped:?}");
        let mapped: serde_json::Value =
            serde_json::from_str(&crate::tools::sch_batch::error_text(&mapped)).unwrap();
        assert_eq!(
            mapped["joined_existing_nets"],
            json!([]),
            "explicit mappings already acknowledge the joins"
        );
    }

    #[tokio::test]
    async fn apply_template_refuses_malformed_net_mappings_before_placing() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_refuses_malformed_net_mappings_before_placing",
        ) {
            return;
        }
        let (_dir, schematic) = wiring_fixture();
        let before = std::fs::read(&schematic).unwrap();
        for mappings in [json!(false), json!({"SDA":123}), json!({"SDA":" "})] {
            let result = handle_apply_template(&json!({"schematic":schematic, "template_id":"i2c_pullups", "net_mappings":mappings}), &test_ctx()).await.unwrap();
            let body: serde_json::Value =
                serde_json::from_str(&crate::tools::sch_batch::error_text(&result)).unwrap();
            assert!(result.is_error, "{body}");
            assert_eq!(body["error"]["kind"], "invalid_argument");
            assert_eq!(body["error"]["field"], "net_mappings");
            assert_eq!(std::fs::read(&schematic).unwrap(), before);
        }
    }

    /// The map is executed, not handed back (#89): each entry that names a
    /// pin is wired to its (mapped) net, an entry naming no pin is reported
    /// with why, and pins no entry reaches are listed as still unconnected.
    #[tokio::test]
    async fn apply_template_wires_its_connection_map() {
        // Copperpour's fixture writes templates to user_templates_dir(), so
        // the test runs in a child with its own HOME, as the other fixture
        // tests do. Without that, the merged test wrote into the developer's
        // real ~/.copperpour/templates (found by step 6 review 1).
        if !in_isolated_home("tools::templates::tests::apply_template_wires_its_connection_map") {
            return;
        }
        let (_dir, schematic) = wiring_fixture();
        let template_dir = user_templates_dir().unwrap();
        std::fs::write(
            template_dir.join("pullups.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "test_pullups",
                "name": "Pull-ups",
                "description": "Wiring fixture",
                "category": "test",
                "components": [
                    {"ref_prefix": "R", "lib_id": "Device:R", "value": "4.7k", "quantity": 3}
                ],
                "connections": [
                    {"from": "R1.1", "to_net": "SDA"},
                    {"from": "R1.2", "to_net": "VCC_3V3"},
                    {"from": "R2.1", "to_net": "SCL"},
                    {"from": "R2.2", "to_net": "VCC_3V3"},
                    {"from": "R3.9", "to_net": "X"},
                    {"from": "Q.1", "to_net": "Y"}
                ],
                "design_notes": "test"
            }))
            .unwrap(),
        )
        .unwrap();
        let ctx = test_ctx();

        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 89, "method": "tools/call",
                "params": {"name": "apply_template", "arguments": {
                    "schematic": schematic.display().to_string(),
                    "template_id": "test_pullups",
                    "position_x": 180.34,
                    "position_y": 101.6,
                    "net_mappings": {"SDA": "I2C_SDA", "VCC_3V3": "+3V3"}
                }}
            }))
            .await
            .unwrap();
        let applied: CallToolResult = serde_json::from_value(response.result.unwrap()).unwrap();
        assert!(!applied.is_error, "{applied:?}");
        let crate::mcp::protocol::ToolContent::Text { text } = &applied.content[0] else {
            panic!("expected text")
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();

        // The fixture already holds R1 and R2, so the template's R1..R3 are R3..R5.
        let made: Vec<(String, Vec<String>)> = body["connections_made"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["net"].as_str().unwrap().to_string(),
                    serde_json::from_value(m["pins"].clone()).unwrap(),
                )
            })
            .collect();
        let expected: Vec<(String, Vec<String>)> = vec![
            ("I2C_SDA".into(), vec!["R3.1".into()]),
            ("+3V3".into(), vec!["R3.2".into()]),
            ("SCL".into(), vec!["R4.1".into()]),
            ("+3V3".into(), vec!["R4.2".into()]),
        ];
        assert_eq!(made, expected, "{body}");
        let reasons: Vec<&str> = body["connections_unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["reason"].as_str().unwrap())
            .collect();
        assert_eq!(
            reasons,
            [
                "R5 has no pin numbered or named '9'",
                "the template has no part 'Q'"
            ],
            "{body}"
        );
        assert_eq!(body["unconnected_pins"], json!(["R5.1", "R5.2"]), "{body}");
        assert!(body["next_steps"].is_string(), "{body}");

        // The written file says the same: each pin is on its net.
        for (reference, pin, net) in [
            ("R3", "1", "I2C_SDA"),
            ("R3", "2", "+3V3"),
            ("R4", "1", "SCL"),
            ("R4", "2", "+3V3"),
        ] {
            let result = call_registered(
                crate::tools::sch_analysis::tools(),
                "get_pin_net_name",
                json!({
                    "schematic": schematic.display().to_string(),
                    "reference": reference,
                    "pin_number": pin
                }),
                ctx.clone(),
            )
            .await;
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text")
            };
            let pin_net: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(pin_net["net"], net, "{reference}.{pin}: {pin_net}");
        }
        let (_, tree) = copperpour_sexp::schematic::read_schematic(&schematic).unwrap();
        let libraries = tree.find("lib_symbols").unwrap().find_all("symbol");
        let mut bottom: Option<f64> = None;
        for instance in copperpour_sexp::schematic::extract_symbol_instances(&tree)
            .iter()
            .filter(|instance| ["R3", "R4", "R5"].contains(&instance.reference.as_str()))
        {
            let symbol = copperpour_sexp::schematic::find_lib_symbol(&libraries, instance).unwrap();
            let reported = body["components_placed"]
                .as_array()
                .unwrap()
                .iter()
                .find(|placed| placed["reference"].as_str() == Some(instance.reference.as_str()))
                .unwrap();
            assert_eq!(reported["x"].as_f64(), Some(instance.x));
            assert_eq!(reported["y"].as_f64(), Some(instance.y));
            let bounds =
                copperpour_sexp::schematic::symbol_bounds_for_instance(symbol, instance).unwrap();
            if let Some(previous) = bottom {
                assert!(
                    bounds.min_y - previous >= 10.16 - 1e-6,
                    "room for stubs below the previous symbol"
                );
            }
            bottom = Some(bounds.max_y);
        }
    }

    #[tokio::test]
    async fn apply_template_uses_project_identity_and_leaves_batch_writes_usable() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_uses_project_identity_and_leaves_batch_writes_usable",
        ) {
            return;
        }
        let (_dir, schematic) = template_fixture();
        let ctx = test_ctx();

        let applied = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor",
                "position_x": 180.0,
                "position_y": 100.0
            }),
            ctx.clone(),
        )
        .await;
        assert!(!applied.is_error, "{applied:?}");

        let saved = copperpour_schematic_editor::Schematic::load(&schematic).unwrap();
        let placed = saved
            .symbols
            .as_slice()
            .iter()
            .find(|symbol| symbol.position().0 > 175.0)
            .expect("template-created symbol");
        let instances = placed.instances();
        assert_eq!(instances.len(), 1, "{instances:?}");
        assert_eq!(instances[0].project.as_deref(), Some("derived_lib_name"));
        assert_eq!(
            instances[0].path.as_deref(),
            Some("/11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(instances[0].reference.as_deref(), Some("R3"));
        assert_eq!(instances[0].unit, Some(1));
        let source = std::fs::read_to_string(&schematic).unwrap();
        assert!(!source.contains("(project \"\""), "{source}");

        // Exercise the registered batch tool after the template mutation. The
        // original defect made this exact next call fail stale_target.
        let batch = call_registered(
            crate::tools::sch_batch::tools(),
            "batch_place_components",
            json!({
                "schematic": schematic.display().to_string(),
                "components": [{
                    "lib_id": "Device:R", "reference": "R99",
                    "x": 190.0, "y": 100.0
                }]
            }),
            ctx,
        )
        .await;
        assert!(!batch.is_error, "{batch:?}");
        assert!(copperpour_schematic_editor::Schematic::load(&schematic)
            .unwrap()
            .symbols
            .by_reference("R99")
            .is_some());
    }

    #[tokio::test]
    async fn apply_template_refuses_malformed_instance_metadata_without_writing() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_refuses_malformed_instance_metadata_without_writing",
        ) {
            return;
        }
        let (_dir, schematic) = template_fixture();
        let malformed = std::fs::read_to_string(&schematic).unwrap().replacen(
            "(project \"derived_lib_name\"",
            "(project \"\"",
            1,
        );
        assert_ne!(
            malformed,
            std::fs::read_to_string(&schematic).unwrap(),
            "fixture must contain an instance project to corrupt"
        );
        std::fs::write(&schematic, &malformed).unwrap();
        let before = std::fs::read(&schematic).unwrap();

        let refused = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor"
            }),
            test_ctx(),
        )
        .await;
        assert!(refused.is_error, "{refused:?}");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refused).as_deref(),
            Some("stale_target")
        );
        assert_eq!(std::fs::read(&schematic).unwrap(), before);
    }

    /// KiCad's editor lock refuses the template before anything is written,
    /// so the caller hears `conflict`, not an uncertain outcome (#427).
    #[tokio::test]
    async fn apply_template_refuses_a_locked_schematic_without_writing() {
        if !in_isolated_home(
            "tools::templates::tests::apply_template_refuses_a_locked_schematic_without_writing",
        ) {
            return;
        }
        let (_dir, schematic) = template_fixture();
        let lock = schematic.with_file_name("~derived_lib_name.kicad_sch.lck");
        std::fs::write(&lock, r#"{"hostname":"x","username":"y"}"#).unwrap();
        let before = std::fs::read(&schematic).unwrap();

        let refused = call_registered(
            tools(),
            "apply_template",
            json!({
                "schematic": schematic.display().to_string(),
                "template_id": "test_one_resistor"
            }),
            test_ctx(),
        )
        .await;
        assert!(refused.is_error, "{refused:?}");
        assert_eq!(
            crate::mcp::error::extract_error_kind(&refused).as_deref(),
            Some("conflict"),
            "{refused:?}"
        );
        let crate::mcp::protocol::ToolContent::Text { text } = &refused.content[0] else {
            panic!("expected text content: {refused:?}");
        };
        // The decoded message: JSON escapes a Windows path's backslashes.
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(&lock.display().to_string()), "{text}");
        assert!(message.contains("wrote nothing"), "{text}");
        assert_eq!(std::fs::read(&schematic).unwrap(), before);
        assert!(lock.exists(), "the lock is KiCad's to remove");
    }

    /// Every name a pin answers to: its number, its name and its alternate
    /// functions (`(alternate "SYS_JTMS-SWDIO" …)`).
    fn pin_names(symbol: &copperpour_schematic_editor::sexp::SexpNode, out: &mut BTreeSet<String>) {
        for child in symbol.args() {
            if child.tag() == Some("pin") {
                for tag in ["name", "number", "alternate"] {
                    for node in child.find_all(tag) {
                        if let Some(value) = node.value() {
                            out.insert(value.to_string());
                        }
                    }
                }
            } else {
                pin_names(child, out);
            }
        }
    }

    /// `"U.VIN"` → (`"U"`, `None`, `"VIN"`); `"C2.1"` → (`"C"`, `Some(2)`, `"1"`).
    /// Endpoints without a pin (`"GPIO"`) are not pin references.
    fn split_endpoint(endpoint: &str) -> Option<(&str, Option<usize>, &str)> {
        let (designator, pin) = endpoint.split_once('.')?;
        let digits = designator.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        let prefix = &designator[..designator.len() - digits.len()];
        let index = if digits.is_empty() {
            None
        } else {
            Some(digits.parse().ok()?)
        };
        Some((prefix, index, pin))
    }

    /// The bundled templates name real KiCad symbols and real pins (#783).
    ///
    /// Templates are hand-written JSON that nothing compiles: `Conn_ARM_SWD_10`
    /// and `USB_C_Receptacle_USB2.0` are not in KiCad 10's library, so
    /// `apply_template` refused both templates outright, and maps such as
    /// `U.VIN` on AMS1117 (whose pins are `VI`/`VO`) sent the agent to pins
    /// that do not exist. Checked against the installed library, so the next
    /// KiCad rename fails here rather than in a user's schematic. Skips when
    /// no KiCad symbol library is installed.
    #[test]
    fn builtin_templates_name_symbols_and_pins_that_exist_in_kicad() {
        let _guard = KICAD_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if crate::tools::find_kicad_library_dirs("symbols").is_empty() {
            // The E2E workflow installs KiCad and exports KICAD_CLI; a skip
            // there would mean the check never runs anywhere.
            assert!(
                std::env::var_os("KICAD_CLI").is_none(),
                "KICAD_CLI is set but no KiCad symbol library was found"
            );
            eprintln!("skipped: no installed KiCad symbol library");
            return;
        }
        let source = crate::tools::library::KiCadSymbolSource::new(None);
        let mut problems = Vec::new();

        for template in builtin_templates() {
            let id = template["id"].as_str().unwrap();
            // Expand quantities so `C2` means the template's second capacitor.
            let mut parts: Vec<(String, String, BTreeSet<String>)> = Vec::new();
            for component in template["components"].as_array().unwrap() {
                let prefix = component["ref_prefix"].as_str().unwrap().to_string();
                let lib_id = component["lib_id"].as_str().unwrap().to_string();
                let mut pins = BTreeSet::new();
                match copperpour_schematic_editor::library::resolve_lib_symbol_flattened_node(
                    &lib_id, &source,
                ) {
                    Some(symbol) => {
                        pin_names(&symbol, &mut pins);
                        if pins.is_empty() {
                            problems.push(format!("{id}: read no pins from {lib_id}"));
                        }
                    }
                    None => problems.push(format!("{id}: {lib_id} is not in the KiCad library")),
                }
                for _ in 0..component["quantity"].as_u64().unwrap_or(1) {
                    parts.push((prefix.clone(), lib_id.clone(), pins.clone()));
                }
            }

            for connection in template["connections"].as_array().unwrap() {
                for key in ["from", "to"] {
                    let Some(endpoint) = connection[key].as_str() else {
                        continue;
                    };
                    let Some((prefix, index, pin)) = split_endpoint(endpoint) else {
                        continue;
                    };
                    let candidates: Vec<_> = parts.iter().filter(|p| p.0 == prefix).collect();
                    let part = match index {
                        Some(n) => candidates.get(n.wrapping_sub(1)).copied(),
                        None if candidates.iter().all(|p| p.1 == candidates[0].1) => {
                            candidates.first().copied()
                        }
                        None => None,
                    };
                    match part {
                        None => problems.push(format!("{id}: {endpoint} names no single part")),
                        Some((_, lib_id, pins)) if !pins.is_empty() && !pins.contains(pin) => {
                            problems.push(format!("{id}: {endpoint} — {lib_id} has no pin {pin}"))
                        }
                        Some(_) => {}
                    }
                }
            }
        }

        assert!(
            problems.is_empty(),
            "templates disagree with the KiCad library:\n  {}",
            problems.join("\n  ")
        );
    }
}
