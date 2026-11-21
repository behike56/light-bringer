use std::{cell::Cell, env, fs, sync::Barrier, thread};

use reqwest::Url;
use tempfile::tempdir;

use super::*;
use crate::project_tests::{legacy_fixture, sample};

#[test]
fn source_formats_produce_the_same_snapshot_without_opening_an_editing_session() -> Result<()> {
    let dir = tempdir()?;
    let (graph, _, _) = sample()?;
    let container = dir.path().join("orders.lbproj");
    let sqlite = dir.path().join("orders.sqlite");
    let json = dir.path().join("orders.json");
    let project = project::ProjectFile::create(&container, &graph)?;
    let before = fs::read(&container)?;

    let legacy_json = legacy_fixture(&sqlite, &graph)?;
    fs::write(&json, legacy_json)?;
    for (file, version) in [(&container, 2), (&sqlite, 1), (&json, 1)] {
        let (found, loaded) = read_source(file)?;
        assert_eq!(found, version);
        assert_eq!(loaded, graph);
        assert_eq!(graph_hash(&loaded)?, graph_hash(&graph)?);
    }
    assert_eq!(project.saved_graph()?, graph);
    assert_eq!(fs::read(&container)?, before);

    let mut reordered: Value = serde_json::from_slice(&fs::read(&json)?)?;
    for key in ["nodes", "edges", "node_positions"] {
        reordered[key].as_array_mut().unwrap().reverse();
    }
    fs::write(&json, serde_json::to_vec(&reordered)?)?;
    assert_eq!(graph_hash(&read_source(&json)?.1)?, graph_hash(&graph)?);

    reordered["edges"][0]["target_id"] = json!(Uuid::new_v4());
    fs::write(&json, serde_json::to_vec(&reordered)?)?;
    assert!(read_source(&json).is_err());
    Ok(())
}

fn database(role: &str, project_id: Uuid) -> Result<Neo4j> {
    let url = Url::parse(&env::var("NEO4J_TEST_QUERY_URL").context(
        "NEO4J_TEST_QUERY_URL に使い捨て DB の /db/<name>/query/v2 を指定してください",
    )?)?;
    Ok(Neo4j {
        client: QueryClient::connect(url, role)?,
        project_id,
    })
}

