mod canvas;
mod files;
mod form;

use std::sync::Arc;

use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::{Button, ButtonVariants},
        input::{Copy, Cut, Paste, Redo, SelectAll, Undo},
        label::Label,
        scroll::ScrollableElement,
    },
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

use crate::{
    graph::{Graph, ProjectMode, card_label},
    project::ProjectFile,
};

use self::{canvas::Viewport, files::Command, form::Form};

gpui_kit::actions!(
    light_bringer,
    [
        NewProject,
        OpenProject,
        SaveProject,
        SaveProjectAs,
        ImportJson,
        ExportJson,
        CloseProject,
        Quit,
        CloseWindow
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    Node(Uuid),
    Edge(Uuid),
}

struct Editor {
    focus: FocusHandle,
    project: Option<Arc<ProjectFile>>,
    graph: Graph,
    form: Option<Form>,
    viewport: Viewport,
    dirty: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
}

pub fn run() {
    application()
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-n", NewProject, None),
                KeyBinding::new("cmd-o", OpenProject, None),
                KeyBinding::new("cmd-s", SaveProject, None),
                KeyBinding::new("cmd-shift-s", SaveProjectAs, None),
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.set_menus([
                Menu::new("Light Bringer").items([MenuItem::action("Light Bringer を終了", Quit)]),
                Menu::new("ファイル").items([
                    MenuItem::action("新規プロジェクト…", NewProject),
                    MenuItem::action("開く…", OpenProject),
                    MenuItem::action("保存", SaveProject),
                    MenuItem::action("別名で保存…", SaveProjectAs),
                    MenuItem::separator(),
                    MenuItem::action("JSON を取り込む…", ImportJson),
                    MenuItem::action("JSON を書き出す…", ExportJson),
                    MenuItem::separator(),
                    MenuItem::action("プロジェクトを閉じる", CloseProject),
                    MenuItem::action("ウィンドウを閉じる", CloseWindow),
                ]),
                Menu::new("編集").items([
                    MenuItem::os_action("取り消す", Undo, OsAction::Undo),
                    MenuItem::os_action("やり直す", Redo, OsAction::Redo),
                    MenuItem::separator(),
                    MenuItem::os_action("切り取り", Cut, OsAction::Cut),
                    MenuItem::os_action("コピー", Copy, OsAction::Copy),
                    MenuItem::os_action("ペースト", Paste, OsAction::Paste),
                    MenuItem::os_action("すべて選択", SelectAll, OsAction::SelectAll),
                ]),
            ]);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1240.), px(780.)),
                    cx,
                ))),
                window_min_size: Some(size(px(920.), px(590.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Light Bringer".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            match open_window(options, cx, |window, cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                cx.new(|_| Editor::new(focus))
            }) {
                Ok((handle, editor)) => {
                    let editor = editor.downgrade();
                    handle
                        .update(cx, |_, window, cx| {
                            window.on_window_should_close(cx, move |window, cx| {
                                let editor = editor.clone();
                                window.defer(cx, move |window, cx| {
                                    editor
                                        .update(cx, |editor, cx| {
                                            editor.command(Command::Close, window, cx)
                                        })
                                        .ok();
                                });
                                false
                            });
                        })
                        .ok();
                    cx.activate(true);
                }
                Err(error) => {
                    eprintln!("Light Bringer のウィンドウを開けません: {error:#}");
                    cx.quit();
                }
            }
        });
}

impl Editor {
    fn new(focus: FocusHandle) -> Self {
        Self {
            focus,
            project: None,
            graph: Graph::default(),
            form: None,
            viewport: Viewport::default(),
            dirty: false,
            busy: false,
            status: None,
            error: None,
        }
    }

    fn selection(&self) -> Option<Selection> {
        self.form.as_ref().map(|form| form.selection)
    }

    fn has_changes(&self, cx: &App) -> bool {
        self.dirty || self.form.as_ref().is_some_and(|form| form.dirty(cx))
    }

    fn can_save(&self, cx: &App) -> bool {
        self.project
            .as_ref()
            .is_some_and(|project| self.has_changes(cx) || project.needs_upgrade())
    }

