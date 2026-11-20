use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use gpui_kit::{
    component::{ActiveTheme, Disableable, Sizable, button::Button, label::Label},
    *,
};

use crate::graph::{CardKind, Edge, Graph, Position, ProjectMode, card_label};

use super::{Editor, Selection, alignment::MOVE_THRESHOLD};

const NODE_WIDTH: f64 = 156.0;
const NODE_HEIGHT: f64 = 44.0;
const CARD_HEIGHT: f64 = 78.0;
const ZOOM_MIN: f64 = 0.2;
const ZOOM_MAX: f64 = 3.0;

type Coord = Point<f64>;

pub(super) struct Viewport {
    bounds: Rc<Cell<Bounds<Pixels>>>,
    transform: Transform,
    drag: Option<Drag>,
}

#[derive(Clone, Copy)]
struct Transform {
    pan: Coord,
    scale: f64,
}

#[derive(Clone, Copy)]
enum Drag {
    Pan {
        start: Coord,
        pan: Coord,
    },
    Node {
        initial: Position,
        preview: Position,
        offset: Coord,
    },
    Select {
        start: Coord,
        end: Coord,
    },
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            bounds: Rc::new(Cell::new(Bounds::default())),
            transform: Transform::default(),
            drag: None,
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            pan: point(0.0, 0.0),
            scale: 1.0,
        }
    }
}

impl Transform {
    fn project(self, p: Coord) -> Coord {
        p * self.scale + self.pan
    }

    fn unproject(self, p: Coord) -> Coord {
        (p - self.pan) / self.scale
    }

    fn zoom(&mut self, anchor: Coord, factor: f64) {
        let world = self.unproject(anchor);
        self.scale = (self.scale * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        self.pan = anchor - world * self.scale;
    }
}

impl Viewport {
    pub(super) fn reset(&mut self) {
        self.transform = Transform::default();
        self.drag = None;
    }

    pub(super) fn cancel_drag(&mut self) {
        self.drag = None;
    }

    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn selection_bounds(&self, start: Coord, end: Coord) -> Bounds<f64> {
        let extent = self.bounds.get().size;
        let limit = coord(point(extent.width, extent.height));
        let clamp = |position: Coord| position.clamp(&point(0.0, 0.0), &limit);
        let (start, end) = (clamp(start), clamp(end));
        Bounds::from_corners(
            point(start.x.min(end.x), start.y.min(end.y)),
            point(start.x.max(end.x), start.y.max(end.y)),
        )
    }

    fn local(&self, p: Point<Pixels>) -> Coord {
        coord(p - self.bounds.get().origin)
    }

    fn position(&self, stored: &Position) -> Coord {
        match self.drag {
            Some(Drag::Node { preview, .. }) if preview.node_id == stored.node_id => {
                point(preview.x, preview.y)
            }
            _ => point(stored.x, stored.y),
        }
    }

    fn hit(
        &self,
        window_position: Point<Pixels>,
        graph: &Graph,
        window: &Window,
    ) -> Option<Selection> {
        let local = self.local(window_position);
        let world = self.transform.unproject(local);
        let half = node_half_size(graph.mode());
        if let Some(position) = graph.positions().values().rev().find(|position| {
            let delta = self.position(position) - world;
            delta.x.abs() <= half.x && delta.y.abs() <= half.y
        }) {
            return Some(Selection::Node(position.node_id));
        }

        let curves = edge_curves(graph, self);
        let label = curves.iter().rev().find(|(edge, curve)| {
            let delta = curve.label() - local;
            delta.x.abs() <= 87.5 && delta.y.abs() <= 9.0 && {
                let width = shape(
                    &caption(&edge.label),
                    px(11.),
                    black(),
                    &window.text_style().font(),
                    window,
                )
                .width();
                label_bounds(pixel(curve.label()), width).contains(&pixel(local))
            }
        });
        label
            .or_else(|| curves.iter().rev().find(|(_, curve)| curve.hit(local)))
            .map(|(edge, _)| Selection::Edge(edge.id))
    }
}

impl Editor {
    pub(super) fn canvas(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.alignment_toolbar(cx))
            .child(self.graph_canvas(cx))
            .children((self.graph.mode() == ProjectMode::Ddd).then(|| legend(cx)))
    }

