use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, Row, Transaction, TransactionBehavior, params};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::graph::{Edge, Graph, Node, Position};

const SCHEMA_VERSION: i32 = 1;
const APPLICATION_ID: i32 = i32::from_be_bytes(*b"LBGR");

const SCHEMA: &str = "
    CREATE TABLE nodes (
        id TEXT PRIMARY KEY NOT NULL,
        name TEXT NOT NULL,
        kind TEXT,
        description TEXT,
        source_reference TEXT
    ) STRICT;
    CREATE TABLE edges (
        id TEXT PRIMARY KEY NOT NULL,
        source_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        target_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        label TEXT NOT NULL,
        note TEXT,
        source_reference TEXT
    ) STRICT;
    CREATE INDEX edges_source_idx ON edges(source_id);
    CREATE INDEX edges_target_idx ON edges(target_id);
    CREATE TABLE node_positions (
        node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        x REAL NOT NULL,
        y REAL NOT NULL
    ) STRICT;
";

pub struct ProjectFile {
    path: PathBuf,
    file: File,
    _session_lock: File,
}

impl ProjectFile {
    pub fn create(path: &Path, graph: &Graph) -> Result<Self> {
        let parent = path.parent().context("保存先のフォルダがありません")?;
        let parent = parent
            .canonicalize()
            .context("保存先のフォルダを開けません")?;
        let path = parent.join(path.file_name().context("ファイル名がありません")?);
        let temp = NamedTempFile::new_in(parent).context("プロジェクトを作成できません")?;
        let session_lock = lock_session(temp.as_file())?;
        {
            let mut connection = connect(temp.path())?;
            let transaction = connection.transaction()?;
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            insert_graph(&transaction, graph)?;
            transaction.commit()?;
        }
        let file = temp.persist_noclobber(&path).context(
            "プロジェクトを作成できません。保存先に同名ファイルがないか確認してください",
        )?;
        Ok(Self {
            path,
            file,
            _session_lock: session_lock,
        })
    }

    pub fn open(path: &Path) -> Result<(Self, Graph)> {
        let path = path
            .canonicalize()
            .context("プロジェクトのファイルが見つかりません")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .context("プロジェクトを編集用に開けません")?;
        let session_lock = lock_session(&file)?;
        let project = Self {
            path,
            file,
            _session_lock: session_lock,
        };
        let graph = project.read()?;
        Ok((project, graph))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn matches_path(&self, path: &Path) -> bool {
        let (Ok(candidate), Ok(locked)) = (path.metadata(), self.file.metadata()) else {
            return false;
        };
        (candidate.dev(), candidate.ino()) == (locked.dev(), locked.ino())
    }

    pub fn read(&self) -> Result<Graph> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        check_version(&transaction)?;
        let nodes = read_rows(
            &transaction,
            "SELECT id, name, kind, description, source_reference FROM nodes",
            |row| {
                Ok(Node {
                    id: id_column(row, 0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    description: row.get(3)?,
                    source_reference: row.get(4)?,
                })
            },
        )?;
        let edges = read_rows(
            &transaction,
            "SELECT id, source_id, target_id, label, note, source_reference FROM edges",
            |row| {
                Ok(Edge {
                    id: id_column(row, 0)?,
                    source_id: id_column(row, 1)?,
                    target_id: id_column(row, 2)?,
                    label: row.get(3)?,
                    note: row.get(4)?,
                    source_reference: row.get(5)?,
                })
            },
        )?;
        let positions = read_rows(
            &transaction,
            "SELECT node_id, x, y FROM node_positions",
            |row| {
                Ok(Position {
                    node_id: id_column(row, 0)?,
                    x: row.get(1)?,
                    y: row.get(2)?,
                })
            },
        )?;
        Graph::from_records(nodes, edges, positions).context("保存済みグラフの検証に失敗しました")
    }

    pub fn save(&self, graph: &Graph) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_version(&transaction)?;
        // Foreign-key cascades remove the old edges and positions in the same transaction.
        transaction.execute("DELETE FROM nodes", [])?;
        insert_graph(&transaction, graph)?;
        transaction.commit()?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection> {
        let current = self
            .path
            .metadata()
            .context("プロジェクトの保存先を参照できません")?;
        let locked = self.file.metadata()?;
        ensure!(
            (current.dev(), current.ino()) == (locked.dev(), locked.ino()),
            "開いているプロジェクトのファイルが置き換えられました"
        );
        connect(&self.path)
    }
}

fn lock_session(file: &File) -> Result<File> {
    let id = file.metadata()?;
    // SQLite also locks the database inode on macOS. Use a separate lock file
    // keyed by the database identity so that aliases share the same session lock.
    let directory = session_directory()?.join("light-bringer-locks");
    fs::create_dir_all(&directory).context("セッションロックのフォルダを作成できません")?;
    let path = directory.join(format!("{}-{}.lock", id.dev(), id.ino()));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .context("セッションロックファイルを開けません")?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => bail!("このプロジェクトは別のセッションで編集中です"),
        Err(TryLockError::Error(error)) => Err(error).context("プロジェクトをロックできません"),
    }
}

#[cfg(target_os = "macos")]
fn session_directory() -> Result<PathBuf> {
    use std::{
        ffi::{CStr, OsStr},
        os::unix::ffi::OsStrExt,
    };

    let mut buffer = [0; libc::PATH_MAX as usize];
    // Use the OS-provided per-user directory so Finder and terminal launches
    // agree on the lock namespace even when TMPDIR has been overridden.
    // SAFETY: The buffer is writable and confstr receives its exact capacity.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    ensure!(
        length > 1 && length <= buffer.len(),
        "macOS のユーザー用一時フォルダを取得できません"
    );
    let value = CStr::from_bytes_with_nul(&buffer[..length])?;
    Ok(PathBuf::from(OsStr::from_bytes(value.to_bytes())))
}

#[cfg(not(target_os = "macos"))]
fn session_directory() -> Result<PathBuf> {
    Ok(std::env::temp_dir())
}

fn connect(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .context("SQLite を開けません")?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(connection)
}

fn check_version(connection: &Connection) -> Result<()> {
    let id: i32 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        id == APPLICATION_ID,
        "Light Bringer のプロジェクトファイルを選んでください"
    );
    ensure!(
        version == SCHEMA_VERSION,
        "未対応のプロジェクトバージョンです（{version}）"
    );
    Ok(())
}

fn insert_graph(transaction: &Transaction<'_>, graph: &Graph) -> Result<()> {
    let mut insert_node = transaction.prepare("INSERT INTO nodes VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for node in graph.nodes().values() {
        insert_node.execute(params![
            node.id.to_string(),
            node.name,
            node.kind,
            node.description,
            node.source_reference
        ])?;
    }
    let mut insert_edge =
        transaction.prepare("INSERT INTO edges VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for edge in graph.edges().values() {
        insert_edge.execute(params![
            edge.id.to_string(),
            edge.source_id.to_string(),
            edge.target_id.to_string(),
            edge.label,
            edge.note,
            edge.source_reference
        ])?;
    }
    let mut insert_position =
        transaction.prepare("INSERT INTO node_positions VALUES (?1, ?2, ?3)")?;
    for position in graph.positions().values() {
        insert_position.execute(params![
            position.node_id.to_string(),
            position.x,
            position.y
        ])?;
    }
    Ok(())
}

fn read_rows<T>(
    connection: &Connection,
    query: &str,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(query)?;
    let rows = statement
        .query_map([], map)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn id_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value = row.get::<_, String>(index)?;
    Uuid::parse_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