#[test]
#[ignore = "requires a disposable Neo4j database over HTTPS"]
fn neo4j_publication_is_atomic_idempotent_and_project_scoped() -> Result<()> {
    let project_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();
    let admin = database("ADMIN", project_id)?;
    admin.init()?;
    admin.init()?;
    let db = database("PUBLISH", project_id)?;
    let other = database("PUBLISH", other_id)?;
    let reader = database("READ", project_id)?;
    let other_reader = database("READ", other_id)?;
    db.require_schema()?;
    assert!(reader.head()?.is_none());

    let (graph, parent, child) = sample()?;
    let first_request = Uuid::new_v4();
    let first = db.publish(&graph, 0, first_request)?;
    assert_eq!(first.receipt.revision, 1);
    assert!(!first.replayed);
    assert_eq!(reader.snapshot()?.1, graph);
    assert_eq!(reader.receipt(first_request)?, Some(first.receipt));

    let mut changed_nodes: Vec<_> = graph.nodes().values().cloned().collect();
    for node in &mut changed_nodes {
        node.kind = None;
        node.description = None;
        node.source_reference = None;
    }
    let mut kept_edge = graph.edges().values().next().unwrap().clone();
    kept_edge.source_id = child;
    kept_edge.target_id = child;
    kept_edge.note = None;
    kept_edge.source_reference = None;
    let changed = Graph::from_records(
        ProjectMode::Ddd,
        changed_nodes,
        vec![kept_edge],
        vec![Position {
            node_id: parent,
            x: -0.0,
            y: 197.03567891234567,
        }],
    )?;
    assert!(db.publish(&changed, 0, first_request).is_err());
    assert!(db.publish(&graph, 1, first_request).is_err());

    assert_eq!(
        other.publish(&graph, 0, Uuid::new_v4())?.receipt.revision,
        1
    );
    let second = db.publish(&changed, 1, Uuid::new_v4())?;
    assert_eq!(second.receipt.revision, 2);
    let replay = db.publish(&graph, 0, first_request)?;
    assert!(replay.replayed);
    assert_eq!(replay.receipt.revision, 1);
    assert_eq!(reader.snapshot()?.1, changed);

    // A foreign relationship prevents a stale node from being deleted. By this point the
    // outgoing snapshot's own relationship has already been deleted in the transaction.
    let endpoints = json!({ "own": project_id, "other": other_id, "node": parent });
    admin.client.query(
        "MATCH (a:LBNode {project_id: $own, id: $node}), (b:LBNode {project_id: $other, id: $node})
         CREATE (a)-[:LB_TEST_LINK]->(b)",
        endpoints.clone(),
    )?;
    let empty = Graph::default();
    let failed = Uuid::new_v4();
    assert!(db.publish(&empty, 2, failed).is_err());
    assert!(reader.receipt(failed)?.is_none());
    assert_eq!(reader.snapshot()?.1, changed);
    admin.client.query(
        "MATCH (:LBNode {project_id: $own, id: $node})-[r:LB_TEST_LINK]->
               (:LBNode {project_id: $other, id: $node})
         DELETE r",
        endpoints,
    )?;

    let competing = [Uuid::new_v4(), Uuid::new_v4()];
    let barrier = Barrier::new(competing.len());
    let results = thread::scope(|scope| {
        let workers = competing.map(|id| {
            let barrier = &barrier;
            let db = &db;
            let empty = &empty;
            scope.spawn(move || {
                barrier.wait();
                db.publish(empty, 2, id)
            })
        });
        workers.map(|worker| worker.join().unwrap())
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let conflict = results.into_iter().find_map(Result::err).unwrap();
    assert!(
        conflict
            .to_string()
            .contains("公開リビジョンが競合しました"),
        "{conflict:#}"
    );
    assert_eq!(reader.snapshot()?.0.revision, 3);
    assert_eq!(reader.snapshot()?.1, empty);
    assert_eq!(other_reader.snapshot()?.1, graph);
    Ok(())
}

#[test]
#[ignore = "requires a disposable Neo4j database over HTTPS"]
fn neo4j_search_returns_the_directed_ddd_path_and_retries_if_publication_changes() -> Result<()> {
    let project_id = Uuid::new_v4();
    database("ADMIN", project_id)?.init()?;
    let db = database("PUBLISH", project_id)?;
    let reader = database("READ", project_id)?;
    let ids = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let mut graph = Graph::new(ProjectMode::Ddd);
    for (id, kind) in ids.into_iter().zip([
        CardKind::Command,
        CardKind::Aggregate,
        CardKind::DomainEvent,
        CardKind::Policy,
    ]) {
        graph.put_node(Node {
            id,
            name: kind.label().to_owned(),
            kind: None,
            description: None,
            source_reference: Some(format!("仕様書 {}", kind.key())),
            event_storming_kind: Some(kind),
        })?;
    }
    for ([source_id, target_id], kind) in ids.array_windows().copied().zip([
        DddRelation::HandledBy,
        DddRelation::Emits,
        DddRelation::Triggers,
    ]) {
        graph.put_edge(Edge {
            id: Uuid::new_v4(),
            source_id,
            target_id,
            label: kind.label().to_owned(),
            note: None,
            source_reference: Some("仕様書の関係".to_owned()),
            event_storming_relation: Some(kind),
        })?;
    }
    db.publish(&graph, 0, Uuid::new_v4())?;
    for depth in 1..=3 {
        let (head, paths) = reader.search(ids[0], depth, 1)?;
        assert_eq!(head.revision, 1);
        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(
            path.nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
            ids[..=usize::from(depth)]
        );
        assert!(
            path.nodes
                .iter()
                .all(|node| node.source_reference.is_some())
        );
        assert_eq!(path.edges.len(), usize::from(depth));
        assert!(
            path.edges
                .iter()
                .all(|edge| edge.source_reference.is_some())
        );
    }
    assert!(reader.search(ids[3], 3, 1)?.1.is_empty());

    let read_count = Cell::new(0);
    let (head, ()) = reader.stable_read(|tx| {
        let count = read_count.get();
        read_count.set(count + 1);
        tx.query(cypher::READ_NODES, reader.scope())?;
        if count == 0 {
            db.publish(&Graph::default(), 1, Uuid::new_v4())?;
        }
        Ok(())
    })?;
    assert_eq!(head.revision, 2);
    assert_eq!(read_count.get(), 2);
    assert!(reader.search(ids[0], 3, 1)?.1.is_empty());
    Ok(())
}
