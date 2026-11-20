use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tempfile::{NamedTempFile, tempdir};
use zip::{
    CompressionMethod, ZipArchive, ZipWriter,
    read::{ArchiveOffset, Config},
    unstable::read::ZipIterable,
    write::SimpleFileOptions,
};

use crate::{exchange, graph::Graph, storage};

const FORMAT: &str = "light-bringer-project";
const VERSION: u32 = 1;
const MANIFEST: &str = "manifest.json";
const DATABASE: &str = "project.sqlite";
const GRAPH: &str = "graph.json";
const MIB: u64 = 1024 * 1024;
const ENTRIES: [(&str, u64); 3] = [(MANIFEST, 4096), (DATABASE, 128 * MIB), (GRAPH, 64 * MIB)];
const MAX_EXPANDED: u64 = 160 * MIB;
pub(super) const MAX_ARCHIVE: u64 = MAX_EXPANDED + 2 * MIB;

#[derive(Deserialize, Serialize)]
struct Manifest {
    format: String,
    container_version: u32,
}

pub(super) fn read(file: &mut File) -> Result<(u32, Graph)> {
    let mut reader = BufReader::new(file);
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    // Check the declared count before allocating an index for the central directory.
    let entry_count = ZipIterable::try_new(&mut reader, config)
        .context("ZIP を読み込めません")?
        .len();
    ensure!(
        entry_count == ENTRIES.len(),
        "ZIP の収録ファイルは3件必要です"
    );

    let mut zip = ZipArchive::with_config(config, reader).context("ZIP を読み込めません")?;
    ensure!(
        !zip.has_duplicate_names(),
        "ZIP に重複したファイル名があります"
    );
    ensure!(
        zip.len() == ENTRIES.len(),
        "ZIP の収録ファイルは3件必要です"
    );
    let mut declared_bytes = 0;
    for (name, limit) in ENTRIES {
        let index = zip
            .index_for_name_raw(name.as_bytes())
            .with_context(|| format!("ZIP に {name} がありません"))?;
        let entry = zip.by_index_data(index)?;
        ensure!(
            entry.name()? == name
                && entry.is_file()
                && entry
                    .unix_mode()
                    .is_none_or(|mode| matches!(mode & 0o170000, 0 | 0o100000)),
            "{name} は通常ファイルである必要があります"
        );
        ensure!(
            entry.size() <= limit,
            "{name} が展開サイズの上限を超えています"
        );
        declared_bytes += entry.size();
    }
    ensure!(
        declared_bytes <= MAX_EXPANDED,
        "ZIP 全体の展開サイズの上限を超えています"
    );
    zip.validate_header_consistency()
        .context("ZIP のヘッダーが不正です")?;

    let mut expanded_bytes = 0;
    let mut manifest = Vec::new();
    extract(
        &mut zip,
        MANIFEST,
        ENTRIES[0].1,
        &mut expanded_bytes,
        &mut manifest,
    )?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest).context("manifest.json を読み込めません")?;
    ensure!(
        manifest.format == FORMAT,
        "manifest.json の format が未対応です"
    );
    ensure!(
        manifest.container_version == VERSION,
        "manifest.json の container_version {} は未対応です",
        manifest.container_version
    );

    let directory = tempdir().context("展開用の一時フォルダを作成できません")?;
    for (name, limit) in &ENTRIES[1..] {
        let mut output = BufWriter::new(
            File::create(directory.path().join(name))
                .with_context(|| format!("{name} の一時ファイルを作成できません"))?,
        );
        extract(&mut zip, name, *limit, &mut expanded_bytes, &mut output)?;
        output
            .flush()
            .with_context(|| format!("{name} を書き込めません"))?;
    }

    let (sqlite_version, sqlite) = storage::read_snapshot(&directory.path().join(DATABASE))
        .context("project.sqlite を読み込めません")?;
    let (json_version, json) = exchange::read_snapshot(&directory.path().join(GRAPH))
        .context("graph.json を読み込めません")?;
    ensure!(
        sqlite_version == json_version,
        "project.sqlite（形式 {sqlite_version}）と graph.json（形式 {json_version}）のバージョンが一致しません"
    );
    ensure!(
        sqlite.mode() == json.mode(),
        "project.sqlite と graph.json の mode が一致しません"
    );
    ensure!(
        sqlite.nodes() == json.nodes(),
        "project.sqlite と graph.json の nodes が一致しません"
    );
    ensure!(
        sqlite.edges() == json.edges(),
        "project.sqlite と graph.json の edges が一致しません"
    );
    ensure!(
        sqlite.positions() == json.positions(),
        "project.sqlite と graph.json の node_positions が一致しません"
    );
    Ok((sqlite_version, sqlite))
}

