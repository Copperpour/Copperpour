//! Served checks of `transform_pcb_items` against a KiCad double seeded from
//! KiCad 10's own captured answers (`issue_96_ipc.README.md`,
//! `issue_240_ipc.README.md`, `pcb_item_transform_kicad10/README.md`).
use super::*;
use crate::mcp::handler::McpHandler;
use crate::test_support::MockIpcServer;
use crate::tools::pcb_board::board_mock::{
    kicad_bounding_boxes, spawn_kicad_holding_board, spawn_kicad_holding_board_refusing,
};
use copperpour_ipc::gen::kiapi::board::commands as board_commands;
use copperpour_ipc::gen::kiapi::board::types::{BoardLayer, Field};
use copperpour_ipc::gen::kiapi::common::commands;
use copperpour_ipc::gen::kiapi::common::types::{
    graphic_shape::Geometry as Shape, poly_line_node::Geometry as Node, ItemRequestStatus,
};
use copperpour_ipc::transform::{transform_polygon, Xform};
use std::sync::Mutex;

/// One board's captured KiCad answers, as the double serves them.
struct Capture {
    /// The file name KiCad holds the board under.
    board_name: &'static str,
    board: &'static str,
    /// `GetItems` of each kind the board holds.
    items: &'static [&'static [u8]],
    /// `GetItems` of groups, where captured.
    groups: Option<&'static [u8]>,
    enabled_layers: &'static [u8],
    presence: &'static [u8],
    /// `GetPadShapeAsPolygon` for every pad as placed, by layer.
    outlines: [(BoardLayer, &'static [u8]); 2],
    /// `GetBoundingBox` of every footprint as placed, where captured.
    boxes: Option<&'static [u8]>,
}

/// The #96 repro board: R1, R2, three NET_C tracks and two vias.
static ISSUE_96: Capture = Capture {
    board_name: "issue_96.kicad_pcb",
    board: include_str!("../../tests/fixtures/issue_96.kicad_pcb"),
    items: &[
        include_bytes!("../../tests/fixtures/issue_96_footprints.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_96_tracks.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_96_vias.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_96_arcs.ipc.bin"),
    ],
    groups: None,
    enabled_layers: include_bytes!("../../tests/fixtures/issue_96_enabled_layers.ipc.bin"),
    presence: include_bytes!("../../tests/fixtures/issue_96_presence.ipc.bin"),
    outlines: [
        (
            BoardLayer::BlFCu,
            include_bytes!("../../tests/fixtures/issue_96_polygons_before_F_Cu.ipc.bin"),
        ),
        (
            BoardLayer::BlBCu,
            include_bytes!("../../tests/fixtures/issue_96_polygons_before_B_Cu.ipc.bin"),
        ),
    ],
    boxes: None,
};

/// Y1 with a rounded rectangle and U1 with textboxes, no copper.
static ISSUE_240: Capture = Capture {
    board_name: "issue_240.kicad_pcb",
    board: include_str!("../../tests/fixtures/issue_240.kicad_pcb"),
    items: &[
        include_bytes!("../../tests/fixtures/issue_240_footprints.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_240_tracks.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_240_vias.ipc.bin"),
        include_bytes!("../../tests/fixtures/issue_240_arcs.ipc.bin"),
    ],
    groups: None,
    enabled_layers: include_bytes!("../../tests/fixtures/issue_240_enabled_layers.ipc.bin"),
    presence: include_bytes!("../../tests/fixtures/issue_240_presence.ipc.bin"),
    outlines: [
        (
            BoardLayer::BlFCu,
            include_bytes!("../../tests/fixtures/issue_240_polygons_F_Cu.ipc.bin"),
        ),
        (
            BoardLayer::BlBCu,
            include_bytes!("../../tests/fixtures/issue_240_polygons_B_Cu.ipc.bin"),
        ),
    ],
    boxes: None,
};

/// Every kind the tool moves, nine groups and the items it refuses
/// (`pcb_item_transform_kicad10/README.md`), with pcbnew's own moves of
/// each item in `oracle.json`.
static KINDS: Capture = Capture {
    board_name: "pcb_item_transform_kicad10.kicad_pcb",
    board: include_str!(
        "../../tests/fixtures/pcb_item_transform_kicad10/pcb_item_transform_kicad10.kicad_pcb"
    ),
    items: &[
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/footprints.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/tracks.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/vias.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/arcs.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/texts.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/text_boxes.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/shapes.ipc.bin"),
        include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/zones.ipc.bin"),
    ],
    groups: Some(include_bytes!(
        "../../tests/fixtures/pcb_item_transform_kicad10/groups.ipc.bin"
    )),
    enabled_layers: include_bytes!(
        "../../tests/fixtures/pcb_item_transform_kicad10/enabled_layers.ipc.bin"
    ),
    presence: include_bytes!("../../tests/fixtures/pcb_item_transform_kicad10/presence.ipc.bin"),
    outlines: [
        (
            BoardLayer::BlFCu,
            include_bytes!(
                "../../tests/fixtures/pcb_item_transform_kicad10/pad_polygons_F_Cu.ipc.bin"
            ),
        ),
        (
            BoardLayer::BlBCu,
            include_bytes!(
                "../../tests/fixtures/pcb_item_transform_kicad10/pad_polygons_B_Cu.ipc.bin"
            ),
        ),
    ],
    boxes: Some(include_bytes!(
        "../../tests/fixtures/pcb_item_transform_kicad10/footprint_boxes.ipc.bin"
    )),
};

/// pcbnew's own moves of every item of [`KINDS`].
const ORACLE: &str = include_str!("../../tests/fixtures/pcb_item_transform_kicad10/oracle.json");

// The kinds board's items, as its README names them.
const K_R1: &str = "1246cf7f-374e-404f-934e-571386b0b1a4";
const K_R1_VIA: &str = "c7635528-f2bd-4d58-a0e5-8381c5e06ed6";
const K_R2: &str = "b01d0162-75af-4bc0-88ef-b24aeea43f19";
const K_R2_TRACK: &str = "6fd2a0cb-accf-4103-b913-32d83cf914f9";
const K_R2_VIA: &str = "7337bfc6-a9a6-4e33-a112-9493eca84890";
const K_ARC_GROUPED: &str = "c8b9d2dd-6289-4df1-bab0-94a851fc7e92";
const K_TEXT_CENTRED: &str = "ed03cddb-89f0-4402-811c-c42daae39c81";
const K_TEXT_LOCKED: &str = "99c2fc57-728b-4c1e-8287-079820d10778";
const K_TEXT_BOX: &str = "cfce4625-a94f-4851-815a-d2560a456841";
const K_TEXT_GROUPED: &str = "ee1d28ce-633e-48a3-9285-19d77b920bd3";
const K_SEGMENT: &str = "a2885af4-60b0-428a-b775-dcd5d61d0db1";
const K_ROUNDED: &str = "dd96496c-36a3-4820-8819-83e89fc79a30";
const K_COPPER_SEGMENT: &str = "68957760-5eb5-4c79-8fe1-b75b147d9f19";
const K_LOCKED_SEGMENT: &str = "d7a70902-c034-489b-8580-8b42c2d2ad40";
const K_GROUPED_SEGMENT: &str = "96f65347-cafb-4759-bf45-da9a15caf068";
const K_ZONE_GND: &str = "4227b8fa-ac49-4681-beaa-830ab93169f6";
const K_RULE_AREA: &str = "ab6e0302-903f-4143-8626-12509431f43e";
const K_ZONE_TWO_LAYERS: &str = "3b6f0eec-80e9-4357-9923-b2ef6997b7e5";
const K_ZONE_LOCKED: &str = "450aa490-a376-42a6-9416-d6588e2e4a92";
const K_RULE_AREA_LOCKED: &str = "7a19b987-636d-46ec-926d-1773cd7c7808";
const K_ZONE_GROUPED: &str = "f97b6dd2-b590-43d7-a952-733d21736975";
const K_FLAT_TRACK: &str = "a6c9d48a-4c63-4c0f-96ca-d2807e6fd2e1";
const K_FLAT_VIA: &str = "435ec50a-230b-4d28-a3b7-ef795459dbd2";
const K_INNER_TRACK: &str = "9655b5a0-b4c9-4efd-905e-125fdd00ccbb";
const K_INNER_VIA: &str = "9b98ffbe-27fe-4b2b-a7b0-9f8699c17720";
const K_OUTER_TRACK: &str = "d47eaaea-bb7a-44f1-b72d-e8a6b2e4ef52";
const K_LOCKED_TRACK: &str = "fa2fb718-b36f-407e-9eae-59091f3dd1c2";
const K_LOCKED_VIA: &str = "75d7c08f-94f1-4131-b32f-152135278086";
const K_THREE_VIA: &str = "ab25ca20-e715-4243-9404-8a79df88742a";
const K_THREE_TRACK_A: &str = "5b4f3d5a-ee77-494c-9d34-fa7f340fdd46";
const K_THREE_TRACK_B: &str = "e10cf4bf-67e4-4c81-97c5-c815fe3d287a";
const K_U1_TRACK: &str = "39d2a434-b469-4cbd-baef-4d738c430b87";
const K_R1_TRACK: &str = "1fb1cfb9-ba2a-4a62-9725-57dee911eb77";
const G_FLAT_VIA_TRACK: &str = "1b291518-96d0-44dc-bc77-3d4f5a424991";
const G_FLAT_KINDS: &str = "4b7fc793-81d8-4490-bb07-88d663dc99f0";
const G_FOOTPRINT_VIA: &str = "c3c198cd-6433-46ca-81a1-7caf6ef3d2e2";
const G_INNER: &str = "11369ef1-3a5b-4c95-af61-f91fcb30e7ad";
const G_OUTER: &str = "c726217b-52ef-47c6-bb8c-c7a0412989d0";
const G_MIXED_INNER: &str = "b54ad519-cb4c-4c37-a5f1-014e93b7f4b1";
const G_MIXED_OUTER: &str = "9e1d1756-9122-4ee2-91e2-0f1a16a9300f";
const G_LOCKED: &str = "ce97d006-d9a9-4371-bc45-98f7008a8ccf";
const G_THREE: &str = "07dcbb38-a16c-4021-90aa-137b37777382";
/// The locked items of the kinds board, and the members of its locked group.
const K_LOCKED: [&str; 6] = [
    K_TEXT_LOCKED,
    K_LOCKED_SEGMENT,
    K_ZONE_LOCKED,
    K_RULE_AREA_LOCKED,
    K_LOCKED_TRACK,
    K_LOCKED_VIA,
];

/// An arc track of the four-layer board (`issue_96_layers_ipc.README.md`).
const LAYERS_ARCS: &[u8] = include_bytes!("../../tests/fixtures/issue_96_layers_arcs.ipc.bin");
const LAYERS_ARC: &str = "37d64abc-9509-4951-b192-03a717da9b67";

// The repro board's items, as KiCad names them.
const R1: &str = "10037c7b-af43-4fde-a3cd-11a43a5c9055";
const R1_PAD_1: &str = "868530b1-e4d1-41a1-9f30-416b9442cd39";
const R2: &str = "a23adadf-1dd1-491b-9f4b-997b6570350a";
const R2_PAD_1: &str = "4933a322-03a2-4f6c-872a-de7458bd94b7";
/// (99.175, 110)-(99.175, 106), from R2 pad 1.
const R2_TRACK: &str = "0335fe60-230b-4695-95ac-698836799eeb";
/// (99.175, 106)-(115, 106), ending in the tied via.
const TIED_TRACK: &str = "e86c73df-a536-483a-8c1e-cf0b7f96cf64";
/// (125, 100)-(128, 100).
const STUB: &str = "3236141c-2c15-4b0f-9c9f-b39f826067c1";
/// (115, 106).
const TIED_VIA: &str = "df431fc3-7c5e-4772-b3f8-33744514f439";
/// (120, 100).
const LONE_VIA: &str = "424e71ba-90b9-4287-835a-410e3510db12";
const COPPER_IDS: [&str; 5] = [R2_TRACK, TIED_TRACK, STUB, TIED_VIA, LONE_VIA];

/// The last reason of a move a net change undid.
const UNDONE: &str =
    "the move was undone by a second commit, and KiCad's undo history holds both steps";

/// A change KiCad makes to an item it holds.
type Mangle = fn(&mut Item);

/// A change to KiCad's answer to a `GetItemsById`.
type Misread = fn(&mut Vec<prost_types::Any>);

/// A change to the board text: in the block of item `.0`, or anywhere where
/// that is empty, `.1` becomes `.2`.
type TextChange = (&'static str, &'static str, &'static str);

/// What the double does besides answering as KiCad does.
#[derive(Default)]
struct Faults {
    /// The double puts this copper on NET_A when the first update is pushed:
    /// on this flag, not from geometry, standing in for KiCad re-netting
    /// copper that touches another net's pad.
    renet: Option<&'static str>,
    /// The net it goes to, where not NET_A.
    renet_to: Option<&'static str>,
    /// And again on every later update.
    renet_restore: bool,
    /// KiCad holds a group the first update carried changed by this; with
    /// `mangle_on_restore`, a group the later updates carried.
    group_mangle: Option<fn(&mut Group)>,
    /// Every group is renamed after the second `BeginCommit`.
    restore_group_race: bool,
    /// KiCad moves this copper 1 nm when the first update is pushed, its
    /// message and its block of the board text.
    drift: Option<&'static str>,
    /// KiCad changes its board text when the first update is pushed: in the
    /// block of item `.0`, or anywhere where that is empty, `.1` becomes `.2`.
    text_changes: Vec<TextChange>,
    /// KiCad refills this zone when the first update is pushed: its fill
    /// moves 1 nm.
    refill: Option<&'static str>,
    /// KiCad deletes this copper when the first update is pushed.
    vanish: Option<&'static str>,
    /// A via appears when the first update is pushed.
    appear: bool,
    /// Unselected copper changes net after the second `BeginCommit`.
    restore_copper_race: bool,
    /// `GetItemsById` matches a UUID's text in either case.
    ids_ignore_case: bool,
    /// KiCad holds every item of the first update changed by this.
    mangle: Option<Mangle>,
    /// The mangle falls on the later updates instead.
    mangle_on_restore: bool,
    refuse_second_item: bool,
    failed_restore: bool,
    failed_drop: bool,
    /// `GetItemsById` answers changed by this once an update was sent.
    misread: Option<Misread>,
    /// `GetItemsById` finds nothing inside the first commit.
    blind_in_commit: bool,
    late_update: bool,
    /// The board changes after `BeginCommit`.
    race: bool,
    /// A target changes after `BeginCommit` while the board text does not.
    /// KiCad never does this: its board text holds every target.
    hidden_race: bool,
    /// A target changes after the second `BeginCommit`.
    restore_race: bool,
    /// `GetItems` of groups is refused.
    groups_unreadable: bool,
    /// A command left unanswered.
    mute: Option<&'static str>,
    /// `GetItemsById` answers this UUID with an empty message of this type,
    /// standing in for the types the tool does not move. KiCad never sends
    /// one empty.
    foreign: Option<(&'static str, &'static str)>,
}

struct State {
    capture: &'static Capture,
    /// Every item, in KiCad's order.
    items: Vec<prost_types::Any>,
    groups: Vec<Group>,
    /// The board text: the capture's, and a block for each cloned footprint.
    /// A commit's update leaves it as it is, so a moved item's block keeps
    /// its old geometry, which the board text comparison leaves out; a fault
    /// that stands for KiCad changing the text on the push edits it.
    text: String,
    generation: u32,
    /// How many times an item was changed outside a commit's update.
    edits: u32,
    /// The update count at each `GetItems` of groups.
    group_reads: Vec<usize>,
    /// The board text `SaveDocumentToString` answers whatever the items are.
    frozen: Option<String>,
    /// A clone's pad or via KIID: the captured one it was made from, and how
    /// far it was moved.
    sources: HashMap<String, (String, (i64, i64))>,
    precommit: Option<(Vec<prost_types::Any>, Vec<Group>)>,
    sent: Vec<String>,
    updates: usize,
    /// The KIID of every item each `UpdateItems` carried.
    updated: Vec<String>,
    faults: Faults,
}

impl State {
    fn of(capture: &'static Capture) -> Self {
        Self {
            capture,
            items: capture
                .items
                .iter()
                .flat_map(|bytes| commands::GetItemsResponse::decode(*bytes).unwrap().items)
                .collect(),
            groups: capture.groups.map_or_else(Vec::new, |bytes| {
                let listed = commands::GetItemsResponse::decode(bytes).unwrap().items;
                let group = |any: prost_types::Any| Group::decode(any.value.as_slice()).unwrap();
                listed.into_iter().map(group).collect()
            }),
            text: capture.board.to_string(),
            generation: 0,
            edits: 0,
            group_reads: Vec::new(),
            frozen: None,
            sources: HashMap::new(),
            precommit: None,
            sent: Vec::new(),
            updates: 0,
            updated: Vec::new(),
            faults: Faults::default(),
        }
    }

    fn item(&self, id: &str) -> Item {
        let any = self
            .items
            .iter()
            .find(|item| item_kiid(item).as_deref() == Some(id))
            .unwrap();
        Item::decode(any).unwrap().unwrap()
    }

    fn footprint(&self, id: &str) -> FootprintInstance {
        match self.item(id) {
            Item::Footprint(footprint) => *footprint,
            other => panic!("{id} is a {}", other.kind()),
        }
    }

    /// Change item `id` as `change` does, keeping KiCad's message otherwise,
    /// outside a commit's update: the board text's state node changes too.
    fn edit(&mut self, id: &str, change: impl FnOnce(&mut Item)) {
        self.edits += 1;
        self.change(id, change);
    }

    /// Change item `id` as KiCad does on the push; its block of the board
    /// text is the fault's to change.
    fn change(&mut self, id: &str, change: impl FnOnce(&mut Item)) {
        let slot = self
            .items
            .iter_mut()
            .find(|item| item_kiid(item).as_deref() == Some(id))
            .unwrap();
        let mut item = Item::decode(slot).unwrap().unwrap();
        change(&mut item);
        *slot = item.pack();
    }

    fn pads(&self) -> Vec<Pad> {
        self.items
            .iter()
            .filter_map(|any| match Item::decode(any).unwrap() {
                Some(Item::Footprint(footprint)) => Some(pads_of(&footprint).unwrap()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn net(&self, name: &str) -> Net {
        self.pads()
            .into_iter()
            .find_map(|pad| pad.net.filter(|net| net.name == name))
            .unwrap()
    }

    /// Footprint `source` again as `reference`, `by` nm away, under KIIDs of
    /// its own for it and its pads: the captured message and its block of the
    /// board text, standing in for a footprint KiCad would send the same way.
    fn clone_footprint(
        &mut self,
        source: &str,
        reference: &str,
        index: usize,
        by: (i64, i64),
    ) -> String {
        let id = format!("65200000-0000-4000-8000-{index:012}");
        let mut footprint = self.footprint(source);
        let (dx_nm, dy_nm) = by;
        transform_footprint_children(&mut footprint, &Xform::Translate { dx_nm, dy_nm }).unwrap();
        let anchor = footprint.position.as_mut().unwrap();
        anchor.x_nm += dx_nm;
        anchor.y_nm += dy_nm;
        footprint.id = Some(Kiid { value: id.clone() });
        let field = footprint.reference_field.as_mut().unwrap();
        field.text.as_mut().unwrap().text.as_mut().unwrap().text = reference.to_string();
        let items = &mut footprint.definition.as_mut().unwrap().items;
        for (pad_index, item) in items.iter_mut().enumerate() {
            if !any_is(item, PAD) {
                continue;
            }
            let mut pad = Pad::decode(item.value.as_slice()).unwrap();
            let pad_id = format!("65200000-0000-4000-8001-{index:06}{pad_index:06}");
            let was = pad.id.replace(Kiid {
                value: pad_id.clone(),
            });
            self.sources.insert(pad_id, (was.unwrap().value, by));
            if pad.parent.is_some() {
                pad.parent = Some(Kiid { value: id.clone() });
            }
            item.value = pad.encode_to_vec();
        }
        self.items.push(pack_any(&footprint, FOOTPRINT));
        let anchor = format!("\t\t(uuid \"{source}\")");
        let at = self.text.find(&anchor).unwrap();
        let start = self.text[..at].rfind("\t(footprint ").unwrap();
        let end = at + self.text[at..].find("\n\t)\n").unwrap() + "\n\t)".len();
        let block = self.text[start..end].replace(&anchor, &format!("\t\t(uuid \"{id}\")"));
        let body = self.text.trim_end().strip_suffix(')').unwrap();
        self.text = format!("{body}{block}\n)\n");
        id
    }

    /// Via `source` again, `by` nm away, under a KIID of its own. KiCad never
    /// sends this: a via in the items but not in the board text.
    fn clone_via(&mut self, source: &str, index: usize, by: (i64, i64)) -> String {
        let id = format!("65300000-0000-4000-8000-{index:012}");
        let Item::Via(mut via) = self.item(source) else {
            panic!("{source} is not a via")
        };
        via.id = Some(Kiid { value: id.clone() });
        let position = via.position.as_mut().unwrap();
        position.x_nm += by.0;
        position.y_nm += by.1;
        self.sources.insert(id.clone(), (source.to_string(), by));
        self.items.push(pack_any(via.as_ref(), VIA));
        id
    }

    /// KiCad's board text, and a digest of the groups and of the changes made
    /// outside a commit's update, so that it changes whenever KiCad's own
    /// text would for them. KiCad never writes that last node.
    fn snapshot(&self) -> String {
        if let Some(frozen) = &self.frozen {
            return frozen.clone();
        }
        let mut hash = Sha256::new();
        for group in &self.groups {
            hash.update(group.encode_to_vec());
        }
        hash.update(self.edits.to_le_bytes());
        hash.update(self.generation.to_le_bytes());
        let body = self.text.trim_end().strip_suffix(')').unwrap();
        format!(
            "{body}\t(copperpour_test_state \"{}\")\n)\n",
            encode_hex(&hash.finalize())
        )
    }

    fn count(&self, command: &str) -> usize {
        self.sent.iter().filter(|sent| *sent == command).count()
    }

    /// The board text with `from` replaced by `to` once: in the top-level
    /// block of item `id`, or anywhere when `id` is empty.
    fn change_text(&mut self, id: &str, from: &str, to: &str) {
        let (start, end) = match id {
            "" => (0, self.text.len()),
            id => {
                let at = self.text.find(&format!("(uuid \"{id}\")")).unwrap();
                let start = self.text[..at].rfind("\n\t(").unwrap();
                let end = at
                    + self.text[at..]
                        .find("\n\t(")
                        .unwrap_or(self.text.len() - at);
                (start, end)
            }
        };
        let found = start + self.text[start..end].find(from).unwrap();
        self.text.replace_range(found..found + from.len(), to);
    }

    /// Footprint `id`'s box, `(x, y, width, height)` in mm. KiCad never sent
    /// these: it is the box of the footprint's pads' captured outlines on
    /// every captured layer, inside KiCad's own box, which holds every pad's
    /// (`pcbnew/footprint.cpp:1802-1803` at 10.0.6).
    fn footprint_box(&self, id: &str) -> (f64, f64, f64, f64) {
        let (mut low, mut high) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
        for pad in pads_of(&self.footprint(id)).unwrap() {
            let pad_id = pad.id.unwrap().value;
            let (source, (dx, dy)) = self
                .sources
                .get(&pad_id)
                .cloned()
                .unwrap_or((pad_id, (0, 0)));
            for (_, bytes) in &self.capture.outlines {
                let captured = board_commands::PadShapeAsPolygonResponse::decode(*bytes).unwrap();
                let Some(index) = captured.pads.iter().position(|id| id.value == source) else {
                    continue;
                };
                let outline = captured.polygons[index].outline.as_ref().unwrap();
                for node in &outline.nodes {
                    let Some(Node::Point(p)) = node.geometry else {
                        panic!("a captured outline node that is not a point")
                    };
                    low = (low.0.min(p.x_nm + dx), low.1.min(p.y_nm + dy));
                    high = (high.0.max(p.x_nm + dx), high.1.max(p.y_nm + dy));
                }
            }
        }
        let mm = nm_to_mm;
        (mm(low.0), mm(low.1), mm(high.0 - low.0), mm(high.1 - low.1))
    }
}

fn locked(item: &mut Item) {
    let locked = LockedState::LsLocked as i32;
    match item {
        Item::Footprint(footprint) => footprint.locked = locked,
        Item::Via(via) => via.locked = locked,
        Item::Track(track) => track.locked = locked,
        Item::Arc(arc) => arc.locked = locked,
        Item::Text(text) => text.locked = locked,
        Item::TextBox(text_box) => text_box.locked = locked,
        Item::Graphic(graphic) => graphic.locked = locked,
        Item::Zone(zone) => zone.locked = locked,
    }
}

fn kind_of(any: &prost_types::Any) -> Kot {
    match Item::decode(any).unwrap().unwrap() {
        Item::Footprint(_) => Kot::KotPcbFootprint,
        Item::Via(_) => Kot::KotPcbVia,
        Item::Track(_) => Kot::KotPcbTrace,
        Item::Arc(_) => Kot::KotPcbArc,
        Item::Text(_) => Kot::KotPcbText,
        Item::TextBox(_) => Kot::KotPcbTextbox,
        Item::Graphic(_) => Kot::KotPcbShape,
        Item::Zone(_) => Kot::KotPcbZone,
    }
}

/// The KIID of an item or a group, as KiCad sent it.
fn item_kiid(any: &prost_types::Any) -> Option<String> {
    if any_is(any, GROUP) {
        return Group::decode(any.value.as_slice())
            .ok()?
            .id
            .map(|id| id.value);
    }
    Item::decode(any).ok().flatten().map(|item| item.id())
}

/// The points of a via, track or arc.
fn copper_points(item: &mut Item) -> Vec<&mut Option<Vector2>> {
    match item {
        Item::Via(via) => vec![&mut via.position],
        Item::Track(track) => vec![&mut track.start, &mut track.end],
        Item::Arc(arc) => vec![&mut arc.start, &mut arc.mid, &mut arc.end],
        other => panic!("{} is not copper", other.kind()),
    }
}

/// KiCad's captured box of each footprint a `GetBoundingBox` names, as the
/// footprint was held when it was captured.
fn captured_boxes(command: &prost_types::Any, captured: &[u8]) -> prost_types::Any {
    let request = commands::GetBoundingBox::decode(command.value.as_slice()).unwrap();
    let captured = commands::GetBoundingBoxResponse::decode(captured).unwrap();
    let boxes = request
        .items
        .iter()
        .map(|id| {
            let index = captured.items.iter().position(|held| held == id);
            captured.boxes[index.unwrap_or_else(|| panic!("no captured box for {}", id.value))]
        })
        .collect();
    pack_any(
        &commands::GetBoundingBoxResponse {
            items: request.items,
            boxes,
        },
        "kiapi.common.commands.GetBoundingBoxResponse",
    )
}

fn items_response(items: Vec<prost_types::Any>) -> prost_types::Any {
    pack_any(
        &commands::GetItemsResponse {
            status: ItemRequestStatus::IrsOk as i32,
            items,
            ..Default::default()
        },
        "kiapi.common.commands.GetItemsResponse",
    )
}

fn recorded(type_name: &str, bytes: &[u8]) -> prost_types::Any {
    prost_types::Any {
        type_url: format!("type.googleapis.com/{type_name}"),
        value: bytes.to_vec(),
    }
}

/// KiCad's answer to `command` against `state`: its items, its commits, and
/// the copper check's reads replayed from the capture. A clone's pad or via
/// answers as the captured one it was made from, its outline carried by the
/// clone's offset; KiCad never drew those outlines.
fn answer(state: &mut State, command: &prost_types::Any) -> Option<prost_types::Any> {
    let name = any_type_name(command)
        .rsplit('.')
        .next()
        .unwrap()
        .to_string();
    state.sent.push(name.clone());
    if state.faults.mute == Some(name.as_str()) {
        return None;
    }
    let bytes = command.value.as_slice();
    Some(match name.as_str() {
        "SaveDocumentToString" => pack_any(
            &commands::SavedDocumentResponse {
                document: commands::SaveDocumentToString::decode(bytes)
                    .unwrap()
                    .document,
                contents: state.snapshot(),
            },
            "kiapi.common.commands.SavedDocumentResponse",
        ),
        "GetItems" => {
            let request = commands::GetItems::decode(bytes).unwrap();
            let mut items: Vec<_> = state
                .items
                .iter()
                .filter(|item| request.types.contains(&(kind_of(item) as i32)))
                .cloned()
                .collect();
            if request.types.contains(&(Kot::KotPcbGroup as i32)) {
                state.group_reads.push(state.updates);
                items.extend(
                    state
                        .groups
                        .iter()
                        .map(|group| pack_any(group, "kiapi.board.types.Group")),
                );
            }
            items_response(items)
        }
        "GetItemsById" => {
            let request = commands::GetItemsById::decode(bytes).unwrap();
            let ids: Vec<_> = request.items.into_iter().map(|id| id.value).collect();
            // KiCad parses a UUID's text into its value (`common/kiid.cpp:135-139`
            // at 10.0.6) rather than comparing text; the double reads either case
            // only on this flag.
            let either_case = state.faults.ids_ignore_case;
            let asked = |id: &str| {
                ids.iter()
                    .any(|asked| asked == id || (either_case && asked.eq_ignore_ascii_case(id)))
            };
            let wanted = |id: &Option<Kiid>| id.as_ref().is_some_and(|id| asked(&id.value));
            let mut items: Vec<_> = state
                .items
                .iter()
                .filter(|item| item_kiid(item).is_some_and(|id| asked(&id)))
                .cloned()
                .collect();
            // KiCad resolves a footprint's child and a group by its KIID too
            // (`BOARD::ResolveItem`, `pcbnew/board.cpp:1870-1937` at 10.0.6).
            for footprint in state.items.iter().filter(|item| any_is(item, FOOTPRINT)) {
                let footprint = FootprintInstance::decode(footprint.value.as_slice()).unwrap();
                for field in fields_of(&footprint) {
                    if wanted(&field.text.as_ref().and_then(|text| text.id.clone())) {
                        items.push(pack_any(field, FIELD));
                    }
                }
                for child in footprint.definition.iter().flat_map(|def| &def.items) {
                    let id = match any_type_name(child) {
                        PAD => Pad::decode(child.value.as_slice()).unwrap().id,
                        _ => Item::decode(child)
                            .unwrap()
                            .map(|child| Kiid { value: child.id() }),
                    };
                    if wanted(&id) {
                        items.push(child.clone());
                    }
                }
            }
            items.extend(
                state
                    .groups
                    .iter()
                    .filter(|group| wanted(&group.id))
                    .map(|group| pack_any(group, "kiapi.board.types.Group")),
            );
            if let Some((_, kind)) = state.faults.foreign.filter(|(id, _)| asked(id)) {
                items.push(recorded(kind, &[]));
            }
            if let Some(misread) = state.faults.misread.filter(|_| state.updates > 0) {
                misread(&mut items);
            }
            if state.faults.blind_in_commit && state.precommit.is_some() && state.updates == 0 {
                items.clear();
            }
            items_response(items)
        }
        "BeginCommit" => {
            if state.faults.race {
                state.generation += 1;
            }
            if state.faults.hidden_race && state.updates == 0 {
                state.frozen = Some(state.snapshot());
                state.edit(LONE_VIA, |item| {
                    if let Item::Via(via) = item {
                        via.is_free = !via.is_free;
                    }
                });
            }
            if state.faults.restore_race && state.updates > 0 {
                state.edit(LONE_VIA, |item| {
                    if let Item::Via(via) = item {
                        via.is_free = !via.is_free;
                    }
                });
            }
            if state.faults.restore_copper_race && state.updates > 0 {
                let net = state.net("NET_A");
                state.edit(TIED_TRACK, |item| {
                    if let Item::Track(track) = item {
                        track.net = Some(net);
                    }
                });
            }
            if state.faults.restore_group_race && state.updates > 0 {
                for group in &mut state.groups {
                    group.name.push_str(" (renamed)");
                }
            }
            state.precommit = Some((state.items.clone(), state.groups.clone()));
            pack_any(
                &commands::BeginCommitResponse {
                    id: Some(Kiid {
                        value: "transform-652".into(),
                    }),
                },
                "kiapi.common.commands.BeginCommitResponse",
            )
        }
        "UpdateItems" => {
            let request = commands::UpdateItems::decode(bytes).unwrap();
            state.updates += 1;
            let first = state.updates == 1;
            state
                .updated
                .extend(request.items.iter().filter_map(item_kiid));
            let mut updated_items = Vec::new();
            for (index, sent) in request.items.iter().enumerate() {
                let refused = (state.faults.refuse_second_item && first && index == 1)
                    || (state.faults.failed_restore && !first);
                if !refused && any_is(sent, GROUP) {
                    let mut group = Group::decode(sent.value.as_slice()).unwrap();
                    let mangled = first != state.faults.mangle_on_restore;
                    if let Some(mangle) = state.faults.group_mangle.filter(|_| mangled) {
                        mangle(&mut group);
                    }
                    let slot = state.groups.iter_mut().find(|held| held.id == group.id);
                    *slot.unwrap() = group;
                } else if !refused {
                    let mut item = sent.clone();
                    let mangled = first != state.faults.mangle_on_restore;
                    if let (true, Some(mangle)) = (mangled, state.faults.mangle) {
                        let mut held = Item::decode(&item).unwrap().unwrap();
                        mangle(&mut held);
                        item = held.pack();
                    }
                    let id = item_kiid(&item);
                    *state
                        .items
                        .iter_mut()
                        .find(|held| item_kiid(held) == id)
                        .unwrap() = item;
                }
                updated_items.push(commands::ItemUpdateResult {
                    status: Some(commands::ItemStatus {
                        code: if refused {
                            commands::ItemStatusCode::IscInvalidData
                        } else {
                            commands::ItemStatusCode::IscOk
                        } as i32,
                        error_message: if refused {
                            "injected refusal".into()
                        } else {
                            String::new()
                        },
                    }),
                    item: Some(sent.clone()),
                });
            }
            if let Some(id) = state
                .faults
                .renet
                .filter(|_| first || state.faults.renet_restore)
            {
                let net = state.net(state.faults.renet_to.unwrap_or("NET_A"));
                state.change(id, |item| match item {
                    Item::Via(via) => via.net = Some(net),
                    Item::Track(track) => track.net = Some(net),
                    Item::Graphic(graphic) => graphic.net = Some(net),
                    _ => panic!("{id} has no net KiCad changes"),
                });
            }
            if let Some(id) = state.faults.drift.filter(|_| first) {
                state.change(id, |item| {
                    for point in copper_points(item).into_iter().flatten() {
                        point.x_nm += 1;
                    }
                });
            }
            if let Some(id) = state.faults.refill.filter(|_| first) {
                state.change(id, |item| {
                    let Item::Zone(zone) = item else {
                        panic!("{id} is not a zone")
                    };
                    let fills = zone.filled_polygons.iter_mut();
                    for fill in fills.filter_map(|fill| fill.shapes.as_mut()) {
                        let lines = fill
                            .polygons
                            .iter_mut()
                            .flat_map(|p| p.outline.iter_mut().chain(&mut p.holes));
                        for node in lines.flat_map(|line| &mut line.nodes) {
                            if let Some(Node::Point(p)) = &mut node.geometry {
                                p.x_nm += 1;
                            }
                        }
                    }
                });
            }
            if first {
                for (id, from, to) in state.faults.text_changes.clone() {
                    state.change_text(id, from, to);
                }
            }
            if let Some(id) = state.faults.vanish.filter(|_| first) {
                state
                    .items
                    .retain(|item| item_kiid(item).as_deref() != Some(id));
            }
            if state.faults.appear && first {
                state.clone_via(TIED_VIA, 900, (0, 1_000_000));
            }
            if state.faults.late_update && first {
                // The production 30 s receive window and its late-reply
                // recovery, not an injected envelope.
                std::thread::sleep(std::time::Duration::from_secs(31));
            }
            pack_any(
                &commands::UpdateItemsResponse {
                    status: ItemRequestStatus::IrsOk as i32,
                    updated_items,
                    ..Default::default()
                },
                "kiapi.common.commands.UpdateItemsResponse",
            )
        }
        "EndCommit" => {
            let request = commands::EndCommit::decode(bytes).unwrap();
            if request.action == commands::CommitAction::CmaDrop as i32 && !state.faults.failed_drop
            {
                (state.items, state.groups) = state.precommit.take().unwrap();
            }
            pack_any(
                &commands::EndCommitResponse {},
                "kiapi.common.commands.EndCommitResponse",
            )
        }
        "GetBoundingBox" => match state.capture.boxes {
            Some(bytes) => captured_boxes(command, bytes),
            None => kicad_bounding_boxes(command, |id| state.footprint_box(id)),
        },
        "GetBoardEnabledLayers" => recorded(
            "kiapi.board.commands.BoardEnabledLayersResponse",
            state.capture.enabled_layers,
        ),
        "CheckPadstackPresenceOnLayers" => {
            let request = board_commands::CheckPadstackPresenceOnLayers::decode(bytes).unwrap();
            let captured =
                board_commands::PadstackPresenceResponse::decode(state.capture.presence).unwrap();
            let mut answer = board_commands::PadstackPresenceResponse::default();
            for item in &request.items {
                let source = state
                    .sources
                    .get(&item.value)
                    .map_or(item.value.as_str(), |(source, _)| source.as_str());
                for entry in &captured.entries {
                    if entry.item.as_ref().unwrap().value == source
                        && request.layers.contains(&entry.layer)
                    {
                        let mut entry = entry.clone();
                        entry.item = Some(item.clone());
                        answer.entries.push(entry);
                    }
                }
            }
            pack_any(&answer, "kiapi.board.commands.PadstackPresenceResponse")
        }
        "GetPadShapeAsPolygon" => {
            let request = board_commands::GetPadShapeAsPolygon::decode(bytes).unwrap();
            let &(_, outlines) = state
                .capture
                .outlines
                .iter()
                .find(|(layer, _)| *layer as i32 == request.layer)?;
            let captured = board_commands::PadShapeAsPolygonResponse::decode(outlines).unwrap();
            let mut answer = board_commands::PadShapeAsPolygonResponse::default();
            for pad in &request.pads {
                let (source, (dx_nm, dy_nm)) = state
                    .sources
                    .get(&pad.value)
                    .cloned()
                    .unwrap_or((pad.value.clone(), (0, 0)));
                let index = captured
                    .pads
                    .iter()
                    .position(|id| id.value == source)
                    .unwrap();
                let mut outline = captured.polygons[index].clone();
                transform_polygon(&mut outline, &Xform::Translate { dx_nm, dy_nm });
                answer.pads.push(pad.clone());
                answer.polygons.push(outline);
            }
            pack_any(&answer, "kiapi.board.commands.PadShapeAsPolygonResponse")
        }
        _ => return None,
    })
}

struct Scene {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    state: std::sync::Arc<Mutex<State>>,
    _server: MockIpcServer,
    handler: McpHandler,
}

impl Scene {
    async fn new(state: State) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(state.capture.board_name);
        std::fs::write(&path, state.capture.board).unwrap();
        let unreadable = state.faults.groups_unreadable;
        let state = std::sync::Arc::new(Mutex::new(state));
        let held = state.clone();
        let respond = move |command: &prost_types::Any| answer(&mut held.lock().unwrap(), command);
        let server = if unreadable {
            spawn_kicad_holding_board_refusing(&path, vec![Kot::KotPcbGroup], respond)
        } else {
            spawn_kicad_holding_board(&path, respond)
        };
        let handler = McpHandler::new(crate::tools::ServerConfig {
            ipc_address: server.address().into(),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            path,
            state,
            _server: server,
            handler,
        }
    }

    async fn call(&self, mut args: Value) -> (bool, Value) {
        args["board"] = json!(self.path);
        let response = self
            .handler
            .handle_message(json!({"jsonrpc": "2.0", "id": 652,
            "method": "tools/call", "params": {"name": TOOL, "arguments": args}}))
            .await
            .unwrap();
        let result = response.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap_or_else(|_| json!({"message": text})),
        )
    }

    async fn apply(&self, mut args: Value) -> (bool, Value) {
        let (error, planned) = self.call(args.clone()).await;
        assert!(!error, "{planned}");
        args["dry_run"] = json!(false);
        args["expected_plan_revision"] = planned["plan_revision"].clone();
        self.call(args).await
    }

    fn items(&self) -> Vec<prost_types::Any> {
        self.state.lock().unwrap().items.clone()
    }

    fn assert_no_writes(&self) {
        let state = self.state.lock().unwrap();
        for command in ["BeginCommit", "UpdateItems", "EndCommit"] {
            assert_eq!(
                state.count(command),
                0,
                "{command} reached KiCad: {:?}",
                state.sent
            );
        }
    }

    /// Asserts a refusal before mutation whose reasons include each of `expected`.
    async fn assert_blocked(&self, args: Value, expected: &[String]) {
        let (error, result) = self.call(args).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
        let reasons = result["error"]["reasons"].as_array().unwrap();
        for reason in expected {
            assert!(reasons.contains(&json!(reason)), "{reason} not in {result}");
        }
        self.assert_no_writes();
    }
}

/// The repro board's five copper items and both footprints.
fn whole_board(offset: Value) -> Value {
    json!({"references": ["R1", "R2"], "uuids": COPPER_IDS, "offset": offset})
}

fn shift(p: &mut Option<Vector2>, (dx, dy): (i64, i64)) {
    let p = p.as_mut().unwrap();
    p.x_nm += dx;
    p.y_nm += dy;
}

/// A text's position moved by `by`, where it has one.
fn shift_text(text: Option<&mut BoardText>, by: (i64, i64)) {
    if let Some(text) = text.and_then(|text| text.text.as_mut()) {
        if text.position.is_some() {
            shift(&mut text.position, by);
        }
    }
}

/// Every point of a footprint's graphic moved by `by`. A shape this test
/// does not know fails it.
fn shift_graphic(graphic: &mut BoardGraphicShape, by: (i64, i64)) {
    match graphic
        .shape
        .as_mut()
        .and_then(|shape| shape.geometry.as_mut())
    {
        Some(Shape::Segment(segment)) => {
            shift(&mut segment.start, by);
            shift(&mut segment.end, by);
        }
        Some(Shape::Rectangle(rectangle)) => {
            shift(&mut rectangle.top_left, by);
            shift(&mut rectangle.bottom_right, by);
        }
        Some(Shape::Arc(arc)) => {
            shift(&mut arc.start, by);
            shift(&mut arc.mid, by);
            shift(&mut arc.end, by);
        }
        Some(Shape::Circle(circle)) => {
            shift(&mut circle.center, by);
            shift(&mut circle.radius_point, by);
        }
        Some(Shape::Polygon(set)) => {
            for polygon in &mut set.polygons {
                for line in polygon.outline.iter_mut().chain(&mut polygon.holes) {
                    for node in &mut line.nodes {
                        let Some(Node::Point(p)) = &mut node.geometry else {
                            panic!("a polygon node this test does not move: {node:?}")
                        };
                        p.x_nm += by.0;
                        p.y_nm += by.1;
                    }
                }
            }
        }
        other => panic!("a graphic this test does not move: {other:?}"),
    }
}

/// Footprint `was` moved by `by`, worked out from its message alone: the
/// anchor, the four fields and every child shifted, every other field kept.
/// A child this test does not know fails it.
fn assert_footprint_moved(held: &FootprintInstance, was: &FootprintInstance, by: (i64, i64)) {
    let mut expected = was.clone();
    shift(&mut expected.position, by);
    for field in [
        &mut expected.reference_field,
        &mut expected.value_field,
        &mut expected.datasheet_field,
        &mut expected.description_field,
    ] {
        shift_text(field.as_mut().and_then(|field| field.text.as_mut()), by);
    }
    for item in &mut expected.definition.as_mut().unwrap().items {
        let bytes = item.value.as_slice();
        item.value = match any_type_name(item) {
            "kiapi.board.types.Pad" => {
                let mut pad = Pad::decode(bytes).unwrap();
                shift(&mut pad.position, by);
                pad.encode_to_vec()
            }
            "kiapi.board.types.BoardGraphicShape" => {
                let mut graphic = BoardGraphicShape::decode(bytes).unwrap();
                shift_graphic(&mut graphic, by);
                graphic.encode_to_vec()
            }
            "kiapi.board.types.BoardText" => {
                let mut text = BoardText::decode(bytes).unwrap();
                shift_text(Some(&mut text), by);
                text.encode_to_vec()
            }
            "kiapi.board.types.Field" => {
                let mut field = Field::decode(bytes).unwrap();
                shift_text(field.text.as_mut(), by);
                field.encode_to_vec()
            }
            "kiapi.board.types.Footprint3DModel" => continue,
            other => panic!("a footprint child this test does not move: {other}"),
        };
    }
    assert_eq!(*held, expected);
}

#[tokio::test]
async fn the_issues_block_moves_by_one_offset_in_one_commit() {
    let mut state = State::of(&ISSUE_96);
    let mut references = vec!["R1".to_string(), "R2".to_string()];
    for index in 0..19 {
        let reference = format!("R{}", index + 3);
        let source = if index % 2 == 0 { R1 } else { R2 };
        state.clone_footprint(
            source,
            &reference,
            index,
            (0, 40_000_000 + index as i64 * 6_000_000),
        );
        references.push(reference);
    }
    let mut uuids: Vec<String> = COPPER_IDS.iter().map(|id| id.to_string()).collect();
    for index in 0..18 {
        uuids.push(state.clone_via(LONE_VIA, index, (0, 40_000_000 + index as i64 * 3_000_000)));
    }
    let before = state.items.clone();
    let scene = Scene::new(state).await;
    let args = json!({"references": references, "uuids": uuids, "offset": {"x": 1.25, "y": -2.5}});
    let (error, result) = scene.apply(args).await;
    assert!(!error, "{result}");
    assert_eq!(result["footprint_count"], 21);
    assert_eq!(result["via_count"], 20);
    assert_eq!(result["track_count"], 3);
    assert_eq!(result["source"], "ipc_readback");
    assert_eq!(result["outcome"]["status"], "complete");
    assert_eq!(result["copper_check"]["status"], "clear");
    assert_eq!(result["boundary_connection_count"], 0);
    let state = scene.state.lock().unwrap();
    assert_eq!(state.count("BeginCommit"), 1);
    assert_eq!(state.count("UpdateItems"), 1);
    assert_eq!(state.count("SaveDocument") + state.count("RefillZones"), 0);
    let by = (1_250_000, -2_500_000);
    for (held, was) in state.items.iter().zip(&before) {
        match (
            Item::decode(held).unwrap().unwrap(),
            Item::decode(was).unwrap().unwrap(),
        ) {
            (Item::Footprint(held), Item::Footprint(was)) => {
                assert_footprint_moved(&held, &was, by)
            }
            (held, mut was) => {
                for point in copper_points(&mut was) {
                    shift(point, by);
                }
                assert_eq!(held, was);
            }
        }
    }
}

/// KiCad's `RotatePoint` by 90° about `(cx, cy)`, exact in integers:
/// `(x, y)` from the centre goes to `(y, -x)`.
fn quarter_turn((x, y): (i64, i64), (cx, cy): (i64, i64)) -> (i64, i64) {
    (cx + (y - cy), cy - (x - cx))
}

fn at(p: Option<Vector2>) -> (i64, i64) {
    let p = p.unwrap();
    (p.x_nm, p.y_nm)
}

#[tokio::test]
async fn a_quarter_turn_lands_where_kicads_integer_rotation_puts_it() {
    let state = State::of(&ISSUE_96);
    let before: Vec<_> = state
        .items
        .iter()
        .map(|any| Item::decode(any).unwrap().unwrap())
        .collect();
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1", "R2"], "uuids": COPPER_IDS,
            "rotation": 90, "origin": {"x": 105, "y": 100}}))
        .await;
    assert!(!error, "{result}");
    let centre = (105_000_000, 100_000_000);
    let state = scene.state.lock().unwrap();
    for was in before {
        let held = state.item(&was.id());
        match (&held, &was) {
            (Item::Footprint(held), Item::Footprint(was)) => {
                assert_eq!(at(held.position), quarter_turn(at(was.position), centre));
                assert_eq!(orientation(held), normalize_deg(orientation(was) + 90.0));
                for (pad, old) in pads_of(held).unwrap().iter().zip(pads_of(was).unwrap()) {
                    assert_eq!(pad.id, old.id);
                    assert_eq!(at(pad.position), quarter_turn(at(old.position), centre));
                    let angle = |pad: &Pad| pad.pad_stack.as_ref().unwrap().angle.unwrap();
                    assert_eq!(
                        angle(pad).value_degrees,
                        within_a_turn(angle(&old).value_degrees + 90.0)
                    );
                }
            }
            (Item::Via(held), Item::Via(was)) => {
                assert_eq!(at(held.position), quarter_turn(at(was.position), centre))
            }
            (Item::Track(held), Item::Track(was)) => {
                assert_eq!(at(held.start), quarter_turn(at(was.start), centre));
                assert_eq!(at(held.end), quarter_turn(at(was.end), centre));
            }
            _ => panic!("{} changed kind", was.id()),
        }
    }
}

#[tokio::test]
async fn many_whole_turns_and_a_quarter_land_as_a_quarter_turn() {
    let turn = |degrees: f64| {
        json!({"references": ["R1", "R2"], "uuids": COPPER_IDS,
            "rotation": degrees, "origin": {"x": 105, "y": 100}})
    };
    let quarter = Scene::new(State::of(&ISSUE_96)).await;
    let (error, result) = quarter.apply(turn(90.0)).await;
    assert!(!error, "{result}");
    // 90 + 360 * 10_000, and 90 + 360 * 25_000_000_000_000, which is below
    // 2^53 and so exact in f64. Near 1e17 none is: f64 steps by 16 there, and
    // 90 + 360k is never a multiple of 16.
    for degrees in [3_600_090.0, 9_000_000_000_000_090.0] {
        let scene = Scene::new(State::of(&ISSUE_96)).await;
        let (_, plan) = scene.call(turn(90.0)).await;
        let (error, planned) = scene.call(turn(degrees)).await;
        assert!(!error, "{planned}");
        for key in ["footprints", "vias", "tracks", "plan_revision"] {
            assert_eq!(planned[key], plan[key], "{degrees}: {key}");
        }
        let (error, result) = scene.apply(turn(degrees)).await;
        assert!(!error, "{result}");
        assert!(
            scene.items() == quarter.items(),
            "{degrees}° lands elsewhere"
        );
    }
}

#[tokio::test]
async fn a_locked_target_is_named_although_another_lands_out_of_range() {
    let mut state = State::of(&ISSUE_96);
    state.edit(TIED_VIA, locked);
    let scene = Scene::new(state).await;
    // The tied via at x = 115 mm stays in range; the lone one at 120 mm leaves it.
    scene
        .assert_blocked(
            json!({"uuids": [TIED_VIA, LONE_VIA], "offset": {"x": 2030, "y": 0}}),
            &[format!(
                "via '{TIED_VIA}' is locked or its lock state is unknown"
            )],
        )
        .await;
}

#[tokio::test]
async fn a_uuid_kicad_finds_under_another_spelling_names_the_one_it_holds() {
    let mut state = State::of(&ISSUE_96);
    state.faults.ids_ignore_case = true;
    let scene = Scene::new(state).await;
    let shouted = STUB.to_uppercase();
    scene
        .assert_blocked(
            json!({"uuids": [shouted], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "item '{shouted}' is track '{STUB}'; give its UUID as KiCad writes it"
            )],
        )
        .await;
}

fn nm_pair(p: Option<Vector2>) -> Value {
    let p = p.unwrap_or_default();
    json!([p.x_nm, p.y_nm])
}

/// An alignment as `oracle.json` names it: pcbnew's `GR_TEXT_*_ALIGN_*`.
fn alignment(attributes: &TextAttributes) -> (&'static str, &'static str) {
    use copperpour_ipc::gen::kiapi::common::types::{
        HorizontalAlignment as H, VerticalAlignment as V,
    };
    let h = match H::try_from(attributes.horizontal_alignment) {
        Ok(H::HaLeft) => "left",
        Ok(H::HaCenter) => "center",
        Ok(H::HaRight) => "right",
        _ => "unknown",
    };
    let v = match V::try_from(attributes.vertical_alignment) {
        Ok(V::VaTop) => "top",
        Ok(V::VaCenter) => "center",
        Ok(V::VaBottom) => "bottom",
        _ => "unknown",
    };
    (h, v)
}

/// A text as `oracle.json` gives one.
fn text_record(text: &copperpour_ipc::gen::kiapi::common::types::Text) -> Value {
    let attributes = text.attributes.clone().unwrap_or_default();
    let (h, v) = alignment(&attributes);
    json!({"at": nm_pair(text.position), "angle": angle_of(Some(&attributes)), "h": h, "v": v,
        "mirrored": attributes.mirrored})
}

/// A graphic as `oracle.json` gives one: pcbnew's shape name and points.
fn shape_record(graphic: &BoardGraphicShape) -> Value {
    let (shape, points) = match geometry(graphic) {
        Some(Geometry::Segment(s)) => ("S_SEGMENT", vec![s.start, s.end]),
        Some(Geometry::Rectangle(r)) => ("S_RECT", vec![r.top_left, r.bottom_right]),
        Some(Geometry::Arc(a)) => ("S_ARC", vec![a.start, a.mid, a.end]),
        Some(Geometry::Circle(c)) => ("S_CIRCLE", vec![c.center, c.radius_point]),
        Some(Geometry::Polygon(set)) => {
            ("S_POLYGON", line_points(set.polygons[0].outline.as_ref()))
        }
        Some(Geometry::Bezier(b)) => ("S_CURVE", vec![b.start, b.control1, b.control2, b.end]),
        None => ("none", Vec::new()),
    };
    let points: Vec<_> = points.into_iter().map(nm_pair).collect();
    json!({"shape": shape, "points": points})
}

fn line_points(
    line: Option<&copperpour_ipc::gen::kiapi::common::types::PolyLine>,
) -> Vec<Option<Vector2>> {
    let nodes = line.map_or(&[][..], |line| &line.nodes);
    nodes
        .iter()
        .map(|node| match node.geometry {
            Some(Node::Point(p)) => Some(p),
            ref other => panic!("a node this test does not read: {other:?}"),
        })
        .collect()
}

/// What `oracle.json` gives for `item`, read from its message.
fn oracle_record(item: &Item) -> Value {
    match item {
        Item::Footprint(footprint) => {
            let mut fields = json!({});
            for (name, field) in [
                ("Reference", &footprint.reference_field),
                ("Value", &footprint.value_field),
                ("Datasheet", &footprint.datasheet_field),
                ("Description", &footprint.description_field),
            ] {
                let text = field.as_ref().and_then(|field| field.text.as_ref());
                let text = text.and_then(|text| text.text.clone()).unwrap_or_default();
                fields[name] = text_record(&text);
            }
            let (mut pads, mut graphics) = (json!({}), json!({}));
            for child in footprint
                .definition
                .iter()
                .flat_map(|definition| &definition.items)
            {
                if any_is(child, PAD) {
                    let pad = Pad::decode(child.value.as_slice()).unwrap();
                    let angle = pad.pad_stack.as_ref().and_then(|stack| stack.angle);
                    pads[pad.id.unwrap().value] = json!({"at": nm_pair(pad.position),
                        "angle": angle.map_or(0.0, |angle| angle.value_degrees)});
                } else if any_is(child, GRAPHIC) {
                    let graphic = BoardGraphicShape::decode(child.value.as_slice()).unwrap();
                    graphics[graphic.id.clone().unwrap().value] = shape_record(&graphic);
                } else if any_is(child, "kiapi.board.types.Field") {
                    let field = Field::decode(child.value.as_slice()).unwrap();
                    let text = field.text.and_then(|text| text.text).unwrap_or_default();
                    fields[field.name] = text_record(&text);
                }
            }
            json!({"at": nm_pair(footprint.position), "orientation": orientation(footprint),
                "pads": pads, "fields": fields, "graphics": graphics})
        }
        Item::Via(via) => json!({"at": nm_pair(via.position)}),
        Item::Track(track) => json!({"start": nm_pair(track.start), "end": nm_pair(track.end)}),
        Item::Arc(arc) => json!({"start": nm_pair(arc.start), "mid": nm_pair(arc.mid),
            "end": nm_pair(arc.end)}),
        Item::Text(text) => text_record(&text.text.clone().unwrap_or_default()),
        Item::TextBox(text_box) => {
            let textbox = text_box.textbox.clone().unwrap_or_default();
            let (tl, br) = (textbox.top_left.unwrap(), textbox.bottom_right.unwrap());
            // `PCB_TEXTBOX::GetCorners` of an unturned box: start, then round.
            let corners = [
                (tl.x_nm, tl.y_nm),
                (br.x_nm, tl.y_nm),
                (br.x_nm, br.y_nm),
                (tl.x_nm, br.y_nm),
            ];
            json!({"shape": "S_RECT", "points": corners.map(|(x, y)| json!([x, y])),
                "angle": angle_of(textbox.attributes.as_ref())})
        }
        Item::Graphic(graphic) => shape_record(graphic),
        Item::Zone(zone) => {
            let polygon = &zone.outline.as_ref().unwrap().polygons[0];
            let outline: Vec<_> = line_points(polygon.outline.as_ref())
                .into_iter()
                .map(nm_pair)
                .collect();
            let holes: Vec<Vec<_>> = polygon
                .holes
                .iter()
                .map(|hole| line_points(Some(hole)).into_iter().map(nm_pair).collect())
                .collect();
            json!({"outline": outline, "holes": holes})
        }
    }
}

/// Each place `ours` differs from pcbnew's `theirs`, with both values.
fn differences(path: &str, ours: &Value, theirs: &Value, out: &mut Vec<String>) {
    match theirs.as_object() {
        Some(fields) => {
            for (key, value) in fields.iter().filter(|(key, _)| *key != "kind") {
                differences(&format!("{path}.{key}"), &ours[key], value, out);
            }
        }
        None if ours != theirs => out.push(format!("{path}: ours {ours}, pcbnew {theirs}")),
        None => {}
    }
}

/// Every case of `oracle.json` applied through the tool to every item the
/// case holds but the locked ones (each group whole), each moved item then
/// compared with pcbnew's own `Move` or `Rotate` of it, to the nanometre and
/// the exact angle.
#[tokio::test]
async fn every_kind_lands_where_pcbnew_puts_it() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let (cx, cy) = (
        oracle["centre_nm"][0].as_i64().unwrap(),
        oracle["centre_nm"][1].as_i64().unwrap(),
    );
    let (dx, dy) = (
        oracle["offset_nm"][0].as_i64().unwrap(),
        oracle["offset_nm"][1].as_i64().unwrap(),
    );
    let mut mismatches = Vec::new();
    for (case, pcbnew) in oracle["cases"].as_object().unwrap() {
        let mut args = match case.strip_prefix("turn_") {
            Some(degrees) => json!({"rotation": degrees.parse::<f64>().unwrap(),
                "origin": {"x": nm_to_mm(cx), "y": nm_to_mm(cy)}}),
            None if case == "offset" => json!({"offset": {"x": nm_to_mm(dx), "y": nm_to_mm(dy)}}),
            None => continue,
        };
        let uuids: Vec<&String> = pcbnew
            .as_object()
            .unwrap()
            .keys()
            .filter(|id| !K_LOCKED.contains(&id.as_str()))
            .collect();
        args["uuids"] = json!(uuids);
        let scene = Scene::new(State::of(&KINDS)).await;
        let (error, result) = scene.apply(args).await;
        assert!(!error, "{case}: {result}");
        let state = scene.state.lock().unwrap();
        for id in uuids {
            let ours = oracle_record(&state.item(id));
            differences(&format!("{case} {id}"), &ours, &pcbnew[id], &mut mismatches);
        }
    }
    // U1's Reference with keep-upright cleared, as the oracle script cleared
    // it before each turn: the capture has it set.
    const U1: &str = "ab4b2e28-2bae-48d6-925a-da00f845a634";
    let variant = &oracle["variants"]["u1_reference_not_keep_upright"];
    for (case, pcbnew) in variant.as_object().unwrap() {
        let degrees: f64 = case.strip_prefix("turn_").unwrap().parse().unwrap();
        let mut state = State::of(&KINDS);
        state.edit(U1, |item| {
            let Item::Footprint(footprint) = item else {
                panic!("U1 is a footprint")
            };
            let reference = footprint.reference_field.as_mut().unwrap();
            let text = reference.text.as_mut().unwrap().text.as_mut().unwrap();
            text.attributes.as_mut().unwrap().keep_upright = false;
        });
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"uuids": [U1], "rotation": degrees,
                "origin": {"x": nm_to_mm(cx), "y": nm_to_mm(cy)}}))
            .await;
        assert!(!error, "{case}: {result}");
        let ours = oracle_record(&scene.state.lock().unwrap().item(U1));
        let label = format!("u1_reference_not_keep_upright {case} {U1}");
        differences(&label, &ours, &pcbnew[U1], &mut mismatches);
    }
    assert!(
        mismatches.is_empty(),
        "{} differences:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The KIIDs every `UpdateItems` carried, in order. The double cannot show
/// KiCad's editor, where the order matters: this is the order KiCad needs
/// (measured on KiCad 10.0.8), which `live_transform_fixture_kinds_and_groups`
/// checks in pcbnew.
fn sent(scene: &Scene) -> Vec<String> {
    scene.state.lock().unwrap().updated.clone()
}

#[tokio::test]
async fn a_whole_group_goes_after_its_moved_members_and_before_its_footprints() {
    let cases: [(&str, &[&str]); 5] = [
        (
            G_FLAT_VIA_TRACK,
            &[K_FLAT_VIA, K_FLAT_TRACK, G_FLAT_VIA_TRACK],
        ),
        (G_FOOTPRINT_VIA, &[K_R1_VIA, G_FOOTPRINT_VIA, K_R1]),
        (
            G_OUTER,
            &[K_INNER_TRACK, K_INNER_VIA, K_OUTER_TRACK, G_OUTER, G_INNER],
        ),
        (
            G_MIXED_OUTER,
            &[K_R2_TRACK, K_R2_VIA, G_MIXED_OUTER, G_MIXED_INNER, K_R2],
        ),
        (
            G_FLAT_KINDS,
            &[
                K_GROUPED_SEGMENT,
                K_ARC_GROUPED,
                K_TEXT_GROUPED,
                K_ZONE_GROUPED,
                G_FLAT_KINDS,
            ],
        ),
    ];
    for (group, order) in cases {
        let state = State::of(&KINDS);
        let groups = state.groups.clone();
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"uuids": [group], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(!error, "{group}: {result}");
        assert_eq!(result["outcome"]["status"], "complete", "{group}: {result}");
        assert_eq!(sent(&scene), order, "{group}");
        let touched = order
            .iter()
            .filter(|id| groups.iter().any(|g| group_id(g) == **id));
        assert_eq!(result["group_count"], touched.count(), "{group}: {result}");
        // The groups were read back after the update.
        let reads = scene.state.lock().unwrap().group_reads.clone();
        assert!(
            reads.iter().any(|updates| *updates > 0),
            "{group}: {reads:?}"
        );
    }
}

#[tokio::test]
async fn a_group_uuid_stands_for_its_members_through_its_inner_groups() {
    let scene = Scene::new(State::of(&KINDS)).await;
    let (error, result) = scene
        .call(json!({"uuids": [G_OUTER], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(!error, "{result}");
    let ids = |key: &str| -> Vec<Value> {
        result[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["uuid"].clone())
            .collect()
    };
    assert_eq!(
        ids("tracks"),
        json!([K_INNER_TRACK, K_OUTER_TRACK])
            .as_array()
            .unwrap()
            .clone()
    );
    assert_eq!(ids("vias"), vec![json!(K_INNER_VIA)]);
    assert_eq!(
        result["groups"],
        json!([{"uuid": G_OUTER, "name": "OUTER", "member_count": 2},
            {"uuid": G_INNER, "name": "INNER", "member_count": 2}])
    );
}

#[tokio::test]
async fn an_item_reached_by_its_uuid_and_through_its_group_is_one_target() {
    for uuids in [
        [G_FLAT_VIA_TRACK, K_FLAT_VIA],
        [K_FLAT_VIA, G_FLAT_VIA_TRACK],
    ] {
        let scene = Scene::new(State::of(&KINDS)).await;
        let (error, result) = scene
            .apply(json!({"uuids": uuids, "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(!error, "{result}");
        assert_eq!(result["via_count"], 1, "{result}");
        assert_eq!(sent(&scene), [K_FLAT_VIA, K_FLAT_TRACK, G_FLAT_VIA_TRACK]);
    }
}

#[tokio::test]
async fn a_group_left_partly_behind_or_locked_refuses_naming_it() {
    let scene = Scene::new(State::of(&KINDS)).await;
    scene
        .assert_blocked(
            json!({"uuids": [K_THREE_VIA, K_THREE_TRACK_A, G_LOCKED], "offset": {"x": 1, "y": 0}}),
            &[
                format!(
                    "group 'THREE' ({G_THREE}) would move only in part, leaving out 1 of its 3 \
                     members ({K_THREE_TRACK_B}); give the group's UUID to move it whole"
                ),
                format!("group 'LOCKED' ({G_LOCKED}) is locked or its lock state is unknown"),
            ],
        )
        .await;
    // INNER alone leaves OUTER's own track behind.
    scene
        .assert_blocked(
            json!({"uuids": [G_INNER], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "group 'OUTER' ({G_OUTER}) would move only in part, leaving out 1 of its 2 \
                 members ({K_OUTER_TRACK}); give the group's UUID to move it whole"
            )],
        )
        .await;
}

#[tokio::test]
async fn a_group_holding_an_item_the_tool_does_not_move_refuses_naming_it() {
    const DIMENSION: &str = "65700000-0000-4000-8000-000000000002";
    const ONLY: &str = "65400000-0000-4000-8000-000000000011";
    const WITH_VIA: &str = "65400000-0000-4000-8000-000000000012";
    const EMPTY: &str = "65400000-0000-4000-8000-000000000013";
    let mut state = State::of(&KINDS);
    // KiCad never writes these groups; a group can hold a dimension, which
    // the double answers empty.
    state.faults.foreign = Some((DIMENSION, "kiapi.board.types.Dimension"));
    state.groups.extend([
        group(ONLY, "ONLY_DIMENSION", &[DIMENSION]),
        group(WITH_VIA, "DIMENSION_AND_VIA", &[DIMENSION, K_LONE_VIA]),
        group(EMPTY, "EMPTY", &[]),
    ]);
    let scene = Scene::new(state).await;
    let holds = |id: &str, name: &str| {
        format!(
            "group '{name}' ({id}) holds '{DIMENSION}', a Dimension, which this tool does not move"
        )
    };
    for (id, name) in [(ONLY, "ONLY_DIMENSION"), (WITH_VIA, "DIMENSION_AND_VIA")] {
        scene
            .assert_blocked(
                json!({"uuids": [id], "offset": {"x": 1, "y": 0}}),
                &[holds(id, name)],
            )
            .await;
    }
    scene
        .assert_blocked(
            json!({"uuids": [EMPTY], "offset": {"x": 1, "y": 0}}),
            &[format!("group 'EMPTY' ({EMPTY}) holds nothing")],
        )
        .await;
}

#[tokio::test]
async fn a_group_kicad_holds_changed_after_the_commit_is_uncertain() {
    let mut state = State::of(&KINDS);
    state.faults.group_mangle = Some(|group| {
        group.items.pop();
    });
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [G_FLAT_VIA_TRACK], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["kind"], "mutation_outcome_uncertain",
        "{result}"
    );
    assert_eq!(
        result["uuids"],
        json!([K_FLAT_VIA, K_FLAT_TRACK, G_FLAT_VIA_TRACK])
    );
    let message = result["message"].as_str().unwrap();
    assert!(
        message.contains(&format!(
            "read back differs from the plan: group 'FLAT_VIA_TRACK' ({G_FLAT_VIA_TRACK})"
        )),
        "{result}"
    );
}

#[tokio::test]
async fn a_group_changed_after_the_dry_run_makes_the_plan_stale() {
    let mut state = State::of(&KINDS);
    // KiCad never does this: board text that stays as it was while a group
    // changes. The plan binds the group's own message.
    state.frozen = Some(state.snapshot());
    let scene = Scene::new(state).await;
    let mut args = json!({"uuids": [G_FLAT_VIA_TRACK], "offset": {"x": 1, "y": 0}});
    let (error, planned) = scene.call(args.clone()).await;
    assert!(!error, "{planned}");
    {
        let mut state = scene.state.lock().unwrap();
        let flat = state
            .groups
            .iter_mut()
            .find(|g| group_id(g) == G_FLAT_VIA_TRACK);
        flat.unwrap().name = "RENAMED".into();
    }
    args["dry_run"] = json!(false);
    args["expected_plan_revision"] = planned["plan_revision"].clone();
    let (error, result) = scene.call(args).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "stale_target", "{result}");
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_restore_sends_the_touched_groups_again_and_proves_them() {
    let order = [K_FLAT_VIA, K_FLAT_TRACK, G_FLAT_VIA_TRACK];
    for case in 0..3 {
        let mut state = State::of(&KINDS);
        state.faults.renet = Some(K_FLAT_TRACK);
        state.faults.renet_to = Some("GND");
        match case {
            1 => {
                state.faults.group_mangle = Some(|group| {
                    group.items.pop();
                });
                state.faults.mangle_on_restore = true;
            }
            2 => state.faults.restore_group_race = true,
            _ => {}
        }
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"uuids": [G_FLAT_VIA_TRACK], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(error, "{case}: {result}");
        let (kind, sent_twice) = match case {
            0 => ("plan_blocked", true),
            1 => ("mutation_outcome_uncertain", true),
            _ => ("mutation_outcome_uncertain", false),
        };
        assert_eq!(result["error"]["kind"], kind, "{case}: {result}");
        let expected = match sent_twice {
            true => [order, order].concat(),
            false => order.to_vec(),
        };
        assert_eq!(sent(&scene), expected, "{case}");
    }
}

/// A tuning pattern's block as KiCad 10.0.8 writes it, holding `members`.
/// The kinds board holds none: pcbnew's Python cannot make one.
fn tuning_pattern(id: &str, members: &[&str]) -> String {
    let members: Vec<_> = members.iter().map(|id| format!("\"{id}\"")).collect();
    format!(
        "\t(generated\n\t\t(uuid \"{id}\")\n\t\t(type tuning_pattern)\n\t\t(name \"Tuning \
         Pattern\")\n\t\t(layer \"F.Cu\")\n\t\t(base_line\n\t\t\t(pts\n\t\t\t\t(xy 115.571487 \
         50.14) (xy 123.13735 50.14)\n\t\t\t)\n\t\t)\n\t\t(corner_radius_percent 100)\n\t\t(end\n\
         \t\t\t(xy 123.13735 50.14)\n\t\t)\n\t\t(initial_side \"left\")\n\t\t(is_time_domain no)\n\
         \t\t(last_netname \"\")\n\t\t(last_status \"too_short\")\n\t\t(last_track_width 0.2)\n\
         \t\t(max_amplitude 2)\n\t\t(min_amplitude 0.2)\n\t\t(min_spacing 0.6)\n\t\t(origin\n\
         \t\t\t(xy 115.571487 50.14)\n\t\t)\n\t\t(rounded yes)\n\t\t(single_sided no)\n\
         \t\t(target_length 100)\n\t\t(tuning_mode \"single\")\n\t\t(members {})\n\t)\n",
        members.join(" ")
    )
}

#[tokio::test]
async fn a_tuning_pattern_and_its_tracks_refuse_naming_the_pattern() {
    const PATTERN: &str = "65500000-0000-4000-8000-000000000001";
    let mut state = State::of(&KINDS);
    let body = state.text.trim_end().strip_suffix(')').unwrap().to_string();
    state.text = format!(
        "{body}{})\n",
        tuning_pattern(PATTERN, &[K_U1_TRACK, K_R1_TRACK])
    );
    let scene = Scene::new(state).await;
    let why = "KiCad's API moves the pattern's tracks but not its base line";
    scene
        .assert_blocked(
            json!({"uuids": [K_U1_TRACK], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "track '{K_U1_TRACK}' is a member of tuning pattern '{PATTERN}'; {why}"
            )],
        )
        .await;
    scene
        .assert_blocked(
            json!({"uuids": [PATTERN], "offset": {"x": 1, "y": 0}}),
            &[format!("'{PATTERN}' is a tuning pattern; {why}")],
        )
        .await;
    assert_eq!(scene.state.lock().unwrap().count("GetItemsById"), 0);
}

#[tokio::test]
async fn locked_zones_and_a_zone_the_board_text_lacks_refuse() {
    let mut state = State::of(&KINDS);
    // KiCad never writes this: a zone it lists under another UUID in the
    // board text.
    state.text = state.text.replace(
        &format!("(uuid \"{K_ZONE_TWO_LAYERS}\")"),
        "(uuid \"65600000-0000-4000-8000-000000000001\")",
    );
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"uuids": [K_ZONE_LOCKED, K_RULE_AREA_LOCKED, K_ZONE_TWO_LAYERS],
                "offset": {"x": 1, "y": 0}}),
            &[
                format!("zone '{K_ZONE_LOCKED}' is locked or its lock state is unknown"),
                format!("rule area '{K_RULE_AREA_LOCKED}' is locked or its lock state is unknown"),
                format!(
                    "zone '{K_ZONE_TWO_LAYERS}' is absent from the board text, so its lock state \
                     is unknown"
                ),
            ],
        )
        .await;
    let (error, result) = scene
        .call(json!({"uuids": [K_RULE_AREA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(!error, "{result}");
}

#[tokio::test]
async fn a_teardrop_zone_refuses() {
    let mut state = State::of(&KINDS);
    // The kinds board holds no teardrop: pcbnew's Python cannot build one.
    // KiCad sends a teardrop's type as ZT_TEARDROP, and its copper settings'
    // teardrop type as the kind of teardrop it is (measured: ZTDT_VIA_PAD).
    state.edit(K_ZONE_GND, |item| {
        use copperpour_ipc::gen::kiapi::board::types::{
            zone::Settings, ZoneTeardropSettings, ZoneTeardropType,
        };
        let Item::Zone(zone) = item else {
            panic!("a zone")
        };
        zone.r#type = ZoneType::ZtTeardrop as i32;
        let Some(Settings::CopperSettings(settings)) = zone.settings.as_mut() else {
            panic!("a copper zone")
        };
        settings.teardrop = Some(ZoneTeardropSettings {
            r#type: ZoneTeardropType::ZtdtViaPad as i32,
        });
    });
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"uuids": [K_ZONE_GND], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "zone '{K_ZONE_GND}' is a teardrop zone KiCad generates and rebuilds itself"
            )],
        )
        .await;
}

#[tokio::test]
async fn a_text_box_and_a_rounded_rectangle_turn_by_right_angles_only() {
    let scene = Scene::new(State::of(&KINDS)).await;
    let ids = [K_TEXT_CENTRED, K_TEXT_BOX, K_ROUNDED];
    let turn =
        |degrees: f64| json!({"uuids": ids, "rotation": degrees, "origin": {"x": 110, "y": 105}});
    scene
        .assert_blocked(
            turn(45.0),
            &[
                format!(
                    "text box '{K_TEXT_BOX}': KiCad cannot hold a text box turned by an angle \
                     that is not a multiple of 90°"
                ),
                format!(
                    "graphic '{K_ROUNDED}': its rounded rectangle on F.SilkS has no exact form \
                     at a turn that is not a multiple of 90°"
                ),
            ],
        )
        .await;
    let (error, result) = scene.apply(turn(90.0)).await;
    assert!(!error, "{result}");
    assert_eq!(result["texts"][0]["rotation"], 90.0, "{result}");
    assert_eq!(result["text_boxes"][0]["rotation"], 90.0, "{result}");
    assert_eq!(result["graphics"][0]["shape"], "rectangle", "{result}");
}

/// A turn a hair off a right angle is not one: the text box would go to
/// KiCad at an angle that is not a multiple of 90°.
#[tokio::test]
async fn a_turn_a_hair_off_a_right_angle_refuses_text_boxes_and_rounded_rectangles() {
    let near = 90.000_000_000_1;
    let scene = Scene::new(State::of(&KINDS)).await;
    let (error, result) = scene
        .call(json!({"uuids": [K_TEXT_BOX, K_ROUNDED], "rotation": near,
            "origin": {"x": 110, "y": 105}}))
        .await;
    assert!(error, "{result}");
    let reasons = result["error"]["reasons"].as_array().unwrap();
    assert_eq!(reasons.len(), 2, "{result}");
    let scene = Scene::new(State::of(&ISSUE_240)).await;
    let (error, result) = scene
        .call(json!({"references": ["U1"], "rotation": near, "origin": {"x": 110, "y": 95}}))
        .await;
    assert!(error, "{result}");
    let reasons = result["error"]["reasons"].to_string();
    assert!(reasons.contains("it holds a text box"), "{result}");
    scene.assert_no_writes();
}

/// The KiCad 10.0.8 forms of the two losses measured on an update: a hatch
/// fill of a graphic and a hatched zone's smoothing. pcbnew wrote the kinds
/// fixture without them; these tests put them in.
const HATCHED_ZONE_FILL: &str = "(fill yes\n\t\t\t(mode hatch) (hatch_thickness 0.3) \
     (hatch_gap 0.6) (hatch_orientation 30) (hatch_smoothing_level 2) \
     (hatch_smoothing_value 0.5)";

#[tokio::test]
async fn a_hatch_kicads_api_would_drop_refuses_before_any_write() {
    const U1: &str = "ab4b2e28-2bae-48d6-925a-da00f845a634";
    const U1_RECTANGLE: &str = "3faceec2-359d-4ecd-a9c8-b2289014431f";
    const RECTANGLE: &str = "d003f2ea-312a-495a-8256-599856b55f28";
    let mut state = State::of(&KINDS);
    state.change_text(RECTANGLE, "(fill no)", "(fill cross_hatch)");
    state.change_text(U1_RECTANGLE, "(fill no)", "(fill hatch)");
    state.change_text(K_ZONE_GND, "(fill yes", HATCHED_ZONE_FILL);
    let scene = Scene::new(state).await;
    let drops = "the update would drop it";
    scene
        .assert_blocked(
            json!({"uuids": [RECTANGLE, U1, K_ZONE_GND], "offset": {"x": 1, "y": 0}}),
            &[
                format!("graphic '{RECTANGLE}': KiCad's API does not carry a hatch fill; {drops}"),
                format!(
                    "footprint U1 ({U1}): its fp_rect '{U1_RECTANGLE}': KiCad's API does not \
                     carry a hatch fill; {drops}"
                ),
                format!(
                    "zone '{K_ZONE_GND}': KiCad's API does not carry the zone's hatch \
                     smoothing; {drops}"
                ),
            ],
        )
        .await;
}

/// The end of the kinds board's text.
const END: &str = "\n\t(embedded_fonts no)\n)";

/// A teardrop zone's block as KiCad 10 writes one (`teardrops_kicad10.kicad_pcb`
/// in copperpour-ipc's fixtures), a zone with no `uuid`, then [`END`].
const TEARDROP_THEN_END: &str = "\n\t(zone\n\t\t(net \"R1_PAD1\")\n\t\t(layer \"F.Cu\")\n\t\t\
     (hatch none 0.1)\n\t\t(priority 30000)\n\t\t(attr\n\t\t\t(teardrop\n\t\t\t\t(type padvia)\
     \n\t\t\t)\n\t\t)\n\t\t(connect_pads yes\n\t\t\t(clearance 0)\n\t\t)\n\t\t\
     (min_thickness 0.0254)\n\t\t(fill yes\n\t\t\t(island_removal_mode 1)\n\t\t)\n\t\t\
     (polygon\n\t\t\t(pts\n\t\t\t\t(xy 92.6 89.9) (xy 93.3 90) (xy 92.6 90.1)\n\t\t\t)\n\t\t)\n\t)\
     \n\t(embedded_fonts no)\n)";

#[tokio::test]
async fn the_board_text_after_the_commit_must_hold_only_the_move() {
    const RECTANGLE: &str = "d003f2ea-312a-495a-8256-599856b55f28";
    let fill = "(xy 145.943039 110.019685)";
    let cases: [(&str, Vec<TextChange>, Option<&str>); 4] = [
        // KiCad rewrites the moved rectangle's coordinates: the move.
        (
            "geometry",
            vec![
                (RECTANGLE, "(start 152 89)", "(start 153 89)"),
                (RECTANGLE, "(end 158 92)", "(end 159 92)"),
            ],
            None,
        ),
        // A moved item loses a token the move does not explain.
        (
            "moved",
            vec![
                (RECTANGLE, "(start 152 89)", "(start 153 89)"),
                (RECTANGLE, "(fill no)", "(fill yes)"),
            ],
            Some("gr_rect[d003f2ea-312a-495a-8256-599856b55f28]/fill"),
        ),
        // KiCad refills an unmoved zone and rebuilds a teardrop.
        (
            "refill",
            vec![
                (K_ZONE_TWO_LAYERS, fill, "(xy 145.94304 110.019685)"),
                ("", END, TEARDROP_THEN_END),
            ],
            None,
        ),
        // An unmoved graphic changes, which only the board text shows.
        (
            "unmoved",
            vec![(K_SEGMENT, "(layer \"F.SilkS\")", "(layer \"F.Fab\")")],
            Some("gr_line[a2885af4-60b0-428a-b775-dcd5d61d0db1]/layer"),
        ),
    ];
    for (name, changes, named) in cases {
        let mut state = State::of(&KINDS);
        state.faults.text_changes = changes;
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"uuids": [RECTANGLE], "offset": {"x": 1, "y": 0}}))
            .await;
        match named {
            None => assert!(!error, "{name}: {result}"),
            Some(path) => {
                assert_eq!(
                    result["error"]["kind"], "mutation_outcome_uncertain",
                    "{name}: {result}"
                );
                let message = result["message"].as_str().unwrap();
                assert!(message.contains(path), "{name}: {result}");
                assert!(
                    message.contains("one undo in KiCad reverses the commit"),
                    "{name}"
                );
            }
        }
    }
}

/// KiCad 10.0.8 writes a hatch fill a graphic loses on an update as
/// `(fill no)` (measured); an unmoved graphic losing it is found in the
/// board text.
#[tokio::test]
async fn a_hatch_fill_an_unmoved_graphic_loses_is_uncertain_naming_it() {
    const RECTANGLE: &str = "d003f2ea-312a-495a-8256-599856b55f28";
    let mut state = State::of(&KINDS);
    state.change_text(RECTANGLE, "(fill no)", "(fill hatch)");
    state.faults.text_changes = vec![(RECTANGLE, "(fill hatch)", "(fill no)")];
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [K_LONE_VIA], "offset": {"x": 0.5, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["kind"], "mutation_outcome_uncertain",
        "{result}"
    );
    let message = result["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("gr_rect[{RECTANGLE}]/fill")),
        "{result}"
    );
    assert_eq!(result["uuids"], json!([K_LONE_VIA, RECTANGLE]));
}

/// With "Automatically refill zones" on, KiCad refills a moved zone on the
/// push: the zone holds, its row says what it read.
#[tokio::test]
async fn a_moved_zone_kicad_refills_on_the_push_is_complete() {
    let mut state = State::of(&KINDS);
    state.faults.refill = Some(K_ZONE_GND);
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [K_ZONE_GND], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["outcome"]["status"], "complete", "{result}");
    assert_eq!(result["zones"][0]["filled"], true, "{result}");
}

