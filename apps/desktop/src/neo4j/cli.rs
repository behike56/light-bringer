use std::{
    fs::File,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use uuid::Uuid;

use super::{Neo4j, SCHEMA_VERSION, client::QueryClient, graph_hash, read_source};
use crate::{exchange, graph::Graph};

#[derive(Parser)]
#[command(
    bin_name = "light-bringer neo4j",
    about = "保存済みグラフを Neo4j に公開します"
)]
struct Cli {
    #[arg(short, long, value_name = "FILE", help = "公開先の設定ファイル")]
    target: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 公開先をローカル設定に登録する
    Register {
        /// Neo4j Query API サーバーの HTTPS オリジン
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "neo4j")]
        database: String,
    },
    /// Neo4j に公開用の制約とスキーマを作成する
    Init,
    /// 入力をオフラインで検証し、公開先と内容を確認する
    Inspect { source: PathBuf },
    /// 公開中のリビジョンを確認する
    Status,
    /// 検証済みのスナップショットを公開する
    Publish {
        source: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(i64).range(0..))]
        expected_revision: i64,
        #[arg(long)]
        request_id: Uuid,
    },
    /// 指定した公開要求のコミット結果を照会する
    Receipt { request_id: Uuid },
    /// 公開中のスナップショットを新規 JSON に書き出す
    Export { output: PathBuf },
    /// 公開中のスナップショットを元ファイルと照合する
    Verify { source: PathBuf },
    /// コマンドから集約、イベント、ポリシーへ向かう経路を検索する
    Search {
        command_id: Uuid,
        /// 返す経路の長さ。1 と 2 は先頭部分を返す
        #[arg(long, default_value = "3", value_parser = clap::value_parser!(u8).range(1..=3))]
        depth: u8,
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    project_id: Uuid,
    url: String,
    database: String,
}

impl Target {
    fn query_url(&self) -> Result<Url> {
        let mut url = Url::parse(&self.url).context("公開先の URL を解釈できません")?;
        ensure!(
            url.scheme() == "https"
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none(),
            "公開先には資格情報やパスを含まない HTTPS オリジンを指定してください"
        );
        ensure!(
            !self.database.is_empty()
                && self
                    .database
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
                && self.database != "."
                && self.database != "..",
            "データベース名には英数字・ハイフン・ドットを指定してください"
        );
        url.path_segments_mut()
            .expect("HTTPS URL")
            .extend(["db", &self.database, "query", "v2"]);
        Ok(url)
    }

    fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).context("公開先の設定ファイルを開けません")?;
        let target: Self = serde_json::from_reader(BufReader::new(file))
            .context("公開先の設定を読み込めません")?;
        target.query_url()?;
        Ok(target)
    }

    fn write(&self, path: &Path) -> Result<()> {
        self.query_url()?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = File::open(parent).context("設定ファイルの保存先を開けません")?;
        let mut file = NamedTempFile::new_in(parent).context("設定ファイルを作成できません")?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path)
            .context("設定を保存できません。新しいファイル名を指定してください")?;
        directory
            .sync_all()
            .context("設定を作成しましたが、保存内容の永続化を確認できませんでした")?;
        Ok(())
    }

    fn connect(&self, role: &str) -> Result<Neo4j> {
        Ok(Neo4j {
            client: QueryClient::connect(self.query_url()?, role)?,
            project_id: self.project_id,
        })
    }

    fn ready(&self, role: &str) -> Result<Neo4j> {
        let db = self.connect(role)?;
        db.require_schema()?;
        Ok(db)
    }
}

pub fn run_cli() -> ExitCode {
    let cli = Cli::parse_from(std::env::args_os().skip(1));
    match execute(cli).and_then(|output| {
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &output)?;
        writeln!(stdout)?;
        Ok(())
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<Value> {
    let start = Instant::now();
    if let Command::Register { url, database } = cli.command {
        let target = Target {
            project_id: Uuid::new_v4(),
            url,
            database,
        };
        target.write(&cli.target)?;
        return Ok(json!({ "target": target, "target_file": cli.target.display().to_string() }));
    }

    let target = Target::read(&cli.target)?;
    let result = match cli.command {
        Command::Register { .. } => unreachable!(),
        Command::Init => {
            target.connect("ADMIN")?.init()?;
            json!({ "schema_version": SCHEMA_VERSION })
        }
        Command::Inspect { source } => {
            let (version, graph) = read_source(&source)?;
            describe(&source, version, &graph)?
        }
        Command::Status => {
            let head = target.ready("READ")?.head()?;
            json!({ "revision": head.as_ref().map_or(0, |head| head.revision), "head": head })
        }
        Command::Publish {
            source,
            expected_revision,
            request_id,
        } => {
            let (version, graph) = read_source(&source)?;
            let input = describe(&source, version, &graph)?;
            eprintln!(
                "{}",
                json!({
                    "target": target, "input": input,
                    "expected_revision": expected_revision, "request_id": request_id,
                })
            );
            let publication =
                target
                    .ready("PUBLISH")?
                    .publish(&graph, expected_revision, request_id)?;
            json!({ "publication": publication, "request_id": request_id, "input": input })
        }
        Command::Receipt { request_id } => {
            let receipt = target.ready("READ")?.receipt(request_id)?;
            json!({ "request_id": request_id, "receipt": receipt })
        }
        Command::Export { output } => {
            ensure!(
                crate::project::has_extension(&output, "json"),
                "書き出し先の拡張子は .json を指定してください"
            );
            let (head, graph) = target.ready("READ")?.snapshot()?;
            // The existing atomic export requires an explicit parent for a relative filename.
            let path = Path::new(".").join(&output);
            exchange::write(&path, &graph)?;
            json!({ "head": head, "output": output.display().to_string(), "counts": counts(&graph) })
        }
        Command::Verify { source } => {
            let (version, expected) = read_source(&source)?;
            let (head, graph) = target.ready("READ")?.snapshot()?;
            ensure!(
                graph == expected,
                "元ファイルと Neo4j の公開内容が一致しません（公開リビジョン {}）",
                head.revision
            );
            json!({ "head": head, "input": describe(&source, version, &expected)? })
        }
        Command::Search {
            command_id,
            depth,
            limit,
        } => {
            let (head, paths) = target.ready("READ")?.search(command_id, depth, limit)?;
            json!({ "head": head, "command_id": command_id, "depth": depth, "paths": paths })
        }
    };
    Ok(json!({ "target": target, "result": result, "elapsed_ms": start.elapsed().as_millis() }))
}

fn describe(path: &Path, version: u32, graph: &Graph) -> Result<Value> {
    Ok(json!({
        "source": path.display().to_string(),
        "format_version": version,
        "mode": graph.mode(),
        "content_hash": graph_hash(graph)?,
        "counts": counts(graph),
    }))
}

fn counts(graph: &Graph) -> Value {
    json!({
        "nodes": graph.nodes().len(),
        "edges": graph.edges().len(),
        "positions": graph.positions().len(),
    })
}
