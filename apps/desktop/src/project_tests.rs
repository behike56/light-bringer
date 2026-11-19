use std::fs;

use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use crate::{
    exchange,
    graph::{Edge, Graph, Node, Position},
    storage::ProjectFile,
};

fn sample() -> Result<(Graph, Uuid, Uuid)> {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let mut graph = Graph::from_records(
        vec![
            Node {
                id: parent,
                name: " 注文 ".into(),
                kind: Some("業務概念".into()),
                description: Some("注文の単位".into()),
                source_reference: Some("注文仕様書 §2".into()),
            },
            Node {
                id: child,
                name: "注文".into(),
                kind: Some("注文明細".into()),
                description: None,
                source_reference: None,
            },
        ],
        Vec::new(),
        vec![Position {
            node_id: parent,
            x: 110.0,
            y: 100.0,
        }],
    )?;
    for (source_id, target_id, label) in [
        (parent, child, "含む"),
        (parent, child, "優先する"),
        (child, parent, "属する"),
        (parent, parent, "参照する"),
    ] {
        graph.put_edge(Edge {
            id: Uuid::new_v4(),
            source_id,
            target_id,
            label: label.into(),
            note: Some("1件以上".into()),
            source_reference: Some("注文仕様書 §2.1".into()),
        })?;
    }
    graph.place_unpositioned();
    Ok((graph, parent, child))
}

#[test]
fn preserves_ids_direction_annotations_and_layout_through_both_formats() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("orders.sqlite");
    let json_path = dir.path().join("orders.json");
    let restored = dir.path().join("restored.sqlite");
    let (graph, parent, child) = sample()?;
    assert_eq!(graph.nodes()[&parent].name, graph.nodes()[&child].name);
    assert_eq!(graph.positions()[&parent].x, 110.0);
    assert_ne!(graph.positions()[&child].x, 110.0);

    let project = ProjectFile::create(&db, &Graph::default())?;
    project.save(&graph)?;
    drop(project);
    let (project, reopened) = ProjectFile::open(&db)?;
    assert_eq!(reopened, graph);

    exchange::write(&json_path, &project.read()?)?;
    let imported = exchange::read(&json_path)?;
    assert_eq!(imported, graph);
    let recreated = ProjectFile::create(&restored, &imported)?;
    assert_eq!(recreated.read()?, graph);

    let json_before = fs::read(&json_path)?;
    assert!(exchange::write(&json_path, &Graph::default()).is_err());
    assert_eq!(fs::read(&json_path)?, json_before);
    assert!(ProjectFile::create(&db, &Graph::default()).is_err());
    assert_eq!(project.read()?, graph);
    Ok(())
}

#[test]
fn locks_aliases_until_the_editing_session_ends() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("orders.sqlite");
    let symlink = dir.path().join("symbolic.sqlite");
    let hardlink = dir.path().join("hard.sqlite");
    let project = ProjectFile::create(&db, &Graph::default())?;
    std::os::unix::fs::symlink(&db, &symlink)?;
    fs::hard_link(&db, &hardlink)?;

    for path in [&db, &symlink, &hardlink] {
        let Err(error) = ProjectFile::open(path) else {
            panic!("another editing session acquired {}", path.display());
        };
        assert!(error.to_string().contains("別のセッション"));
    }
    drop(project);
    ProjectFile::open(&symlink)?;
    Ok(())
}

#[test]
fn failed_save_rolls_back_and_a_retry_can_remove_persisted_edges_and_position() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("orders.sqlite");
    let (graph, parent, child) = sample()?;
    let project = ProjectFile::create(&db, &graph)?;
    let connection = Connection::open(&db)?;
    connection.execute_batch(
        "CREATE TRIGGER fail_save BEFORE INSERT ON nodes
           BEGIN SELECT RAISE(ABORT, 'simulated storage failure'); END;",
    )?;

    let mut edited = graph.clone();
    assert_eq!(edited.related_edge_count(parent), 4);
    edited.remove_node(parent);
    assert!(project.save(&edited).is_err());
    assert_eq!(project.read()?, graph);

    connection.execute_batch("DROP TRIGGER fail_save;")?;
    project.save(&edited)?;
    drop(project);
    let (_, reopened) = ProjectFile::open(&db)?;
    assert_eq!(reopened, edited);
    assert!(reopened.edges().is_empty());
    assert!(!reopened.positions().contains_key(&parent));
    assert!(reopened.nodes().contains_key(&child));
    Ok(())
}

#[test]
fn rejects_future_sqlite_version_without_changing_the_file() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("future.sqlite");
    drop(ProjectFile::create(&db, &Graph::default())?);
    Connection::open(&db)?.pragma_update(None, "user_version", 99)?;
    let before = fs::read(&db)?;
    let Err(error) = ProjectFile::open(&db) else {
        panic!("opened a future version");
    };
    assert!(error.to_string().contains("99"));
    assert_eq!(fs::read(db)?, before);
    Ok(())
}

#[test]
fn reports_invalid_exchange_records_together_and_rejects_unknown_versions() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("invalid.json");
    let good = Uuid::new_v4();
    let missing = Uuid::new_v4();
    let edge = Uuid::new_v4();
    let mut document = json!({
        "format_version": 1,
        "nodes": [
            { "id": good, "name": "注文" },
            { "id": good, "name": "重複" },
            { "id": missing, "name": "   " }
        ],
        "edges": [
            { "id": edge, "source_id": good, "target_id": missing, "label": "含む" },
            { "id": edge, "source_id": good, "target_id": good, "label": "別の関係" }
        ],
        "node_positions": [
            { "node_id": good, "x": 10, "y": 20 },
            { "node_id": good, "x": 30, "y": 40 },
            { "node_id": missing, "x": 50, "y": 60 }
        ]
    });
    fs::write(&path, document.to_string())?;
    let error = exchange::read(&path).unwrap_err().to_string();
    for field in [
        "nodes[1]",
        "nodes[2]",
        "edges[0]",
        "edges[1]",
        "node_positions[1]",
        "node_positions[2]",
    ] {
        assert!(error.contains(field), "{error}");
    }
    document["format_version"] = 99.into();
    fs::write(&path, document.to_string())?;
    assert!(
        exchange::read(&path)
            .unwrap_err()
            .to_string()
            .contains("バージョン")
    );
    Ok(())
}
