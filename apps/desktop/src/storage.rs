use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, Row, Transaction, TransactionBehavior, params};
use serde::de::{DeserializeOwned, value::StringDeserializer};
#[cfg(test)]
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::graph::{CardKind, DddRelation, Edge, Graph, Node, Position, ProjectMode};

const SCHEMA_VERSION: u32 = 2;
const APPLICATION_ID: i32 = i32::from_be_bytes(*b"LBGR");

const SCHEMA_V1: &str = "
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

const MIGRATE_TO_V2: &str = "
    ALTER TABLE nodes ADD COLUMN event_storming_kind TEXT;
    ALTER TABLE edges ADD COLUMN event_storming_relation TEXT;
    CREATE TABLE project_settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        mode TEXT NOT NULL
    ) STRICT;
";

pub struct SqliteProject {
    path: PathBuf,
    file: File,
    _session_lock: File,
    needs_upgrade: AtomicBool,
}

impl SqliteProject {
    #[cfg(test)]
    pub fn create(path: &Path, graph: &Graph) -> Result<Self> {
        let parent = path.parent().context("保存先のフォルダがありません")?;
        let parent = parent
            .canonicalize()
            .context("保存先のフォルダを開けません")?;
        let path = parent.join(path.file_name().context("ファイル名がありません")?);
        let temp = NamedTempFile::new_in(parent).context("プロジェクトを作成できません")?;
        let session_lock = lock_session(temp.as_file())?;
        initialize(connect(temp.path())?, graph)?;
        let file = temp.persist_noclobber(&path).context(
            "プロジェクトを作成できません。保存先に同名ファイルがないか確認してください",
        )?;
        Ok(Self {
            path,
            file,
            _session_lock: session_lock,
            needs_upgrade: AtomicBool::new(false),
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
            needs_upgrade: AtomicBool::new(false),
        };
        let graph = project.reload()?;
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
        self.read_versioned().map(|(_, graph)| graph)
    }

    pub fn reload(&self) -> Result<Graph> {
        let (version, graph) = self.read_versioned()?;
        self.needs_upgrade.store(version == 1, Ordering::Relaxed);
        Ok(graph)
    }

    pub fn needs_upgrade(&self) -> bool {
        self.needs_upgrade.load(Ordering::Relaxed)
    }

    fn read_versioned(&self) -> Result<(u32, Graph)> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let version = check_version(&transaction)?;
        Ok((version, read_graph(&transaction, version)?))
    }

    pub fn save(&self, graph: &Graph) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if check_version(&transaction)? == 1 {
            migrate_to_v2(&transaction)?;
        }
        // Foreign-key cascades remove the old edges and positions in the same transaction.
        transaction.execute("DELETE FROM nodes", [])?;
        insert_graph(&transaction, graph)?;
        transaction.commit()?;
        self.needs_upgrade.store(false, Ordering::Relaxed);
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

pub fn write_snapshot(path: &Path, graph: &Graph) -> Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    connection.pragma_update(None, "foreign_keys", true)?;
    initialize(connection, graph)
}

pub fn read_snapshot(path: &Path) -> Result<(u32, Graph)> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    let transaction = connection.transaction()?;
    let version = check_version(&transaction)?;
    let check: String = transaction.pragma_query_value(None, "quick_check", |row| row.get(0))?;
    ensure!(
        check == "ok",
        "SQLite の整合性検査に失敗しました（{check}）"
    );
    let tables = ["nodes", "edges", "node_positions", "project_settings"];
    for table in &tables[..if version == 1 { 3 } else { 4 }] {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_list
                           WHERE schema = 'main' AND name = ?1 AND type = 'table' AND strict = 1)",
            [table],
            |row| row.get(0),
        )?;
        ensure!(exists, "SQLite にテーブル {table} がありません");
    }
    Ok((version, read_graph(&transaction, version)?))
}

fn initialize(mut connection: Connection, graph: &Graph) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA_V1)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    migrate_to_v2(&transaction)?;
    insert_graph(&transaction, graph)?;
    transaction.commit()?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn migrate_to_v2(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(MIGRATE_TO_V2)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn read_graph(connection: &Connection, version: u32) -> Result<Graph> {
    let (mode, kind, relation) = if version == 1 {
        (ProjectMode::Free, "NULL", "NULL")
    } else {
        let settings = read_rows(connection, "SELECT id, mode FROM project_settings", |row| {
            Ok((row.get::<_, i64>(0)?, enum_column(row, 1)?))
        })
        .context("project_settings の読み込みに失敗しました")?;
        let [(1, Some(mode))] = settings.as_slice() else {
            bail!("project_settings には mode を指定した設定が1件必要です");
        };
        (*mode, "event_storming_kind", "event_storming_relation")
    };
    let nodes = read_rows(
        connection,
        &format!("SELECT id, name, kind, description, source_reference, {kind} FROM nodes"),
        |row| {
            Ok(Node {
                id: id_column(row, 0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                description: row.get(3)?,
                source_reference: row.get(4)?,
                event_storming_kind: enum_column(row, 5)?,
            })
        },
    )?;
    let edges = read_rows(
        connection,
        &format!(
            "SELECT id, source_id, target_id, label, note, source_reference, {relation} FROM edges"
        ),
        |row| {
            Ok(Edge {
                id: id_column(row, 0)?,
                source_id: id_column(row, 1)?,
                target_id: id_column(row, 2)?,
                label: row.get(3)?,
                note: row.get(4)?,
                source_reference: row.get(5)?,
                event_storming_relation: enum_column(row, 6)?,
            })
        },
    )?;
    let positions = read_rows(
        connection,
        "SELECT node_id, x, y FROM node_positions",
        |row| {
            Ok(Position {
                node_id: id_column(row, 0)?,
                x: row.get(1)?,
                y: row.get(2)?,
            })
        },
    )?;
    Graph::from_records(mode, nodes, edges, positions).context("保存済みグラフの検証に失敗しました")
}

fn check_version(connection: &Connection) -> Result<u32> {
    let id: i32 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        id == APPLICATION_ID,
        "Light Bringer のプロジェクトファイルを選んでください"
    );
    ensure!(
        version == 1 || version == i64::from(SCHEMA_VERSION),
        "未対応のプロジェクトバージョンです（{version}）"
    );
    Ok(version as u32)
}

fn insert_graph(transaction: &Transaction<'_>, graph: &Graph) -> Result<()> {
    transaction.execute(
        "INSERT INTO project_settings (id, mode) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET mode = excluded.mode",
        [graph.mode().key()],
    )?;
    let mut insert_node =
        transaction.prepare("INSERT INTO nodes VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for node in graph.nodes().values() {
        insert_node.execute(params![
            node.id.to_string(),
            node.name,
            node.kind,
            node.description,
            node.source_reference,
            node.event_storming_kind.map(CardKind::key)
        ])?;
    }
    let mut insert_edge =
        transaction.prepare("INSERT INTO edges VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
    for edge in graph.edges().values() {
        insert_edge.execute(params![
            edge.id.to_string(),
            edge.source_id.to_string(),
            edge.target_id.to_string(),
            edge.label,
            edge.note,
            edge.source_reference,
            edge.event_storming_relation.map(DddRelation::key)
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

fn enum_column<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            T::deserialize(StringDeserializer::<serde::de::value::Error>::new(value)).map_err(
                |error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        index,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                },
            )
        })
        .transpose()
}