/// R2's Value lies 1.43 mm beyond its pads and anchor: a move that keeps
/// the pads and the anchor in KiCad's range and puts the Value beyond it
/// refuses.
#[tokio::test]
async fn a_footprint_whose_field_would_leave_kicads_range_refuses() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    let y = nm_to_mm(LIMIT) - 110.5;
    let (error, result) = scene
        .call(json!({"references": ["R2"], "offset": {"x": 0, "y": y}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "invalid_argument", "{result}");
    assert_eq!(result["error"]["field"], "offset", "{result}");
}

#[tokio::test]
async fn a_zone_moves_whole_with_its_fill_as_it_was() {
    let state = State::of(&KINDS);
    let Item::Zone(mut expected) = state.item(K_ZONE_GND) else {
        panic!("a zone")
    };
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [K_ZONE_GND], "offset": {"x": 1.25, "y": -2.5}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(
        result["zones"],
        json!([{"uuid": K_ZONE_GND, "name": "ZONE_GND", "filled": true,
            "outline_point_count": 4, "hole_count": 1}])
    );
    assert_eq!(result["refill_required_before_drc"], true);
    let shift = |set: &mut PolySet| {
        let lines = set
            .polygons
            .iter_mut()
            .flat_map(|p| p.outline.iter_mut().chain(&mut p.holes));
        for node in lines.flat_map(|line| &mut line.nodes) {
            let Some(Node::Point(p)) = &mut node.geometry else {
                panic!("a zone node this test does not move")
            };
            p.x_nm += 1_250_000;
            p.y_nm -= 2_500_000;
        }
    };
    shift(expected.outline.as_mut().unwrap());
    for fill in &mut expected.filled_polygons {
        shift(fill.shapes.as_mut().unwrap());
    }
    assert!(!expected.filled_polygons.is_empty());
    assert_eq!(
        scene.state.lock().unwrap().item(K_ZONE_GND),
        Item::Zone(expected)
    );
}

#[tokio::test]
async fn a_graphic_kicad_renets_is_restored_with_the_batch() {
    let mut state = State::of(&KINDS);
    state.faults.renet = Some(K_COPPER_SEGMENT);
    state.faults.renet_to = Some("GND");
    let was = state.item(K_COPPER_SEGMENT);
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [K_COPPER_SEGMENT], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["reasons"],
        json!([
            format!("graphic '{K_COPPER_SEGMENT}' changed net from 'COPPER_GRAPHIC' to 'GND'"),
            UNDONE
        ]),
        "{result}"
    );
    let state = scene.state.lock().unwrap();
    assert_eq!(state.updates, 2);
    assert_eq!(state.item(K_COPPER_SEGMENT), was);
}