    fn graph_canvas(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let bounds = Rc::clone(&self.viewport.bounds);
        let percent = format!("{:.0}%", self.viewport.transform.scale * 100.0);
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().muted)
            .track_focus(&self.canvas_focus)
            .on_key_down(cx.listener(Self::canvas_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::canvas_down))
            .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, window, cx| {
                if event.dragging() {
                    editor.update_drag(event.position, cx);
                } else if editor.viewport.drag.is_some() {
                    editor.finish_drag(window, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::canvas_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::canvas_up))
            .on_scroll(cx.listener(|editor, event: &ScrollEvent, _, cx| {
                let delta = f64::from(f32::from(event.delta.pixel_delta(px(32.)).y));
                editor.zoom_at(
                    event.position,
                    (delta.clamp(-200.0, 200.0) / 240.0).exp(),
                    cx,
                );
            }))
            .on_pinch(cx.listener(|editor, event: &PinchEvent, _, cx| {
                editor.zoom_at(event.position, f64::from(1.0 + event.delta).max(0.1), cx);
            }))
            .child(
                canvas(
                    move |layout, _, cx| {
                        bounds.set(layout);
                        view.upgrade().map(|view| Scene::new(view.read(cx), layout))
                    },
                    |bounds, scene, window, cx| {
                        if let Some(scene) = scene {
                            draw_scene(&scene, bounds, window, cx);
                        }
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_3()
                    .right_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .occlude()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(self.zoom_button("zoom-out", "−", "縮小", 1.0 / 1.2, cx))
                    .child(Label::new(percent).text_xs().w(px(42.)).text_center())
                    .child(self.zoom_button("zoom-in", "＋", "拡大", 1.2, cx))
                    .child(
                        Button::new("reset-view")
                            .label("リセット")
                            .small()
                            .outline()
                            .disabled(self.busy)
                            .on_click(cx.listener(|editor, _, _, cx| {
                                if !editor.busy {
                                    editor.viewport.reset();
                                    cx.notify();
                                }
                            })),
                    ),
            )
    }

    fn zoom_button(
        &self,
        id: &'static str,
        label: &'static str,
        tooltip: &'static str,
        factor: f64,
        cx: &Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .small()
            .outline()
            .tooltip(tooltip)
            .disabled(self.busy)
            .on_click(cx.listener(move |editor, _, _, cx| {
                editor.zoom_at(editor.viewport.bounds.get().center(), factor, cx);
            }))
    }

    fn zoom_at(&mut self, pivot: Point<Pixels>, factor: f64, cx: &mut Context<Self>) {
        if !self.busy && self.viewport.drag.is_none() && factor.is_finite() {
            let anchor = self.viewport.local(pivot);
            self.viewport.transform.zoom(anchor, factor);
            cx.notify();
        }
    }

    fn canvas_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let item = self.viewport.hit(event.position, &self.graph, window);
        let start = self.viewport.local(event.position);
        let shift = event.modifiers.shift;
        self.viewport.drag = match item {
            None if shift => Some(Drag::Select { start, end: start }),
            None => Some(Drag::Pan {
                start,
                pan: self.viewport.transform.pan,
            }),
            Some(Selection::Node(id)) if shift => {
                if !self.toggle_card(id, window, cx) {
                    return;
                }
                None
            }
            Some(item) => {
                if !self.select(Some(item), false, window, cx) {
                    return;
                }
                if let Selection::Node(id) = item {
                    let initial = self.graph.positions()[&id];
                    let offset =
                        point(initial.x, initial.y) - self.viewport.transform.unproject(start);
                    Some(Drag::Node {
                        initial,
                        preview: initial,
                        offset,
                    })
                } else {
                    None
                }
            }
        };
        self.canvas_focus.focus(window, cx);
        cx.notify();
    }

    fn canvas_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key != "escape" || self.busy {
            return;
        }
        match self.viewport.drag {
            Some(Drag::Select { .. }) => {
                self.viewport.cancel_drag();
                cx.notify();
            }
            None => {
                self.select(None, false, window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn canvas_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.update_drag(event.position, cx);
        self.finish_drag(window, cx);
    }

    fn update_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let local = self.viewport.local(position);
        match &mut self.viewport.drag {
            Some(Drag::Pan { start, pan }) => self.viewport.transform.pan = *pan + local - *start,
            Some(Drag::Select { end, .. }) => *end = local,
            Some(Drag::Node {
                initial,
                preview,
                offset,
            }) => {
                let world = self.viewport.transform.unproject(local) + *offset;
                *preview = Position {
                    node_id: initial.node_id,
                    x: world.x,
                    y: world.y,
                };
            }
            None => return,
        }
        cx.notify();
    }

    fn finish_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.viewport.drag.take() {
            Some(Drag::Node {
                initial, preview, ..
            }) if (preview.x - initial.x).hypot(preview.y - initial.y) > MOVE_THRESHOLD => {
                match self.graph.put_position(preview) {
                    Ok(()) => {
                        self.invalidate_alignment(preview.node_id);
                        self.refresh_positions_dirty();
                        self.status = None;
                    }
                    Err(error) => self.show_error(format!("{error:#}"), cx),
                }
            }
            Some(Drag::Select { start, end }) => self.finish_range(start, end, window, cx),
            None => return,
            _ => {}
        }
        cx.notify();
    }

    fn finish_range(
        &mut self,
        start: Coord,
        end: Coord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = self.viewport.selection_bounds(start, end);
        let transform = self.viewport.transform;
        let top_left = transform.unproject(bounds.origin);
        let bottom_right = transform.unproject(bounds.bottom_right());
        let half = node_half_size(self.graph.mode());
        let mut cards: BTreeSet<_> = self.selected_cards().collect();
        let previous_count = cards.len();
        cards.extend(self.graph.positions().values().filter_map(|position| {
            let left = position.x - half.x;
            let right = position.x + half.x;
            let top = position.y - half.y;
            let bottom = position.y + half.y;
            (left >= top_left.x
                && right <= bottom_right.x
                && top >= top_left.y
                && bottom <= bottom_right.y)
                .then_some(position.node_id)
        }));
        if cards.len() != previous_count {
            self.select_cards(cards, window, cx);
        }
    }
}

struct Scene {
    mode: ProjectMode,
    transform: Transform,
    nodes: Vec<CanvasNode>,
    edges: Vec<CanvasEdge>,
    selection_box: Option<Bounds<f64>>,
}

struct CanvasNode {
    center: Coord,
    name: SharedString,
    kind: Option<CardKind>,
    selected: bool,
}

struct CanvasEdge {
    curve: Curve,
    label: SharedString,
    selected: bool,
    dashed: bool,
}

impl Scene {
    fn new(editor: &Editor, bounds: Bounds<Pixels>) -> Self {
        let viewport = &editor.viewport;
        let graph = &editor.graph;
        let mode = graph.mode();
        let transform = viewport.transform;
        let extent = coord(point(bounds.size.width, bounds.size.height));
        let margin = 100.0 * transform.scale + 24.0;
        let nodes = editor
            .graph
            .positions()
            .values()
            .filter_map(|position| {
                let center = transform.project(viewport.position(position));
                if center.x >= -margin
                    && center.x <= extent.x + margin
                    && center.y >= -margin
                    && center.y <= extent.y + margin
                {
                    let node = &graph.nodes()[&position.node_id];
                    let name = match mode {
                        ProjectMode::Free => caption(&node.name),
                        ProjectMode::Ddd => node.name.chars().take(150).collect::<String>().into(),
                    };
                    Some(CanvasNode {
                        center,
                        name,
                        kind: node.event_storming_kind,
                        selected: editor.is_selected(Selection::Node(node.id)),
                    })
                } else {
                    None
                }
            })
            .collect();
        let edges = edge_curves(graph, viewport)
            .into_iter()
            .filter(|(_, curve)| curve.visible(extent))
            .map(|(edge, curve)| CanvasEdge {
                curve,
                label: caption(&edge.label),
                selected: editor.selection() == Some(Selection::Edge(edge.id)),
                dashed: mode == ProjectMode::Ddd && edge.event_storming_relation.is_none(),
            })
            .collect();
        Self {
            mode,
            transform,
            nodes,
            edges,
            selection_box: match viewport.drag {
                Some(Drag::Select { start, end }) => Some(viewport.selection_bounds(start, end)),
                _ => None,
            },
        }
    }
}

// Cubic Bézier control points in canvas-local screen coordinates.
#[derive(Clone, Copy)]
struct Curve([Coord; 4]);

impl Curve {
    fn at(self, t: f64) -> Coord {
        let [a, b, c, d] = self.0;
        let s = 1.0 - t;
        a * (s * s * s) + b * (3.0 * s * s * t) + c * (3.0 * s * t * t) + d * (t * t * t)
    }

