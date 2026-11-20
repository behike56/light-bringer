use anyhow::{Context as _, Result};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, IndexPath, Sizable,
        button::Button,
        input::{InputEvent, Textarea, TextareaState},
        label::Label,
        select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    },
    *,
};
use uuid::Uuid;

use crate::graph::{CardKind, DddRelation, Edge, Graph, Node, ProjectMode, card_label};

use super::{Editor, Selection};

pub(super) struct Form {
    pub selection: Selection,
    name: Entity<TextareaState>,
    kind: Option<Entity<TextareaState>>,
    card_kind: Option<Entity<ChoiceSelect<Option<CardKind>>>>,
    description: Entity<TextareaState>,
    reference: Entity<TextareaState>,
    ends: Option<[Entity<ChoiceSelect<Uuid>>; 2]>,
    relation: Option<DddRelation>,
    suggested_name: Option<&'static str>,
    original: Option<Fields>,
    _subscriptions: Vec<Subscription>,
}

type ChoiceSelect<T> = SelectState<SearchableVec<Choice<T>>>;

#[derive(Clone)]
struct Choice<T> {
    id: SharedString,
    value: T,
    title: SharedString,
}

impl<T: Clone + PartialEq + 'static> SelectItem for Choice<T> {
    type Value = T;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &T {
        &self.value
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id(self.id.clone())
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
    card_kind: Option<CardKind>,
    description: Option<String>,
    reference: Option<String>,
    ends: [Option<Uuid>; 2],
    relation: Option<DddRelation>,
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
                card_kind: node.event_storming_kind,
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
                relation: edge.event_storming_relation,
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
        let card_kind = (kind.is_some() && graph.mode() == ProjectMode::Ddd).then(|| {
            let choices: Vec<_> = CardKind::ALL
                .into_iter()
                .map(Some)
                .chain(original.is_some().then_some(None))
                .map(|kind| Choice {
                    id: kind.map_or("unclassified", CardKind::key).into(),
                    value: kind,
                    title: card_label(kind).into(),
                })
                .collect();
            let index = original.as_ref().and_then(|fields| {
                choices
                    .iter()
                    .position(|choice| choice.value == fields.card_kind)
            });
            let select = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(choices),
                    index.map(IndexPath::new),
                    window,
                    cx,
                )
            });
            subscriptions.push(cx.subscribe(&select, |_, _, _: &SelectEvent<_>, cx| cx.notify()));
            select
        });

        let ends = matches!(selection, Selection::Edge(_)).then(|| {
            let mut choices: Vec<_> = graph
                .nodes()
                .values()
                .map(|node| Choice {
                    id: node.id.to_string().into(),
                    value: node.id,
                    title: node_choice(node, graph.mode()).into(),
                })
                .collect();
            choices.sort_by(|a, b| a.title.cmp(&b.title));
            [values.ends[0].or(source), values.ends[1]].map(|id| {
                let index = choices
                    .iter()
                    .position(|choice| Some(choice.value) == id)
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
            card_kind,
            description,
            reference,
            ends,
            relation: values.relation,
            suggested_name: None,
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
            card_kind: self.selected_card_kind(cx),
            description: optional(self.description.read(cx).value()),
            reference: optional(self.reference.read(cx).value()),
            ends: self.selected_ends(cx),
            relation: self.relation,
        }
    }

    fn selected_card_kind(&self, cx: &App) -> Option<CardKind> {
        match &self.card_kind {
            Some(select) => select.read(cx).selected_value().copied().flatten(),
            None => self.original.as_ref().and_then(|fields| fields.card_kind),
        }
    }

    fn selected_ends(&self, cx: &App) -> [Option<Uuid>; 2] {
        self.ends.as_ref().map_or([None; 2], |[source, target]| {
            [
                source.read(cx).selected_value().copied(),
                target.read(cx).selected_value().copied(),
            ]
        })
    }

    fn set_relation(&mut self, relation: Option<DddRelation>, window: &mut Window, cx: &mut App) {
        let name = self.name.read(cx).value();
        let next_name = relation.map(DddRelation::label);
        if name.trim().is_empty() || Some(name.as_str()) == self.suggested_name {
            self.name.update(cx, |input, cx| {
                input.set_value(next_name.unwrap_or_default(), window, cx);
            });
            self.suggested_name = next_name;
        } else {
            self.suggested_name = None;
        }
        self.relation = relation;
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
                if graph.mode() == ProjectMode::Ddd && !self.exists() {
                    fields.card_kind.context("カード種別を選んでください")?;
                }
                graph.put_node(Node {
                    id,
                    name: fields.name.clone(),
                    kind: fields.kind.clone(),
                    description: fields.description.clone(),
                    source_reference: fields.reference.clone(),
                    event_storming_kind: fields.card_kind,
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
                    event_storming_relation: fields.relation,
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

    pub fn render(&self, graph: &Graph, disabled: bool, cx: &Context<Editor>) -> impl IntoElement {
        let is_node = self.kind.is_some();
        let ddd = graph.mode() == ProjectMode::Ddd;
        let name_label = match (is_node, ddd) {
            (true, true) => "カード名",
            (true, false) => "ノード名",
            (false, _) => "関係名",
        };
        let description_label = if is_node { "説明" } else { "補足" };
        let mut form = div().flex().flex_col().gap_3();
        if let Some(kind) = &self.card_kind {
            form = form.child(field(
                "カード種別",
                Select::new(kind)
                    .id("card-kind")
                    .w_full()
                    .placeholder("種別を選択")
                    .accessibility_label("カード種別")
                    .disabled(disabled),
                cx,
            ));
        } else if let Some(kind) = self.selected_card_kind(cx) {
            form = form.child(field("カード種別", Label::new(kind.label()).text_sm(), cx));
        }
        form = form.child(field(
            name_label,
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    Textarea::new(&self.name)
                        .w_full()
                        .disabled(disabled)
                        .aria_label(name_label),
                )
                .children(self.selected_card_kind(cx).filter(|_| ddd).map(|kind| {
                    Label::new(format!("例　{}", kind.name_example()))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                })),
            cx,
        ));
        if let Some(kind) = &self.kind
            && (!ddd || self.exists())
        {
            let label = if ddd {
                "自由記述の種別"
            } else {
                "種別"
            };
            form = form.child(field(
                label,
                Textarea::new(kind)
                    .w_full()
                    .disabled(disabled)
                    .aria_label(label),
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
            if ddd || self.relation.is_some() {
                form = form.child(self.relation_field(graph, disabled, cx));
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

    fn relation_field(
        &self,
        graph: &Graph,
        disabled: bool,
        cx: &Context<Editor>,
    ) -> impl IntoElement {
        let [source, target] = self.selected_ends(cx);
        let eligible = source
            .zip(target)
            .and_then(|(source, target)| graph.relation_for(source, target));
        let mut choices = div().flex().flex_col().items_start().gap_1();
        if let Some(relation) = self.relation {
            choices = choices.child(Label::new(relation.label()).text_sm());
            if eligible != Some(relation) {
                choices = choices.child(
                    Label::new("この種別に合う端点を選んでください。")
                        .text_xs()
                        .text_color(cx.theme().danger),
                );
            }
        } else {
            choices = choices.child(Label::new("自由記述").text_sm());
        }
        if let Some(relation) = eligible.filter(|candidate| {
            graph.mode() == ProjectMode::Ddd && Some(*candidate) != self.relation
        }) {
            choices = choices.child(relation_button(
                "defined-relation",
                relation.label(),
                Some(relation),
                disabled,
                cx,
            ));
        }
        if self.relation.is_some() {
            choices = choices.child(relation_button(
                "free-relation",
                "自由記述に変更",
                None,
                disabled,
                cx,
            ));
        }
        field("関係種別", choices, cx)
    }
}

fn relation_button(
    id: &'static str,
    label: &'static str,
    relation: Option<DddRelation>,
    disabled: bool,
    cx: &Context<Editor>,
) -> Button {
    Button::new(id)
        .label(label)
        .small()
        .outline()
        .disabled(disabled)
        .on_click(cx.listener(move |editor, _, window, cx| {
            if !editor.busy
                && let Some(form) = &mut editor.form
            {
                form.set_relation(relation, window, cx);
                editor.error = None;
                cx.notify();
            }
        }))
}

fn field(label: &'static str, input: impl IntoElement, cx: &App) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("field-{label}")))
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

fn node_choice(node: &Node, mode: ProjectMode) -> String {
    let mut title = node.name.clone();
    if mode == ProjectMode::Ddd {
        title.push_str(&format!(" · {}", card_label(node.event_storming_kind)));
    }
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
