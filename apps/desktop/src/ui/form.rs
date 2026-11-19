use anyhow::{Context as _, Result};
use gpui_kit::{
    component::{
        ActiveTheme, IndexPath,
        input::{InputEvent, Textarea, TextareaState},
        label::Label,
        select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    },
    *,
};
use uuid::Uuid;

use crate::graph::{Edge, Graph, Node};

use super::{Editor, Selection};

pub(super) struct Form {
    pub selection: Selection,
    name: Entity<TextareaState>,
    kind: Option<Entity<TextareaState>>,
    description: Entity<TextareaState>,
    reference: Entity<TextareaState>,
    ends: Option<[Entity<NodeSelect>; 2]>,
    original: Option<Fields>,
    _subscriptions: Vec<Subscription>,
}

type NodeSelect = SelectState<SearchableVec<NodeChoice>>;

#[derive(Clone)]
struct NodeChoice {
    id: Uuid,
    title: SharedString,
}

impl SelectItem for NodeChoice {
    type Value = Uuid;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Uuid {
        &self.id
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id(SharedString::from(self.id.to_string()))
            .test_support()
            .truncate()
            .child(self.title.clone())
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            div()
                .truncate()
                .child(self.title.clone())
                .into_any_element(),
        )
    }
}

#[derive(Default, PartialEq)]
struct Fields {
    name: String,
    kind: Option<String>,
    description: Option<String>,
    reference: Option<String>,
    ends: [Option<Uuid>; 2],
}

