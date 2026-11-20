use std::{fs, path::Path};

use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use crate::{
    exchange,
    graph::{CardKind, DddRelation, Edge, Graph, Node, Position, ProjectMode},
    project::ProjectFile,
    storage::SqliteProject,
};

pub(crate) fn sample() -> Result<(Graph, Uuid, Uuid)> {
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let mut graph = Graph::from_records(
        ProjectMode::Free,
        vec![
            Node {
                id: parent,
                name: " 注文 ".into(),
                kind: Some("業務概念".into()),
                description: Some("注文の単位".into()),
                source_reference: Some("注文仕様書 §2".into()),
                event_storming_kind: None,
            },
            Node {
                id: child,
                name: "注文".into(),
                kind: Some("注文明細".into()),
                description: None,
                source_reference: None,
                event_storming_kind: None,
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
            event_storming_relation: None,
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

    let project = SqliteProject::create(&db, &Graph::default())?;
    project.save(&graph)?;
    drop(project);
    let (project, reopened) = SqliteProject::open(&db)?;
    assert_eq!(reopened, graph);

    exchange::write(&json_path, &project.read()?)?;
    let imported = exchange::read(&json_path)?;
    assert_eq!(imported, graph);
    let recreated = SqliteProject::create(&restored, &imported)?;
    assert_eq!(recreated.read()?, graph);

    let json_before = fs::read(&json_path)?;
    assert!(exchange::write(&json_path, &Graph::default()).is_err());
    assert_eq!(fs::read(&json_path)?, json_before);
    assert!(SqliteProject::create(&db, &Graph::default()).is_err());
    assert_eq!(project.read()?, graph);
    Ok(())
}

#[test]
fn locks_aliases_until_the_editing_session_ends() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("orders.sqlite");
    let symlink = dir.path().join("symbolic.sqlite");
    let hardlink = dir.path().join("hard.sqlite");
    let project = SqliteProject::create(&db, &Graph::default())?;
    std::os::unix::fs::symlink(&db, &symlink)?;
    fs::hard_link(&db, &hardlink)?;

    for path in [&db, &symlink, &hardlink] {
        let Err(error) = SqliteProject::open(path) else {
            panic!("another editing session acquired {}", path.display());
        };
        assert!(error.to_string().contains("別のセッション"));
    }
    drop(project);
    SqliteProject::open(&symlink)?;
    Ok(())
}

pub(crate) fn legacy_fixture(path: &Path, graph: &Graph) -> Result<Vec<u8>> {
    crate::storage::write_snapshot(path, graph)?;
    Connection::open(path)?.execute_batch(
        "ALTER TABLE nodes DROP COLUMN event_storming_kind;
         ALTER TABLE edges DROP COLUMN event_storming_relation;
         DROP TABLE project_settings;
         PRAGMA user_version = 1;",
    )?;
    let mut bytes = Vec::new();
    exchange::encode(&mut bytes, graph)?;
    let mut document: serde_json::Value = serde_json::from_slice(&bytes)?;
    document["format_version"] = 1.into();
    document.as_object_mut().unwrap().remove("mode");
    Ok(serde_json::to_vec(&document)?)
}

#[test]
fn legacy_sqlite_migration_is_deferred_and_rolls_back_alongside_data() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("legacy.sqlite");
    let json_path = dir.path().join("legacy.json");
    let (graph, _, _) = sample()?;
    fs::write(&json_path, legacy_fixture(&db, &graph)?)?;
    assert_eq!(exchange::read_snapshot(&json_path)?, (1, graph.clone()));
    let connection = Connection::open(&db)?;
    connection.execute_batch(
        "CREATE TRIGGER fail_save BEFORE INSERT ON nodes
           BEGIN SELECT RAISE(ABORT, 'simulated storage failure'); END;",
    )?;
    let before = fs::read(&db)?;
    let (project, mut edited) = SqliteProject::open(&db)?;
    assert_eq!(edited, graph);
    assert_eq!(edited.mode(), ProjectMode::Free);
    assert_eq!(fs::read(&db)?, before);

    edited.set_mode(ProjectMode::Ddd);
    assert!(project.save(&edited).is_err());
    assert_eq!(fs::read(&db)?, before);
    assert_eq!(crate::storage::read_snapshot(&db)?, (1, graph));
    connection.execute_batch("DROP TRIGGER fail_save")?;
    project.save(&edited)?;
    assert_eq!(crate::storage::read_snapshot(&db)?, (2, edited));
    Ok(())
}

#[test]
fn ddd_flow_and_typed_edges_round_trip_and_validate_in_both_modes() -> Result<()> {
    use CardKind::*;
    use DddRelation::*;
    let mut graph = Graph::new(ProjectMode::Ddd);
    let cards = [
        (ViewRead, "注文画面"),
        (Actor, "会員"),
        (Command, "書籍を注文する"),
        (Aggregate, "注文"),
        (DomainEvent, "書籍が注文された"),
        (ViewRead, "注文履歴"),
        (Policy, "注文受付後に決済を依頼する"),
        (Command, "支払いを依頼する"),
        (ExternalSystem, "Stripe"),
        (DomainEvent, "支払いが完了した"),
        (Aggregate, "注文"),
    ];
    let ids = cards.map(|(kind, name)| {
        let id = Uuid::new_v4();
        graph
            .put_node(Node {
                id,
                name: name.into(),
                kind: Some("既存種別".into()),
                description: Some("説明".into()),
                source_reference: Some("参考資料".into()),
                event_storming_kind: Some(kind),
            })
            .unwrap();
        id
    });
    for (index, (source, target, relation)) in [
        (0, 1, Some(Informs)),
        (1, 2, Some(Issues)),
        (2, 3, Some(HandledBy)),
        (3, 4, Some(Emits)),
        (4, 5, Some(Updates)),
        (4, 6, Some(Triggers)),
        (6, 7, Some(Issues)),
        (7, 8, Some(Calls)),
        (8, 9, Some(Reports)),
        (9, 5, Some(Updates)),
        (5, 1, Some(Informs)),
        (4, 3, None),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(graph.relation_for(ids[source], ids[target]), relation);
        graph.put_edge(Edge {
            id: Uuid::from_u128(index as u128 + 1),
            source_id: ids[source],
            target_id: ids[target],
            label: relation.map_or("関連する", DddRelation::label).into(),
            note: Some("補足事項".into()),
            source_reference: Some("出典".into()),
            event_storming_relation: relation,
        })?;
    }
    graph.place_unpositioned();
    assert_eq!(graph.related_edge_count(ids[10]), 0);
    let original = graph.clone();
    for mode in [ProjectMode::Ddd, ProjectMode::Free] {
        graph.set_mode(mode);
        let before = graph.clone();
        let mut changed = graph.nodes()[&ids[4]].clone();
        changed.event_storming_kind = Some(ViewRead);
        let error = graph.put_node(changed).unwrap_err().to_string();
        for label in [Emits, Updates, Triggers].map(DddRelation::label) {
            assert!(error.contains(label), "{error}");
        }
        let mut edge = graph.edges()[&Uuid::from_u128(4)].clone();
        std::mem::swap(&mut edge.source_id, &mut edge.target_id);
        assert!(graph.put_edge(edge).is_err());
        assert_eq!(graph, before);
    }
    graph.set_mode(ProjectMode::Ddd);
    assert_eq!(graph, original);

    let dir = tempdir()?;
    let path = dir.path().join("ddd.lbproj");
    let project = ProjectFile::create(&path, &graph)?;
    assert_eq!(project.reload()?, graph);
    let json = dir.path().join("ddd.json");
    exchange::write(&json, &graph)?;
    assert_eq!(exchange::read_snapshot(&json)?, (2, graph));

    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&json)?)?;
    for (pointer, invalid, expected) in [
        ("/mode", "unknown", "mode"),
        (
            "/nodes/0/event_storming_kind",
            "unknown",
            "event_storming_kind",
        ),
        (
            "/edges/0/event_storming_relation",
            "unknown",
            "event_storming_relation",
        ),
        ("/edges/0/event_storming_relation", "emits", "edges[0]"),
    ] {
        let mut rejected = document.clone();
        *rejected.pointer_mut(pointer).unwrap() = invalid.into();
        fs::write(&json, rejected.to_string())?;
        let error = format!("{:#}", exchange::read(&json).unwrap_err());
        assert!(error.contains(expected), "{error}");
    }
    document.as_object_mut().unwrap().remove("mode");
    fs::write(&json, document.to_string())?;
    assert!(format!("{:#}", exchange::read(&json).unwrap_err()).contains("mode"));
    Ok(())
}

