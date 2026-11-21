mod archive;

use std::{
    fs::{DirBuilder, File, Metadata, OpenOptions, TryLockError},
    io::{self, Read, Seek},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

use crate::{graph::Graph, storage};

pub enum ProjectFile {
    Container(Container),
    Sqlite(storage::SqliteProject),
}

impl ProjectFile {
    pub fn create(path: &Path, graph: &Graph) -> Result<Self> {
        Ok(Self::Container(Container::create(path, graph)?))
    }

    pub fn open(path: &Path) -> Result<(Self, Graph)> {
        let path = path
            .canonicalize()
            .context("プロジェクトのファイルが見つかりません")?;
        if has_extension(&path, "lbproj") {
            let (container, graph) = Container::open(path)?;
            Ok((Self::Container(container), graph))
        } else if has_extension(&path, "sqlite") {
            let (project, graph) = storage::SqliteProject::open(&path)?;
            Ok((Self::Sqlite(project), graph))
        } else {
            bail!(".lbproj または .sqlite のプロジェクトを選んでください")
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Container(project) => &project.path,
            Self::Sqlite(project) => project.path(),
        }
    }

    pub fn matches_path(&self, path: &Path) -> bool {
        match self {
            Self::Container(project) => project.matches_path(path),
            Self::Sqlite(project) => project.matches_path(path),
        }
    }

    pub fn reload(&self) -> Result<Graph> {
        match self {
            Self::Container(project) => project.reload(),
            Self::Sqlite(project) => project.reload(),
        }
    }

    pub fn needs_upgrade(&self) -> bool {
        match self {
            Self::Container(project) => project.state().is_ok_and(|state| state.needs_upgrade),
            Self::Sqlite(project) => project.needs_upgrade(),
        }
    }

    pub fn saved_graph(&self) -> Result<Graph> {
        match self {
            Self::Container(project) => Ok(project.state()?.saved_graph.clone()),
            Self::Sqlite(project) => project.read(),
        }
    }

    pub fn save(&self, graph: &Graph) -> Result<()> {
        match self {
            Self::Container(project) => project.save(graph, File::sync_all),
            Self::Sqlite(project) => project.save(graph),
        }
    }
}

pub fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

pub fn read_container_snapshot(path: &Path) -> Result<(u32, Graph)> {
    let path = path
        .canonicalize()
        .context("プロジェクトのファイルが見つかりません")?;
    let (_, version, graph) = read_project(&path)?;
    Ok((version, graph))
}

pub struct Container {
    path: PathBuf,
    _session_lock: File,
    state: Mutex<State>,
}

struct State {
    baseline: Baseline,
    saved_graph: Graph,
    needs_upgrade: bool,
}

struct Baseline {
    // Keep the inode alive for as long as its identity is used to detect replacement.
    _file: File,
    fingerprint: Fingerprint,
}

#[derive(PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    sha256: [u8; 32],
}

impl Container {
    fn create(path: &Path, graph: &Graph) -> Result<Self> {
        ensure!(
            has_extension(path, "lbproj"),
            "保存先の拡張子は .lbproj を指定してください"
        );
        let parent = path.parent().context("保存先のフォルダがありません")?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let parent = parent
            .canonicalize()
            .context("保存先のフォルダを開けません")?;
        let path = parent.join(path.file_name().context("ファイル名がありません")?);

        let directory = File::open(&parent).context("保存先のフォルダを開けません")?;
        ensure_new_path(&path)?;
        let lock = lock_path(&path)?;
        let mut temporary = archive::prepare(&parent, graph)?;
        let (fingerprint, _) = fingerprint(temporary.as_file_mut())?;
        let file = match temporary.persist_noclobber(&path) {
            Ok(file) => file,
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                bail!("保存先に同名ファイルがあります。別のファイル名を指定してください")
            }
            Err(error) => {
                return Err(error.error).context("新しいプロジェクトファイルを確定できません");
            }
        };
        directory.sync_all().with_context(|| {
            format!(
                "{} を作成しましたが、保存内容の永続化を確認できませんでした",
                path.display()
            )
        })?;

        Ok(Self {
            path,
            _session_lock: lock,
            state: Mutex::new(State {
                baseline: Baseline {
                    _file: file,
                    fingerprint,
                },
                saved_graph: graph.clone(),
                needs_upgrade: false,
            }),
        })
    }

    fn open(path: PathBuf) -> Result<(Self, Graph)> {
        let lock = lock_path(&path)?;
        let (baseline, version, graph) = read_project(&path)?;
        let project = Self {
            path,
            _session_lock: lock,
            state: Mutex::new(State {
                baseline,
                saved_graph: graph.clone(),
                needs_upgrade: version == 1,
            }),
        };
        Ok((project, graph))
    }

    fn matches_path(&self, candidate: &Path) -> bool {
        if candidate.canonicalize().is_ok_and(|path| path == self.path) {
            return true;
        }
        let (Ok(current), Ok(candidate)) = (self.path.metadata(), candidate.metadata()) else {
            return false;
        };
        (current.dev(), current.ino()) == (candidate.dev(), candidate.ino())
    }

    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("プロジェクトの保存状態を参照できません"))
    }

    fn reload(&self) -> Result<Graph> {
        let mut state = self.state()?;
        let (baseline, version, graph) = read_project(&self.path)?;
        *state = State {
            baseline,
            saved_graph: graph.clone(),
            needs_upgrade: version == 1,
        };
        Ok(graph)
    }

    fn save(
        &self,
        graph: &Graph,
        sync_directory: impl FnOnce(&File) -> io::Result<()>,
    ) -> Result<()> {
        let mut state = self.state()?;
        let parent = self.path.parent().context("保存先のフォルダがありません")?;
        let directory = File::open(parent).context("保存先のフォルダを開けません")?;
        let mut temporary = archive::prepare(parent, graph)?;
        let (replacement, _) = fingerprint(temporary.as_file_mut())?;

        let verify = || -> Result<()> {
            let mut file = open_regular(&self.path)?;
            let (current, metadata) = fingerprint(&mut file)?;
            ensure!(
                current == state.baseline.fingerprint,
                "ファイルの識別情報または内容が変わりました"
            );
            check_path(&self.path, &metadata)
        };
        verify()
            .context("開いているプロジェクトの外部変更を検出しました。別名で保存してください")?;

        let file = temporary
            .persist(&self.path)
            .context("プロジェクトファイルを置き換えられません")?;
        state.baseline = Baseline {
            _file: file,
            fingerprint: replacement,
        };
        sync_directory(&directory).context(
            "プロジェクトファイルを置き換えましたが、保存内容の永続化を確認できませんでした。保存を再試行してください",
        )?;
        state.saved_graph = graph.clone();
        state.needs_upgrade = false;
        Ok(())
    }
}

