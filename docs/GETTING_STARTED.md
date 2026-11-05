# Getting started

You drive Copperpour by talking to your AI client; it picks the tools. The prompts
below are examples, not fixed commands. Copperpour loads tools in groups on demand
(`list_toolboxes` / `load_toolset`), so you never need to enable anything by hand.

### 1. Open or create a project

> Create a new KiCad project called `blinky` in `~/kicad/blinky`.

> Open the project at `~/kicad/blinky/blinky.kicad_pro`.

Copperpour edits the schematic file directly, so KiCad's **Schematic Editor must be
closed** while the AI works on it. Copperpour refuses to write while KiCad's lock file
(`~<name>.kicad_sch.lck`) exists, so it never overwrites unsaved changes. Use the
viewer (next step) to follow along instead.

### 2. Watch the schematic live

> Open the schematic viewer.

The viewer (`open_schematic_viewer`) re-renders every time the schematic changes,
including hierarchical sub-sheets. Pan with click-drag, zoom with the wheel, `0` to
fit, `R` to refresh. Keep it beside your AI client while you work.

### 3. Draw the schematic

> Add an STM32 minimal circuit from the templates, then a 3.3 V LDO powered from USB-C.

> Connect PA5 to an LED with a 1 k series resistor to GND.

> Run ERC and fix what you can.

Parts come from KiCad's own symbol and footprint libraries, the same ones you use
in KiCad (`search_symbols`, `search_footprints`). Reference templates (USB-C sink,
LDO, buck, STM32, I2C, LED) come with verified component values.

### 4. Optional: source parts from JLCPCB

If you plan to have JLCPCB assemble the board, Copperpour can match parts against
their stock. This needs a one-time download of their catalog (about 800k parts,
takes a while):

> Download the JLCPCB parts database.

> Find an in-stock 0402 100 nF capacitor, Basic part preferred.

> Suggest JLCPCB alternatives for every part without an LCSC number.

> Import LCSC part C2856837 and place it on the schematic.

Imported parts get their symbol, footprint, LCSC number and datasheet link, and
are registered in the project's libraries.

### 5. Lay out the PCB

Now open the board in KiCad's **PCB Editor**, with the KiCad API enabled
([Installation](../README.md#installation), Step 1). Copperpour edits the open
board live, and every change lands in KiCad's undo history, so **Ctrl+Z** works.

> Update the PCB from the schematic and set the board outline to 50 × 30 mm.

> Place the USB-C connector on the left edge and the decoupling caps next to the MCU.

> Route the board with Freerouting. *(needs Freerouting installed; see
> [Autorouting](../README.md#advanced-installation))*

> Run DRC and list the violations.

### 6. Review

> Run a full design review.

This checks decoupling, connections, power rails, manufacturability and the BOM,
and reports `INCOMPLETE` rather than passing when it couldn't check everything.

### 7. Export for manufacturing

> Export Gerbers, drill files, BOM and pick-and-place.

> Validate the board for my fab house, then export the manufacturing package.

Validation knows the limits of JLCPCB, PCBWay and OSH Park; for any other fab house
it uses generic limits and the export is a generic package. For JLCPCB there are
extras: cost estimates priced from the catalog, and assembly files that need the
board open in KiCad. Always check the fab's placement preview before ordering.

For the full list of tools, see [tool-directory.md](../tool-directory.md).