#[test]
fn failed_save_rolls_back_and_a_retry_can_remove_persisted_edges_and_position() -> Result<()> {
    let dir = tempdir()?;
    let db = dir.path().join("orders.sqlite");
    let (graph, parent, child) = sample()?;
    let project = SqliteProject::create(&db, &graph)?;
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
    let (_, reopened) = SqliteProject::open(&db)?;
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
    drop(SqliteProject::create(&db, &Graph::default())?);
    Connection::open(&db)?.pragma_update(None, "user_version", 99)?;
    let before = fs::read(&db)?;
    let Err(error) = SqliteProject::open(&db) else {
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

#[test]
fn container_round_trip_restores_all_records_and_full_precision_coordinates() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("orders.lbproj");
    let (mut graph, parent, child) = sample()?;
    graph.put_position(Position {
        node_id: parent,
        x: f64::from_bits(1),
        y: -87.138_910_169_595_36,
    })?;
    graph.put_position(Position {
        node_id: child,
        x: 1.000_000_000_000_000_2,
        y: f64::MAX,
    })?;

    let project = ProjectFile::create(&path, &Graph::default())?;
    assert_eq!(project.reload()?, Graph::default());
    project.save(&graph)?;
    drop(project);

    let (_, actual) = ProjectFile::open(&path)?;
    assert_eq!(actual, graph);
    for (id, position) in graph.positions() {
        assert_eq!(actual.positions()[id].x.to_bits(), position.x.to_bits());
        assert_eq!(actual.positions()[id].y.to_bits(), position.y.to_bits());
    }
    Ok(())
}

#[test]
fn container_locks_symlinks_and_filesystem_aliases_across_atomic_replacement() -> Result<()> {
    use std::os::unix::fs::symlink;

    let dir = tempdir()?;
    let file = dir.path().join("Café.lbproj");
    let symlink_path = dir.path().join("alias.lbproj");
    let case_variant = dir.path().join("CAFÉ.LBPROJ");
    let decomposed = dir.path().join("Cafe\u{301}.lbproj");
    let project = ProjectFile::create(&file, &Graph::default())?;
    symlink(&file, &symlink_path)?;
    let (graph, _, _) = sample()?;
    for current in [&Graph::default(), &graph] {
        project.save(current)?;
        assert_eq!(project.saved_graph()?, *current);
        for path in [&file, &symlink_path, &case_variant, &decomposed] {
            // Case and Unicode normalization depend on the volume used to run the test.
            if path.exists() {
                let Err(error) = ProjectFile::open(path) else {
                    panic!("opened an already locked project at {}", path.display());
                };
                assert!(format!("{error:#}").contains("別のセッション"));
            }
        }
    }
    drop(project);
    assert_eq!(ProjectFile::open(&symlink_path)?.1, graph);
    Ok(())
}

#[test]
fn container_detects_hardlinks_on_open_and_save() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("orders.lbproj");
    let alias = dir.path().join("alias.lbproj");
    let project = ProjectFile::create(&path, &Graph::default())?;
    let before = fs::read(&path)?;
    fs::hard_link(&path, &alias)?;
    let (graph, _, _) = sample()?;
    let error = project.save(&graph).unwrap_err();
    assert!(format!("{error:#}").contains("ハードリンク"));
    assert_eq!(fs::read(&path)?, before);
    drop(project);
    let Err(error) = ProjectFile::open(&alias) else {
        panic!("opened a hard-linked project");
    };
    assert!(format!("{error:#}").contains("ハードリンク"));
    fs::remove_file(&alias)?;
    assert_eq!(ProjectFile::open(&path)?.1, Graph::default());
    Ok(())
}

