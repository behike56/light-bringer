use std::{cell::Cell, collections::BTreeMap, rc::Rc};

use gpui_kit::{
    component::{ActiveTheme, Disableable, Sizable, button::Button, label::Label},
    *,
};

use crate::graph::{Edge, Graph, Position};

use super::{Editor, Selection};

const NODE_WIDTH: f64 = 156.0;
const NODE_HEIGHT: f64 = 44.0;
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

    fn hit(&self, window_position: Point<Pixels>, graph: &Graph) -> Option<Selection> {
        let local = self.local(window_position);
        let world = self.transform.unproject(local);
        if let Some(position) = graph.positions().values().rev().find(|position| {
            let delta = self.position(position) - world;
            delta.x.abs() <= NODE_WIDTH / 2.0 && delta.y.abs() <= NODE_HEIGHT / 2.0
        }) {
            return Some(Selection::Node(position.node_id));
        }

        edge_curves(graph, self)
            .into_iter()
            .rev()
            .find(|(_, curve)| curve.hit(local))
            .map(|(edge, _)| Selection::Edge(edge.id))
    }
}

impl Editor {
    pub(super) fn canvas(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let bounds = Rc::clone(&self.viewport.bounds);
        let percent = format!("{:.0}%", self.viewport.transform.scale * 100.0);
        div()
            .flex_1()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().muted)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::canvas_down))
            .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    editor.update_drag(event.position, cx);
                } else if editor.viewport.drag.is_some() {
                    editor.finish_drag(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|editor, event: &MouseUpEvent, _, cx| {
                    editor.update_drag(event.position, cx);
                    editor.finish_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|editor, event: &MouseUpEvent, _, cx| {
                    editor.update_drag(event.position, cx);
                    editor.finish_drag(cx);
                }),
            )
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
        let item = self.viewport.hit(event.position, &self.graph);
        let start = self.viewport.local(event.position);
        if item.is_none() {
            self.viewport.drag = Some(Drag::Pan {
                start,
                pan: self.viewport.transform.pan,
            });
            // Keep the inspector available while exploring the canvas.
        } else if self.select(item, false, window, cx) {
            if let Some(Selection::Node(id)) = item {
                let initial = self.graph.positions()[&id];
                let offset = point(initial.x, initial.y) - self.viewport.transform.unproject(start);
                self.viewport.drag = Some(Drag::Node {
                    initial,
                    preview: initial,
                    offset,
                });
            }
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn update_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let local = self.viewport.local(position);
        match &mut self.viewport.drag {
            Some(Drag::Pan { start, pan }) => self.viewport.transform.pan = *pan + local - *start,
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

    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(Drag::Node {
            initial, preview, ..
        }) = self.viewport.drag.take()
            && (preview.x - initial.x).hypot(preview.y - initial.y) > 0.001
        {
            match self.graph.put_position(preview) {
                Ok(()) => {
                    self.dirty = true;
                    self.status = None;
                }
                Err(error) => self.show_error(format!("{error:#}"), cx),
            }
        }
        cx.notify();
    }
}

struct Scene {
    transform: Transform,
    nodes: Vec<(Coord, SharedString, bool)>,
    edges: Vec<(Curve, SharedString, bool)>,
}

impl Scene {
    fn new(editor: &Editor, bounds: Bounds<Pixels>) -> Self {
        let viewport = &editor.viewport;
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
                    let id = position.node_id;
                    let selected = editor.selection() == Some(Selection::Node(id));
                    Some((center, caption(&editor.graph.nodes()[&id].name), selected))
                } else {
                    None
                }
            })
            .collect();
        let edges = edge_curves(&editor.graph, viewport)
            .into_iter()
            .filter(|(_, curve)| curve.visible(extent))
            .map(|(edge, curve)| {
                (
                    curve,
                    caption(&edge.label),
                    editor.selection() == Some(Selection::Edge(edge.id)),
                )
            })
            .collect();
        Self {
            transform,
            nodes,
            edges,
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
        let label = self.label();
        if (cursor.x - label.x).abs() < 20.0 && (cursor.y - label.y).abs() < 8.0 {
            return true;
        }
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
                    first_pos + point(17.0, -NODE_HEIGHT / 2.0),
                    first_pos + point(36.0, -NODE_HEIGHT / 2.0 - reach),
                    first_pos + point(NODE_WIDTH / 2.0 + reach, -35.0),
                    first_pos + point(NODE_WIDTH / 2.0, 4.0),
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
                [node_boundary(source, a), a, b, node_boundary(target, b)]
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

fn node_boundary(center: Coord, toward: Coord) -> Coord {
    let delta = toward - center;
    let ratio = (delta.x.abs() / (NODE_WIDTH / 2.0)).max(delta.y.abs() / (NODE_HEIGHT / 2.0));
    center + delta / ratio.max(0.000_001)
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

        for (curve, _, selected) in &scene.edges {
            let [start, a, b, end] = curve.0;
            let color = if *selected {
                selected_color
            } else {
                edge_color
            };
            let mut path = PathBuilder::stroke(px(if *selected { 2.5 } else { 1.5 }));
            path.move_to(origin + pixel(start));
            path.cubic_bezier_to(origin + pixel(end), origin + pixel(a), origin + pixel(b));
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

        for (curve, label, selected) in &scene.edges {
            let color = if *selected {
                selected_color
            } else {
                text_color
            };
            let center = origin + pixel(curve.label());
            let line = shape(label, px(11.), color, &font, window);
            let width = line.width().min(px(165.));
            let rect = Bounds::centered_at(center, size(width + px(10.), px(18.)));
            window.paint_quad(quad(
                rect,
                px(4.),
                node_color,
                px(1.),
                border,
                BorderStyle::Solid,
            ));
            paint_text(line, center, width, px(16.), window, cx);
        }

        for (center, name, selected) in &scene.nodes {
            let center = origin + pixel(*center);
            let scale = scene.transform.scale as f32;
            let width = px(NODE_WIDTH as f32 * scale);
            let height = px(NODE_HEIGHT as f32 * scale);
            let rect = Bounds::centered_at(center, size(width, height));
            let color = if *selected { selected_color } else { border };
            window.paint_quad(quad(
                rect,
                px(8. * scale),
                node_color,
                px(if *selected { 2. } else { 1. }),
                color,
                BorderStyle::Solid,
            ));
            if scale >= 0.36 {
                let line = shape(
                    name,
                    px((13.0 * scale).clamp(8.0, 25.0)),
                    text_color,
                    &font,
                    window,
                );
                paint_text(line, center, width - px(12. * scale), height, window, cx);
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
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.clone(), font_size, &[run], None)
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
    use super::{NODE_HEIGHT, NODE_WIDTH, Transform, node_boundary};
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
        assert_eq!(
            node_boundary(node, node + point(-100.0, 0.0)),
            point(120.0 - NODE_WIDTH / 2.0, 100.0),
        );
        assert_eq!(
            node_boundary(node, node + point(0.0, -100.0)),
            point(120.0, 100.0 - NODE_HEIGHT / 2.0),
        );
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
        use crate::{graph::Graph, storage::ProjectFile};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.sqlite");
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
                let mut editor = Editor::new(cx.focus_handle());
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
        let graph = project.read().unwrap();
        assert_eq!(graph.nodes()[&order_id].name, "注文");
        assert_eq!(graph.edges().values().next().unwrap().label, "含む");
        assert!(graph.positions()[&order_id].y > original.y);
    }
}