fn ensure_new_path(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(_) => bail!("保存先に同名ファイルがあります。別のファイル名を指定してください"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("保存先のファイルを確認できません"),
    }
}

fn lock_path(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .context("プロジェクトのフォルダがありません")?;
    let directory = parent.join(".light-bringer-locks");
    match DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("セッションロックのフォルダを作成できません"),
    }
    ensure!(
        directory.symlink_metadata()?.is_dir(),
        "セッションロックの保存先がフォルダではありません"
    );
    // A sibling directory uses the same volume's name equivalence as the destination.
    // Keep the lock file in place so later sessions always lock the same inode.
    let name = path.file_name().context("ファイル名がありません")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(name))
        .context("セッションロックファイルを開けません")?;
    ensure!(
        lock.metadata()?.is_file(),
        "セッションロックが通常ファイルではありません"
    );
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => bail!("このプロジェクトは別のセッションで編集中です"),
        Err(TryLockError::Error(error)) => Err(error).context("プロジェクトをロックできません"),
    }
}

fn read_project(path: &Path) -> Result<(Baseline, u32, Graph)> {
    let mut file = open_regular(path)?;
    let (fingerprint, metadata) = fingerprint(&mut file)?;
    let (version, graph) = archive::read(&mut file)?;
    ensure!(
        same_revision(&metadata, &file.metadata()?),
        "読み込み中にファイルが変更されました"
    );
    check_path(path, &metadata)?;
    Ok((
        Baseline {
            _file: file,
            fingerprint,
        },
        version,
        graph,
    ))
}

fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .context("プロジェクトファイルを開けません")?;
    check_metadata(&file.metadata()?)?;
    Ok(file)
}

fn check_metadata(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_file(),
        "プロジェクトは通常ファイルである必要があります"
    );
    ensure!(
        metadata.nlink() == 1,
        "ハードリンクのある .lbproj は編集できません"
    );
    ensure!(
        metadata.len() <= archive::MAX_ARCHIVE,
        ".lbproj がファイルサイズの上限を超えています"
    );
    Ok(())
}

fn fingerprint(file: &mut File) -> Result<(Fingerprint, Metadata)> {
    let before = file.metadata()?;
    check_metadata(&before)?;
    file.rewind()?;
    let mut hash = Sha256::new();
    let limit = archive::MAX_ARCHIVE + 1;
    let mut input = file.take(limit);
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let bytes = limit - input.limit();
    let after = file.metadata()?;
    check_metadata(&after)?;
    ensure!(
        bytes == before.len() && same_revision(&before, &after),
        "読み込み中にファイルが変更されました"
    );
    file.rewind()?;
    let fingerprint = Fingerprint {
        device: before.dev(),
        inode: before.ino(),
        sha256: hash.finalize().into(),
    };
    Ok((fingerprint, before))
}

fn check_path(path: &Path, expected: &Metadata) -> Result<()> {
    let current = path
        .symlink_metadata()
        .context("プロジェクトの保存先を参照できません")?;
    check_metadata(&current)?;
    ensure!(
        same_revision(expected, &current),
        "プロジェクトファイルが変更されました"
    );
    Ok(())
}

fn same_revision(a: &Metadata, b: &Metadata) -> bool {
    (
        a.dev(),
        a.ino(),
        a.len(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) == (
        b.dev(),
        b.ino(),
        b.len(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    )
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::graph::Node;

    #[test]
    fn save_can_retry_after_directory_sync_fails_following_replacement() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("retry.lbproj");
        let mut graph = Graph::default();
        let project = Container::create(&path, &graph)?;
        graph.put_node(Node {
            id: Uuid::new_v4(),
            name: "注文".to_owned(),
            kind: None,
            description: None,
            source_reference: None,
            event_storming_kind: None,
        })?;

        let error = project
            .save(&graph, |_| Err(io::Error::other("simulated fsync failure")))
            .unwrap_err();
        assert!(format!("{error:#}").contains("永続化を確認できません"));
        assert_eq!(project.state()?.saved_graph, Graph::default());
        let (_, _, replaced_graph) = read_project(&path)?;
        assert_eq!(replaced_graph, graph);
        project.save(&graph, File::sync_all)?;
        assert_eq!(project.state()?.saved_graph, graph);
        Ok(())
    }
}
