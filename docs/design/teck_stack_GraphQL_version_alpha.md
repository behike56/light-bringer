---
title: 技術スタック
version: alpha
marp: true
---

## 1. 全体像

┌────────────────────────┐
│  Tauri Shell (Rust)    │  ↩︎ 起動/終了を司るネイティブ層
│  ├─ spawn Roc BIN ─┐   │
│  └─ serve Elm SPA ─┼─────▶ ① UI  : Elm SPA
└───────────────────┘│        • elm-graphql で型安全なクエリを生成
                     │        • WebView 内で GraphQL over HTTP/WSS
                     │
                     ▼
② API : Roc GraphQL Server
   • `basic-webserver` ＋ `roc-gql` _or_ FFI → `async-graphql`（Rust）:contentReference[oaicite:0]{index=0}  
   • リゾルバから Neo4j へ Bolt 接続 (`neo4rs`) :contentReference[oaicite:1]{index=1}
   • Roc の効果システムで I/O を分離

③ DB : Neo4j  
   • Docker 同梱
   • Bolt 7687/TCP を Roc から直結

Tauri の構成上、フロントは HTML/CSS/JS（Elm が生成）を表示し、**バックエンドは Rust バイナリ（今回は Roc を生成物として起動）**という 2-プロセス構成が最適です
（Tauri README にある “backend of the application is a Rust-sourced binary with an API that the front-end can interact with”
github.com
）。

---

## 2. Elm フロントエンド

| 項目 | 詳細 |
| --- | --- |
| GraphQL クライアント | **`dillonkearns/elm-graphql`** でスキーマから Elm 型・エンコーダ/デコーダを自動生成。API 変更はコンパイルエラーで検知 ([github.com][1]) |
| ビュー層 | The Elm Architecture (TEA)。Ports 無しで素直に fetch / WebSocket を呼ぶだけで OK。 |
| リアルタイム | `elm-graphql` が標準で Subscriptions を生成 ⇒ Roc 側で `async-graphql` の WS Transport を有効化すれば差分更新が可能。 |
| Tauri 連携 | `tauri.conf.json` の `distDir` を Elm build 出力へ設定。バックエンドの待ち受けポート（例: 127.0.0.1:4000）を `beforeBuildCommand` で注入すれば CORS 不要。 |
| ホットリロード | `elm-watch` や Vite ではなく、Tauri Dev Server の proxy 機能でフロント単体ホットリロードが軽快 ([discourse.elm-lang.org][2])。 |

[1]: https://github.com/dillonkearns/elm-graphql?utm_source=chatgpt.com "dillonkearns/elm-graphql - GitHub"
[2]: https://discourse.elm-lang.org/t/announcing-tauri-elm-app-a-template-for-building-desktop-apps-with-tauri/9460 "Announcing \"Tauri Elm App\" - A template for building desktop apps with Tauri - Show and Tell - Elm"

---

## 3. Roc GraphQL サーバ

| レイヤ | 選択肢 | メリット |
| --- | --- | --- |
| GraphQL 実装 | **A. roc-gql**（実験的）<br>**B. async-graphql を Rust 側で実装し FFI 呼び出し** | A は純 Roc で完結し小バイナリ。B は成熟度が高く、Subscriptions・Federation まで完備 ([async-graphql.github.io][1])。 |
| HTTP/WebSocket | `basic-webserver` （hyper＋tokio）([github.com][2])|                                                                                            |
| Neo4j Driver | Rust Crate **`neo4rs`** （Bolt v5.x 対応、async/await）([github.com][3])| |
| 依存注入 | Roc の Record-of-Caps や Rust 側で `Arc<AppState>` を用いてドライバ共有。| |
| GraphQL スキーマ例  | `graphql type Node { id: ID! label: String! props: Json! } type Edge { id: ID! src: ID! dst: ID! label: String! } type Query { node(id: ID!): Node graph(limit: Int = 100): [Node!]! } type Mutation { mergeNode(label: String!, props: Json!): Node! link(src: ID!, dst: ID!, label: String!): Edge! }` | |

[3]: https://github.com/neo4j-labs/neo4rs "GitHub - neo4j-labs/neo4rs: Rust driver for Neo4j"

---

## 4. Neo4j の組み込み方

| 環境 | 方法 |
| --- | --- |
| **開発** | `docker compose` で `neo4j:5` → `Roc`（Depends\_on）。データボリュームは `./data` をマウントで永続化。 |
| **デスクトップ配布** | ① **同梱**: `neo4j-admin server` を Tauri の `sidecar` として起動（ライセンスは Neo4j Desktop CE と同等）。<br>② **外部**: Aura、AuraDS、NEO4j Cloud Free Tier へ接続するオプションを UI で切替。 |
| Migrations   | Roc 起動時に Cypher ファイルを `CALL apoc.cypher.runFile()` で自動適用。 |

---

## 5. Tauri ランタイム統合

| ステップ | コマンド／設定 |
| --- | --- |
| ① ビルド | `roc build src/Main.roc --output ./bin/kg-backend` |
| ② Tauri Rust コード | `Command::new("kg-backend").arg("--port=4000")` を `tauri::plugin::process::spawn()` で起動。終了時は drop で kill。 |
| ③ セキュリティ | `tauri.conf.json -> security.dangerousRemoteDomainIpcAccess = false` で外部アクセス遮断。 |
| ④ バンドル | `tauri build --target universal-apple-darwin` 等。Neo4j 同梱時は Resources に DB フォルダを配置。 |

---

## 6. リポジトリ構成

モノレポとして１つにまとめる

``` zsh
root/
├─ .github/workflows/
│   └─ ci.yml  # pnpm + roc + cargo をジョブ分割
├─ apps/
│   ├─ elm-ui/       # package.json, elm.json
│   ├─ roc-api/      # src/*.roc
│   └─ tauri-shell/  # src-tauri/ (Cargo.toml)
└─ shared/
    └─ schema.graphql  # Single source of truth
```

GraphQL スキーマを shared/ に置き、CI で

1. elm-graphql 生成物を Elm へコピー
2. Rust (async-graphql) で include_str! してビルド
3. Roc 側は roc-gql で同スキーマをパース

生成アーティファクトをコミットせず、CI 内キャッシュ or GitHub Release Asset に置くとクリーン。
