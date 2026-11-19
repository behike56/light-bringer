mod exchange;
mod graph;
mod storage;
mod ui;

fn main() {
    ui::run();
}

#[cfg(test)]
mod project_tests;
