use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::graph::{Edge, Graph, Node, Position, ProjectMode};

const FORMAT_VERSION: u32 = 2;

#[derive(Deserialize)]
struct ImportData {
    format_version: u32,
    mode: Option<ProjectMode>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    node_positions: Vec<Position>,
}

#[derive(Serialize)]
struct ExportData<'a> {
    format_version: u32,
    mode: ProjectMode,
    nodes: Vec<&'a Node>,
    edges: Vec<&'a Edge>,
    node_positions: Vec<&'a Position>,
}

pub fn read(path: &Path) -> Result<Graph> {
    read_snapshot(path).map(|(_, graph)| graph)
}

pub fn read_snapshot(path: &Path) -> Result<(u32, Graph)> {
    let file = File::open(path).context("JSON ファイルを開けません")?;
    let mut data: ImportData =
        serde_json::from_reader(BufReader::new(file)).context("JSON の読み込みに失敗しました")?;
    ensure!(
        matches!(data.format_version, 1 | FORMAT_VERSION),
        "未対応の JSON 形式バージョンです（{}）",
        data.format_version
    );
    let mode = if data.format_version == 1 {
        for node in &mut data.nodes {
            node.event_storming_kind = None;
        }
        for edge in &mut data.edges {
            edge.event_storming_relation = None;
        }
        ProjectMode::Free
    } else {
        data.mode.context("JSON 形式 2 には mode が必要です")?
    };
    let graph = Graph::from_records(mode, data.nodes, data.edges, data.node_positions)?;
    Ok((data.format_version, graph))
}

pub fn write(path: &Path, graph: &Graph) -> Result<()> {
    let parent = path.parent().context("書き出し先のフォルダがありません")?;
    let mut file = NamedTempFile::new_in(parent).context("書き出し先にファイルを作成できません")?;
    let mut writer = BufWriter::new(file.as_file_mut());
    encode(&mut writer, graph)?;
    writer.flush()?;
    drop(writer);
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .context("JSON を保存できません。書き出し先に同名ファイルがないか確認してください")?;
    Ok(())
}

pub fn encode(mut writer: impl Write, graph: &Graph) -> Result<()> {
    let data = ExportData {
        format_version: FORMAT_VERSION,
        mode: graph.mode(),
        nodes: graph.nodes().values().collect(),
        edges: graph.edges().values().collect(),
        node_positions: graph.positions().values().collect(),
    };

    serde_json::to_writer_pretty(&mut writer, &data).context("JSON を書き込めません")?;
    writer.write_all(b"\n")?;
    Ok(())
}
