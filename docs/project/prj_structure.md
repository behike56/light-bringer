---
title: プロジェクト構成
version: alpha
marp: true
---

## プロジェクト構成の基本形

モノレポとして１つにまとめる

``` zsh
root/
├─ .github/workflows/
│   └─ ci.yml  # pnpm + roc + cargo をジョブ分割
├─ apps/
│   ├─ elm-ui/       # package.json, elm.json
│   ├─ roc-api/      # src/*.roc
│   └─ tauri-shell/  # src-tauri/ (Cargo.toml)
├─ docker/
│   └─ neo4j-compose.yml   # Neo4j だけ切り出し
└─ shared/ # POCでは未使用
    └─ schema.graphql  # POCでは未使用
```

## 詳細な構成

``` zsh
$ pwd
/Volumes/CODE/00_Development/000-proj/light-bringer

$ tree -L 4
.
├── apps
│   ├── elm-ui
│   │   ├── dist
│   │   │   └── main.js
│   │   ├── elm-stuff
│   │   │   └── 0.19.1
│   │   ├── elm.js
│   │   ├── elm.json
│   │   ├── node_modules
│   │   │   ├── elm -> .pnpm/elm@0.19.1-6/node_modules/elm
│   │   │   └── elm-live -> .pnpm/elm-live@4.0.2/node_modules/elm-live
│   │   ├── package.json
│   │   ├── pnpm-lock.yaml
│   │   ├── src
│   │   │   └── Main.elm
│   │   └── static
│   │       ├── favicon.ico
│   │       └── index.html
│   ├── roc-api
│   │   ├── kg-backend
│   │   └── src
│   │       ├── backend
│   │       └── backend.roc
│   └── tauri-shell
│       ├── node_modules
│       │   └── @tauri-apps
│       ├── package-lock.json
│       ├── package.json
│       ├── pnpm-lock.yaml
│       └── src-tauri
│           ├── build.rs
│           ├── capabilities
│           ├── Cargo_old.toml
│           ├── Cargo.lock
│           ├── Cargo.toml
│           ├── gen
│           ├── icons
│           ├── src
│           ├── target
│           ├── tauri.conf_old.json
│           └── tauri.conf.json
├── docker
│   ├── data
│   │   ├── databases
│   │   │   ├── neo4j
│   │   │   ├── store_lock
│   │   │   └── system
│   │   ├── dbms
│   │   │   └── auth.ini
│   │   ├── server_id
│   │   └── transactions
│   │       ├── neo4j
│   │       └── system
│   └── neo4j-compose.yml
├── docs
│   ├── design
│   │   ├── tech_stack_REST_version_alpha.md
│   │   └── teck_stack_GraphQL_version_alpha.md
│   ├── POC
│   │   ├── 0_lang_environment.md
│   │   └── 1_develop.md
│   └── project
│       └── prj_structure.md
├── README.md
└── shard
    └── schema.graphql

37 directories, 30 files
```
