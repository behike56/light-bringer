# tech_stack_REST_version_alpha

## Elm ＋ Roc × Neo4j × Tauri ― REST 版・詳細設計サマリ

| レイヤ | 主要役割 | 技術 | 変更点／ポイント |
| --- | --- | --- | --- |
| **フロント**  | UI・UX | **Elm 0.19.1** + elm/http + elm/json | GraphQL 依存を排除し、`elm/http` で **REST 呼び出し**。<br>型安全性は **Decoder/Encoder** に集約し、`Result` チェーンでハンドリング。 |
| **API**   | 業務ロジック／永続化境界 | **Roc nightly** | ① **`roc_http`**（あるいは rocの `cli serve` β）で **REST エンドポイント**実装。<br>② JSON（`roc/json`）を入出力に。<br>③ Neo4j 操作は **FFI 経由で Rust ラッパ**を呼び出す（下記）。 |
| **DB** | グラフデータストア | **Neo4j 5.x / AuraDB** | Cypher クエリを Rust ラッパで共通管理。REST 化に伴いスキーマ駆動生成は不要。 |
| **Shell** | デスクトップ配布 | **Tauri 2 + Rust 1.77** | Elm ビルド成果物を `frontendDist`、Roc API を **サイドカープロセス**として起動 or Rust に静的リンク。 |

### 1. フロント (Elm) ― REST 化の実装指針

| 目的 | 実装パターン |
| --- | --- |
| **API 呼び出し共通化** | `Api.elm` モジュールに `get : String -> Cmd Msg` などをラップ。Base URL は `flags` で注入し、Tauri dev/prod を切替。 |
| **レスポンス型安全化**   | GraphQL 型生成が無くなるため、**各エンドポイントごとに Decoder** を手書き。Success/Failure を `Result` で一元管理。 |
| **ポーリング／Push**  | 簡易に済ませるなら `Browser.Events.onAnimationFrame` + `Cmd.batch` でポーリング。将来的に WebSocket が欲しければ Roc 側に追加。 |

### 2. Roc API ― REST サーバ構成
