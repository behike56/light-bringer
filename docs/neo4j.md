# Neo4j に保存済みグラフを公開する

Light Bringer の保存済みグラフを CLI から Neo4j に公開し、公開リビジョンを読み戻せます。ローカルの保存と再読込はオフラインで行えます。公開のデータモデルと導入段階は [Neo4j 導入設計](design/neo4j-integration-design.md) を参照してください。

接続先を新設する場合は、[サーバーの構築・復旧手順](neo4j-server.md) に沿って構築してください。

## 接続を設定する

Neo4j 5.26 以降の Query API を、クライアントが信頼する証明書を使った HTTPS で利用します。接続先に単一インスタンスを指定します。プライベート CA を使う場合は、PEM 形式の CA 証明書のパスを環境変数 `NEO4J_CA_CERT` に設定します。 [Neo4j Query API のトランザクション](https://neo4j.com/docs/query-api/current/transactions/)

実行する操作に合わせて、実行環境に認証情報を設定します。

| 用途 | 環境変数 | 操作 |
| --- | --- | --- |
| スキーマの初期化 | `NEO4J_ADMIN_USER`、`NEO4J_ADMIN_PASSWORD` | `init` |
| 公開 | `NEO4J_PUBLISH_USER`、`NEO4J_PUBLISH_PASSWORD` | `publish` |
| 閲覧 | `NEO4J_READ_USER`、`NEO4J_READ_PASSWORD` | `status`、`receipt`、`export`、`verify`、`search` |

運用時は公開用と閲覧用に別アカウントを割り当て、Neo4j 側で閲覧用の書き込みを禁止します。プロジェクトごとに閲覧者が異なる場合は、データベースやインスタンスの分離などでアクセスを制御します。以下の `project_id` は公開・検索を対象グラフに絞るための識別子です。 [Neo4j の組み込みロールと権限](https://neo4j.com/docs/operations-manual/current/authentication-authorization/built-in-roles/)

リポジトリのルートで次を実行します。配布済みの実行ファイルを使う場合は、ビルドを省略して `lb` に実行ファイルのパスを指定します。`--url` と `--database` は利用する接続先に置き換えてください。

```sh
cargo build --locked --manifest-path apps/desktop/Cargo.toml
lb=apps/desktop/target/debug/light-bringer
target=orders-neo4j.json

"$lb" neo4j --target "$target" register --url https://neo4j.example.com:7473 --database neo4j
"$lb" neo4j --target "$target" init
```

`register` はプロジェクト UUID を発行し、接続先とともに新規ファイルに保存します。設定ファイルは同じプロジェクトを再公開するたびに使います。元ファイルを移動した場合も同じ設定を使い、コピーを独立して公開したい場合は新しい設定を作ります。

## 公開と照合

デスクトップ版の「JSON 書出」で作ったファイル、保存済みの `.lbproj`、旧 `.sqlite` を使えます。`inspect` で入力、公開先、モード、件数を確認します。`status` の `result.revision` を期待値として指定します。初回は `0` です。

```sh
source_file=orders.json
"$lb" neo4j --target "$target" inspect "$source_file"
"$lb" neo4j --target "$target" status

expected_revision=0
request_id=$(uuidgen)
"$lb" neo4j --target "$target" publish "$source_file" \
  --expected-revision "$expected_revision" --request-id "$request_id"
"$lb" neo4j --target "$target" verify "$source_file"
"$lb" neo4j --target "$target" export orders-from-neo4j.json
```

`publish` の成功時には確定リビジョンと内容の SHA-256 が返ります。通信が切れた場合は `receipt "$request_id"` で確定記録を照会するか、同じファイル内容・期待リビジョン・要求 ID で再送します。確定済みの要求を再送すると、元の確定リビジョンが返ります。競合した場合は、`status` と元ファイルを見直してから新しい要求 ID で公開します。

`verify` は公開データを読み戻して元ファイルのグラフと照合します。`export` で作ったファイルはデスクトップ版の「JSON 取込」で開けます。`export` には新しいファイル名を指定してください。

## DDD の経路を検索する

JSON の `nodes` からコマンドの UUID を確認し、`<command-uuid>` をその値に置き換えて実行します。

```sh
"$lb" neo4j --target "$target" search '<command-uuid>' --depth 3 --limit 100
```

コマンドから `handled_by → emits → triggers` の順にたどり、カードと関係の UUID、表示名、出典、公開リビジョンを返します。`--depth 1` は集約まで、`--depth 2` はイベントまでの経路を返します。検索中に公開リビジョンが変わった場合は読み直します。

## 実データベースで検証する

使い捨ての Neo4j に接続する認証情報を設定し、`NEO4J_TEST_QUERY_URL` に `https://<host>/db/<database>/query/v2` を指定します。テストはスキーマ、テスト用プロジェクト、公開データを作成します。

```sh
cargo test --locked --manifest-path apps/desktop/Cargo.toml \
  neo4j::tests::neo4j_ -- --ignored --test-threads=1
```