fn extract(
    zip: &mut ZipArchive<impl Read + Seek>,
    name: &str,
    entry_limit: u64,
    expanded_bytes: &mut u64,
    output: &mut impl Write,
) -> Result<()> {
    let mut entry = zip
        .by_name(name)
        .with_context(|| format!("{name} を開けません"))?;
    let declared = entry.size();
    let allowed = entry_limit.min(MAX_EXPANDED - *expanded_bytes);
    let actual = io::copy(&mut entry.by_ref().take(allowed), output)
        .with_context(|| format!("{name} を展開できません"))?;
    let mut extra = [0];
    let extra = entry
        .read(&mut extra)
        .with_context(|| format!("{name} を展開できません"))?;
    ensure!(extra == 0, "{name} の展開中にサイズの上限を超えました");
    ensure!(
        actual == declared,
        "{name} の展開サイズが ZIP のヘッダーと一致しません"
    );
    *expanded_bytes += actual;
    Ok(())
}

pub(super) fn prepare(parent: &Path, graph: &Graph) -> Result<NamedTempFile> {
    let directory = tempdir().context("保存用の一時フォルダを作成できません")?;
    let database = directory.path().join(DATABASE);
    storage::write_snapshot(&database, graph).context("project.sqlite を作成できません")?;

    let graph_path = directory.path().join(GRAPH);
    let mut json =
        BufWriter::new(File::create(&graph_path).context("graph.json を作成できません")?);
    exchange::encode(&mut json, graph).context("graph.json を書き込めません")?;
    json.flush().context("graph.json を書き込めません")?;
    drop(json);

    let mut archive = NamedTempFile::new_in(parent).context("保存先に一時 ZIP を作成できません")?;
    write_archive(archive.as_file_mut(), &database, &graph_path).context("ZIP を書き込めません")?;
    archive
        .as_file()
        .sync_all()
        .context("一時 ZIP を同期できません")?;
    Ok(archive)
}

