use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result};
use gpui_kit::*;

use crate::{
    exchange,
    graph::{Graph, ProjectMode},
    project::{ProjectFile, has_extension},
};

use super::Editor;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    New,
    Open,
    Import,
    Export,
    Save,
    SaveAs,
    CloseProject,
    Close,
    Quit,
}

impl Command {
    fn leaves_project(self) -> bool {
        matches!(
            self,
            Self::New | Self::Open | Self::Import | Self::CloseProject | Self::Close | Self::Quit
        )
    }
}

enum Outcome {
    Stay,
    SavedAs(Arc<ProjectFile>, Graph),
    Exported,
    Opened(Arc<ProjectFile>, Graph),
    ProjectClosed,
    Close,
    Quit,
}

impl Editor {
    pub(super) fn command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || command == Command::Save && !self.can_save(cx)
            || matches!(
                command,
                Command::SaveAs | Command::Export | Command::CloseProject
            ) && self.project.is_none()
        {
            return;
        }

        self.busy = true;
        self.error = None;
        self.viewport.cancel_drag();
        let confirmation = (command.leaves_project() && self.has_changes(cx)).then(|| {
            window.prompt(
                PromptLevel::Warning,
                "未保存の変更があります",
                Some("プロジェクトへの変更を保存しますか？"),
                &[
                    PromptButton::Ok("保存".into()),
                    PromptButton::Other("破棄".into()),
                    PromptButton::Cancel("キャンセル".into()),
                ],
                cx,
            )
        });

        let project = self.project.clone();
        cx.notify();
        cx.spawn_in(window, async move |editor, cx| {
            let result = async {
                if let Some(confirmation) = confirmation {
                    match confirmation.await {
                        Ok(0) => save(&editor, cx).await?,
                        Ok(1) => {}
                        _ => return Ok(Outcome::Stay),
                    }
                }
                run_command(command, project, &editor, cx).await
            }
            .await;

            cx.update(|window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        match result {
                            Ok(Outcome::Stay) => {}
                            Ok(Outcome::ProjectClosed) => {
                                editor.project = None;
                                editor.graph = Graph::default();
                                editor.form = None;
                                editor.multi_selection.clear();
                                editor.viewport = Default::default();
                                editor.alignment_change = None;
                                editor.saved_positions.clear();
                                editor.positions_dirty = false;
                                editor.content_dirty = false;
                                editor.status = None;
                                editor.error = None;
                                window.set_window_title("Light Bringer");
                                editor.focus.focus(window, cx);
                            }
                            Ok(Outcome::Close) => window.remove_window(),
                            Ok(Outcome::Quit) => cx.quit(),
                            Ok(Outcome::SavedAs(project, graph)) => {
                                editor.mark_saved(&graph);
                                editor.status = Some("別名で保存しました".to_owned());
                                editor.set_project(project, window);
                            }
                            Ok(Outcome::Exported) => {
                                editor.status =
                                    Some("保存済みのグラフを JSON に書き出しました".to_owned());
                            }
                            Ok(Outcome::Opened(project, mut graph)) => {
                                editor.saved_positions.clone_from(graph.positions());
                                graph.place_unpositioned();
                                editor.graph = graph;
                                editor.refresh_positions_dirty();
                                editor.content_dirty = false;
                                editor.form = None;
                                editor.multi_selection.clear();
                                editor.alignment_change = None;
                                editor.viewport.reset();
                                editor.status = None;
                                editor.set_project(project, window);
                                editor.focus.focus(window, cx);
                            }
                            Err(error) => editor.show_error(format!("{error:#}"), cx),
                        }
                        editor.busy = false;
                        cx.notify();
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    fn set_project(&mut self, project: Arc<ProjectFile>, window: &mut Window) {
        let name = project
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        window.set_window_title(&format!("{name} — Light Bringer"));
        self.project = Some(project);
    }
}

fn capture_snapshot(
    editor: &WeakEntity<Editor>,
    cx: &mut AsyncWindowContext,
) -> Result<(Arc<ProjectFile>, Graph)> {
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| -> Result<_> {
            editor.apply_form(window, cx)?;
            let project = editor
                .project
                .clone()
                .context("プロジェクトを開いてください")?;
            Ok((project, editor.graph.clone()))
        })
    })??
}