    fn select(
        &mut self,
        selection: Option<Selection>,
        new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.busy {
            return false;
        }
        if self.selection() == selection {
            return true;
        }
        if self.form.as_ref().is_some_and(|form| form.dirty(cx)) {
            self.show_error("詳細フォームの入力を反映するか、取り消してください", cx);
            return false;
        }
        let source = match self.selection() {
            Some(Selection::Node(id)) if new => Some(id),
            _ => None,
        };
        self.form = selection.map(|item| Form::new(item, &self.graph, source, window, cx));
        if new && let Some(form) = &self.form {
            form.focus(window, cx);
        }
        self.error = None;
        cx.notify();
        true
    }

    fn apply_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        if let Some(form) = &mut self.form
            && form.dirty(cx)
        {
            form.apply(&mut self.graph, window, cx)?;
            self.dirty = true;
            self.status = None;
        }
        self.error = None;
        cx.notify();
        Ok(())
    }

    fn revert_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.form = self
            .form
            .as_ref()
            .filter(|form| form.exists())
            .map(|form| Form::new(form.selection, &self.graph, None, window, cx));
        self.error = None;
        cx.notify();
    }

    fn switch_mode(&mut self, mode: ProjectMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.project.is_none() || self.graph.mode() == mode {
            return;
        }
        if self.form.as_ref().is_some_and(|form| form.dirty(cx)) {
            self.show_error(
                "モードを切り替えるには、詳細フォームの入力を反映するか、取り消してください",
                cx,
            );
            return;
        }
        self.viewport.cancel_drag();
        self.graph.set_mode(mode);
        self.form = self
            .selection()
            .map(|item| Form::new(item, &self.graph, None, window, cx));
        self.dirty = true;
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection().filter(|_| !self.busy) else {
            return;
        };
        let (title, detail) = match selection {
            Selection::Node(id) => (
                "ノードを削除しますか？",
                format!(
                    "「{}」と関連する関係 {} 件を削除します。",
                    self.graph.nodes()[&id].name,
                    self.graph.related_edge_count(id)
                ),
            ),
            Selection::Edge(id) => (
                "関係を削除しますか？",
                format!("「{}」を削除します。", self.graph.edges()[&id].label),
            ),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            title,
            Some(&detail),
            &[
                PromptButton::Cancel("キャンセル".into()),
                PromptButton::Other("削除".into()),
            ],
            cx,
        );
        self.busy = true;
        self.viewport.cancel_drag();
        cx.notify();
        cx.spawn_in(window, async move |editor, cx| {
            let answer = answer.await;
            editor
                .update(cx, |editor, cx| {
                    editor.busy = false;
                    if matches!(answer, Ok(1)) {
                        match selection {
                            Selection::Node(id) => editor.graph.remove_node(id),
                            Selection::Edge(id) => editor.graph.remove_edge(id),
                        }
                        editor.form = None;
                        editor.dirty = true;
                        editor.status = None;
                        editor.error = None;
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn show_error(&mut self, error: impl Into<String>, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        cx.notify();
    }

    fn toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let has_project = self.project.is_some();
        let disabled = self.busy;
        let can_save = !disabled && self.can_save(cx);
        let mode = self.graph.mode();
        let next_mode = match mode {
            ProjectMode::Free => ProjectMode::Ddd,
            ProjectMode::Ddd => ProjectMode::Free,
        };
        let name = self
            .project
            .as_ref()
            .and_then(|p| p.path().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "プロジェクト未選択".to_owned());
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .min_h(px(54.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name),
            )
            .when(has_project, |toolbar| {
                toolbar.child(
                    Button::new("switch-mode")
                        .label(format!("{} ⇄", mode.label()))
                        .tooltip(format!("{}に切り替え", next_mode.label()))
                        .small()
                        .outline()
                        .disabled(disabled)
                        .on_click(cx.listener(move |editor, _, window, cx| {
                            editor.switch_mode(next_mode, window, cx);
                        })),
                )
            })
            .children(
                [
                    (Command::New, "新規", false),
                    (Command::Open, "開く", false),
                    (Command::Import, "JSON 取込", false),
                    (Command::Export, "JSON 書出", !has_project),
                    (Command::Save, "保存", !can_save),
                    (Command::SaveAs, "別名で保存", !has_project),
                ]
                .into_iter()
                .map(|(command, label, unavailable)| {
                    let button = Button::new(label)
                        .label(label)
                        .small()
                        .disabled(disabled || unavailable)
                        .on_click(cx.listener(move |editor, _, window, cx| {
                            editor.command(command, window, cx)
                        }));
                    if command == Command::Save {
                        button.primary()
                    } else {
                        button.outline()
                    }
                }),
            )
    }

    fn sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let disabled = self.busy;
        let ddd = self.graph.mode() == ProjectMode::Ddd;
        let mut nodes: Vec<_> = self.graph.nodes().values().collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut edges: Vec<_> = self.graph.edges().values().collect();
        edges.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));

        div()
            .w(px(238.))
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_3()
                    .child(
                        Label::new(format!(
                            "{} {}",
                            if ddd { "カード" } else { "ノード" },
                            nodes.len()
                        ))
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        Button::new("add-node")
                            .label("＋ 追加")
                            .small()
                            .disabled(disabled)
                            .on_click(cx.listener(|editor, _, window, cx| {
                                editor.select(
                                    Some(Selection::Node(Uuid::new_v4())),
                                    true,
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .id("node-list")
                    .p_2()
                    .pt_0()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(nodes.into_iter().map(|node| {
                        let kind = if ddd {
                            Some(card_label(node.event_storming_kind))
                        } else {
                            node.kind.as_deref()
                        };
                        let secondary = [kind, node.description.as_deref()]
                            .into_iter()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ");
                        self.list_row(Selection::Node(node.id), &node.name, secondary, cx)
                    }))
                    .overflow_y_scrollbar(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Label::new(format!("関係 {}", edges.len()))
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        Button::new("add-edge")
                            .label("＋ 追加")
                            .small()
                            .disabled(disabled || self.graph.nodes().is_empty())
                            .on_click(cx.listener(|editor, _, window, cx| {
                                editor.select(
                                    Some(Selection::Edge(Uuid::new_v4())),
                                    true,
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .id("edge-list")
                    .p_2()
                    .pt_0()
                    .min_h_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(edges.into_iter().map(|edge| {
                        let source = &self.graph.nodes()[&edge.source_id].name;
                        let target = &self.graph.nodes()[&edge.target_id].name;
                        self.list_row(
                            Selection::Edge(edge.id),
                            &edge.label,
                            format!("{source} → {target}"),
                            cx,
                        )
                    }))
                    .overflow_y_scrollbar(),
            )
    }

    fn list_row(
        &self,
        item: Selection,
        title: &str,
        secondary: String,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection() == Some(item);
        let (category, id) = match item {
            Selection::Node(id) => ("node", id),
            Selection::Edge(id) => ("edge", id),
        };
        let accent = cx.theme().accent;
        div()
            .id(SharedString::from(format!("{category}-{id}")))
            .test_support()
            .flex_shrink_0()
            .rounded_md()
            .px_2()
            .py_1()
            .when(!self.busy, |row| {
                row.cursor_pointer().hover(move |row| row.bg(accent))
            })
            .when(selected, |row| {
                row.bg(accent).text_color(cx.theme().accent_foreground)
            })
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_owned()),
            )
            .when(!secondary.is_empty(), |row| {
                row.child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(secondary),
                )
            })
            .on_click(cx.listener(move |editor, _, window, cx| {
                editor.select(Some(item), false, window, cx);
            }))
    }

    fn inspector(&self, cx: &Context<Self>) -> impl IntoElement {
        let pane = div()
            .w(px(284.))
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(cx.theme().border)
            .p_4()
            .gap_4();
        if let Some(form) = &self.form {
            let title = match (form.selection, form.exists(), self.graph.mode()) {
                (Selection::Node(_), true, ProjectMode::Ddd) => "カードの詳細",
                (Selection::Node(_), false, ProjectMode::Ddd) => "カードを追加",
                (Selection::Node(_), true, ProjectMode::Free) => "ノードの詳細",
                (Selection::Node(_), false, ProjectMode::Free) => "ノードを追加",
                (Selection::Edge(_), true, _) => "関係の詳細",
                (Selection::Edge(_), false, _) => "関係を追加",
            };
            let disabled = self.busy || !form.dirty(cx);
            pane.child(Label::new(title).font_weight(FontWeight::SEMIBOLD))
                .child(
                    div()
                        .id("inspector-form")
                        .min_h_0()
                        .flex_1()
                        .child(form.render(&self.graph, self.busy, cx))
                        .overflow_y_scrollbar(),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("apply")
                                .label("反映")
                                .primary()
                                .small()
                                .disabled(disabled)
                                .on_click(cx.listener(|editor, _, window, cx| {
                                    if !editor.busy
                                        && let Err(error) = editor.apply_form(window, cx)
                                    {
                                        editor.show_error(format!("{error:#}"), cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("revert")
                                .label("入力を取消")
                                .outline()
                                .small()
                                .disabled(disabled)
                                .on_click(cx.listener(|editor, _, window, cx| {
                                    editor.revert_form(window, cx)
                                })),
                        )
                        .when(form.exists(), |buttons| {
                            buttons.child(div().flex_1()).child(
                                Button::new("delete")
                                    .label("削除")
                                    .danger()
                                    .small()
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|editor, _, window, cx| {
                                        editor.delete_selection(window, cx)
                                    })),
                            )
                        }),
                )
        } else {
            pane.child(Label::new("詳細").font_weight(FontWeight::SEMIBOLD))
                .child(
                    Label::new("ノードまたは関係を選択してください。")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
        }
    }

    fn footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let status = if self.busy {
            "処理中…".to_owned()
        } else if self.has_changes(cx) {
            self.status.as_ref().map_or_else(
                || "未保存の変更あり".to_owned(),
                |status| format!("{status} · 未保存の変更あり"),
            )
        } else {
            self.status.clone().unwrap_or_else(|| {
                if self.project.is_some() {
                    "保存済み".to_owned()
                } else {
                    String::new()
                }
            })
        };
        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(cx.theme().border)
            .when_some(self.error.clone(), |footer, error| {
                footer.child(
                    div()
                        .id("error")
                        .max_h(px(128.))
                        .px_4()
                        .py_2()
                        .text_sm()
                        .bg(cx.theme().danger.opacity(0.10))
                        .text_color(cx.theme().danger)
                        .child(error)
                        .overflow_y_scrollbar(),
                )
            })
            .when(self.project.is_some() || self.busy, |footer| {
                footer.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_4()
                        .px_4()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status)
                        .child("空白をドラッグして移動 · スクロールで拡大・縮小"),
                )
            })
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.project.is_some() {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.sidebar(cx))
                .child(self.canvas(cx))
                .child(self.inspector(cx))
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .px_8()
                .bg(cx.theme().sidebar)
                .child(
                    Label::new("Light Bringer")
                        .text_3xl()
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    Label::new("プロジェクトを作成するか、保存済みのファイルを開いてください。")
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    Button::new("welcome-new")
                        .label("新規プロジェクト")
                        .primary()
                        .disabled(self.busy)
                        .on_click(cx.listener(|editor, _, window, cx| {
                            editor.command(Command::New, window, cx)
                        })),
                )
        };

        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|editor, _: &NewProject, window, cx| {
                editor.command(Command::New, window, cx)
            }))
            .on_action(cx.listener(|editor, _: &OpenProject, window, cx| {
                editor.command(Command::Open, window, cx)
            }))
            .on_action(cx.listener(|editor, _: &SaveProject, window, cx| {
                editor.command(Command::Save, window, cx)
            }))
            .on_action(cx.listener(|editor, _: &SaveProjectAs, window, cx| {
                editor.command(Command::SaveAs, window, cx)
            }))
            .on_action(cx.listener(|editor, _: &ImportJson, window, cx| {
                editor.command(Command::Import, window, cx)
            }))
            .on_action(cx.listener(|editor, _: &ExportJson, window, cx| {
                editor.command(Command::Export, window, cx)
            }))
            .when(self.project.is_some() && !self.busy, |root| {
                root.on_action(cx.listener(|editor, _: &CloseProject, window, cx| {
                    editor.command(Command::CloseProject, window, cx)
                }))
            })
            .on_action(
                cx.listener(|editor, _: &Quit, window, cx| {
                    editor.command(Command::Quit, window, cx)
                }),
            )
            .on_action(cx.listener(|editor, _: &CloseWindow, window, cx| {
                editor.command(Command::Close, window, cx)
            }))
            .child(self.toolbar(cx))
            .child(body)
            .child(self.footer(cx))
    }
}
