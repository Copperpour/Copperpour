### Rigid moves preserve named nets without local pins (patch release)

`move_region` with `include: "all"` now returns `plan_blocked` before
writing if the move changes a named net, even when no pin on the moved
sheet witnesses the change. For example, moving a global label onto a
different named wire may join pins on other sheets. Move the label and
wire together, or keep their anchors separate if the nets should stay
independent. Arguments and result keys are unchanged.