    fn label(self) -> Coord {
        self.at(0.5) - point(0., 9.)
    }

    fn hit(self, cursor: Coord) -> bool {
        let mut prev = self.0[0];
        for i in 1..=20 {
            let next = self.at(f64::from(i) / 20.0);
            let segment = next - prev;
            let rel = cursor - prev;
            let len = segment.x * segment.x + segment.y * segment.y;
            let t = if len == 0.0 {
                0.0
            } else {
                ((segment.x * rel.x + segment.y * rel.y) / len).clamp(0.0, 1.0)
            };
            let delta = cursor - (prev + segment * t);
            if delta.x.hypot(delta.y) < 7.0 {
                return true;
            }
            prev = next;
        }
        false
    }

    fn visible(self, extent: Coord) -> bool {
        let p = self.0;
        let margin = 30.0;
        !p.iter().all(|p| p.x < -margin)
            && !p.iter().all(|p| p.y < -margin)
            && !p.iter().all(|p| p.x > extent.x + margin)
            && !p.iter().all(|p| p.y > extent.y + margin)
    }

    fn trace(self, path: &mut PathBuilder, origin: Point<Pixels>, dashed: bool) {
        let [start, a, b, end] = self.0;
        if !dashed {
            path.move_to(origin + pixel(start));
            path.cubic_bezier_to(origin + pixel(end), origin + pixel(a), origin + pixel(b));
            return;
        }

        // Flatten the curve and carry the dash phase across segments.
        let length_bound = self
            .0
            .windows(2)
            .map(|pair| {
                let delta = pair[1] - pair[0];
                delta.x.hypot(delta.y)
            })
            .sum::<f64>();
        let dash = 6.0_f64.max(length_bound / 1000.0);
        let mut phase = 0.0;
        let mut start = start;
        for step in 1..=32 {
            let end = self.at(f64::from(step) / 32.0);
            let delta = end - start;
            let length = delta.x.hypot(delta.y);
            let mut offset = -phase;
            while offset < length {
                let from = offset.max(0.0);
                let to = (offset + dash).min(length);
                if to > from {
                    path.move_to(origin + pixel(start + delta * (from / length)));
                    path.line_to(origin + pixel(start + delta * (to / length)));
                }
                offset += 2.0 * dash;
            }
            phase = (phase + length) % (2.0 * dash);
            start = end;
        }
    }
}

fn edge_curves<'a>(graph: &'a Graph, viewport: &Viewport) -> Vec<(&'a Edge, Curve)> {
    let mut groups = BTreeMap::<_, Vec<&Edge>>::new();
    for edge in graph.edges().values() {
        let pair = (
            edge.source_id.min(edge.target_id),
            edge.source_id.max(edge.target_id),
        );
        groups.entry(pair).or_default().push(edge);
    }

    let mut curves = Vec::with_capacity(graph.edges().len());
    let half = node_half_size(graph.mode());
    for ((first, second), group) in groups {
        let first_pos = viewport.position(&graph.positions()[&first]);
        let second_pos = viewport.position(&graph.positions()[&second]);
        let vector = second_pos - first_pos;
        let length = vector.x.hypot(vector.y);
        let middle = (group.len() - 1) as f64 / 2.0;
        for (index, edge) in group.into_iter().enumerate() {
            let world = if length < 0.001 {
                let reach = 60.0 + index as f64 * 34.0;
                [
                    first_pos + point(17.0, -half.y),
                    first_pos + point(36.0, -half.y - reach),
                    first_pos + point(half.x + reach, -35.0),
                    first_pos + point(half.x, 4.0),
                ]
            } else {
                let (source, target) = if edge.source_id == first {
                    (first_pos, second_pos)
                } else {
                    (second_pos, first_pos)
                };
                let normal = point(-vector.y, vector.x) / length;
                let offset = normal * ((index as f64 - middle) * 48.0);
                let a = source + (target - source) / 3.0 + offset;
                let b = target + (source - target) / 3.0 + offset;
                [
                    node_boundary(source, a, half),
                    a,
                    b,
                    node_boundary(target, b, half),
                ]
            };
            let screen = world.map(|p| viewport.transform.project(p));
            // Keep GPU path coordinates within tessellation precision even for extreme imports.
            if screen
                .iter()
                .all(|p| p.x.abs() < 1_000_000.0 && p.y.abs() < 1_000_000.0)
            {
                curves.push((edge, Curve(screen)));
            }
        }
    }
    curves
}

