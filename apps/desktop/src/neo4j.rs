mod cli;
mod client;
mod cypher;

#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    exchange,
    graph::{CardKind, DddRelation, Edge, Graph, Node, Position, ProjectMode},
    project, storage,
};
use client::{QueryClient, Reply, Transaction};

pub use cli::run_cli;

const SCHEMA_VERSION: u32 = 1;
const BATCH_SIZE: usize = 1_000;
const READ_ATTEMPTS: usize = 3;

struct Neo4j {
    client: QueryClient,
    project_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct Head {
    revision: i64,
    mode: ProjectMode,
    content_hash: String,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Receipt {
    expected_revision: i64,
    revision: i64,
    content_hash: String,
}

#[derive(Debug, Serialize)]
struct Publication {
    #[serde(flatten)]
    receipt: Receipt,
    replayed: bool,
}

#[derive(Deserialize)]
struct StoredNode {
    #[serde(flatten)]
    node: Node,
    x: Option<f64>,
    y: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PathNode {
    id: Uuid,
    name: String,
    source_reference: Option<String>,
    event_storming_kind: CardKind,
}

#[derive(Debug, Serialize, Deserialize)]
struct PathEdge {
    id: Uuid,
    label: String,
    source_reference: Option<String>,
    event_storming_relation: DddRelation,
}

#[derive(Debug, Serialize)]
struct SearchPath {
    nodes: Vec<PathNode>,
    edges: Vec<PathEdge>,
}

impl Neo4j {
    fn init(&self) -> Result<()> {
        let (version,): (Option<u32>,) = self.client.query(cypher::SCHEMA, json!({}))?.one()?;
        if let Some(version) = version {
            check_schema_version(version)?;
        }
        for statement in cypher::CONSTRAINTS {
            self.client
                .query(statement, json!({}))
                .context("Neo4j の一意制約を作成できません")?;
        }
        let (version,): (u32,) = self
            .client
            .query(cypher::INSTALL_SCHEMA, json!({ "version": SCHEMA_VERSION }))?
            .one()?;
        check_schema_version(version)
    }

    fn require_schema(&self) -> Result<()> {
        let (version,): (Option<u32>,) = self.client.query(cypher::SCHEMA, json!({}))?.one()?;
        check_schema_version(version.context("先に neo4j init で公開スキーマを作成してください")?)
    }

    fn scope(&self) -> Value {
        json!({ "project_id": self.project_id })
    }

    fn head(&self) -> Result<Option<Head>> {
        let (head,): (Option<Head>,) = self.client.query(cypher::HEAD, self.scope())?.one()?;
        Ok(head)
    }

    fn receipt(&self, request_id: Uuid) -> Result<Option<Receipt>> {
        lookup_receipt(
            self.client
                .query(cypher::REQUEST, self.request_scope(request_id))?,
        )
    }

    fn request_scope(&self, request_id: Uuid) -> Value {
        json!({ "project_id": self.project_id, "request_id": request_id })
    }

    fn publish(
        &self,
        graph: &Graph,
        expected_revision: i64,
        request_id: Uuid,
    ) -> Result<Publication> {
        ensure!(
            expected_revision >= 0,
            "期待リビジョンには 0 以上の整数を指定してください"
        );
        let content_hash = graph_hash(graph)?;
        // Build parameters before taking the write lock.
        let nodes = graph
            .nodes()
            .values()
            .map(|node| {
                let mut row = serde_json::to_value(node)?;
                if let Some(position) = graph.positions().get(&node.id) {
                    row["x"] = json!(position.x);
                    row["y"] = json!(position.y);
                }
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;
        let edges = graph
            .edges()
            .values()
            .map(|edge| {
                let mut properties = serde_json::to_value(edge)?;
                let properties_map = properties
                    .as_object_mut()
                    .context("関係のプロパティを作成できません")?;
                properties_map.remove("source_id");
                properties_map.remove("target_id");
                Ok(json!({
                    "id": edge.id, "source_id": edge.source_id, "target_id": edge.target_id,
                    "properties": properties,
                }))
            })
            .collect::<Result<Vec<_>>>()?;

        let (tx, reply) = self.client.begin(cypher::LOCK_PROJECT, self.scope())?;
        let (current,): (i64,) = reply.one()?;

        if let Some(receipt) =
            lookup_receipt(tx.query(cypher::REQUEST, self.request_scope(request_id))?)?
        {
            ensure!(
                receipt.expected_revision == expected_revision
                    && receipt.content_hash == content_hash,
                "公開要求 ID {request_id} は異なる期待リビジョンまたは内容で使用済みです"
            );
            return Ok(Publication {
                receipt,
                replayed: true,
            });
        }
        ensure!(
            current == expected_revision,
            "公開リビジョンが競合しました（期待値 {expected_revision}、現在値 {current}）"
        );
        let revision = current
            .checked_add(1)
            .context("公開リビジョンが上限に達しました")?;

        let (invalid_edges,): (u64,) = tx.query(cypher::CHECK_EDGE_SCOPE, self.scope())?.one()?;
        ensure!(
            invalid_edges == 0,
            "Neo4j にプロジェクトをまたぐ不正な関係があります"
        );

        for rows in nodes.chunks(BATCH_SIZE) {
            let (count,): (usize,) = tx
                .query(
                    cypher::UPSERT_NODES,
                    json!({
                        "project_id": self.project_id, "revision": revision, "rows": rows,
                    }),
                )?
                .one()?;
            ensure!(
                count == rows.len(),
                "Neo4j に反映したノード件数が入力と一致しません"
            );
        }
        for rows in edges.chunks(BATCH_SIZE) {
            tx.query(
                cypher::DELETE_MOVED_EDGES,
                json!({
                    "project_id": self.project_id, "rows": rows,
                }),
            )?;
            let (count,): (usize,) = tx
                .query(
                    cypher::UPSERT_EDGES,
                    json!({
                        "project_id": self.project_id, "revision": revision, "rows": rows,
                    }),
                )?
                .one()?;
            ensure!(
                count == rows.len(),
                "Neo4j に反映した関係件数が入力と一致しません"
            );
        }

        let scope = json!({ "project_id": self.project_id, "revision": revision });
        tx.query(cypher::DELETE_STALE_EDGES, scope.clone())?;
        // Plain DELETE leaves foreign relationships intact and aborts the publication if found.
        tx.query(cypher::DELETE_STALE_NODES, scope)?;
        let (recorded,): (i64,) = tx.query(cypher::FINISH_PUBLICATION, json!({
            "project_id": self.project_id, "mode": graph.mode(), "revision": revision,
            "request_id": request_id, "expected_revision": expected_revision, "content_hash": content_hash,
        }))?.one()?;
        ensure!(recorded == revision, "公開リビジョンを記録できませんでした");
        tx.commit().with_context(|| format!(
            "コミット結果が不明です。同じ公開要求 ID {request_id} を照会するか、元の入力と期待値 {expected_revision} で再送してください"
        ))?;

        Ok(Publication {
            receipt: Receipt {
                expected_revision,
                revision,
                content_hash,
            },
            replayed: false,
        })
    }

    fn snapshot(&self) -> Result<(Head, Graph)> {
        let (head, (nodes, edges)) = self.stable_read(|tx| {
            Ok((
                tx.query(cypher::READ_NODES, self.scope())?,
                tx.query(cypher::READ_EDGES, self.scope())?,
            ))
        })?;

        let mut node_records = Vec::new();
        let mut positions = Vec::new();
        for (row,) in nodes.rows::<(StoredNode,)>()? {
            match (row.x, row.y) {
                (Some(x), Some(y)) => positions.push(Position {
                    node_id: row.node.id,
                    x,
                    y,
                }),
                (None, None) => {}
                _ => bail!("Neo4j のノード {} に片方の座標しかありません", row.node.id),
            }
            node_records.push(row.node);
        }
        let edge_records = edges
            .rows::<(Value, bool)>()?
            .into_iter()
            .map(|(edge, valid)| {
                ensure!(valid, "Neo4j にプロジェクトをまたぐ不正な関係があります");
                serde_json::from_value::<Edge>(edge).context("Neo4j の関係を読み戻せません")
            })
            .collect::<Result<Vec<_>>>()?;
        let graph = Graph::from_records(head.mode, node_records, edge_records, positions)?;
        ensure!(
            graph_hash(&graph)? == head.content_hash,
            "Neo4j の内容が公開時のハッシュと一致しません"
        );
        Ok((head, graph))
    }

    fn search(&self, command_id: Uuid, depth: u8, limit: u16) -> Result<(Head, Vec<SearchPath>)> {
        ensure!(
            (1..=3).contains(&depth),
            "経路長には 1〜3 を指定してください"
        );
        ensure!(
            (1..=1000).contains(&limit),
            "結果件数の上限には 1〜1000 を指定してください"
        );
        let statement = cypher::search(depth);
        let (head, reply) = self.stable_read(|tx| {
            tx.query(&statement, json!({
                "project_id": self.project_id,
                "command_id": command_id,
                "limit": limit,
                "relations": [DddRelation::HandledBy, DddRelation::Emits, DddRelation::Triggers],
                "kinds": [CardKind::Aggregate, CardKind::DomainEvent, CardKind::Policy],
            }))
        })?;
        let paths = reply
            .rows::<(Vec<PathNode>, Vec<PathEdge>)>()?
            .into_iter()
            .map(|(nodes, edges)| SearchPath { nodes, edges })
            .collect();
        Ok((head, paths))
    }

    fn stable_read<T>(&self, read: impl Fn(&Transaction<'_>) -> Result<T>) -> Result<(Head, T)> {
        for _ in 0..READ_ATTEMPTS {
            // One server transaction pins both revision reads and the data queries to the same member.
            let (tx, reply) = self.client.begin(cypher::HEAD, self.scope())?;
            let (before,): (Option<Head>,) = reply.one()?;
            let before = before.context("このプロジェクトには公開済みのリビジョンがありません")?;
            let result = read(&tx)?;
            let (after,): (Option<Head>,) = tx.query(cypher::HEAD, self.scope())?.one()?;
            if after.as_ref() == Some(&before) {
                return Ok((before, result));
            }
        }
        bail!("取得中に公開リビジョンが繰り返し更新されました。再試行してください")
    }
}

fn lookup_receipt(reply: Reply) -> Result<Option<Receipt>> {
    let (receipt,) = reply.one()?;
    Ok(receipt)
}

fn check_schema_version(version: u32) -> Result<()> {
    ensure!(
        version == SCHEMA_VERSION,
        "未対応の Neo4j スキーマバージョンです（{version}）"
    );
    Ok(())
}

fn read_source(path: &Path) -> Result<(u32, Graph)> {
    if project::has_extension(path, "json") {
        exchange::read_snapshot(path)
    } else if project::has_extension(path, "sqlite") {
        storage::read_snapshot(path)
    } else if project::has_extension(path, "lbproj") {
        project::read_container_snapshot(path)
    } else {
        bail!("入力には .json、.lbproj、.sqlite を指定してください");
    }
}

fn graph_hash(graph: &Graph) -> Result<String> {
    // Hash the normalized records in UUID order, independently of the exchange format version.
    let encoded = serde_json::to_vec(&(
        graph.mode(),
        graph.nodes(),
        graph.edges(),
        graph.positions(),
    ))?;
    Ok(Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