async fn save(editor: &WeakEntity<Editor>, cx: &mut AsyncWindowContext) -> Result<()> {
    let (project, graph) = capture_snapshot(editor, cx)?;
    let saved_graph = cx
        .background_executor()
        .spawn(async move {
            project
                .save(&graph)
                .context("プロジェクトの保存に失敗しました")?;
            Ok::<_, anyhow::Error>(graph)
        })
        .await?;
    editor.update(cx, |editor, cx| {
        editor.mark_saved(&saved_graph);
        editor.status = Some("保存しました".to_owned());
        cx.notify();
    })?;
    Ok(())
}

async fn run_command(
    command: Command,
    project: Option<Arc<ProjectFile>>,
    editor: &WeakEntity<Editor>,
    cx: &mut AsyncWindowContext,
) -> Result<Outcome> {
    let directory = project
        .as_ref()
        .and_then(|project| project.path().parent().map(Path::to_path_buf))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    match command {
        Command::New => {
            let answer = cx.update(|window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    "プロジェクトのモードを選択",
                    None,
                    &[
                        PromptButton::Ok(ProjectMode::Free.label().into()),
                        PromptButton::Other(ProjectMode::Ddd.label().into()),
                        PromptButton::Cancel("キャンセル".into()),
                    ],
                    cx,
                )
            })?;
            let mode = match answer.await {
                Ok(0) => ProjectMode::Free,
                Ok(1) => ProjectMode::Ddd,
                _ => return Ok(Outcome::Stay),
            };
            let Some(path) =
                choose_project_destination(&directory, "新規プロジェクト.lbproj", cx).await?
            else {
                return Ok(Outcome::Stay);
            };
            let (project, graph) = create_project(path, Graph::new(mode), cx).await?;
            Ok(Outcome::Opened(project, graph))
        }
        Command::Open => {
            let Some(path) = choose_file("プロジェクトを開く（.lbproj / .sqlite）", cx).await?
            else {
                return Ok(Outcome::Stay);
            };
            cx.background_executor()
                .spawn(async move {
                    if let Some(project) = project
                        && project.matches_path(&path)
                    {
                        let graph = project.reload().context("プロジェクトを読み直せません")?;
                        return Ok(Outcome::Opened(project, graph));
                    }
                    let (project, graph) = ProjectFile::open(&path)
                        .with_context(|| format!("{} を開けません", path.display()))?;
                    Ok(Outcome::Opened(Arc::new(project), graph))
                })
                .await
        }
        Command::Import => {
            let Some(input) = choose_file("取り込む JSON を選択", cx).await? else {
                return Ok(Outcome::Stay);
            };
            let graph = cx
                .background_executor()
                .spawn(async move {
                    exchange::read(&input).context("JSON の取り込みに失敗しました")
                })
                .await?;
            let Some(path) =
                choose_project_destination(&directory, "取り込んだプロジェクト.lbproj", cx).await?
            else {
                return Ok(Outcome::Stay);
            };
            let (project, graph) = create_project(path, graph, cx).await?;
            Ok(Outcome::Opened(project, graph))
        }
        Command::Export => {
            let project = project.context("プロジェクトを開いてください")?;
            let name = project
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let Some(path) = choose_destination(&directory, &format!("{name}.json"), cx).await?
            else {
                return Ok(Outcome::Stay);
            };
            cx.background_executor()
                .spawn(async move {
                    let graph = project
                        .saved_graph()
                        .context("保存済みのグラフを読み込めません")?;
                    exchange::write(&path, &graph).context("JSON の書き出しに失敗しました")?;
                    Ok(Outcome::Exported)
                })
                .await
        }
        Command::Save => {
            save(editor, cx).await?;
            Ok(Outcome::Stay)
        }
        Command::SaveAs => {
            let project = project.context("プロジェクトを開いてください")?;
            let name = project
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let suggested = format!("{name} のコピー.lbproj");
            let Some(path) = choose_project_destination(&directory, &suggested, cx).await? else {
                return Ok(Outcome::Stay);
            };
            let (_, graph) = capture_snapshot(editor, cx)?;
            let (project, graph) = create_project(path, graph, cx).await?;
            Ok(Outcome::SavedAs(project, graph))
        }
        Command::CloseProject => Ok(Outcome::ProjectClosed),
        Command::Close => Ok(Outcome::Close),
        Command::Quit => Ok(Outcome::Quit),
    }
}

