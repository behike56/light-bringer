use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    nodes: BTreeMap<Uuid, Node>,
    edges: BTreeMap<Uuid, Edge>,
    positions: BTreeMap<Uuid, Position>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: Uuid,
    pub source_id: Uuid,
    pub target_id: Uuid,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub node_id: Uuid,
    pub x: f64,
    pub y: f64,
}

impl Graph {
    pub fn from_records(
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        positions: Vec<Position>,
    ) -> Result<Self> {
        let mut graph = Self::default();
        let mut errors = Vec::new();
        let mut seen = HashSet::new();

        for (index, node) in nodes.into_iter().enumerate() {
            let result = check_unique(&mut seen, node.id).and_then(|()| graph.put_node(node));
            record_error(&mut errors, "nodes", index, result);
        }
        seen.clear();
        for (index, edge) in edges.into_iter().enumerate() {
            let result = check_unique(&mut seen, edge.id).and_then(|()| graph.put_edge(edge));
            record_error(&mut errors, "edges", index, result);
        }
        seen.clear();
        for (index, position) in positions.into_iter().enumerate() {
            let result = check_unique(&mut seen, position.node_id)
                .and_then(|()| graph.put_position(position));
            record_error(&mut errors, "node_positions", index, result);
        }

        if !errors.is_empty() {
            bail!(errors.join("\n"));
        }
        Ok(graph)
    }

    pub fn nodes(&self) -> &BTreeMap<Uuid, Node> {
        &self.nodes
    }

    pub fn edges(&self) -> &BTreeMap<Uuid, Edge> {
        &self.edges
    }

    pub fn positions(&self) -> &BTreeMap<Uuid, Position> {
        &self.positions
    }

    pub fn put_node(&mut self, mut node: Node) -> Result<()> {
        node.name = required_name(&node.name, "name", "ノード名")?;
        self.nodes.insert(node.id, node);
        Ok(())
    }

    pub fn put_edge(&mut self, mut edge: Edge) -> Result<()> {
        edge.label = required_name(&edge.label, "label", "関係名")?;
        ensure!(
            self.nodes.contains_key(&edge.source_id),
            "source_id {} のノードがありません",
            edge.source_id
        );
        ensure!(
            self.nodes.contains_key(&edge.target_id),
            "target_id {} のノードがありません",
            edge.target_id
        );
        self.edges.insert(edge.id, edge);
        Ok(())
    }

    pub fn put_position(&mut self, position: Position) -> Result<()> {
        ensure!(
            self.nodes.contains_key(&position.node_id),
            "node_id {} のノードがありません",
            position.node_id
        );
        ensure!(
            position.x.is_finite() && position.y.is_finite(),
            "x と y には有限の値を指定してください"
        );
        self.positions.insert(position.node_id, position);
        Ok(())
    }

    pub fn related_edge_count(&self, id: Uuid) -> usize {
        self.edges
            .values()
            .filter(|edge| connects(edge, id))
            .count()
    }

    pub fn remove_node(&mut self, id: Uuid) {
        self.nodes.remove(&id);
        self.edges.retain(|_, edge| !connects(edge, id));
        self.positions.remove(&id);
    }

    pub fn remove_edge(&mut self, id: Uuid) {
        self.edges.remove(&id);
    }

    pub fn place_unpositioned(&mut self) {
        if self.nodes.len() == self.positions.len() {
            return;
        }
        const COLUMNS: usize = 4;
        const FIRST_X: f64 = 110.0;
        const FIRST_Y: f64 = 100.0;
        const STEP_X: f64 = 190.0;
        const STEP_Y: f64 = 115.0;
        let mut occupied = HashSet::new();
        for position in self.positions.values() {
            let row = ((position.y - FIRST_Y) / STEP_Y).round();
            if row >= 0.0 && (position.y - (FIRST_Y + row * STEP_Y)).abs() < 55.0 {
                for column in 0..COLUMNS {
                    if (position.x - (FIRST_X + column as f64 * STEP_X)).abs() < 170.0 {
                        occupied.insert((column, row as usize));
                    }
                }
            }
        }

        let mut slot = 0;
        for id in self.nodes.keys() {
            if self.positions.contains_key(id) {
                continue;
            }
            while occupied.contains(&(slot % COLUMNS, slot / COLUMNS)) {
                slot += 1;
            }
            let x = FIRST_X + (slot % COLUMNS) as f64 * STEP_X;
            let y = FIRST_Y + (slot / COLUMNS) as f64 * STEP_Y;
            self.positions.insert(*id, Position { node_id: *id, x, y });
            slot += 1;
        }
    }
}

fn required_name(value: &str, field: &str, label: &str) -> Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "{label}（{field}）を入力してください");
    Ok(value.to_owned())
}

fn connects(edge: &Edge, id: Uuid) -> bool {
    edge.source_id == id || edge.target_id == id
}

fn check_unique(seen: &mut HashSet<Uuid>, id: Uuid) -> Result<()> {
    ensure!(seen.insert(id), "ID {id} が重複しています");
    Ok(())
}

fn record_error(errors: &mut Vec<String>, collection: &str, index: usize, result: Result<()>) {
    if let Err(error) = result {
        errors.push(format!("{collection}[{index}] {error}"));
    }
}