/// The via inside `ZONE_GND`, in no group.
const K_LONE_VIA: &str = "396ccf33-8665-426c-aba1-4d97b1a40401";

#[tokio::test]
async fn an_unselected_graphic_kicad_renets_is_found_in_the_board_text() {
    let mut state = State::of(&KINDS);
    state.faults.renet = Some(K_COPPER_SEGMENT);
    state.faults.renet_to = Some("GND");
    let net = "(net \"COPPER_GRAPHIC\")";
    state.faults.text_changes = vec![(K_COPPER_SEGMENT, net, "(net \"GND\")")];
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"uuids": [K_LONE_VIA], "offset": {"x": 0.5, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["kind"], "mutation_outcome_uncertain",
        "{result}"
    );
    let message = result["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("gr_line[{K_COPPER_SEGMENT}]/net")),
        "{result}"
    );
    assert_eq!(result["uuids"], json!([K_LONE_VIA, K_COPPER_SEGMENT]));
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

/// Texts and zones are not copper: a move of a via, a text and a zone reads
/// back only tracks, vias and arcs as copper, which a copper set holding the
/// text or the zone would find vanished.
#[tokio::test]
async fn a_move_on_a_board_with_texts_and_zones_compares_its_copper_by_net() {
    let scene = Scene::new(State::of(&KINDS)).await;
    let (error, result) = scene
        .apply(
            json!({"uuids": [K_LONE_VIA, K_TEXT_CENTRED, K_ZONE_TWO_LAYERS],
            "offset": {"x": 0.5, "y": 0}}),
        )
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["outcome"]["status"], "complete", "{result}");
}

