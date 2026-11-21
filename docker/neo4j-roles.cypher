// Run against system in Neo4j Enterprise. Assign roles to provisioned users separately.
CREATE ROLE lb_neo4j_publisher IF NOT EXISTS;
GRANT ACCESS ON DATABASE neo4j TO lb_neo4j_publisher;
GRANT MATCH {*} ON GRAPH neo4j TO lb_neo4j_publisher;
GRANT WRITE ON GRAPH neo4j TO lb_neo4j_publisher;
GRANT NAME MANAGEMENT ON DATABASE neo4j TO lb_neo4j_publisher;

CREATE ROLE lb_neo4j_reader IF NOT EXISTS;
GRANT ACCESS ON DATABASE neo4j TO lb_neo4j_reader;
GRANT MATCH {*} ON GRAPH neo4j TO lb_neo4j_reader;
DENY WRITE ON GRAPH neo4j TO lb_neo4j_reader;
