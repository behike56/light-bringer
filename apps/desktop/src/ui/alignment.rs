use std::cmp::Ordering;

use anyhow::{Context as _, Result, ensure};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::Button,
        menu::{DropdownMenu, PopupMenuItem},
    },
    *,
};
use uuid::Uuid;

use crate::graph::Position;

use super::{Editor, canvas::node_half_size};

pub(super) const MOVE_THRESHOLD: f64 = 0.001;

pub(super) struct AlignmentChange {
    before: Vec<Position>,
    after: Vec<Position>,
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
enum Placement {
    Start,
    Center,
    End,
    Distribute,
}

#[derive(Clone, Copy)]
struct Alignment(Axis, Placement);

impl Axis {
    fn coordinates(self, position: Position) -> (f64, f64) {
        match self {
            Self::Horizontal => (position.x, position.y),
            Self::Vertical => (position.y, position.x),
        }
    }

    fn set(self, position: &mut Position, value: f64) {
        match self {
            Self::Horizontal => position.x = value,
            Self::Vertical => position.y = value,
        }
    }
}

impl Alignment {
    const GROUPS: [&'static [(&'static str, Self)]; 3] = [
        &[
            ("左揃え", Self(Axis::Horizontal, Placement::Start)),
            ("横中央揃え", Self(Axis::Horizontal, Placement::Center)),
            ("右揃え", Self(Axis::Horizontal, Placement::End)),
        ],
        &[
            ("上揃え", Self(Axis::Vertical, Placement::Start)),
            ("縦中央揃え", Self(Axis::Vertical, Placement::Center)),
            ("下揃え", Self(Axis::Vertical, Placement::End)),
        ],
        &[
            (
                "横方向に等間隔",
                Self(Axis::Horizontal, Placement::Distribute),
            ),
            (
                "縦方向に等間隔",
                Self(Axis::Vertical, Placement::Distribute),
            ),
        ],
    ];

    fn minimum_count(self) -> usize {
        match self.1 {
            Placement::Distribute => 3,
            _ => 2,
        }
    }

    fn arrange(self, positions: &mut [Position], card_extent: f64) -> Result<()> {
        let Self(axis, placement) = self;
        let coordinates = positions
            .iter()
            .map(|&position| axis.coordinates(position).0);
        let first = coordinates.clone().fold(f64::INFINITY, f64::min);
        let last = coordinates.fold(f64::NEG_INFINITY, f64::max);

        // All cards in the active mode have the same size. Aligning the centers to a
        // common coordinate also aligns the corresponding edges.
        let center = match placement {
            Placement::Start => first,
            Placement::Center => first.midpoint(last),
            Placement::End => last,
            Placement::Distribute => return self.distribute(positions, first, last, card_extent),
        };
        for position in positions {
            axis.set(position, center);
        }
        Ok(())
    }

    fn distribute(
        self,
        positions: &mut [Position],
        first: f64,
        last: f64,
        card_extent: f64,
    ) -> Result<()> {
        let axis = self.0;
        let gaps = positions.len() - 1;
        let span = last - first;
        ensure!(
            span.is_finite(),
            "カードを等間隔に並べる座標を計算できません"
        );
        // With a shared extent, center spacing is the card extent plus the gap.
        ensure!(
            span >= card_extent * gaps as f64,
            match axis {
                Axis::Horizontal =>
                    "カードを等間隔に並べる幅が足りません。左右端のカードを離してください",
                Axis::Vertical =>
                    "カードを等間隔に並べる高さが足りません。上下端のカードを離してください",
            }
        );
        positions.sort_unstable_by(|&a, &b| {
            axis.coordinates(a)
                .partial_cmp(&axis.coordinates(b))
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        let step = span / gaps as f64;
        for (index, position) in positions[1..gaps].iter_mut().enumerate() {
            let value = step.mul_add((index + 1) as f64, first);
            axis.set(position, value);
        }
        Ok(())
    }
}

impl Editor {
    fn can_arrange(&self) -> bool {
        self.project.is_some() && !self.busy && !self.viewport.is_dragging()
    }

    pub(super) fn alignment_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let editor = cx.entity().downgrade();
        div()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("align-cards")
                    .label("整列")
                    .small()
                    .outline()
                    .tooltip("Shiftを押しながらクリック、または空白をドラッグして複数選択")
                    .disabled(!self.can_arrange())
                    .dropdown_menu(move |mut menu, _, cx| {
                        let Some(view) = editor.upgrade() else {
                            return menu;
                        };
                        let state = view.read(cx);
                        let can_arrange = state.can_arrange();
                        let count = state.selected_card_count();
                        let can_undo = can_arrange && state.alignment_change.is_some();
                        for group in Alignment::GROUPS {
                            for &(label, alignment) in group {
                                let editor = editor.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .disabled(!can_arrange || count < alignment.minimum_count())
                                        .on_click(move |_, _, cx| {
                                            editor
                                                .update(cx, |editor, cx| {
                                                    editor.align(alignment, cx)
                                                })
                                                .ok();
                                        }),
                                );
                            }
                            menu = menu.separator();
                        }
                        let editor = editor.clone();
                        menu.item(
                            PopupMenuItem::new("整列を取り消す")
                                .disabled(!can_undo)
                                .on_click(move |_, _, cx| {
                                    editor
                                        .update(cx, |editor, cx| editor.undo_alignment(cx))
                                        .ok();
                                }),
                        )
                    }),
            )
    }

    fn align(&mut self, alignment: Alignment, cx: &mut Context<Self>) {
        if !self.can_arrange() || self.selected_card_count() < alignment.minimum_count() {
            return;
        }
        match self.apply_alignment(alignment) {
            Ok(()) => {
                self.error = None;
                cx.notify();
            }
            Err(error) => self.show_error(format!("{error:#}"), cx),
        }
    }

    fn apply_alignment(&mut self, alignment: Alignment) -> Result<()> {
        let positions = self.graph.positions();
        let mut arranged = self
            .selected_cards()
            .map(|id| {
                positions
                    .get(&id)
                    .copied()
                    .context("選択したカードの配置が見つかりません")
            })
            .collect::<Result<Vec<_>>>()?;
        let half = node_half_size(self.graph.mode());
        let extent = match alignment.0 {
            Axis::Horizontal => 2.0 * half.x,
            Axis::Vertical => 2.0 * half.y,
        };
        alignment.arrange(&mut arranged, extent)?;

        let (before, after): (Vec<_>, Vec<_>) = arranged
            .into_iter()
            .filter_map(|after| {
                let before = positions[&after.node_id];
                (before != after).then_some((before, after))
            })
            .unzip();
        if before
            .iter()
            .zip(&after)
            .all(|(before, after)| (after.x - before.x).hypot(after.y - before.y) <= MOVE_THRESHOLD)
        {
            return Ok(());
        }

        self.graph.put_positions(&after)?;
        self.alignment_change = Some(AlignmentChange { before, after });
        self.refresh_positions_dirty();
        self.status = None;
        Ok(())
    }

    fn undo_alignment(&mut self, cx: &mut Context<Self>) {
        if !self.can_arrange() {
            return;
        }
        let Some(change) = &self.alignment_change else {
            return;
        };
        if change
            .after
            .iter()
            .any(|position| self.graph.positions().get(&position.node_id) != Some(position))
        {
            self.alignment_change = None;
            self.show_error("対象のカードの配置が変わったため、整列を取り消せません", cx);
            return;
        }
        if let Err(error) = self.graph.put_positions(&change.before) {
            self.show_error(format!("{error:#}"), cx);
            return;
        }
        self.alignment_change = None;
        self.refresh_positions_dirty();
        self.status = None;
        self.error = None;
        cx.notify();
    }

    pub(super) fn invalidate_alignment(&mut self, id: Uuid) {
        if self
            .alignment_change
            .as_ref()
            .is_some_and(|change| change.after.iter().any(|position| position.node_id == id))
        {
            self.alignment_change = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Alignment, Axis, Placement, Position, Uuid};

    #[test]
    fn distribution_accepts_touching_edges_and_orders_ties_by_other_axis_then_id() {
        let position = |id, x, y| Position {
            node_id: Uuid::from_u128(id),
            x,
            y,
        };
        let source = [
            position(3, 0.0, -0.0),
            position(4, 0.0, -10.0),
            position(1, 0.0, 0.0),
            position(2, 468.0, 5.0),
        ];
        let horizontal = Alignment(Axis::Horizontal, Placement::Distribute);
        let mut positions = source;
        horizontal.arrange(&mut positions, 156.0).unwrap();
        assert_eq!(
            positions,
            [
                position(4, 0.0, -10.0),
                position(1, 156.0, 0.0),
                position(3, 312.0, -0.0),
                position(2, 468.0, 5.0),
            ]
        );

        let mut too_close = source;
        too_close[3].x -= 1.0;
        let before = too_close;
        assert!(horizontal.arrange(&mut too_close, 156.0).is_err());
        assert_eq!(too_close, before);

        let mut vertical = [
            position(1, 10.0, 0.0),
            position(2, 25.0, 62.0),
            position(3, 8.0, 100.0),
        ];
        let align = Alignment(Axis::Vertical, Placement::Distribute);
        assert!(align.arrange(&mut vertical, 78.0).is_err());
        align.arrange(&mut vertical, 44.0).unwrap();
        assert_eq!(vertical[1], position(2, 25.0, 50.0));
    }

    #[test]
    fn batch_positions_validate_every_update_before_writing() {
        use crate::graph::{Graph, Node};

        let node_id = Uuid::from_u128(1);
        let mut graph = Graph::default();
        let node = Node {
            id: node_id,
            name: "注文".into(),
            kind: None,
            description: None,
            source_reference: None,
            event_storming_kind: None,
        };
        let other = Uuid::from_u128(2);
        graph.put_node(node.clone()).unwrap();
        graph.put_node(Node { id: other, ..node }).unwrap();
        let original = Position {
            node_id,
            x: 10.0,
            y: 20.0,
        };
        graph.put_position(original).unwrap();
        let moved = Position {
            x: 30.0,
            ..original
        };
        for invalid in [
            Position {
                node_id: Uuid::from_u128(3),
                ..original
            },
            Position {
                node_id: other,
                y: f64::INFINITY,
                ..original
            },
            original,
        ] {
            assert!(graph.put_positions(&[moved, invalid]).is_err());
            assert_eq!(graph.positions()[&node_id], original);
        }
    }
}