#[tokio::test]
async fn a_text_a_text_box_a_graphic_or_a_zone_put_out_of_range_refuses() {
    let scene = Scene::new(State::of(&KINDS)).await;
    let x = nm_to_mm(LIMIT) - 50.0;
    for id in [K_TEXT_CENTRED, K_TEXT_BOX, K_SEGMENT, K_ZONE_GND] {
        let (error, result) = scene
            .call(json!({"uuids": [id], "offset": {"x": x, "y": 0}}))
            .await;
        assert!(error, "{id}: {result}");
        assert_eq!(
            result["error"]["kind"], "invalid_argument",
            "{id}: {result}"
        );
        assert_eq!(result["error"]["field"], "offset", "{id}: {result}");
    }
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_dry_run_only_reads_and_reports_the_plan_and_the_copper_check() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    let before = scene.items();
    let (error, result) = scene
        .call(json!({"references": ["R1"], "uuids": [LONE_VIA, STUB], "offset": {"x": 1, "y": 2}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["source"], "ipc_plan");
    assert!(result["plan_revision"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        result["footprints"],
        json!([{"reference": "R1", "uuid": R1, "x": 111.0, "y": 97.0, "rotation": 0.0}])
    );
    assert_eq!(
        result["vias"],
        json!([{"uuid": LONE_VIA, "x": 121.0, "y": 102.0}])
    );
    assert_eq!(
        result["tracks"],
        json!([{"uuid": STUB, "start": {"x": 126.0, "y": 102.0}, "end": {"x": 129.0, "y": 102.0}}])
    );
    assert_eq!(result["copper_check"]["status"], "clear");
    assert_eq!(result["copper_check"]["coverage"]["pad_count"], 2);
    // KiCad holds a footprint's orientation in (-180, 180].
    let (_, result) = scene
        .call(json!({"references": ["R1"], "rotation": 270, "origin": {"x": 110, "y": 95}}))
        .await;
    assert_eq!(result["footprints"][0]["rotation"], -90.0, "{result}");
    assert_eq!(scene.items(), before);
    let state = scene.state.lock().unwrap();
    for command in &state.sent {
        assert!(
            [
                "SaveDocumentToString",
                "GetItems",
                "GetItemsById",
                "GetBoardEnabledLayers",
                "GetBoundingBox",
                "CheckPadstackPresenceOnLayers",
                "GetPadShapeAsPolygon"
            ]
            .contains(&command.as_str()),
            "{command} reached KiCad"
        );
    }
}

#[tokio::test]
async fn a_stale_or_foreign_revision_refuses_and_writes_nothing() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    let args = whole_board(json!({"x": 1, "y": 2}));
    let (_, planned) = scene.call(args.clone()).await;
    let mut other = whole_board(json!({"x": 2, "y": 2}));
    other["dry_run"] = json!(false);
    for revision in [json!("sha256:00"), planned["plan_revision"].clone()] {
        other["expected_plan_revision"] = revision;
        let (error, result) = scene.call(other.clone()).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "stale_target");
    }
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_board_changed_between_the_dry_run_and_the_apply_refuses() {
    for case in 0..2 {
        let scene = Scene::new(State::of(&ISSUE_96)).await;
        let mut args = whole_board(json!({"x": 1, "y": 2}));
        let (_, planned) = scene.call(args.clone()).await;
        match case {
            0 => scene.state.lock().unwrap().generation += 1,
            _ => scene.state.lock().unwrap().edit(LONE_VIA, |item| {
                if let Item::Via(via) = item {
                    via.is_free = !via.is_free;
                }
            }),
        }
        args["dry_run"] = json!(false);
        args["expected_plan_revision"] = planned["plan_revision"].clone();
        let (error, result) = scene.call(args).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "stale_target");
        scene.assert_no_writes();
    }
}

#[tokio::test]
async fn a_board_changed_after_begin_commit_is_not_overwritten() {
    let mut state = State::of(&ISSUE_96);
    state.faults.race = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(whole_board(json!({"x": 1, "y": 2}))).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "stale_target");
    assert_eq!(scene.state.lock().unwrap().count("UpdateItems"), 0);
}