impl Form {
    pub fn new(
        selection: Selection,
        graph: &Graph,
        source: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Self {
        let original = match selection {
            Selection::Node(id) => graph.nodes().get(&id).map(|node| Fields {
                name: node.name.clone(),
                kind: node.kind.clone().filter(|text| !text.is_empty()),
                description: node.description.clone().filter(|text| !text.is_empty()),
                reference: node
                    .source_reference
                    .clone()
                    .filter(|text| !text.is_empty()),
                ..Fields::default()
            }),
            Selection::Edge(id) => graph.edges().get(&id).map(|edge| Fields {
                name: edge.label.clone(),
                description: edge.note.clone().filter(|text| !text.is_empty()),
                reference: edge
                    .source_reference
                    .clone()
                    .filter(|text| !text.is_empty()),
                ends: [Some(edge.source_id), Some(edge.target_id)],
                ..Fields::default()
            }),
        };
        let empty = Fields::default();
        let values = original.as_ref().unwrap_or(&empty);
        let name = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(1)
                .default_value(values.name.clone())
        });
        let kind = matches!(selection, Selection::Node(_)).then(|| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(1)
                    .default_value(values.kind.clone().unwrap_or_default())
            })
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .default_value(values.description.clone().unwrap_or_default())
        });
        let reference = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(2)
                .default_value(values.reference.clone().unwrap_or_default())
        });
        let mut subscriptions = vec![
            cx.subscribe(&name, input_changed),
            cx.subscribe(&description, input_changed),
            cx.subscribe(&reference, input_changed),
        ];
        if let Some(kind) = &kind {
            subscriptions.push(cx.subscribe(kind, input_changed));
        }

        let ends = matches!(selection, Selection::Edge(_)).then(|| {
            let mut choices: Vec<_> = graph
                .nodes()
                .values()
                .map(|node| NodeChoice {
                    id: node.id,
                    title: node_choice(node).into(),
                })
                .collect();
            choices.sort_by(|a, b| a.title.cmp(&b.title));
            [values.ends[0].or(source), values.ends[1]].map(|id| {
                let index = choices
                    .iter()
                    .position(|choice| Some(choice.id) == id)
                    .map(IndexPath::new);
                let select = cx.new(|cx| {
                    SelectState::new(SearchableVec::new(choices.clone()), index, window, cx)
                        .searchable(true)
                });
                subscriptions
                    .push(cx.subscribe(&select, |_, _, _: &SelectEvent<_>, cx| cx.notify()));
                select
            })
        });
        Self {
            selection,
            name,
            kind,
            description,
            reference,
            ends,
            original,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    fn fields(&self, cx: &App) -> Fields {
        Fields {
            name: self.name.read(cx).value().to_string(),
            kind: self
                .kind
                .as_ref()
                .and_then(|input| optional(input.read(cx).value())),
            description: optional(self.description.read(cx).value()),
            reference: optional(self.reference.read(cx).value()),
            ends: self.ends.as_ref().map_or([None; 2], |[source, target]| {
                [
                    source.read(cx).selected_value().copied(),
                    target.read(cx).selected_value().copied(),
                ]
            }),
        }
    }

    pub fn dirty(&self, cx: &App) -> bool {
        self.original
            .as_ref()
            .is_none_or(|original| *original != self.fields(cx))
    }

    pub fn exists(&self) -> bool {
        self.original.is_some()
    }

    pub fn apply(&mut self, graph: &mut Graph, window: &mut Window, cx: &mut App) -> Result<()> {
        let mut fields = self.fields(cx);
        fields.name = fields.name.trim().to_owned();
        match self.selection {
            Selection::Node(id) => {
                graph.put_node(Node {
                    id,
                    name: fields.name.clone(),
                    kind: fields.kind.clone(),
                    description: fields.description.clone(),
                    source_reference: fields.reference.clone(),
                })?;
                graph.place_unpositioned();
            }
            Selection::Edge(id) => {
                graph.put_edge(Edge {
                    id,
                    source_id: fields.ends[0].context("始点ノードを選んでください")?,
                    target_id: fields.ends[1].context("終点ノードを選んでください")?,
                    label: fields.name.clone(),
                    note: fields.description.clone(),
                    source_reference: fields.reference.clone(),
                })?;
            }
        }
        if self.name.read(cx).value().as_str() != fields.name {
            self.name.update(cx, |input, cx| {
                input.set_value(fields.name.clone(), window, cx)
            });
        }
        self.original = Some(fields);
        Ok(())
    }

    pub fn render(&self, disabled: bool, cx: &App) -> impl IntoElement {
        let is_node = self.kind.is_some();
        let name_label = if is_node { "ノード名" } else { "関係名" };
        let description_label = if is_node { "説明" } else { "補足" };
        let mut form = div().flex().flex_col().gap_3().child(field(
            name_label,
            Textarea::new(&self.name)
                .w_full()
                .disabled(disabled)
                .aria_label(name_label),
            cx,
        ));
        if let Some(kind) = &self.kind {
            form = form.child(field(
                "種別",
                Textarea::new(kind)
                    .w_full()
                    .disabled(disabled)
                    .aria_label("種別"),
                cx,
            ));
        }
        if let Some([source, target]) = &self.ends {
            for (label, select) in [("始点", source), ("終点", target)] {
                form = form.child(field(
                    label,
                    Select::new(select)
                        .id(label)
                        .w_full()
                        .placeholder("ノードを選択")
                        .search_placeholder("ノードを検索")
                        .accessibility_label(label)
                        .disabled(disabled),
                    cx,
                ));
            }
        }
        form.child(field(
            description_label,
            Textarea::new(&self.description)
                .w_full()
                .disabled(disabled)
                .aria_label(description_label),
            cx,
        ))
        .child(field(
            "出典",
            Textarea::new(&self.reference)
                .w_full()
                .disabled(disabled)
                .aria_label("出典"),
            cx,
        ))
    }
}

fn field(label: &'static str, input: impl IntoElement, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            Label::new(label)
                .text_sm()
                .text_color(cx.theme().muted_foreground),
        )
        .child(input)
}

fn input_changed<T: 'static>(
    _: &mut Editor,
    _: Entity<T>,
    event: &InputEvent,
    cx: &mut Context<Editor>,
) {
    if matches!(event, InputEvent::Change) {
        cx.notify();
    }
}

fn optional(value: SharedString) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn node_choice(node: &Node) -> String {
    let mut title = node.name.clone();
    if let Some(kind) = &node.kind {
        title.push_str(&format!(" · {kind}"));
    }
    if let Some(description) = &node.description {
        let summary: String = description
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(30)
            .collect();
        if !summary.is_empty() {
            title.push_str(&format!(" · {summary}"));
        }
    }
    let short_id = node.id.simple().to_string();
    format!("{title}  #{}", &short_id[..8])
}
