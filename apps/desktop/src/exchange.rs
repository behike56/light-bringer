use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::graph::{Edge, Graph, Node, Position};

const FORMAT_VERSION: u32 = 1;

#[derive(Deserialize)]
struct ImportData {
    format_version: u32,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    node_positions: Vec<Position>,
}

#[derive(Serialize)]
struct ExportData<'a> {
    format_version: u32,
    nodes: Vec<&'a Node>,
    edges: Vec<&'a Edge>,
    node_positions: Vec<&'a Position>,
}

pub fn read(path: &Path) -> Result<Graph> {
    let file = File::open(path).context("JSON ファイルを開けません")?;
    let data: ImportData =
        serde_json::from_reader(BufReader::new(file)).context("JSON の読み込みに失敗しました")?;
    ensure!(
        data.format_version == FORMAT_VERSION,
        "未対応の JSON 形式バージョンです（{}）",
        data.format_version
    );
    Graph::from_records(data.nodes, data.edges, data.node_positions)
}

pub fn write(path: &Path, graph: &Graph) -> Result<()> {
    let data = ExportData {
        format_version: FORMAT_VERSION,
        nodes: graph.nodes().values().collect(),
        edges: graph.edges().values().collect(),
        node_positions: graph.positions().values().collect(),
    };

    let parent = path.parent().context("書き出し先のフォルダがありません")?;
    let mut file = NamedTempFile::new_in(parent).context("書き出し先にファイルを作成できません")?;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &data).context("JSON を書き込めません")?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .context("JSON を保存できません。書き出し先に同名ファイルがないか確認してください")?;
    Ok(())
}