#[tokio::test]
async fn missing_ambiguous_and_doubly_selected_targets_refuse_together_before_any_write() {
    let mut state = State::of(&ISSUE_96);
    let twin = state.clone_footprint(R2, "R2", 0, (0, 40_000_000));
    let blank = state.clone_footprint(R1, "", 1, (0, 60_000_000));
    let scene = Scene::new(state).await;
    let missing = "65100000-0000-4000-8000-999999999999";
    scene
        .assert_blocked(
            json!({"references": ["R9", "R1", "R2"], "uuids": [R1, missing, twin, blank],
                "offset": {"x": 1, "y": 0}}),
            &[
                "footprint R9 not found on the board".into(),
                "footprint reference 'R2' is held more than once on the board".into(),
                format!("footprint R1 ({R1}) is selected both by reference and by UUID"),
                format!("item '{missing}' not found on the board"),
                format!(
                    "footprint R2 ({twin}) has an empty or shared reference; the copper check \
                     addresses footprints by reference"
                ),
                format!(
                    "footprint  ({blank}) has an empty or shared reference; the copper check \
                     addresses footprints by reference"
                ),
            ],
        )
        .await;
}

#[tokio::test]
async fn locked_footprints_vias_and_tracks_refuse_before_any_write() {
    let mut state = State::of(&ISSUE_96);
    for id in [R1, TIED_VIA, STUB] {
        state.edit(id, locked);
    }
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"references": ["R1"], "uuids": [TIED_VIA, STUB], "offset": {"x": 1, "y": 0}}),
            &[
                format!("footprint R1 ({R1}) is locked or its lock state is unknown"),
                format!("via '{TIED_VIA}' is locked or its lock state is unknown"),
                format!("track '{STUB}' is locked or its lock state is unknown"),
            ],
        )
        .await;
}

