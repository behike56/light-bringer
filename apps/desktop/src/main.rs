mod exchange;
mod graph;
mod neo4j;
mod project;
mod storage;
mod ui;

fn main() -> std::process::ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some("neo4j".as_ref()) {
        return neo4j::run_cli();
    }
    ui::run();
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod project_tests;
