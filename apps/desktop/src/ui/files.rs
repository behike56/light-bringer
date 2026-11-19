use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result};
use gpui_kit::*;

use crate::{exchange, graph::Graph, storage::ProjectFile};

use super::Editor;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    New,
    Open,
    Import,
    Export,
    Save,
    Close,
    Quit,
}

impl Command {
    fn replaces_project(self) -> bool {
        matches!(
            self,
            Self::New | Self::Open | Self::Import | Self::Close | Self::Quit
        )
    }
}

enum Outcome {
    Cancelled,
    Saved,
    Exported,
    Opened(Arc<ProjectFile>, Graph),
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
        let has_changes = self.has_changes(cx);
        if self.busy
            || command == Command::Save && !has_changes
            || matches!(command, Command::Save | Command::Export) && self.project.is_none()
        {
            return;
        }

        let confirmation = (command.replaces_project() && has_changes).then(|| {
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

        self.busy = true;
        self.error = None;
        self.viewport.cancel_drag();
        let project = self.project.clone();
        cx.notify();
        cx.spawn_in(window, async move |editor, cx| {
            let result = async {
                if let Some(confirmation) = confirmation {
                    match confirmation.await {
                        Ok(0) => save(&editor, cx).await?,
                        Ok(1) => {}
                        _ => return Ok(Outcome::Cancelled),
                    }
                }
                run_command(command, project, &editor, cx).await
            }
            .await;

            cx.update(|window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        editor.busy = false;
                        match result {
                            Ok(Outcome::Cancelled) => {}
                            Ok(Outcome::Close) => window.remove_window(),
                            Ok(Outcome::Quit) => cx.quit(),
                            Ok(Outcome::Saved) => editor.status = Some("保存しました".to_owned()),
                            Ok(Outcome::Exported) => {
                                editor.status =
                                    Some("保存済みのグラフを JSON に書き出しました".to_owned());
                            }
                            Ok(Outcome::Opened(project, mut graph)) => {
                                editor.dirty = graph.nodes().len() != graph.positions().len();
                                graph.place_unpositioned();
                                editor.graph = graph;
                                editor.form = None;
                                editor.viewport.reset();
                                editor.status = None;
                                window.set_window_title(&format!(
                                    "{} — Light Bringer",
                                    project
                                        .path()
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                ));
                                editor.project = Some(project);
                                editor.focus.focus(window, cx);
                            }
                            Err(error) => editor.show_error(format!("{error:#}"), cx),
                        }
                        cx.notify();
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }
}

async fn save(editor: &WeakEntity<Editor>, cx: &mut AsyncWindowContext) -> Result<()> {
    let (project, graph) = cx.update(|window, cx| {
        editor.update(cx, |editor, cx| -> Result<_> {
            editor.apply_form(window, cx)?;
            let project = editor
                .project
                .clone()
                .context("プロジェクトを開いてください")?;
            Ok((project, editor.graph.clone()))
        })
    })???;
    cx.background_executor()
        .spawn(async move {
            project
                .save(&graph)
                .context("プロジェクトの保存に失敗しました")
        })
        .await?;
    editor.update(cx, |editor, cx| {
        editor.dirty = false;
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
            let Some(path) = choose_destination(&directory, "新規プロジェクト.sqlite", cx).await?
            else {
                return Ok(Outcome::Cancelled);
            };
            create_project(path, Graph::default(), cx).await
        }
        Command::Open => {
            let Some(path) = choose_file("プロジェクトを開く", cx).await? else {
                return Ok(Outcome::Cancelled);
            };
            cx.background_executor()
                .spawn(async move {
                    if let Some(project) = project
                        && project.matches_path(&path)
                    {
                        let graph = project.read().context("プロジェクトを読み直せません")?;
                        return Ok(Outcome::Opened(project, graph));
                    }
                    let (project, graph) =
                        ProjectFile::open(&path).context("プロジェクトを開けません")?;
                    Ok(Outcome::Opened(Arc::new(project), graph))
                })
                .await
        }
        Command::Import => {
            let Some(input) = choose_file("取り込む JSON を選択", cx).await? else {
                return Ok(Outcome::Cancelled);
            };
            let graph = cx
                .background_executor()
                .spawn(async move {
                    exchange::read(&input).context("JSON の取り込みに失敗しました")
                })
                .await?;
            let Some(path) =
                choose_destination(&directory, "取り込んだプロジェクト.sqlite", cx).await?
            else {
                return Ok(Outcome::Cancelled);
            };
            create_project(path, graph, cx).await
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
                return Ok(Outcome::Cancelled);
            };
            cx.background_executor()
                .spawn(async move {
                    let graph = project.read().context("保存済みのグラフを読み込めません")?;
                    exchange::write(&path, &graph).context("JSON の書き出しに失敗しました")?;
                    Ok(Outcome::Exported)
                })
                .await
        }
        Command::Save => {
            save(editor, cx).await?;
            Ok(Outcome::Saved)
        }
        Command::Close => Ok(Outcome::Close),
        Command::Quit => Ok(Outcome::Quit),
    }
}

async fn create_project(
    path: PathBuf,
    graph: Graph,
    cx: &mut AsyncWindowContext,
) -> Result<Outcome> {
    cx.background_executor()
        .spawn(async move {
            let project = ProjectFile::create(&path, &graph)?;
            Ok(Outcome::Opened(Arc::new(project), graph))
        })
        .await
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
    use std::time::Duration;

    use gpui_kit::{
        AppContext, Bounds, TestAppContext, WindowBounds, WindowOptions, point, px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    use crate::{exchange, ui::Editor};

    #[gpui_kit::test]
    async fn creates_exports_imports_and_reopens_projects_from_the_toolbar(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.sqlite");
        let export = directory.path().join("orders.json");
        let imported = directory.path().join("restored.sqlite");
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
                cx.new(|_| Editor::new(focus))
            })
            .unwrap()
        });
        let click = |cx: &mut TestAppContext, label: &'static str| {
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| window.click(label, cx))
                    .unwrap()
            });
            cx.run_until_parked();
        };
        let timeout = Duration::from_secs(4);

        click(cx, "新規");
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
    }
}