pub(super) fn node_half_size(mode: ProjectMode) -> Coord {
    let height = match mode {
        ProjectMode::Free => NODE_HEIGHT,
        ProjectMode::Ddd => CARD_HEIGHT,
    };
    point(NODE_WIDTH / 2.0, height / 2.0)
}

fn node_boundary(center: Coord, toward: Coord, half: Coord) -> Coord {
    let delta = toward - center;
    let ratio = (delta.x.abs() / half.x).max(delta.y.abs() / half.y);
    center + delta / ratio.max(0.000_001)
}

fn label_bounds(center: Point<Pixels>, text_width: Pixels) -> Bounds<Pixels> {
    Bounds::centered_at(center, size(text_width.min(px(165.)) + px(10.), px(18.)))
}

fn card_color(kind: CardKind) -> Hsla {
    let color = match kind {
        CardKind::ViewRead => 0x36a85c,
        CardKind::Actor => 0xc99717,
        CardKind::Command => 0x3178d4,
        CardKind::Aggregate => 0xd56077,
        CardKind::DomainEvent => 0xe37c39,
        CardKind::Policy => 0x9564ca,
        CardKind::ExternalSystem => 0x239e9e,
    };
    rgb(color).into()
}

fn card_background(background: Hsla, kind: Option<CardKind>) -> Hsla {
    kind.map_or(background, |kind| {
        background.blend(card_color(kind).opacity(0.27))
    })
}

fn legend(cx: &App) -> impl IntoElement {
    div()
        .id("ddd-legend")
        .flex()
        .flex_wrap()
        .gap_1()
        .px_2()
        .py_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .children(
            CardKind::ALL
                .into_iter()
                .map(Some)
                .chain([None])
                .map(|kind| {
                    div()
                        .id(kind.map_or("unclassified", CardKind::key))
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(card_background(cx.theme().background, kind))
                        .px_2()
                        .py_1()
                        .child(Label::new(card_label(kind)).text_xs())
                }),
        )
}

fn draw_scene(scene: &Scene, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let grid_color = cx.theme().muted_foreground.opacity(0.2);
    let edge_color = cx.theme().muted_foreground;
    let node_color = cx.theme().background;
    let text_color = cx.theme().foreground;
    let border = cx.theme().border;
    let selected_color = cx.theme().primary;
    let font = window.text_style().font();
    let origin = bounds.origin;

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let spacing = 28.0;
        let mut x = scene.transform.pan.x.rem_euclid(spacing);
        while px(x as f32) < bounds.size.width {
            let mut y = scene.transform.pan.y.rem_euclid(spacing);
            while px(y as f32) < bounds.size.height {
                window.paint_quad(fill(
                    Bounds::new(origin + pixel(point(x, y)), size(px(1.4), px(1.4))),
                    grid_color,
                ));
                y += spacing;
            }
            x += spacing;
        }

        for edge in &scene.edges {
            let [_, _, b, end] = edge.curve.0;
            let color = if edge.selected {
                selected_color
            } else {
                edge_color
            };
            let mut path = PathBuilder::stroke(px(if edge.selected { 2.5 } else { 1.5 }));
            edge.curve.trace(&mut path, origin, edge.dashed);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }

            let tangent = end - b;
            let unit = tangent / tangent.x.hypot(tangent.y).max(0.000_001);
            let base = end - unit * 10.0;
            let wing = point(-unit.y, unit.x) * 4.0;
            let mut arrow = PathBuilder::fill();
            arrow.move_to(origin + pixel(end));
            arrow.line_to(origin + pixel(base + wing));
            arrow.line_to(origin + pixel(base - wing));
            arrow.close();
            if let Ok(path) = arrow.build() {
                window.paint_path(path, color);
            }
        }

        for edge in &scene.edges {
            let color = if edge.selected {
                selected_color
            } else {
                text_color
            };
            let center = origin + pixel(edge.curve.label());
            let line = shape(&edge.label, px(11.), color, &font, window);
            let rect = label_bounds(center, line.width());
            window.paint_quad(quad(
                rect,
                px(4.),
                node_color,
                px(1.),
                border,
                BorderStyle::Solid,
            ));
            paint_text(line, center, rect.size.width - px(10.), px(16.), window, cx);
        }

        let half = node_half_size(scene.mode);
        for node in &scene.nodes {
            let center = origin + pixel(node.center);
            let scale = scene.transform.scale as f32;
            let width = px(2.0 * half.x as f32 * scale);
            let height = px(2.0 * half.y as f32 * scale);
            let rect = Bounds::centered_at(center, size(width, height));
            let kind = node.kind.filter(|_| scene.mode == ProjectMode::Ddd);
            let color = if node.selected {
                selected_color
            } else {
                kind.map_or(border, card_color)
            };
            window.paint_quad(quad(
                rect,
                px(8. * scale),
                card_background(node_color, kind),
                px(if node.selected { 2. } else { 1. }),
                color,
                BorderStyle::Solid,
            ));
            if scale >= 0.36 {
                if scene.mode == ProjectMode::Ddd {
                    paint_card(node, rect, scale, &font, window, cx);
                } else {
                    let line = shape(
                        &node.name,
                        px((13.0 * scale).clamp(8.0, 25.0)),
                        text_color,
                        &font,
                        window,
                    );
                    paint_text(line, center, width - px(12. * scale), height, window, cx);
                }
            }
        }
        if let Some(selection) = scene.selection_box {
            window.paint_quad(quad(
                Bounds::from_corners(
                    origin + pixel(selection.origin),
                    origin + pixel(selection.bottom_right()),
                ),
                px(0.),
                selected_color.opacity(0.12),
                px(1.),
                selected_color,
                BorderStyle::Solid,
            ));
        }
    });
}

