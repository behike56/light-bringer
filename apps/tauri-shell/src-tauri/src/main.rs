#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{process::Command, sync::OnceLock};

static BACKEND: OnceLock<std::process::Child> = OnceLock::new();

fn main() {
    // Roc バイナリを spawn（ポートは 4000 固定）
    let child = Command::new("../../roc-api/kg-backend/backend")
        .spawn()
        .expect("failed to start backend");
    BACKEND.set(child).unwrap();

    app_lib::run();
}