fn write_archive(writer: impl Write + Seek, database: &Path, graph_path: &Path) -> Result<()> {
    let manifest = serde_json::to_vec(&Manifest {
        format: FORMAT.to_owned(),
        container_version: VERSION,
    })?;
    let mut database = File::open(database).context("一時 project.sqlite を開けません")?;
    let mut graph = File::open(graph_path).context("一時 graph.json を開けません")?;
    let database_size = database.metadata()?.len();
    let graph_size = graph.metadata()?.len();
    ensure!(
        database_size <= ENTRIES[1].1,
        "project.sqlite が保存サイズの上限を超えています"
    );
    ensure!(
        graph_size <= ENTRIES[2].1,
        "graph.json が保存サイズの上限を超えています"
    );
    ensure!(
        manifest.len() as u64 + database_size + graph_size <= MAX_EXPANDED,
        "プロジェクト全体が保存サイズの上限を超えています"
    );

    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(MANIFEST, options)?;
    zip.write_all(&manifest)?;
    for (name, input) in [(DATABASE, &mut database), (GRAPH, &mut graph)] {
        zip.start_file(name, options)?;
        io::copy(input, &mut zip).with_context(|| format!("{name} を ZIP に書き込めません"))?;
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{
        graph::ProjectMode,
        project::ProjectFile,
        project_tests::{legacy_fixture, sample},
    };

    const MANIFEST_V1: &[u8] = br#"{"format":"light-bringer-project","container_version":1}"#;
    const EMPTY_GRAPH: &[u8] = br#"{"format_version":1,"nodes":[],"edges":[],"node_positions":[]}"#;

    fn make_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, content) in entries {
            writer.start_file(*name, options)?;
            writer.write_all(content)?;
        }
        Ok(writer.finish()?.into_inner())
    }

    fn read_error(bytes: &[u8], expected: &str) -> Result<()> {
        let mut file = tempfile::tempfile()?;
        file.write_all(bytes)?;
        let error = format!("{:#}", read(&mut file).unwrap_err());
        assert!(error.contains(expected), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_missing_duplicate_unexpected_and_non_regular_entries() -> Result<()> {
        let missing = make_zip(&[(MANIFEST, MANIFEST_V1), (GRAPH, EMPTY_GRAPH)])?;
        read_error(&missing, "収録ファイル")?;

        let unexpected = make_zip(&[
            (MANIFEST, MANIFEST_V1),
            ("../project.sqlite", b""),
            (GRAPH, EMPTY_GRAPH),
        ])?;
        read_error(&unexpected, DATABASE)?;

        let placeholder = b"other.json";
        let mut duplicate = make_zip(&[
            (MANIFEST, MANIFEST_V1),
            (GRAPH, EMPTY_GRAPH),
            ("other.json", b""),
        ])?;
        assert_eq!(placeholder.len(), GRAPH.len());
        let offsets: Vec<_> = duplicate
            .windows(placeholder.len())
            .enumerate()
            .filter_map(|(offset, bytes)| (bytes == placeholder).then_some(offset))
            .collect();
        assert_eq!(offsets.len(), 2);
        for offset in offsets {
            duplicate[offset..offset + GRAPH.len()].copy_from_slice(GRAPH.as_bytes());
        }
        read_error(&duplicate, "重複")?;

        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        zip.start_file(MANIFEST, options)?;
        zip.write_all(MANIFEST_V1)?;
        zip.start_file(DATABASE, options)?;
        zip.add_symlink(GRAPH, "other.json", options)?;
        read_error(&zip.finish()?.into_inner(), "graph.json は通常ファイル")?;
        Ok(())
    }

    #[test]
    fn validates_all_versions_and_graph_consistency() -> Result<()> {
        let dir = tempdir()?;
        let db_path = dir.path().join(DATABASE);
        storage::write_snapshot(&db_path, &Graph::default())?;
        let sqlite = std::fs::read(&db_path)?;
        let manifest_v2 = br#"{"format":"light-bringer-project","container_version":2}"#;
        let manifest_other = br#"{"format":"other-project","container_version":1}"#;
        let graph_v2 =
            br#"{"format_version":2,"mode":"free","nodes":[],"edges":[],"node_positions":[]}"#;
        let graph_v2_ddd =
            br#"{"format_version":2,"mode":"ddd","nodes":[],"edges":[],"node_positions":[]}"#;
        let graph_future = br#"{"format_version":99,"nodes":[],"edges":[],"node_positions":[]}"#;
        let graph_with_node = br#"{"format_version":2,"mode":"free","nodes":[
            {"id":"b14d67ce-dc0b-4c5f-a8f1-d45e647cb270","name":"Order"}
        ],"edges":[],"node_positions":[]}"#;
        let sqlite_connection = rusqlite::Connection::open(&db_path)?;
        sqlite_connection.pragma_update(None, "user_version", 99)?;
        drop(sqlite_connection);
        let sqlite_future = std::fs::read(&db_path)?;

        let cases = [
            (
                manifest_v2.as_slice(),
                sqlite.as_slice(),
                EMPTY_GRAPH,
                "container_version 2",
            ),
            (manifest_other, &sqlite, EMPTY_GRAPH, "format が未対応"),
            (MANIFEST_V1, &sqlite, graph_future, "JSON 形式バージョン"),
            (
                MANIFEST_V1,
                &sqlite,
                EMPTY_GRAPH,
                "バージョンが一致しません",
            ),
            (MANIFEST_V1, &sqlite, graph_v2_ddd, "mode が一致しません"),
            (
                MANIFEST_V1,
                &sqlite_future,
                graph_v2,
                "プロジェクトバージョン",
            ),
            (MANIFEST_V1, b"bad SQLite", EMPTY_GRAPH, DATABASE),
            (
                MANIFEST_V1,
                &sqlite,
                graph_with_node,
                "nodes が一致しません",
            ),
        ];
        for (manifest, database, graph, expected) in cases {
            let zip = make_zip(&[(MANIFEST, manifest), (DATABASE, database), (GRAPH, graph)])?;
            read_error(&zip, expected)?;
        }
        Ok(())
    }

    #[test]
    fn opens_legacy_container_and_saves_both_snapshots_at_version_two() -> Result<()> {
        let dir = tempdir()?;
        let database = dir.path().join(DATABASE);
        let path = dir.path().join("legacy.lbproj");
        let (expected, _, _) = sample()?;
        let legacy_json = legacy_fixture(&database, &expected)?;
        let legacy_sqlite = std::fs::read(database)?;
        let bytes = make_zip(&[
            (MANIFEST, MANIFEST_V1),
            (DATABASE, &legacy_sqlite),
            (GRAPH, &legacy_json),
        ])?;
        std::fs::write(&path, &bytes)?;
        let (project, mut graph) = ProjectFile::open(&path)?;
        assert_eq!(graph, expected);
        assert_eq!(std::fs::read(&path)?, bytes);

        graph.set_mode(ProjectMode::Ddd);
        project.save(&graph)?;
        assert_eq!(project.reload()?, graph);
        let mut zip = ZipArchive::new(File::open(path)?)?;
        for entry in [DATABASE, GRAPH] {
            let mut output = File::create(dir.path().join(entry))?;
            io::copy(&mut zip.by_name(entry)?, &mut output)?;
        }
        assert_eq!(
            storage::read_snapshot(&dir.path().join(DATABASE))?,
            (2, graph.clone())
        );
        assert_eq!(
            exchange::read_snapshot(&dir.path().join(GRAPH))?,
            (2, graph)
        );
        Ok(())
    }

    #[test]
    fn checks_declared_and_actual_expansion_limits() -> Result<()> {
        let oversized = vec![b' '; ENTRIES[0].1 as usize + 1];
        let zip = make_zip(&[
            (MANIFEST, &oversized),
            (DATABASE, b""),
            (GRAPH, EMPTY_GRAPH),
        ])?;
        read_error(&zip, "manifest.json が展開サイズの上限")?;

        let bytes = make_zip(&[(MANIFEST, MANIFEST_V1)])?;
        let mut zip = ZipArchive::new(Cursor::new(bytes))?;
        for (entry_limit, mut expanded) in [(5, 0), (ENTRIES[0].1, MAX_EXPANDED - 5)] {
            let before = expanded;
            let mut output = Vec::new();
            let error =
                extract(&mut zip, MANIFEST, entry_limit, &mut expanded, &mut output).unwrap_err();
            assert!(error.to_string().contains("展開中にサイズの上限"));
            assert_eq!(output.len(), 5);
            assert_eq!(expanded, before);
        }
        Ok(())
    }

    #[test]
    fn surfaces_zip_write_failure() -> Result<()> {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("simulated disk write failure"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl Seek for FailingWriter {
            fn seek(&mut self, _: io::SeekFrom) -> io::Result<u64> {
                Ok(0)
            }
        }

        let dir = tempdir()?;
        let database = dir.path().join(DATABASE);
        let graph = dir.path().join(GRAPH);
        storage::write_snapshot(&database, &Graph::default())?;
        std::fs::write(&graph, EMPTY_GRAPH)?;
        let error = write_archive(FailingWriter, &database, &graph).unwrap_err();
        assert!(format!("{error:#}").contains("simulated disk write failure"));
        Ok(())
    }
}
