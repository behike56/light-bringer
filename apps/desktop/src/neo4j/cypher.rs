pub(super) const CONSTRAINTS: &[&str] = &[
    "CREATE CONSTRAINT IF NOT EXISTS FOR (v:LBNeo4jSchema) REQUIRE v.name IS UNIQUE",
    "CREATE CONSTRAINT IF NOT EXISTS FOR (p:LBProject) REQUIRE p.project_id IS UNIQUE",
    "CREATE CONSTRAINT IF NOT EXISTS FOR (n:LBNode) REQUIRE (n.project_id, n.id) IS UNIQUE",
    "CREATE CONSTRAINT IF NOT EXISTS FOR ()-[r:LB_REL]-() REQUIRE (r.project_id, r.id) IS UNIQUE",
    "CREATE CONSTRAINT IF NOT EXISTS FOR (r:LBPublishRequest)
     REQUIRE (r.project_id, r.request_id) IS UNIQUE",
];

pub(super) const SCHEMA: &str = "
    OPTIONAL MATCH (v:LBNeo4jSchema {name: 'light-bringer'})
    RETURN v.version
";

pub(super) const INSTALL_SCHEMA: &str = "
    MERGE (v:LBNeo4jSchema {name: 'light-bringer'})
    ON CREATE SET v.version = $version
    RETURN v.version
";

pub(super) const HEAD: &str = "
    OPTIONAL MATCH (p:LBProject {project_id: $project_id})
    RETURN p{.revision, .mode, .content_hash}
";

// The temporary property acquires the project write lock before the revision is
// read. Neo4j holds that lock until commit or rollback, including after REMOVE.
pub(super) const LOCK_PROJECT: &str = "
    MERGE (p:LBProject {project_id: $project_id})
    ON CREATE SET p.revision = 0
    SET p._lb_write_lock = true
    REMOVE p._lb_write_lock
    RETURN p.revision
";

pub(super) const REQUEST: &str = "
    OPTIONAL MATCH (r:LBPublishRequest {project_id: $project_id, request_id: $request_id})
    RETURN r{.expected_revision, .content_hash, .revision}
";

pub(super) const UPSERT_NODES: &str = "
    UNWIND $rows AS row
    MERGE (n:LBNode {project_id: $project_id, id: row.id})
    SET n = row, n.project_id = $project_id, n._lb_revision = $revision
    RETURN count(n)
";

pub(super) const DELETE_MOVED_EDGES: &str = "
    UNWIND $rows AS row
    MATCH (s)-[r:LB_REL {project_id: $project_id, id: row.id}]->(t)
    WHERE s:LBNode AND t:LBNode AND s.project_id = $project_id AND t.project_id = $project_id
      AND (s.id <> row.source_id OR t.id <> row.target_id)
    DELETE r
";

pub(super) const UPSERT_EDGES: &str = "
    UNWIND $rows AS row
    MATCH (s:LBNode {project_id: $project_id, id: row.source_id}),
          (t:LBNode {project_id: $project_id, id: row.target_id})
    WHERE s._lb_revision = $revision AND t._lb_revision = $revision
    MERGE (s)-[r:LB_REL {project_id: $project_id, id: row.id}]->(t)
    SET r = row.properties, r.project_id = $project_id, r._lb_revision = $revision
    RETURN count(r)
";

pub(super) const CHECK_EDGE_SCOPE: &str = "
    MATCH (s)-[r:LB_REL {project_id: $project_id}]->(t)
    WHERE NOT coalesce(s:LBNode AND t:LBNode AND s.project_id = $project_id
                       AND t.project_id = $project_id, false)
    RETURN count(r)
";

pub(super) const DELETE_STALE_EDGES: &str = "
    MATCH ()-[r:LB_REL {project_id: $project_id}]->()
    WHERE r._lb_revision IS NULL OR r._lb_revision <> $revision
    DELETE r
";

pub(super) const DELETE_STALE_NODES: &str = "
    MATCH (n:LBNode {project_id: $project_id})
    WHERE n._lb_revision IS NULL OR n._lb_revision <> $revision
    DELETE n
";

pub(super) const FINISH_PUBLICATION: &str = "
    MATCH (p:LBProject {project_id: $project_id})
    SET p.revision = $revision, p.mode = $mode, p.content_hash = $content_hash
    CREATE (:LBPublishRequest {
        project_id: $project_id, request_id: $request_id, expected_revision: $expected_revision,
        revision: $revision, content_hash: $content_hash
    })
    RETURN p.revision
";

pub(super) const READ_NODES: &str = "
    MATCH (n:LBNode {project_id: $project_id})
    RETURN properties(n)
";

pub(super) const READ_EDGES: &str = "
    MATCH (s)-[r:LB_REL {project_id: $project_id}]->(t)
    RETURN r{.*, source_id: s.id, target_id: t.id},
           coalesce(s:LBNode AND t:LBNode AND s.project_id = $project_id
                    AND t.project_id = $project_id, false)
";

const SEARCH_START: &str = "
    MATCH path = (:LBNode {project_id: $project_id, id: $command_id, event_storming_kind: 'command'})
";

const SEARCH_STEPS: [&str; 3] = [
    "-[:LB_REL {project_id: $project_id, event_storming_relation: $relations[0]}]->
        (:LBNode {project_id: $project_id, event_storming_kind: $kinds[0]})",
    "-[:LB_REL {project_id: $project_id, event_storming_relation: $relations[1]}]->
        (:LBNode {project_id: $project_id, event_storming_kind: $kinds[1]})",
    "-[:LB_REL {project_id: $project_id, event_storming_relation: $relations[2]}]->
        (:LBNode {project_id: $project_id, event_storming_kind: $kinds[2]})",
];

const SEARCH_RESULT: &str = "
    RETURN [n IN nodes(path) | n{.id, .name, .source_reference, .event_storming_kind}],
           [r IN relationships(path) | r{.id, .label, .source_reference, .event_storming_relation}]
    LIMIT $limit
";

pub(super) fn search(depth: u8) -> String {
    let pattern = SEARCH_STEPS[..usize::from(depth)].join("\n");
    format!("{SEARCH_START}{pattern}{SEARCH_RESULT}")
}