async fn create_project(
    path: PathBuf,
    graph: Graph,
    cx: &mut AsyncWindowContext,
) -> Result<(Arc<ProjectFile>, Graph)> {
    cx.background_executor()
        .spawn(async move {
            let project = ProjectFile::create(&path, &graph)?;
            Ok((Arc::new(project), graph))
        })
        .await
}

async fn choose_project_destination(
    directory: &Path,
    suggested: &str,
    cx: &mut AsyncWindowContext,
) -> Result<Option<PathBuf>> {
    Ok(choose_destination(directory, suggested, cx)
        .await?
        .map(|mut path| {
            if !has_extension(&path, "lbproj") {
                let mut name = path.as_os_str().to_owned();
                name.push(".lbproj");
                path = PathBuf::from(name);
            }
            path
        }))
}

async fn choose_file(title: &str, cx: &mut AsyncWindowContext) -> Result<Option<PathBuf>> {
    let receiver = cx.update(|_, cx| {
        cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(title.to_owned().into()),
        })
    })?;
    Ok(receiver
        .await
        .context("ファイル選択が中断されました")??
        .and_then(|paths| paths.into_iter().next()))
}

async fn choose_destination(
    directory: &Path,
    suggested: &str,
    cx: &mut AsyncWindowContext,
) -> Result<Option<PathBuf>> {
    let receiver = cx.update(|_, cx| cx.prompt_for_new_path(directory, Some(suggested)))?;
    receiver.await.context("保存先の選択が中断されました")?
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use gpui_kit::{
        AnyWindowHandle, AppContext, Bounds, Entity, TestAppContext, VisualTestContext,
        WindowBounds, WindowOptions, point, px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    use crate::{
        exchange,
        graph::Graph,
        project::ProjectFile,
        project_tests::legacy_fixture,
        ui::{CloseProject, Editor},
    };

    fn open_test_editor(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Editor>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1240.), px(780.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                cx.new(|cx| Editor::new(focus, cx))
            })
            .unwrap()
        })
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, label: &'static str) {
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| window.click(label, cx))
                .unwrap()
        });
        cx.run_until_parked();
    }

    fn close_project(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    assert!(window.is_action_available(&CloseProject, cx));
                    window.dispatch_action(Box::new(CloseProject), cx);
                })
                .unwrap()
        });
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    async fn creates_exports_imports_and_reopens_projects_from_the_toolbar(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.lbproj");
        let export = directory.path().join("orders.json");
        let imported = directory.path().join("restored.lbproj");
        let (window, editor) = open_test_editor(cx);
        let click = |cx: &mut TestAppContext, label| click(cx, window, label);
        let timeout = Duration::from_secs(4);

        click(cx, "新規");
        cx.simulate_prompt_answer(crate::graph::ProjectMode::Free.label());
        cx.run_until_parked();
        assert!(cx.did_prompt_for_new_path());
        cx.simulate_new_path_selection(|_| Some(path.clone()));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;

        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click("add-node", cx);
                    window.input("注文", cx);
                    window.click("保存", cx);
                })
                .unwrap()
        });
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        let saved = cx.update(|cx| editor.read(cx).graph.clone());
        assert_eq!(saved.nodes().values().next().unwrap().name, "注文");

        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click("add-node", cx);
                    window.input("保存前のノード", cx);
                })
                .unwrap()
        });

        click(cx, "JSON 書出");
        assert!(cx.did_prompt_for_new_path());
        cx.simulate_new_path_selection(|_| Some(export.clone()));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        assert_eq!(exchange::read(&export).unwrap(), saved);

        let broken = directory.path().join("broken.lbproj");
        fs::write(&broken, b"not a project").unwrap();
        click(cx, "開く");
        cx.simulate_prompt_answer("破棄");
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![broken]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(state.has_changes(cx));
            assert_eq!(state.graph, saved);
            assert_eq!(
                state.project.as_ref().unwrap().path(),
                path.canonicalize().unwrap()
            );
            assert!(state.error.as_ref().unwrap().contains("開けません"));
        });

        click(cx, "JSON 取込");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("キャンセル");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert!(editor.read(cx).has_changes(cx)));

        click(cx, "JSON 取込");
        cx.simulate_prompt_answer("破棄");
        cx.run_until_parked();
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| Some(vec![export.clone()]));
        for _ in 0..10 {
            cx.run_until_parked();
            if cx.did_prompt_for_new_path() {
                break;
            }
        }
        assert!(cx.did_prompt_for_new_path());
        cx.simulate_new_path_selection(|_| Some(imported.clone()));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(!state.has_changes(cx));
            assert_eq!(
                state.project.as_ref().unwrap().path(),
                imported.canonicalize().unwrap()
            );
            assert_eq!(state.graph, saved);
        });

        click(cx, "開く");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert_eq!(
                state.project.as_ref().unwrap().path(),
                path.canonicalize().unwrap()
            );
            assert_eq!(state.graph, saved);
        });

        let legacy = directory.path().join("legacy.sqlite");
        legacy_fixture(&legacy, &saved).unwrap();
        let legacy_before = fs::read(&legacy).unwrap();
        click(cx, "開く");
        cx.simulate_path_prompt_response(|_| Some(vec![legacy.clone()]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click("add-node", cx);
                    window.input("移行時の変更", cx);
                })
                .unwrap()
        });

        click(cx, "別名で保存");
        cx.simulate_new_path_selection(|_| Some(path.clone()));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(state.has_changes(cx));
            assert_eq!(
                state.project.as_ref().unwrap().path(),
                legacy.canonicalize().unwrap()
            );
            assert!(state.error.as_ref().unwrap().contains("同名ファイル"));
        });

        let migrated = directory.path().join("migrated.lbproj");
        click(cx, "別名で保存");
        cx.simulate_new_path_selection(|_| Some(migrated.with_extension("")));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let state = editor.read(cx);
            assert!(!state.has_changes(cx));
            let project = state.project.as_ref().unwrap();
            assert_eq!(project.path(), migrated.canonicalize().unwrap());
            assert_eq!(project.saved_graph().unwrap(), state.graph);
            assert_eq!(state.graph.nodes().len(), 2);
            assert!(
                state
                    .graph
                    .nodes()
                    .values()
                    .any(|node| node.name == "移行時の変更")
            );
        });
        assert_eq!(fs::read(&legacy).unwrap(), legacy_before);

        click(cx, "開く");
        cx.simulate_path_prompt_response(|_| Some(vec![legacy.clone()]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(!editor.has_changes(cx));
            assert!(editor.can_save(cx));
        });

        close_project(cx, window);
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        assert!(!cx.has_pending_prompt());
        cx.update(|cx| assert!(editor.read(cx).project.is_none()));
        assert_eq!(fs::read(&legacy).unwrap(), legacy_before);
        assert!(ProjectFile::open(&legacy).is_ok());

        click(cx, "開く");
        cx.simulate_path_prompt_response(|_| Some(vec![legacy.clone()]));
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        click(cx, "保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert!(!editor.read(cx).can_save(cx)));
        assert_eq!(crate::storage::read_snapshot(&legacy).unwrap(), (2, saved));
    }

    #[gpui_kit::test]
    async fn closes_project_after_confirmation_and_releases_edit_lock(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.lbproj");
        ProjectFile::create(&path, &Graph::default()).unwrap();
        let (window, editor) = open_test_editor(cx);
        let timeout = Duration::from_secs(4);
        let open = |cx: &mut TestAppContext| {
            click(cx, window, "開く");
            cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        };
        let assert_close_disabled = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| {
                        assert!(!window.is_action_available(&CloseProject, cx));
                        window.dispatch_action(Box::new(CloseProject), cx);
                    })
                    .unwrap()
            });
        };

        cx.run_until_parked();
        assert_close_disabled(cx);
        open(cx);
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        click(cx, window, "add-node");
        let selection = cx.update(|cx| editor.read(cx).selection());
        assert!(selection.is_some());

        close_project(cx, window);
        assert!(cx.has_pending_prompt());
        assert_close_disabled(cx);
        assert!(ProjectFile::open(&path).is_err());
        cx.simulate_prompt_answer("キャンセル");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        assert!(!cx.has_pending_prompt());
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(editor.has_changes(cx));
            assert!(editor.selection() == selection);
        });
        assert!(ProjectFile::open(&path).is_err());

        close_project(cx, window);
        cx.simulate_prompt_answer("保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(editor.error.is_some());
            assert!(editor.selection() == selection);
            assert!(editor.has_changes(cx));
        });
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| window.input("注文", cx))
                .unwrap()
        });

        close_project(cx, window);
        cx.simulate_prompt_answer("保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        assert_eq!(
            VisualTestContext::from_window(window, cx)
                .window_title()
                .as_deref(),
            Some("Light Bringer")
        );
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(editor.project.is_none());
            assert!(editor.form.is_none());
            assert!(!editor.has_changes(cx));
            assert!(editor.graph.nodes().is_empty());
            assert!(editor.status.is_none());
            assert!(editor.error.is_none());
            let focus = editor.focus.clone();
            window
                .update(cx, |_, window, cx| {
                    assert!(focus.is_focused(window));
                    assert!(!window.is_action_available(&CloseProject, cx));
                })
                .unwrap();
        });
        let (_, saved) = ProjectFile::open(&path).unwrap();
        assert_eq!(saved.nodes().values().next().unwrap().name, "注文");

        open(cx);
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert_eq!(editor.read(cx).graph, saved));
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    window.click("add-node", cx);
                    window.input("未保存のノード", cx);
                })
                .unwrap()
        });
        let saved_bytes = fs::read(&path).unwrap();
        fs::rename(&path, directory.path().join("original.lbproj")).unwrap();
        fs::write(&path, &saved_bytes).unwrap();

        close_project(cx, window);
        cx.simulate_prompt_answer("保存");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| {
            let editor = editor.read(cx);
            assert!(editor.error.as_ref().unwrap().contains("外部変更"));
            assert!(editor.has_changes(cx));
            assert_eq!(editor.graph.nodes().len(), 2);
        });
        assert!(ProjectFile::open(&path).is_err());

        close_project(cx, window);
        cx.simulate_prompt_answer("破棄");
        cx.wait_for(window, timeout, |_, cx| !editor.read(cx).busy)
            .await;
        cx.update(|cx| assert!(editor.read(cx).project.is_none()));
        assert_eq!(fs::read(&path).unwrap(), saved_bytes);
        assert_eq!(ProjectFile::open(&path).unwrap().1, saved);
    }
}