#[tokio::test]
async fn other_item_types_are_named_and_refused_before_any_write() {
    const DIMENSION: &str = "65700000-0000-4000-8000-000000000001";
    let mut state = State::of(&ISSUE_96);
    state.faults.foreign = Some((DIMENSION, "kiapi.board.types.Dimension"));
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"uuids": [DIMENSION, LONE_VIA], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "item '{DIMENSION}' is a Dimension, which this tool does not move"
            )],
        )
        .await;
}

#[tokio::test]
async fn a_footprints_child_moves_with_its_footprint_and_alone_refuses_naming_it() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    scene
        .assert_blocked(
            json!({"uuids": [R1_PAD_1, LONE_VIA], "offset": {"x": 1, "y": 0}}),
            &[format!(
                "Pad '{R1_PAD_1}' belongs to footprint R1 ({R1}); select the footprint to move it"
            )],
        )
        .await;
    let (error, result) = scene
        .apply(json!({"references": ["R1"], "uuids": [R1_PAD_1], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["outcome"]["status"], "complete", "{result}");
    assert_eq!(result["footprint_count"], 1, "{result}");
    assert_eq!(sent(&scene), [R1]);
}

#[tokio::test]
async fn unreadable_group_membership_refuses_rather_than_reading_as_no_groups() {
    let mut state = State::of(&ISSUE_96);
    state.faults.groups_unreadable = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .call(json!({"uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    let reasons = result["error"]["reasons"].to_string();
    assert!(
        reasons.contains("group membership could not be read"),
        "{result}"
    );
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_footprint_that_owns_a_group_or_is_missing_from_the_board_text_refuses() {
    let anchor = format!("\t\t(uuid \"{R1}\")");
    // KiCad never wrote this text: a footprint-owned group in the form KiCad
    // writes one, added to the captured text, and R1 under another UUID.
    let group = "65600000-0000-4000-8000-000000000001";
    let owned = format!("{anchor}\n\t\t(group \"\" (uuid \"{group}\") (members \"{R1_PAD_1}\"))");
    let elsewhere = "\t\t(uuid \"65600000-0000-4000-8000-000000000002\")";
    let other = format!("\t\t(uuid \"{R2}\")");
    for (text, reason) in [
        (
            ISSUE_96.board.replace(&anchor, &owned),
            format!(
                "footprint R1 ({R1}) owns 1 group(s), which the whole-footprint update a move \
                 needs would drop; move it in KiCad, or ungroup them first"
            ),
        ),
        (
            ISSUE_96.board.replace(&anchor, elsewhere),
            format!("footprint R1 ({R1}) is absent from the board serialization"),
        ),
        (
            ISSUE_96.board.replace(&other, &anchor),
            "could not verify footprint group ownership: ".to_string(),
        ),
    ] {
        let mut state = State::of(&ISSUE_96);
        state.text = text;
        let scene = Scene::new(state).await;
        let (_, result) = scene
            .call(json!({"references": ["R1"], "offset": {"x": 1, "y": 0}}))
            .await;
        assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
        let reasons = result["error"]["reasons"].as_array().unwrap();
        assert!(
            reasons
                .iter()
                .any(|seen| seen.as_str().unwrap().starts_with(&reason)),
            "{reason} not in {result}"
        );
        scene.assert_no_writes();
    }
}

#[tokio::test]
async fn targets_kicad_sent_without_a_position_refuse() {
    let mut state = State::of(&ISSUE_96);
    // KiCad never sends these: each item it lists has its position.
    for id in [R2, LONE_VIA, STUB] {
        state.edit(id, |item| match item {
            Item::Footprint(footprint) => footprint.position = None,
            Item::Via(via) => via.position = None,
            Item::Track(track) => track.end = None,
            other => panic!("{} is not edited here", other.kind()),
        });
    }
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"references": ["R2"], "uuids": [LONE_VIA, STUB], "offset": {"x": 1, "y": 0}}),
            &[
                format!("footprint R2 ({R2}) has no position"),
                format!("via '{LONE_VIA}' has no position"),
                format!("track '{STUB}' has no position"),
            ],
        )
        .await;

    // Nor this: a board text with no text in it.
    let mut state = State::of(&KINDS);
    state.edit(K_TEXT_CENTRED, |item| match item {
        Item::Text(text) => text.text = None,
        other => panic!("{} is not edited here", other.kind()),
    });
    let scene = Scene::new(state).await;
    scene
        .assert_blocked(
            json!({"uuids": [K_TEXT_CENTRED], "offset": {"x": 1, "y": 0}}),
            &[format!("text '{K_TEXT_CENTRED}' has no position")],
        )
        .await;
}

#[tokio::test]
async fn an_off_axis_turn_a_child_cannot_follow_refuses() {
    let scene = Scene::new(State::of(&ISSUE_240)).await;
    let turn = |degrees| json!({"references": ["Y1", "U1"], "rotation": degrees, "origin": {"x": 110, "y": 95}});
    let (error, result) = scene.call(turn(45)).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    let reasons = result["error"]["reasons"].as_array().unwrap();
    assert_eq!(reasons.len(), 2, "{result}");
    for reference in ["footprint U1 (", "footprint Y1 ("] {
        let named = |reason: &Value| reason.as_str().unwrap().starts_with(reference);
        assert!(reasons.iter().any(named), "{result}");
    }
    let (error, result) = scene.call(turn(90)).await;
    assert!(!error, "{result}");
    scene.assert_no_writes();
}

#[tokio::test]
async fn destinations_outside_kicads_coordinate_range_refuse_naming_the_argument() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    // R2's anchor lands 0.5 mm inside the limit, its pad 2 beyond it.
    let limit_mm = nm_to_mm(LIMIT);
    let pad_only = limit_mm - 100.5;
    for (args, field) in [
        (
            json!({"uuids": [LONE_VIA], "offset": {"x": 2100, "y": 0}}),
            "offset",
        ),
        (
            json!({"references": ["R2"], "offset": {"x": pad_only, "y": 0}}),
            "offset",
        ),
        (
            json!({"uuids": [LONE_VIA], "rotation": 180, "origin": {"x": 2000, "y": 0}}),
            "rotation",
        ),
    ] {
        let (error, result) = scene.call(args).await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "invalid_argument", "{result}");
        assert_eq!(result["error"]["field"], field, "{result}");
    }
    scene.assert_no_writes();
}

#[tokio::test]
async fn malformed_arguments_refuse_before_kicad_is_asked() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    let offset = json!({"x": 1, "y": 0});
    let origin = json!({"x": 1, "y": 0});
    // The schema refuses before the handler, whose refusals carry `outcome`.
    for (args, schema) in [
        (json!({"offset": offset}), true),
        (json!({"references": [], "offset": offset}), true),
        (json!({"references": [""], "offset": offset}), true),
        (json!({"references": ["R1"]}), true),
        (
            json!({"references": ["R1"], "offset": offset, "rotation": 90, "origin": origin}),
            true,
        ),
        (json!({"references": ["R1"], "rotation": 90}), true),
        (json!({"references": ["R1"], "origin": origin}), true),
        (
            json!({"references": ["R1"], "offset": offset, "dry_run": "false"}),
            true,
        ),
        (json!({"references": ["R1", "R1"], "offset": offset}), false),
        (json!({"uuids": [R1, R1], "offset": offset}), false),
        (
            json!({"references": ["R1"], "offset": {"x": 1e99, "y": 0}}),
            false,
        ),
        (
            json!({"references": ["R1"], "offset": offset, "dry_run": false}),
            false,
        ),
    ] {
        let (error, result) = scene.call(args.clone()).await;
        assert!(error, "{args}: {result}");
        assert_eq!(
            result["error"]["kind"], "invalid_argument",
            "{args}: {result}"
        );
        assert_eq!(result.get("outcome").is_none(), schema, "{args}: {result}");
    }
    assert!(scene.state.lock().unwrap().sent.is_empty());
}

#[test]
fn the_handler_checks_the_argument_rules_the_schema_states() {
    let offset = json!({"x": 1, "y": 0});
    let origin = json!({"x": 1, "y": 0});
    for (args, field) in [
        (json!({"offset": offset}), "references"),
        (json!({"references": [], "offset": offset}), "references"),
        (json!({"uuids": [" "], "offset": offset}), "uuids"),
        (
            json!({"references": ["R1", "R1"], "offset": offset}),
            "references",
        ),
        (json!({"uuids": [R1, R1], "offset": offset}), "uuids"),
        (json!({"uuids": [1], "offset": offset}), "uuids[0]"),
        (json!({"uuids": [R1]}), "offset"),
        (
            json!({"uuids": [R1], "offset": offset, "rotation": 90, "origin": origin}),
            "offset",
        ),
        (json!({"uuids": [R1], "rotation": 90}), "origin"),
        (json!({"uuids": [R1], "origin": origin}), "rotation"),
        (
            json!({"uuids": [R1], "rotation": "90", "origin": origin}),
            "rotation",
        ),
    ] {
        let Err(result) = parse(&args) else {
            panic!("{args} parsed")
        };
        let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
            panic!("text")
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["error"]["field"], field, "{args}: {body}");
    }
}

#[tokio::test]
async fn a_pad_on_another_nets_copper_is_reported_dry_and_refused_on_apply() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    // R1 from (110, 95) to (114.2, 106), #96's case 1: its pads on the tied
    // track and via, both NET_C.
    let args = json!({"references": ["R1"], "offset": {"x": 4.2, "y": 11}});
    let (error, planned) = scene.call(args.clone()).await;
    assert!(!error, "{planned}");
    assert_eq!(planned["copper_check"]["status"], "conflicts");
    assert_eq!(planned["copper_check"]["conflict_count"], 3);
    let mut apply = args;
    apply["dry_run"] = json!(false);
    apply["expected_plan_revision"] = planned["plan_revision"].clone();
    let (error, result) = scene.call(apply).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    assert_eq!(result["error"]["reasons"].as_array().unwrap().len(), 3);
    assert_eq!(result["copper_check"], planned["copper_check"]);
    scene.assert_no_writes();
}

#[tokio::test]
async fn the_same_landing_is_clear_when_that_copper_is_selected_and_moves_along() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    let (error, result) = scene
        .apply(
            json!({"references": ["R1"], "uuids": [TIED_VIA, TIED_TRACK],
            "offset": {"x": 4.2, "y": 11}}),
        )
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["copper_check"]["status"], "clear");
    assert_eq!(result["copper_check"]["coverage"]["via_count"], 1);
    assert_eq!(result["copper_check"]["coverage"]["track_count"], 2);
}

fn row(kind: &str, uuid: &str, x: f64, y: f64) -> Value {
    json!({"kind": kind, "uuid": uuid, "net": "NET_C", "x": x, "y": y})
}