#[test]
fn container_refuses_in_place_edits_and_same_content_replacements() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("orders.lbproj");
    let alternate = dir.path().join("alternate.lbproj");
    let recovered = dir.path().join("recovered.lbproj");
    let (graph, parent, _) = sample()?;
    let project = ProjectFile::create(&path, &graph)?;
    let mut changes = graph.clone();
    changes.remove_node(parent);
    drop(ProjectFile::create(&alternate, &changes)?);

    let original = fs::read(&path)?;
    let external = fs::read(&alternate)?;
    fs::write(&path, &external)?;
    assert!(format!("{:#}", project.save(&changes).unwrap_err()).contains("外部変更"));
    assert_eq!(fs::read(&path)?, external);
    assert_eq!(project.saved_graph()?, graph);

    fs::write(&alternate, &original)?;
    fs::rename(&alternate, &path)?;
    assert!(format!("{:#}", project.save(&changes).unwrap_err()).contains("外部変更"));
    assert_eq!(fs::read(&path)?, original);
    assert!(ProjectFile::create(&path, &changes).is_err());
    let copy = ProjectFile::create(&recovered, &changes)?;
    assert_eq!(copy.reload()?, changes);
    Ok(())
}

#[test]
#[ignore = "run separately to measure container I/O"]
fn measure_container_io_for_a_representative_graph() -> Result<()> {
    use std::time::Instant;

    const NODE_COUNT: usize = 10_000;
    let dir = tempdir()?;
    let path = dir.path().join("representative.lbproj");
    let mut graph = Graph::default();
    let ids: Vec<_> = (0..NODE_COUNT).map(|_| Uuid::new_v4()).collect();
    for (index, &id) in ids.iter().enumerate() {
        graph.put_node(Node {
            id,
            name: format!("業務概念 {index}"),
            kind: Some("業務概念".to_owned()),
            description: Some("グラフの読み込みと保存の計測に使用する合成データ。".to_owned()),
            source_reference: Some(format!("注文仕様書 §{index}")),
            event_storming_kind: None,
        })?;
        graph.put_position(Position {
            node_id: id,
            x: (index % 100) as f64 * 190.25,
            y: (index / 100) as f64 * 115.25,
        })?;
    }
    for (index, &source_id) in ids.iter().enumerate() {
        for (offset, label) in [(1, "参照する"), (17, "関連する"), (100, "含む")] {
            graph.put_edge(Edge {
                id: Uuid::new_v4(),
                source_id,
                target_id: ids[(index + offset) % NODE_COUNT],
                label: label.to_owned(),
                note: Some("関係の注記".to_owned()),
                source_reference: Some(format!("注文仕様書 §{index}")),
                event_storming_relation: None,
            })?;
        }
    }

    let project = ProjectFile::create(&path, &Graph::default())?;
    let start = Instant::now();
    project.save(&graph)?;
    let save = start.elapsed();
    drop(project);

    let start = Instant::now();
    let (_, restored) = ProjectFile::open(&path)?;
    let open = start.elapsed();
    assert_eq!(restored, graph);
    println!(
        "{} nodes, {} edges, {} bytes, save {:?}, open {:?}",
        graph.nodes().len(),
        graph.edges().len(),
        fs::metadata(path)?.len(),
        save,
        open
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn failed_temporary_file_creation_and_atomic_replacement_keep_previous_version() -> Result<()> {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let dir = tempdir()?;
    let path = dir.path().join("orders.lbproj");
    let project = ProjectFile::create(&path, &Graph::default())?;
    let previous = fs::read(&path)?;
    let (graph, _, _) = sample()?;

    let original_permissions = fs::metadata(dir.path())?.permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_mode(0o500);
    fs::set_permissions(dir.path(), readonly)?;
    let result = project.save(&graph);
    fs::set_permissions(dir.path(), original_permissions)?;
    assert!(format!("{:#}", result.unwrap_err()).contains("一時 ZIP を作成できません"));
    assert_eq!(fs::read(&path)?, previous);

    let set_flag = |flag| {
        Command::new("/usr/bin/chflags")
            .arg(flag)
            .arg(&path)
            .status()
    };
    assert!(set_flag("uchg")?.success());
    let result = project.save(&graph);
    assert!(set_flag("nouchg")?.success());
    assert!(format!("{:#}", result.unwrap_err()).contains("置き換えられません"));
    assert_eq!(fs::read(&path)?, previous);
    assert_eq!(project.saved_graph()?, Graph::default());

    project.save(&graph)?;
    assert_eq!(project.reload()?, graph);
    Ok(())
}
