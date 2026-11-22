//! The `pcb_copper` toolset: copper edits on the board open in KiCad (#95).
//! `apply_copper` creates a planned batch of tracks and through vias in one
//! KiCad commit; deleting and re-netting by UUID list are to follow here.
//! `get_teardrop_settings` and `set_teardrop_settings` read and change native
//! pad and via teardrops (#555). `set_via_positions` batch-moves existing vias (#651).
//! `transform_pcb_items` moves or turns board items as one rigid block (#652).

use crate::tools::ToolDef;

pub fn tools() -> Vec<ToolDef> {
    let mut tools = vec![
        super::pcb_copper_apply::tool(),
        super::pcb_via_positions::tool(),
        super::pcb_item_transform::tool(),
    ];
    tools.extend(super::pcb_teardrops::tools());
    tools
}