#[tokio::test]
async fn copper_the_move_leaves_behind_is_reported_and_left_in_place() {
    let mut state = State::of(&ISSUE_96);
    // A NET_C via on R1's NET_A pad 1 at (109.175, 95): touching, but
    // another net, so the move leaves no connection behind there.
    state.clone_via(TIED_VIA, 0, (-5_825_000, -11_000_000));
    let scene = Scene::new(state).await;
    let before = scene.items();
    let (error, result) = scene
        .call(json!({"references": ["R1"], "offset": {"x": 0, "y": 5}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["boundary_connection_count"], 0, "{result}");
    let (error, result) = scene
        .call(json!({"uuids": [R2_TRACK], "offset": {"x": 0, "y": 5}}))
        .await;
    assert!(!error, "{result}");
    let mut pad = row("pad", R2_PAD_1, 99.175, 110.0);
    pad["reference"] = json!("R2");
    pad["pad"] = json!("1");
    assert_eq!(result["boundary_connection_count"], 2);
    assert_eq!(
        result["boundary_connections"],
        json!([pad, row("track", TIED_TRACK, 99.175, 106.0)])
    );
    assert!(result.get("boundary_connections_truncated").is_none());

    let (error, result) = scene
        .apply(json!({"references": ["R2"], "uuids": [TIED_VIA], "offset": {"x": 0, "y": 5}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["boundary_connection_count"], 2, "{result}");
    assert_eq!(
        result["boundary_connections"],
        json!([
            row("track", R2_TRACK, 99.175, 110.0),
            row("track", TIED_TRACK, 115.0, 106.0)
        ])
    );
    let state = scene.state.lock().unwrap();
    for id in [R2_TRACK, TIED_TRACK, STUB, LONE_VIA] {
        let held = state
            .items
            .iter()
            .find(|item| item_kiid(item).as_deref() == Some(id));
        assert!(before.contains(held.unwrap()), "{id} moved");
    }
}

#[tokio::test]
async fn more_than_a_hundred_boundary_connections_are_counted_and_cut() {
    let mut state = State::of(&ISSUE_96);
    // KiCad never sends this: 101 vias stacked on the tied one.
    for index in 0..101 {
        state.clone_via(TIED_VIA, index, (0, 0));
    }
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .call(json!({"uuids": [TIED_TRACK], "offset": {"x": 0, "y": 5}}))
        .await;
    assert!(!error, "{result}");
    // R2_TRACK's end, the tied via and its 101 copies.
    assert_eq!(result["boundary_connection_count"], 103);
    assert_eq!(
        result["boundary_connections"].as_array().unwrap().len(),
        100
    );
    assert_eq!(result["boundary_connections_truncated"], true);
}

#[tokio::test]
async fn a_via_kicad_renets_restores_the_whole_batch_and_names_the_change() {
    let mut state = State::of(&ISSUE_96);
    state.faults.renet = Some(TIED_VIA);
    let before = state.items.clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1", "R2"], "uuids": [TIED_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    assert_eq!(
        result["error"]["reasons"],
        json!([
            format!("via '{TIED_VIA}' changed net from 'NET_C' to 'NET_A'"),
            UNDONE
        ])
    );
    assert_eq!(scene.items(), before);
    assert_eq!(scene.state.lock().unwrap().updates, 2);
}

#[tokio::test]
async fn an_unselected_track_kicad_renets_is_restored_with_the_batch() {
    let mut state = State::of(&ISSUE_96);
    state.faults.renet = Some(STUB);
    let before = state.items.clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R2"], "uuids": [TIED_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked");
    assert_eq!(
        result["error"]["reasons"],
        json!([
            format!("track '{STUB}' changed net from 'NET_C' to 'NET_A'"),
            UNDONE
        ])
    );
    assert_eq!(scene.items(), before);
    assert_eq!(scene.state.lock().unwrap().updates, 2);
}

#[tokio::test]
async fn a_restoration_that_cannot_be_verified_is_uncertain_and_names_the_items() {
    for case in 0..5 {
        let mut state = State::of(&ISSUE_96);
        state.faults.renet = Some(STUB);
        match case {
            0 => state.faults.renet_restore = true,
            1 => state.faults.failed_restore = true,
            2 => state.faults.restore_race = true,
            // A restored footprint is compared whole, a graphic's point too.
            _ => {
                state.faults.mangle = Some([anchor_off, graphic_off][case - 3]);
                state.faults.mangle_on_restore = true;
            }
        }
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"references": ["R2"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(error, "{result}");
        assert_eq!(
            result["error"]["kind"], "mutation_outcome_uncertain",
            "{result}"
        );
        assert_eq!(result["uuids"], json!([LONE_VIA, R2, STUB]));
        assert_eq!(result["outcome"]["retry"]["safe"], false);
        let updates = scene.state.lock().unwrap().updates;
        assert_eq!(updates, if case == 2 { 1 } else { 2 });
    }
}

fn anchor_off(item: &mut Item) {
    if let Item::Footprint(footprint) = item {
        footprint.position.as_mut().unwrap().x_nm += 1;
    }
}

/// A footprint's first graphic 1 nm off.
fn graphic_off(item: &mut Item) {
    let Item::Footprint(footprint) = item else {
        return;
    };
    let items = &mut footprint.definition.as_mut().unwrap().items;
    let slot = items.iter_mut().find(|item| any_is(item, GRAPHIC)).unwrap();
    let mut graphic = BoardGraphicShape::decode(slot.value.as_slice()).unwrap();
    shift_graphic(&mut graphic, (1, 0));
    slot.value = graphic.encode_to_vec();
}

#[tokio::test]
async fn a_readback_that_differs_beyond_a_net_is_uncertain_and_never_restored() {
    let mangles: [(&str, Mangle); 5] = [
        ("anchor 1 nm off", anchor_off),
        // No angle is compared modulo 360°.
        ("orientation a whole turn on", |item| {
            if let Item::Footprint(footprint) = item {
                footprint.orientation = Some(Angle {
                    value_degrees: orientation(footprint) + 360.0,
                });
            }
        }),
        ("a graphic's point 1 nm off", graphic_off),
        ("a field's text changed", |item| {
            if let Item::Footprint(footprint) = item {
                let field = footprint.value_field.as_mut().unwrap();
                field
                    .text
                    .as_mut()
                    .unwrap()
                    .text
                    .as_mut()
                    .unwrap()
                    .text
                    .push('x');
            }
        }),
        ("a via property changed", |item| {
            if let Item::Via(via) = item {
                via.is_free = !via.is_free;
            }
        }),
    ];
    for (name, mangle) in mangles {
        let mut state = State::of(&ISSUE_96);
        state.faults.mangle = Some(mangle);
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(error, "{name}: {result}");
        assert_eq!(
            result["error"]["kind"], "mutation_outcome_uncertain",
            "{name}: {result}"
        );
        assert_eq!(result["uuids"], json!([R1, LONE_VIA]), "{name}");
        assert_eq!(scene.state.lock().unwrap().updates, 1, "{name}");
    }
}

#[tokio::test]
async fn unselected_copper_kicad_moved_is_found_in_the_board_text() {
    let mut state = State::of(&ISSUE_96);
    state.faults.drift = Some(STUB);
    state.faults.text_changes = vec![
        (STUB, "(start 125 100)", "(start 125.000001 100)"),
        (STUB, "(end 128 100)", "(end 128.000001 100)"),
    ];
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["kind"], "mutation_outcome_uncertain",
        "{result}"
    );
    let message = result["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("segment[{STUB}]/start")),
        "{result}"
    );
    assert!(
        message.contains("one undo in KiCad reverses the commit"),
        "{result}"
    );
    assert_eq!(result["uuids"], json!([R1, LONE_VIA, STUB]));
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

#[tokio::test]
async fn unselected_copper_that_vanished_or_appeared_is_uncertain_and_named() {
    for vanish in [true, false] {
        let mut state = State::of(&ISSUE_96);
        match vanish {
            true => state.faults.vanish = Some(STUB),
            false => state.faults.appear = true,
        }
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(error, "{result}");
        assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
        let other = if vanish {
            STUB
        } else {
            "65300000-0000-4000-8000-000000000900"
        };
        assert_eq!(result["uuids"], json!([R1, LONE_VIA, other]));
        assert_eq!(scene.state.lock().unwrap().updates, 1);
    }
}

/// A group as KiCad lists one; KiCad never wrote this one.
fn group(id: &str, name: &str, members: &[&str]) -> Group {
    Group {
        id: Some(Kiid {
            value: id.to_string(),
        }),
        name: name.into(),
        items: members
            .iter()
            .map(|id| Kiid {
                value: id.to_string(),
            })
            .collect(),
        locked: LockedState::LsUnlocked as i32,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_grouped_item_kicad_renets_is_never_written_and_the_call_is_uncertain() {
    let mut state = State::of(&ISSUE_96);
    state.groups.push(group(
        "65400000-0000-4000-8000-000000000001",
        "block",
        &[STUB],
    ));
    state.faults.renet = Some(STUB);
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R2"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(result["uuids"], json!([LONE_VIA, R2, STUB]));
    let state = scene.state.lock().unwrap();
    assert_eq!(state.updates, 2);
    assert!(
        !state.updated.iter().any(|id| id == STUB),
        "{:?}",
        state.updated
    );
}

#[tokio::test]
async fn copper_changed_before_the_restore_is_uncertain_and_never_restored() {
    let mut state = State::of(&ISSUE_96);
    state.faults.renet = Some(STUB);
    state.faults.restore_copper_race = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R2"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
    assert_eq!(result["uuids"], json!([LONE_VIA, R2, STUB]));
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

/// One expected conflict between a pad and NET_C copper on F.Cu.
fn conflict(reference: &str, pad: &str, pad_net: &str, kind: &str, uuid: &str) -> Value {
    json!({"reference": reference, "pad": pad, "pad_net": pad_net, "layer": "F.Cu",
        "item_kind": kind, "item_uuid": uuid, "item_net": "NET_C"})
}

/// Asserts the dry run of `args` reports exactly `conflicts`, and its apply
/// refuses them with nothing written.
async fn assert_refused_on_apply(scene: &Scene, args: Value, conflicts: Value) {
    let (error, planned) = scene.call(args.clone()).await;
    assert!(!error, "{planned}");
    assert_eq!(planned["copper_check"]["conflicts"], conflicts, "{planned}");
    let mut apply = args;
    apply["dry_run"] = json!(false);
    apply["expected_plan_revision"] = planned["plan_revision"].clone();
    let (error, result) = scene.call(apply).await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
    assert_eq!(result["copper_check"], planned["copper_check"]);
    scene.assert_no_writes();
}

#[tokio::test]
async fn copper_alone_landing_on_another_nets_pad_is_reported_dry_and_refused_on_apply() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    // The stub, (125, 100)-(128, 100), to (109.175, 95)-(112.175, 95): across
    // both pads of R1, which stays.
    let args = json!({"uuids": [STUB], "offset": {"x": -15.825, "y": -5}});
    let conflicts = json!([
        conflict("R1", "1", "NET_A", "track", STUB),
        conflict("R1", "2", "NET_B", "track", STUB),
    ]);
    assert_refused_on_apply(&scene, args.clone(), conflicts).await;
    let (_, planned) = scene.call(args).await;
    let coverage = &planned["copper_check"]["coverage"];
    assert_eq!(coverage["moved_track_count"], 1, "{planned}");
    assert_eq!(coverage["staying_pad_count"], 2, "{planned}");
}

#[tokio::test]
async fn copper_landing_on_a_pad_of_its_own_net_is_clear() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    // The lone via, NET_C, from (120, 100) onto R2's NET_C pad 1 at (99.175, 110).
    let (error, result) = scene
        .apply(json!({"uuids": [LONE_VIA], "offset": {"x": -20.825, "y": 10}}))
        .await;
    assert!(!error, "{result}");
    assert_eq!(result["copper_check"]["status"], "clear", "{result}");
    assert_eq!(
        result["copper_check"]["coverage"]["staying_pad_count"], 2,
        "R2's pads were tested: {result}"
    );
}

#[tokio::test]
async fn a_via_carried_onto_another_nets_pad_while_its_track_stays_on_its_own_is_refused() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    // The tied via, joined by the tied track and R2's track to R2's NET_C
    // pad 1, carried alone from (115, 106) onto R1's NET_B pad 2 at
    // (110.825, 95).
    assert_refused_on_apply(
        &scene,
        json!({"uuids": [TIED_VIA], "offset": {"x": -4.175, "y": -11}}),
        json!([conflict("R1", "2", "NET_B", "via", TIED_VIA)]),
    )
    .await;
}

#[tokio::test]
async fn a_left_justified_reference_turned_past_upright_is_turned_back_and_flipped() {
    use copperpour_ipc::gen::kiapi::common::types::{
        HorizontalAlignment as H, VerticalAlignment as V,
    };
    let (left, right) = (H::HaLeft as i32, H::HaRight as i32);
    let (top, bottom) = (V::VaTop as i32, V::VaBottom as i32);
    let attributes = |footprint: &FootprintInstance| {
        let field = footprint.reference_field.as_ref().unwrap();
        let text = field.text.as_ref().unwrap().text.as_ref().unwrap();
        text.attributes.clone().unwrap()
    };
    // By `PCB_TEXT::KeepUpright`: at 90° the text is upright and keeps its
    // justification; at 180° it turns back to 0° and swaps left for right
    // and top for bottom.
    for (degrees, angle, h, v) in [(90.0, 90.0, left, top), (180.0, 0.0, right, bottom)] {
        let mut state = State::of(&ISSUE_96);
        // The capture's fields are centred; the test sets R2's reference to
        // the left and the top.
        state.edit(R2, |item| {
            let Item::Footprint(footprint) = item else {
                panic!("R2 is a footprint")
            };
            let field = footprint.reference_field.as_mut().unwrap();
            let text = field.text.as_mut().unwrap().text.as_mut().unwrap();
            let attrs = text.attributes.as_mut().unwrap();
            assert!(attrs.keep_upright, "the capture's reference keeps upright");
            attrs.horizontal_alignment = left;
            attrs.vertical_alignment = top;
        });
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"references": ["R1", "R2"], "uuids": COPPER_IDS,
                "rotation": degrees, "origin": {"x": 100, "y": 110}}))
            .await;
        assert!(!error, "{result}");
        let attrs = attributes(&scene.state.lock().unwrap().footprint(R2));
        let held = attrs.angle.unwrap().value_degrees;
        assert_eq!(held, angle, "{degrees}°");
        assert_eq!(
            (attrs.horizontal_alignment, attrs.vertical_alignment),
            (h, v),
            "{degrees}°"
        );
    }
}

#[tokio::test]
async fn an_arc_moves_and_turns_its_three_points_where_the_integer_rule_puts_them() {
    let moved = |turned: bool, (x, y): (i64, i64)| match turned {
        true => quarter_turn((x, y), (105_000_000, 100_000_000)),
        false => (x + 1_250_000, y - 2_500_000),
    };
    for (turned, mut args) in [
        (false, json!({"offset": {"x": 1.25, "y": -2.5}})),
        (
            true,
            json!({"rotation": 90, "origin": {"x": 105, "y": 100}}),
        ),
    ] {
        let mut state = State::of(&ISSUE_96);
        state.items.extend(
            commands::GetItemsResponse::decode(LAYERS_ARCS)
                .unwrap()
                .items,
        );
        let Item::Arc(was) = state.item(LAYERS_ARC) else {
            panic!("an arc")
        };
        let scene = Scene::new(state).await;
        args["uuids"] = json!([LAYERS_ARC]);
        let (error, result) = scene.apply(args).await;
        assert!(!error, "{result}");
        assert_eq!(result["arc_count"], 1);
        assert_eq!(result["arcs"][0]["uuid"], LAYERS_ARC);
        let mut expected = was.clone();
        for point in [&mut expected.start, &mut expected.mid, &mut expected.end] {
            let (x_nm, y_nm) = moved(turned, at(*point));
            *point = Some(Vector2 { x_nm, y_nm });
        }
        let held = scene.state.lock().unwrap().item(LAYERS_ARC);
        assert_eq!(held, Item::Arc(expected));
    }
}

#[tokio::test]
async fn a_readback_kicad_answers_out_of_shape_is_uncertain() {
    // Each misreading, and the cause the answer must give for it.
    let misreads: [(&str, &str, Misread); 4] = [
        ("nothing", "requested item UUIDs are missing", |items| {
            items.clear()
        }),
        ("one twice", "an item UUID read twice", |items| {
            items.push(items[0].clone())
        }),
        ("an unrequested item", "unrequested item", |items| {
            let mut item = Item::decode(&items[0]).unwrap().unwrap();
            if let Item::Footprint(footprint) = &mut item {
                footprint.id.as_mut().unwrap().value =
                    "65600000-0000-4000-8000-000000000004".into();
            }
            items[0] = item.pack();
        }),
        ("another type", "a requested item is a", |items| {
            items[0].type_url = "type.googleapis.com/kiapi.board.types.Dimension".into()
        }),
    ];
    for (name, cause, misread) in misreads {
        let mut state = State::of(&ISSUE_96);
        state.faults.misread = Some(misread);
        let scene = Scene::new(state).await;
        let (error, result) = scene
            .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
            .await;
        assert!(error, "{name}: {result}");
        assert_eq!(
            result["error"]["kind"], "mutation_outcome_uncertain",
            "{name}: {result}"
        );
        assert!(
            result["message"].as_str().unwrap().contains(cause),
            "{name}: {result}"
        );
        assert_eq!(result["uuids"], json!([R1, LONE_VIA]), "{name}");
        assert_eq!(scene.state.lock().unwrap().updates, 1, "{name}");
    }
}

#[tokio::test]
async fn a_target_changed_after_begin_commit_under_unchanged_board_text_is_stale() {
    let mut state = State::of(&ISSUE_96);
    state.faults.hidden_race = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "stale_target", "{result}");
    assert_eq!(scene.state.lock().unwrap().updates, 0);
}

#[tokio::test]
async fn a_recheck_that_cannot_read_the_targets_sends_no_update() {
    let mut state = State::of(&ISSUE_96);
    state.faults.blind_in_commit = true;
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
    let reasons = result["error"]["reasons"].to_string();
    assert!(
        reasons.contains("pre-update check failed; no update was sent"),
        "{result}"
    );
    assert_eq!(scene.state.lock().unwrap().updates, 0);
}

#[tokio::test]
async fn a_refused_update_is_dropped_and_proves_the_originals_or_reports_uncertainty() {
    for fail in [false, true] {
        let mut state = State::of(&ISSUE_96);
        state.faults.refuse_second_item = true;
        state.faults.failed_drop = fail;
        let before = state.items.clone();
        let scene = Scene::new(state).await;
        let (error, result) = scene.apply(whole_board(json!({"x": 1, "y": 0}))).await;
        assert!(error, "{result}");
        if fail {
            assert_eq!(result["error"]["kind"], "mutation_outcome_uncertain");
            assert_eq!(result["uuids"].as_array().unwrap().len(), 7);
        } else {
            assert_eq!(result["error"]["kind"], "plan_blocked");
            assert!(result["message"]
                .as_str()
                .unwrap()
                .contains("the board is as it was"));
            assert_eq!(scene.items(), before);
        }
    }
    // The drop puts every target back, but the board text shows a change
    // the drop did not undo: the board cannot be shown as it was.
    let mut state = State::of(&ISSUE_96);
    state.faults.refuse_second_item = true;
    state.faults.text_changes = vec![(STUB, "(width 0.25)", "(width 0.3)")];
    let scene = Scene::new(state).await;
    let (error, result) = scene.apply(whole_board(json!({"x": 1, "y": 0}))).await;
    assert!(error, "{result}");
    assert_eq!(
        result["error"]["kind"], "mutation_outcome_uncertain",
        "{result}"
    );
    let message = result["message"].as_str().unwrap();
    assert!(message.contains("could not be shown as it was"), "{result}");
}