fn paint_card(
    node: &CanvasNode,
    rect: Bounds<Pixels>,
    scale: f32,
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let inset = px(8. * scale);
    let width = rect.size.width - 2. * inset;
    let label = shape(
        &card_label(node.kind).into(),
        px(11. * scale),
        cx.theme().foreground,
        font,
        window,
    );
    let header_height = px(14. * scale);
    let header = point(rect.center().x, rect.origin.y + inset + header_height / 2.);
    paint_text(label, header, width, header_height, window, cx);

    let origin = rect.origin + point(inset, px(24. * scale));
    let bounds = Bounds::new(origin, size(width, px(48. * scale)));
    let run = text_run(&node.name, cx.theme().foreground, font);
    let Ok(lines) = window.text_system().shape_text(
        node.name.clone(),
        px(13. * scale),
        &[run],
        Some(width),
        Some(2),
    ) else {
        return;
    };
    let line_height = px(16. * scale);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let mut origin = origin;
        for line in lines {
            let _ = line.paint(
                origin,
                line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            );
            origin.y += line_height * (line.wrap_boundaries.len() + 1) as f32;
            if origin.y >= bounds.origin.y + bounds.size.height {
                break;
            }
        }
    });
}

fn shape(
    text: &SharedString,
    font_size: Pixels,
    color: Hsla,
    font: &Font,
    window: &Window,
) -> ShapedLine {
    window.text_system().shape_line(
        text.clone(),
        font_size,
        &[text_run(text, color, font)],
        None,
    )
}

fn text_run(text: &str, color: Hsla, font: &Font) -> TextRun {
    TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn paint_text(
    line: ShapedLine,
    center: Point<Pixels>,
    width: Pixels,
    height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = Bounds::centered_at(center, size(width, height));
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let alignment = if line.width() > width {
            TextAlign::Left
        } else {
            TextAlign::Center
        };
        let _ = line.paint(bounds.origin, height, alignment, Some(width), window, cx);
    });
}

fn caption(text: &str) -> SharedString {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect::<String>()
        .into()
}

fn pixel(p: Coord) -> Point<Pixels> {
    point(px(p.x as f32), px(p.y as f32))
}

fn coord(p: Point<Pixels>) -> Coord {
    point(f64::from(f32::from(p.x)), f64::from(f32::from(p.y)))
}

#[cfg(test)]
mod tests {
    use super::{
        CARD_HEIGHT, NODE_HEIGHT, NODE_WIDTH, ProjectMode, Transform, node_boundary, node_half_size,
    };
    use gpui_kit::point;

    #[test]
    fn zoom_preserves_the_world_coordinate_at_the_pointer() {
        let pivot = point(340., 135.);
        let mut transform = Transform {
            pan: point(140., -45.),
            ..Transform::default()
        };
        let world = transform.unproject(pivot);
        for factor in [1.6, 12.0, 0.01, 0.8] {
            transform.zoom(pivot, factor);
            let projected = transform.project(world);
            assert!((projected.x - pivot.x).abs() < 1e-9);
            assert!((projected.y - pivot.y).abs() < 1e-9);
        }
    }

    #[test]
    fn arrows_attach_to_the_side_facing_the_curve() {
        let node = point(120.0, 100.0);
        let half = node_half_size(ProjectMode::Free);
        assert_eq!(
            node_boundary(node, node + point(-100.0, 0.0), half),
            point(120.0 - NODE_WIDTH / 2.0, 100.0),
        );
        assert_eq!(
            node_boundary(node, node + point(0.0, -100.0), half),
            point(120.0, 100.0 - NODE_HEIGHT / 2.0),
        );
        assert_eq!(
            node_boundary(
                node,
                node + point(0.0, -100.0),
                node_half_size(ProjectMode::Ddd)
            ),
            point(120.0, 100.0 - CARD_HEIGHT / 2.0),
        );
    }

