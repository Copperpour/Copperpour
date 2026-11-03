//! The `pcb_copper` toolset: batched copper edits on the board open in KiCad
//! (#95). `apply_copper` creates a planned batch of tracks and through vias in
//! one KiCad commit; deleting and re-netting by UUID list are to follow here.

use crate::tools::ToolDef;

pub fn tools() -> Vec<ToolDef> {
    vec![super::pcb_copper_apply::tool()]
}