#[tokio::test]
async fn a_late_update_is_recovered_without_advertising_safe_replay() {
    let mut state = State::of(&ISSUE_96);
    state.faults.late_update = true;
    let before = state.items.clone();
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .apply(json!({"references": ["R1"], "uuids": [LONE_VIA], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "ipc_batch_recovered");
    assert_eq!(result["outcome"]["retry"]["safe"], false);
    assert_eq!(scene.items(), before);
    assert_eq!(scene.state.lock().unwrap().updates, 1);
}

#[tokio::test]
async fn a_copper_check_that_cannot_complete_refuses_as_a_plan_not_an_ipc_failure() {
    let mut state = State::of(&ISSUE_96);
    state.faults.mute = Some("GetPadShapeAsPolygon");
    let scene = Scene::new(state).await;
    let (error, result) = scene
        .call(json!({"references": ["R1"], "offset": {"x": 1, "y": 0}}))
        .await;
    assert!(error, "{result}");
    assert_eq!(result["error"]["kind"], "plan_blocked", "{result}");
    assert!(result["message"]
        .as_str()
        .unwrap()
        .contains("copper check could not complete"));
    scene.assert_no_writes();
}

#[tokio::test]
async fn a_zero_offset_and_a_whole_turn_are_unchanged_without_a_commit() {
    let scene = Scene::new(State::of(&ISSUE_96)).await;
    for args in [
        whole_board(json!({"x": 0, "y": 0})),
        json!({"references": ["R1", "R2"], "uuids": COPPER_IDS,
            "rotation": 360, "origin": {"x": 105, "y": 100}}),
        json!({"references": ["R1", "R2"], "uuids": COPPER_IDS,
            "rotation": -720, "origin": {"x": 105, "y": 100}}),
    ] {
        let (error, result) = scene.apply(args).await;
        assert!(!error, "{result}");
        assert_eq!(result["unchanged"], true);
        assert_eq!(result["outcome"]["status"], "complete");
    }
    scene.assert_no_writes();
    // At 33.3°, adding 360 is not exact in floating point: a whole turn
    // must still change nothing.
    let turn = |degrees: f64| {
        json!({"references": ["R2"], "rotation": degrees,
        "origin": {"x": 100, "y": 110}})
    };
    let (error, result) = scene.apply(turn(33.3)).await;
    assert!(!error, "{result}");
    let (error, result) = scene.apply(turn(360.0)).await;
    assert!(!error, "{result}");
    assert_eq!(result["unchanged"], true, "{result}");
    assert_eq!(scene.state.lock().unwrap().count("UpdateItems"), 1);
}

/// Calls, time and bytes of one road through the served tools.
#[derive(Default)]
struct Meter {
    calls: usize,
    elapsed_ms: u128,
    request_bytes: usize,
    response_bytes: usize,
}

impl Meter {
    async fn call(
        &mut self,
        handler: &McpHandler,
        path: &Path,
        name: &str,
        mut args: Value,
    ) -> anyhow::Result<Value> {
        args["board"] = json!(path);
        let message = json!({"jsonrpc": "2.0", "id": 652, "method": "tools/call",
            "params": {"name": name, "arguments": args}});
        self.request_bytes += serde_json::to_vec(&message)?.len();
        let start = std::time::Instant::now();
        let response = handler.handle_message(message).await;
        self.elapsed_ms += start.elapsed().as_millis();
        self.calls += 1;
        let result = response
            .and_then(|response| response.result)
            .unwrap_or_default();
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        self.response_bytes += text.len();
        anyhow::ensure!(result["isError"] != true, "{name}: {text}");
        Ok(serde_json::from_str(text)?)
    }

    async fn plan_and_apply(
        &mut self,
        handler: &McpHandler,
        path: &Path,
        name: &str,
        args: Value,
    ) -> anyhow::Result<Value> {
        let plan = self.call(handler, path, name, args.clone()).await?;
        let mut apply = args;
        apply["dry_run"] = json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        self.call(handler, path, name, apply).await
    }

    fn report(&self) -> Value {
        json!({"calls": self.calls, "elapsed_ms": self.elapsed_ms,
            "request_bytes": self.request_bytes, "response_bytes": self.response_bytes})
    }
}

/// Whether KiCad holds every item of `block` moved by `moved`, by a read of
/// its own: the anchor and every pad of a footprint, each with its UUID and
/// net, and a via, track or arc whole, its points moved.
fn expect_moved(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    block: &Items,
    moved: impl Fn(Vector2) -> Vector2,
) -> anyhow::Result<()> {
    let ids: Vec<_> = block.keys().cloned().collect();
    let held = read_targets(client, document, &ids)?;
    for (id, was) in block {
        let ok = match (&held[id], was) {
            (Item::Footprint(held), Item::Footprint(was)) => {
                held.position == was.position.map(&moved)
                    && pads_of(held)?.iter().zip(pads_of(was)?).all(|(pad, old)| {
                        pad.id == old.id
                            && pad.net == old.net
                            && pad.position == old.position.map(&moved)
                    })
            }
            (held, was) => {
                let mut expected = was.clone();
                for point in copper_points(&mut expected).into_iter().flatten() {
                    *point = moved(*point);
                }
                *held == expected
            }
        };
        anyhow::ensure!(ok, "{} is not where the move puts it", was.label(id));
    }
    Ok(())
}

/// The two placement tools' road and this tool's for the same block, out
/// and back, then a quarter turn about a point and back, then a move one
/// KiCad undo takes back, each checked by a read of its own.
async fn live_block(
    handler: &McpHandler,
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    path: &Path,
    block: &Items,
) -> anyhow::Result<Value> {
    let (dx, dy) = (1.27, -2.54);
    let by = |sign: i64| {
        move |p: Vector2| Vector2 {
            x_nm: p.x_nm + sign * 1_270_000,
            y_nm: p.y_nm - sign * 2_540_000,
        }
    };
    let footprints: Vec<_> = block
        .values()
        .filter_map(|item| match item {
            Item::Footprint(footprint) => Some(footprint.as_ref()),
            _ => None,
        })
        .collect();
    let references: Vec<_> = footprints
        .iter()
        .map(|fp| footprint_reference(fp))
        .collect();
    let vias: Vec<_> = block
        .iter()
        .filter(|(_, item)| matches!(item, Item::Via(_)))
        .map(|(id, _)| id)
        .collect();
    let copper: Vec<_> = block
        .iter()
        .filter(|(_, item)| !matches!(item, Item::Footprint(_)))
        .map(|(id, _)| id)
        .collect();
    let placements = |sign: f64| -> Value {
        footprints
            .iter()
            .map(|fp| {
                let p = fp.position.unwrap_or_default();
                json!({"reference": footprint_reference(fp), "x": nm_to_mm(p.x_nm) + sign * dx,
                    "y": nm_to_mm(p.y_nm) + sign * dy, "rotation": orientation(fp)})
            })
            .collect()
    };

    // The two tools' road: every footprint in one call, then the vias as a
    // batch, out by the offset and back.
    let mut two_tools = Meter::default();
    for sign in [1.0_f64, -1.0] {
        if !footprints.is_empty() {
            let placements = placements(sign.max(0.0));
            two_tools
                .call(
                    handler,
                    path,
                    "set_component_placements",
                    json!({"placements": placements, "allow_copper_conflicts": true}),
                )
                .await?;
        }
        if !vias.is_empty() {
            let offset = json!({"x": sign * dx, "y": sign * dy});
            two_tools
                .plan_and_apply(
                    handler,
                    path,
                    "set_via_positions",
                    json!({"uuids": vias, "offset": offset}),
                )
                .await?;
        }
    }
    expect_moved(client, document, block, by(0))?;

    // This tool, the tracks moving too, out and back.
    let mut ours = Meter::default();
    let mut selection = json!({});
    if !references.is_empty() {
        selection["references"] = json!(references);
    }
    if !copper.is_empty() {
        selection["uuids"] = json!(copper);
    }
    let with = |key: &str, value: Value, more: Option<(&str, Value)>| {
        let mut args = selection.clone();
        args[key] = value;
        if let Some((key, value)) = more {
            args[key] = value;
        }
        args
    };
    ours.plan_and_apply(
        handler,
        path,
        TOOL,
        with("offset", json!({"x": dx, "y": dy}), None),
    )
    .await?;
    expect_moved(client, document, block, by(1))?;
    ours.plan_and_apply(
        handler,
        path,
        TOOL,
        with("offset", json!({"x": -dx, "y": -dy}), None),
    )
    .await?;
    expect_moved(client, document, block, by(0))?;

    // A quarter turn about the first footprint's anchor, and back.
    let centre = footprints
        .first()
        .and_then(|fp| fp.position)
        .unwrap_or_default();
    let origin = json!({"x": nm_to_mm(centre.x_nm), "y": nm_to_mm(centre.y_nm)});
    let turn = |degrees: f64| with("rotation", json!(degrees), Some(("origin", origin.clone())));
    let mut turns = Meter::default();
    turns
        .plan_and_apply(handler, path, TOOL, turn(90.0))
        .await?;
    expect_moved(client, document, block, |p| {
        let (x_nm, y_nm) = quarter_turn((p.x_nm, p.y_nm), (centre.x_nm, centre.y_nm));
        Vector2 { x_nm, y_nm }
    })?;
    turns
        .plan_and_apply(handler, path, TOOL, turn(-90.0))
        .await?;
    expect_moved(client, document, block, by(0))?;

    // One KiCad undo takes the whole block back.
    Meter::default()
        .plan_and_apply(
            handler,
            path,
            TOOL,
            with("offset", json!({"x": dx, "y": dy}), None),
        )
        .await?;
    expect_moved(client, document, block, by(1))?;
    client.run_action("common.Interactive.undo")?;
    expect_moved(client, document, block, by(0))?;

    // The two tools' road moves no track: its tracks stay behind.
    Ok(
        json!({"kicad": format!("{:?}", client.get_kicad_version()?),
        "footprint_count": footprints.len(), "via_count": vias.len(),
        "track_and_arc_count": copper.len() - vias.len(),
        "placements_then_vias_out_and_back": two_tools.report(),
        "transform_pcb_items_offset_out_and_back": ours.report(),
        "transform_pcb_items_quarter_turn_and_back": turns.report()}),
    )
}

/// Run only on a disposable board open in pcbnew, never a user's design: it
/// moves every unlocked, ungrouped footprint, via, track and arc and puts
/// them back, and never saves.
/// KICAD_API_SOCKET=... COPPERPOUR_LIVE_TRANSFORM_BOARD=... cargo test -p copperpour-core
/// --locked live_transform_block_and_benchmark -- --ignored --nocapture
#[tokio::test]
#[ignore = "requires an explicitly named disposable KiCad board and its API socket"]
async fn live_transform_block_and_benchmark() {
    let path = std::path::PathBuf::from(
        std::env::var("COPPERPOUR_LIVE_TRANSFORM_BOARD")
            .expect("name a disposable open board, never a user's design"),
    );
    let address = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET");
    let handler = McpHandler::new(crate::tools::ServerConfig {
        ipc_address: address.clone(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let client = KiCadIpcClient::new(address);
    let document = client.find_open_board(&path).unwrap();
    let board = read_board(&client, &document, &BOARD).unwrap();
    let grouped: HashSet<String> = read_groups(&client, &document)
        .unwrap()
        .into_values()
        .flat_map(|group| group.items.into_iter().map(|member| member.value))
        .collect();
    let contents = snapshot(&client, &document).unwrap();
    let tree = copperpour_sexp::parse_sexp(&contents).unwrap();
    let owned = copperpour_sexp::board::footprint_owned_group_counts(&tree).unwrap();
    let block: Items = board
        .iter()
        .filter(|(id, item)| {
            item.locked() == LockedState::LsUnlocked as i32
                && !grouped.contains(*id)
                && match item {
                    Item::Footprint(footprint) => {
                        let reference = footprint_reference(footprint);
                        !reference.is_empty()
                            && holders(&board, reference).len() == 1
                            && owned.get(id.as_str()) == Some(&0)
                    }
                    other => other.is_copper(),
                }
        })
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect();
    let copper = select(&board, Item::is_copper);
    let outcome = live_block(&handler, &client, &document, &path, &block).await;
    // Leave the board as it was found, whatever happened above.
    let ids: Vec<_> = block.keys().cloned().collect();
    let held = read_targets(&client, &document, &ids).unwrap();
    let now = read_board(&client, &document, &COPPER).unwrap();
    let mut items = Vec::new();
    if !targets_differing(&held, &block).is_empty() {
        items.extend(block.values().map(Item::pack));
    }
    // Other copper is put back only where its net changed, the one change
    // these moves can cause it.
    items.extend(
        copper
            .iter()
            .filter(|(id, item)| {
                !block.contains_key(*id)
                    && now.get(*id).is_some_and(|held| held.net() != item.net())
            })
            .map(|(_, item)| item.pack()),
    );
    if !items.is_empty() {
        client
            .run_commit_recovering_in(document.clone(), "Put back the live block", |scoped| {
                scoped.update_items_in(document.clone(), items)
            })
            .unwrap();
    }
    let held = read_targets(&client, &document, &ids).unwrap();
    assert!(targets_differing(&held, &block).is_empty());
    assert_eq!(
        copper_changes(
            &copper,
            &read_board(&client, &document, &COPPER).unwrap(),
            &block
        ),
        Default::default()
    );
    eprintln!("transform benchmark: {}", outcome.unwrap());
}

/// Every place KiCad holds an item of `ids` other than `pcbnew` has it.
fn live_differences(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    label: &str,
    ids: &[String],
    pcbnew: &Value,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for (id, item) in read_targets(client, document, ids)? {
        differences(
            &format!("{label} {id}"),
            &oracle_record(&item),
            &pcbnew[&id],
            &mut out,
        );
    }
    Ok(out)
}

/// What pcbnew's select-all shows of the groups: each top-level group that
/// is not locked must be listed once and none of its members on its own,
/// and every group's member set read over the API must be as `groups` holds
/// it.
fn live_group_problems(
    client: &KiCadIpcClient,
    document: &DocumentSpecifier,
    editor: &copperpour_ipc::IpcEditorDocument,
    groups: &Groups,
    label: &str,
) -> anyhow::Result<Vec<String>> {
    use copperpour_ipc::IpcSelectionMutation;
    client.mutate_selection(editor, IpcSelectionMutation::Clear, &[])?;
    client.run_action("common.Interactive.selectAll")?;
    let selected: Vec<String> = client
        .observe_selection(editor)?
        .selected_objects
        .into_iter()
        .map(|object| object.kiid)
        .collect();
    client.mutate_selection(editor, IpcSelectionMutation::Clear, &[])?;
    let inner: HashSet<&str> = groups
        .values()
        .flat_map(|group| group.items.iter().map(|member| member.value.as_str()))
        .collect();
    let mut problems = Vec::new();
    for (id, group) in groups.iter().filter(|(id, _)| !inner.contains(id.as_str())) {
        if group.locked == LockedState::LsLocked as i32 {
            continue;
        }
        let listed = selected.iter().filter(|kiid| *kiid == id).count();
        if listed != 1 {
            problems.push(format!("{label}: group {id} listed {listed} times"));
        }
        for member in &group.items {
            if selected.contains(&member.value) {
                problems.push(format!(
                    "{label}: {} listed apart from group {id}",
                    member.value
                ));
            }
        }
    }
    let now = read_groups(client, document)?;
    if !groups_hold(&now, &groups.values().cloned().collect::<Vec<_>>()) {
        problems.push(format!("{label}: a group's members changed"));
    }
    Ok(problems)
}

/// Run only on a disposable copy of `pcb_item_transform_kicad10.kicad_pcb`
/// open in pcbnew, never a user's design: it turns every unlocked item by
/// 45°, then by 90°, comparing each with pcbnew's own turn in `oracle.json`,
/// takes each back with one KiCad undo, checks the groups after each step,
/// and never saves.
/// KICAD_API_SOCKET=... COPPERPOUR_LIVE_TRANSFORM_FIXTURE=... cargo test -p copperpour-core
/// --locked live_transform_fixture_kinds_and_groups -- --ignored --nocapture
#[tokio::test]
#[ignore = "requires a disposable copy of the kinds board open in pcbnew and its API socket"]
async fn live_transform_fixture_kinds_and_groups() {
    let path = std::path::PathBuf::from(
        std::env::var("COPPERPOUR_LIVE_TRANSFORM_FIXTURE")
            .expect("name a disposable copy of the kinds board, open in pcbnew"),
    );
    let address = std::env::var("KICAD_API_SOCKET").expect("KICAD_API_SOCKET");
    let handler = McpHandler::new(crate::tools::ServerConfig {
        ipc_address: address.clone(),
        eager_toolsets: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let client = KiCadIpcClient::new(address);
    let document = client.find_open_board(&path).unwrap();
    let editor = client
        .observe_editor_state()
        .unwrap()
        .editors
        .into_iter()
        .filter(|editor| editor.editor == copperpour_ipc::IpcEditorKind::Pcb)
        .flat_map(|editor| editor.documents)
        .find(|open| {
            open.document_path
                .as_deref()
                .is_some_and(|open| Path::new(open).file_name() == path.file_name())
        })
        .expect("the board's PCB editor");
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let found = read_board(&client, &document, &BOARD).unwrap();
    let groups = read_groups(&client, &document).unwrap();
    let (cx, cy) = (
        oracle["centre_nm"][0].as_i64().unwrap(),
        oracle["centre_nm"][1].as_i64().unwrap(),
    );
    let mut problems = live_group_problems(&client, &document, &editor, &groups, "found").unwrap();
    let mut summary = json!({"kicad": format!("{:?}", client.get_kicad_version().unwrap())});
    for (case, degrees) in [("turn_45", 45.0), ("turn_90", 90.0)] {
        let pcbnew = &oracle["cases"][case];
        let ids: Vec<String> = pcbnew
            .as_object()
            .unwrap()
            .keys()
            .filter(|id| !K_LOCKED.contains(&id.as_str()))
            .cloned()
            .collect();
        let mut meter = Meter::default();
        let args = json!({"uuids": ids, "rotation": degrees,
            "origin": {"x": nm_to_mm(cx), "y": nm_to_mm(cy)}});
        let applied = meter.plan_and_apply(&handler, &path, TOOL, args).await;
        if let Err(error) = applied {
            problems.push(format!("{case}: {error:#}"));
            break;
        }
        let moved = live_differences(&client, &document, case, &ids, pcbnew).unwrap();
        let after = live_group_problems(&client, &document, &editor, &groups, case).unwrap();
        client.run_action("common.Interactive.undo").unwrap();
        let held = &oracle["cases"]["held"];
        let undone = live_differences(&client, &document, "undone", &ids, held).unwrap();
        let back = live_group_problems(&client, &document, &editor, &groups, "undone").unwrap();
        summary[case] = json!({"items": ids.len(), "differences": moved.len(),
            "differences_after_undo": undone.len(), "group_problems": after.len() + back.len(),
            "calls": meter.report()});
        problems.extend(moved.into_iter().chain(after).chain(undone).chain(back));
    }
    // Leave the board as it was found, whatever happened above.
    let ids: Vec<String> = found.keys().cloned().collect();
    let now = read_targets(&client, &document, &ids).unwrap();
    let changed: Items = found
        .iter()
        .filter(|(id, item)| now.get(*id) != Some(*item))
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect();
    if !changed.is_empty() {
        let touched: Vec<Group> = touched_groups(&groups, &changed, &[])
            .into_iter()
            .cloned()
            .collect();
        let items = update_order(&changed, Vec::new(), &touched);
        client
            .run_commit_recovering_in(document.clone(), "Put back the kinds board", |scoped| {
                scoped.update_items_in(document.clone(), items)
            })
            .unwrap();
        problems.push(format!("{} items were put back by hand", changed.len()));
    }
    eprintln!("transform fixture: {summary}");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