    #[gpui_kit::test]
    async fn selects_aligns_and_undoes_cards_through_the_canvas_and_save(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use std::time::Duration;

        use gpui_kit::{
            AppContext, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
            MouseUpEvent, SharedString, WindowBounds, WindowOptions, px, size,
            test::{TestAppContextExt, TestWindowExt},
        };
        use uuid::Uuid;

        use super::{Editor, Scene, pixel};
        use crate::{
            graph::{CardKind, Graph, Node, Position},
            project::ProjectFile,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alignment.lbproj");
        let ids = [1, 2, 3, 4].map(Uuid::from_u128);
        let original = Graph::from_records(
            ProjectMode::Ddd,
            ids.into_iter()
                .zip(["注文する", "注文", "注文された", "キャンセルされた"])
                .map(|(id, name)| Node {
                    id,
                    name: name.into(),
                    kind: None,
                    description: None,
                    source_reference: None,
                    event_storming_kind: Some(CardKind::DomainEvent),
                })
                .collect(),
            vec![],
            ids.into_iter()
                .zip([
                    (140.0, 140.0),
                    (300.0, 260.0),
                    (530.0, 360.0),
                    (540.0, 458.0),
                ])
                .map(|(node_id, (x, y))| Position { node_id, x, y })
                .collect(),
        )
        .unwrap();
        drop(ProjectFile::create(&path, &original).unwrap());
        let (window, editor) = cx.update(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1440.), px(840.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_, cx| {
                let focus = cx.focus_handle();
                cx.new(|cx| Editor::new(focus, cx))
            })
            .unwrap()
        });
        let timeout = Duration::from_secs(4);
        let click = |cx: &mut gpui_kit::TestAppContext, id: &'static str| {
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| window.click(id, cx))
                    .unwrap()
            });
            cx.run_until_parked();
        };
        let open = |cx: &mut gpui_kit::TestAppContext| {
            click(cx, "開く");
            cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        };
        let choose_alignment = |cx: &mut gpui_kit::TestAppContext, steps| {
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| {
                        window.click("align-cards", cx);
                        assert!(window.try_find("popup-menu").is_some());
                        for _ in 0..steps {
                            window.press("down", cx);
                        }
                        window.press("enter", cx);
                    })
                    .unwrap()
            });
            cx.run_until_parked();
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| {
                        window.render_frame(cx);
                        assert!(window.try_find("popup-menu").is_none());
                    })
                    .unwrap()
            });
        };
        open(cx);
        cx.wait_for(window, timeout, |_, cx| {
            editor.read(cx).project.is_some() && !editor.read(cx).busy
        })
        .await;

        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    let row = |id| SharedString::from(format!("node-{id}"));
                    let shift = Modifiers {
                        shift: true,
                        ..Default::default()
                    };
                    window.click("add-node", cx);
                    window.input("編集中", cx);
                    window.click_with_modifiers(row(ids[0]), shift, cx);
                    assert_eq!(editor.read(cx).selected_card_count(), 0);
                    assert!(editor.read(cx).error.as_ref().unwrap().contains("反映"));
                    window.click("revert", cx);
                    window.click(row(ids[0]), cx);
                    window.click_with_modifiers(row(ids[1]), shift, cx);
                    assert_eq!(
                        editor.read(cx).selected_cards().collect::<Vec<_>>(),
                        ids[..2]
                    );
                    assert!(editor.read(cx).form.is_none());
                    window.click_with_modifiers(row(ids[1]), shift, cx);
                    assert_eq!(editor.read(cx).single_card(), Some(ids[0]));
                    assert!(editor.read(cx).form.is_some());

                    window.click("zoom-in", cx);
                    let viewport = &editor.read(cx).viewport;
                    assert!(viewport.transform.scale > 1.0);
                    let to_screen = |world| {
                        viewport.bounds.get().origin + pixel(viewport.transform.project(world))
                    };
                    let start = to_screen(point(200.0, 432.0));
                    let end = to_screen(point(633.0, 206.0));
                    for cancel in [true, false] {
                        window.dispatch_event(
                            MouseDownEvent {
                                button: MouseButton::Left,
                                position: start,
                                modifiers: shift,
                                click_count: 1,
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.dispatch_event(
                            MouseMoveEvent {
                                pressed_button: Some(MouseButton::Left),
                                position: end,
                                modifiers: shift,
                            }
                            .to_platform_input(),
                            cx,
                        );
                        let state = editor.read(cx);
                        assert!(
                            Scene::new(state, state.viewport.bounds.get())
                                .selection_box
                                .is_some()
                        );
                        if cancel {
                            window.press("escape", cx);
                        }
                        window.dispatch_event(
                            MouseUpEvent {
                                button: MouseButton::Left,
                                position: end,
                                modifiers: shift,
                                click_count: 1,
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        let state = editor.read(cx);
                        assert_eq!(state.selected_card_count(), if cancel { 1 } else { 3 });
                        assert!(!state.has_changes(cx));
                    }
                    assert_eq!(
                        editor.read(cx).selected_cards().collect::<Vec<_>>(),
                        ids[..3]
                    );
                })
                .unwrap()
        });

        choose_alignment(cx, 4); // 上揃え
        choose_alignment(cx, 4); // No-op must preserve the previous undo.
        choose_alignment(cx, 9); // 整列を取り消す
        cx.update(|cx| {
            assert!(!editor.read(cx).has_changes(cx));
            assert_eq!(editor.read(cx).graph, original);
        });
        choose_alignment(cx, 4);
        choose_alignment(cx, 7); // 横方向に等間隔
        let arranged = cx.update(|cx| {
            let state = editor.read(cx);
            assert!(state.has_changes(cx));
            for (&id, x) in ids[..3].iter().zip([140.0, 335.0, 530.0]) {
                assert_eq!(
                    state.graph.positions()[&id],
                    Position {
                        node_id: id,
                        x,
                        y: 140.0
                    }
                );
            }
            assert_eq!(
                state.graph.positions()[&ids[3]],
                original.positions()[&ids[3]]
            );
            state.graph.clone()
        });
        click(cx, "保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert!(!editor.read(cx).has_changes(cx)));
        choose_alignment(cx, 9);
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(state.has_changes(cx));
            assert_eq!(state.graph.positions()[&ids[1]].x, 300.0);
            assert_eq!(state.graph.positions()[&ids[1]].y, 140.0);
        });
        click(cx, "開く");
        cx.simulate_prompt_answer("破棄");
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert_eq!(state.graph, arranged);
            assert_eq!(state.selected_card_count(), 0);
            assert!(state.alignment_change.is_none());
            assert!(!state.has_changes(cx));
        });
    }

    #[gpui_kit::test]
    async fn edits_and_saves_a_japanese_graph_after_dragging_at_zoom(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use std::{sync::Arc, time::Duration};

        use gpui_kit::{
            AppContext, Bounds, SharedString, WindowBounds, WindowOptions, px, size,
            test::{TestAppContextExt, TestWindowExt},
        };

        use super::{Editor, Selection, pixel};
        use crate::{graph::Graph, project::ProjectFile};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.lbproj");
        let project = Arc::new(ProjectFile::create(&path, &Graph::default()).unwrap());
        let (window, editor) = cx.update(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1240.), px(780.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_, cx| {
                let mut editor = Editor::new(cx.focus_handle(), cx);
                editor.project = Some(project.clone());
                cx.new(|_| editor)
            })
            .unwrap()
        });

        let (order_id, original) = cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.render_frame(cx);
                    for name in ["注文", "注文明細"] {
                        window.click("add-node", cx);
                        window.input(name, cx);
                        window.click("apply", cx);
                    }
                    let graph = &editor.read(cx).graph;
                    assert_eq!(graph.nodes().len(), 2);
                    let order = graph
                        .nodes()
                        .values()
                        .find(|node| node.name == "注文")
                        .unwrap();
                    let id = order.id;
                    let position = graph.positions()[&id];

                    window.click("zoom-in", cx);
                    let viewport = &editor.read(cx).viewport;
                    assert!(viewport.transform.scale > 1.0);
                    let start = viewport.bounds.get().origin
                        + pixel(viewport.transform.project(point(position.x, position.y)));
                    window.drag(start, start + point(px(32.), px(42.)), cx);
                    let state = editor.read(cx);
                    assert!(state.selection() == Some(Selection::Node(id)));
                    assert!(state.graph.positions()[&id].y > position.y);
                    window.click("add-edge", cx);
                    window.input("含む", cx);
                    let target = editor
                        .read(cx)
                        .graph
                        .nodes()
                        .values()
                        .find(|node| node.name == "注文明細")
                        .unwrap()
                        .id;
                    window.within("終点").click("input", cx);
                    assert_eq!(window.find("終点").expanded(), Some(true));
                    window.click(SharedString::from(target.to_string()), cx);
                    (id, position)
                })
                .unwrap()
        });

        cx.wait_for(window, Duration::from_secs(2), |window, _| {
            window.find("終点").expanded() == Some(false)
        })
        .await;
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click("apply", cx);
                    let state = editor.read(cx);
                    assert!(state.error.is_none(), "{:?}", state.error);
                    let edge = state.graph.edges().values().next().expect("edge applied");
                    assert_eq!(edge.source_id, order_id);
                    assert_eq!(state.graph.nodes()[&edge.target_id].name, "注文明細");
                    window.click("保存", cx);
                })
                .unwrap()
        });

        cx.wait_for(window, Duration::from_secs(4), |_, cx| {
            !editor.read(cx).busy
        })
        .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(state.error.is_none(), "{:?}", state.error);
            assert!(!state.has_changes(cx));
        });
        let graph = project.reload().unwrap();
        assert_eq!(graph.nodes()[&order_id].name, "注文");
        assert_eq!(graph.edges().values().next().unwrap().label, "含む");
        assert!(graph.positions()[&order_id].y > original.y);
    }

    #[gpui_kit::test]
    async fn edits_ddd_cards_and_arrows_and_saves_or_discards_mode_changes(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use std::time::Duration;

        use gpui_kit::{
            AppContext, Bounds, SharedString, WindowBounds, WindowOptions, px, size,
            test::{TestAppContextExt, TestWindowExt},
        };

        use super::{Editor, Scene, Selection, pixel};
        use crate::graph::{CardKind, DddRelation};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.lbproj");
        let (window, editor) = cx.update(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1240.), px(780.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_, cx| {
                let focus = cx.focus_handle();
                cx.new(|cx| Editor::new(focus, cx))
            })
            .unwrap()
        });
        let click = |cx: &mut gpui_kit::TestAppContext, id: &'static str| {
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| window.click(id, cx))
                    .unwrap()
            });
            cx.run_until_parked();
        };
        let choose =
            |cx: &mut gpui_kit::TestAppContext, select: &'static str, item: SharedString| {
                cx.update(|cx| {
                    window
                        .update(cx, |_, window, cx| {
                            window.within(select).click("input", cx);
                            assert_eq!(window.find(select).expanded(), Some(true));
                            window.click(item, cx);
                        })
                        .unwrap()
                });
            };
        let timeout = Duration::from_secs(4);
        click(cx, "新規");
        cx.simulate_prompt_answer(ProjectMode::Ddd.label());
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(path.clone()));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        assert_eq!(
            cx.update(|cx| editor.read(cx).graph.mode()),
            ProjectMode::Ddd
        );

        let mut ids = Vec::new();
        for (kind, name) in [
            (
                CardKind::DomainEvent,
                "会員が注文した書籍の配送先住所が確認された",
            ),
            (CardKind::Policy, "注文受付後に決済を依頼する"),
        ] {
            click(cx, "add-node");
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| window.input(name, cx))
                    .unwrap()
            });
            click(cx, "switch-mode");
            cx.update(|cx| {
                assert_eq!(editor.read(cx).graph.mode(), ProjectMode::Ddd);
                assert!(editor.read(cx).error.as_ref().unwrap().contains("反映"));
            });
            choose(cx, "card-kind", kind.key().into());
            cx.wait_for(window, timeout, |window, _| {
                window.find("card-kind").expanded() == Some(false)
            })
            .await;
            click(cx, "apply");
            cx.update(|cx| {
                let editor = editor.read(cx);
                let Some(Selection::Node(id)) = editor.selection() else {
                    panic!("missing card selection")
                };
                assert_eq!(editor.graph.nodes()[&id].event_storming_kind, Some(kind));
                assert_eq!(editor.graph.nodes()[&id].name, name);
                ids.push(id);
            });
        }
        let [event, policy] = ids.as_slice() else {
            unreachable!()
        };
        let (event, policy) = (*event, *policy);
        click(cx, "zoom-in");
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    let state = editor.read(cx);
                    let position = state.graph.positions()[&event];
                    let viewport = &state.viewport;
                    let start = viewport.bounds.get().origin
                        + pixel(
                            viewport
                                .transform
                                .project(point(position.x, position.y + 30.)),
                        );
                    assert!(
                        viewport.hit(start, &state.graph, window) == Some(Selection::Node(event))
                    );
                    window.drag(start, start + point(px(12.), px(18.)), cx);
                    assert!(editor.read(cx).graph.positions()[&event].y > position.y);
                })
                .unwrap()
        });

        click(cx, "add-edge");
        choose(cx, "終点", policy.to_string().into());
        cx.wait_for(window, timeout, |window, _| {
            window.find("終点").expanded() == Some(false)
        })
        .await;
        click(cx, "defined-relation");
        click(cx, "apply");
        let typed_edge = cx.update(|cx| {
            let graph = &editor.read(cx).graph;
            let edge = graph.edges().values().next().expect("typed arrow");
            assert_eq!((edge.source_id, edge.target_id), (event, policy));
            assert_eq!(edge.event_storming_relation, Some(DddRelation::Triggers));
            assert_eq!(edge.label, DddRelation::Triggers.label());
            edge.id
        });
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click(SharedString::from(format!("node-{policy}")), cx);
                    window.click("add-edge", cx);
                    window.input("関連する", cx);
                })
                .unwrap()
        });
        choose(cx, "終点", event.to_string().into());
        cx.wait_for(window, timeout, |window, _| {
            window.find("終点").expanded() == Some(false)
        })
        .await;
        click(cx, "apply");
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert_eq!(editor.graph.edges().len(), 2);
            let scene = Scene::new(editor, editor.viewport.bounds.get());
            assert_eq!(scene.edges.iter().filter(|edge| edge.dashed).count(), 1);
        });
        click(cx, "保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        let saved = cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(!editor.has_changes(cx));
            let saved = editor.project.as_ref().unwrap().reload().unwrap();
            assert_eq!(saved, editor.graph);
            saved
        });

        click(cx, "switch-mode");
        cx.update(|cx| assert!(editor.read(cx).has_changes(cx)));
        click(cx, "開く");
        cx.simulate_prompt_answer("破棄");
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![path]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert_eq!(editor.read(cx).graph, saved));

        click(cx, "switch-mode");
        click(cx, "保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        let mut expected = saved;
        expected.set_mode(ProjectMode::Free);
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(!editor.has_changes(cx));
            assert_eq!(editor.project.as_ref().unwrap().reload().unwrap(), expected);
        });
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click(SharedString::from(format!("edge-{typed_edge}")), cx);
                })
                .unwrap()
        });
        choose(cx, "終点", event.to_string().into());
        cx.wait_for(window, timeout, |window, _| {
            window.find("終点").expanded() == Some(false)
        })
        .await;
        click(cx, "apply");
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert_eq!(editor.graph, expected);
            assert!(
                editor
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("event_storming_relation")
            );
        });
        click(cx, "free-relation");
        click(cx, "apply");
        cx.update(|cx| {
            let editor = editor.read(cx);
            let edge = &editor.graph.edges()[&typed_edge];
            assert_eq!((edge.source_id, edge.target_id), (event, event));
            assert!(edge.event_storming_relation.is_none());
            assert!(editor.error.is_none());
        });
    }
}
