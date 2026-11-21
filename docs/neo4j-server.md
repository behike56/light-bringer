# Light Bringer 用 Neo4j サーバーの構築と復旧

保存済みのグラフを CLI で公開するための Neo4j を、Docker Compose で単一インスタンスとして起動します。単独検証は Community Edition、閲覧者を分けるチーム共有は Enterprise Edition を使います。どちらも公式イメージの Neo4j 5.26.33 を使用します。[エディション別のユーザー権限](https://neo4j.com/docs/operations-manual/current/authentication-authorization/manage-users/)

## 目次

- [単独検証用サーバーを起動する](#単独検証用サーバーを起動する)
- [チーム共有用サーバーを起動する](#チーム共有用サーバーを起動する)
- [公開と検索を確認する](#公開と検索を確認する)
- [バックアップと復旧を行う](#バックアップと復旧を行う)

## 単独検証用サーバーを起動する

Docker Compose と OpenSSL 1.1.1 以降を用意し、リポジトリのルートで実行します。既定の接続先は `https://localhost:7473` です。証明書と初期パスワードを作る次のブロックは、初回だけ実行します。

```sh
runtime="$PWD/docker/.runtime"
(
  set -eu
  umask 077
  mkdir "$runtime"
  mkdir -p "$runtime/https/trusted" "$runtime/https/revoked" "$runtime/backups"

  password=$(openssl rand -hex 24)
  printf 'neo4j/%s\n' "$password" > "$runtime/auth.txt"
  openssl req -x509 -newkey rsa:2048 -nodes -days 30 -subj '/CN=Light Bringer Local CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -keyout "$runtime/ca.key" -out "$runtime/ca.crt"
  openssl req -new -newkey rsa:2048 -nodes -subj '/CN=localhost' \
    -keyout "$runtime/https/private.key" -out "$runtime/server.csr"
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' \
    > "$runtime/server.ext"
  openssl x509 -req -days 30 -in "$runtime/server.csr" \
    -CA "$runtime/ca.crt" -CAkey "$runtime/ca.key" -CAcreateserial -extfile "$runtime/server.ext" \
    -out "$runtime/https/public.crt"
)
```

起動時は現在の UID・GID を指定し、コンテナから検証用のファイルを読み取れるようにします。`--wait` は HTTPS ポートが開くまで待ちます。

```sh
compose() { docker compose -f docker/neo4j-compose.yml "$@"; }
export LB_NEO4J_RUN_AS="$(id -u):$(id -g)"
compose up -d --wait --wait-timeout 120

export NEO4J_CA_CERT="$runtime/ca.crt"
export NEO4J_ADMIN_USER=neo4j NEO4J_PUBLISH_USER=neo4j NEO4J_READ_USER=neo4j
NEO4J_ADMIN_PASSWORD=$(cut -d / -f 2- "$runtime/auth.txt")
export NEO4J_ADMIN_PASSWORD
export NEO4J_PUBLISH_PASSWORD="$NEO4J_ADMIN_PASSWORD" NEO4J_READ_PASSWORD="$NEO4J_ADMIN_PASSWORD"
```

再起動には同じ `compose up` を使います。初期パスワードのファイルは、空のデータボリュームを初期化するときに使います。運用中のパスワード変更は Neo4j 側で行います。[Compose の認証設定](https://neo4j.com/docs/operations-manual/current/docker/docker-compose-standalone/)

## チーム共有用サーバーを起動する

社内ネットワークまたは VPN から接続できる Linux VM を用意します。VM のファイアウォールで、利用端末から `7473/tcp` への接続を許可してください。Bolt とオンラインバックアップ用ポートはコンテナ内のループバックで待ち受けます。

ホストの保護された場所に次のファイルとディレクトリを用意します。

| 用途 | Compose に渡す変数 | 用意するもの |
| --- | --- | --- |
| 初期管理者認証 | `LB_NEO4J_AUTH_FILE` | `neo4j/<初期パスワード>` を 1 行に記載したファイル |
| HTTPS | `LB_NEO4J_TLS_DIR` | `private.key`、サーバー証明書と中間証明書を含む `public.crt`、`trusted/`、`revoked/` |
| バックアップ | `LB_NEO4J_BACKUP_DIR` | DB の実行ユーザーが書き込めるディレクトリ |

既定のイメージでは DB は UID・GID `7474:7474` で動作します。そのユーザーに鍵と認証ファイルの読み取りを許可し、所有者または必要なグループにアクセスを絞ってください。証明書の SAN には CLI が使う DNS 名を含めてください。[Docker での TLS 設定](https://neo4j.com/docs/operations-manual/current/docker/security/)

Enterprise の利用条件を確認したうえで、以下のパスと IP を実環境の値に置き換えます。`-p` で Compose プロジェクトを指定すると、検証用と共有用でデータボリュームを分けられます。

```sh
compose() {
  docker compose -p light-bringer-neo4j-team \
    -f docker/neo4j-compose.yml -f docker/neo4j-compose.enterprise.yml "$@"
}
export LB_NEO4J_ACCEPT_LICENSE_AGREEMENT=yes
export LB_NEO4J_BIND_IP=10.0.0.10
export LB_NEO4J_AUTH_FILE=/srv/light-bringer/neo4j/auth.txt
export LB_NEO4J_TLS_DIR=/srv/light-bringer/neo4j/https
export LB_NEO4J_BACKUP_DIR=/srv/light-bringer/neo4j/backups
compose up -d --wait --wait-timeout 120
```

管理者としてコンテナ内の Cypher Shell を開きます。`-u neo4j` で接続すると初期パスワードの入力を求められます。

```sh
compose exec neo4j cypher-shell -a bolt://127.0.0.1:7687 -d system -u neo4j --history in-memory
```

[ロール定義](../docker/neo4j-roles.cypher) を Cypher Shell に貼り付けて実行します。続けて、公開担当者と閲覧者のアカウントを作成します。以下の利用者名とパスワードは個別に置き換えてください。

```cypher
CREATE USER lb_publish_user SET PASSWORD '<公開担当者のパスワード>' CHANGE NOT REQUIRED;
CREATE USER lb_read_user SET PASSWORD '<閲覧者のパスワード>' CHANGE NOT REQUIRED;
GRANT ROLE lb_neo4j_publisher TO lb_publish_user;
GRANT ROLE lb_neo4j_reader TO lb_read_user;
```

`neo4j` DB は同じ閲覧範囲を持つ利用者で共有します。閲覧範囲が異なる場合は DB を分け、各 DB 名と専用ロール名を指定したロール定義を適用します。そのうえで `SHOW USER <利用者名> PRIVILEGES` と接続試験で、対象外の DB を読めないことを確認してください。`project_id` は CLI が検索・更新の範囲を指定するための識別子です。[Neo4j のロールと権限](https://neo4j.com/docs/operations-manual/current/authentication-authorization/manage-privileges/)

## 公開と検索を確認する

クライアント端末で [Neo4j CLI の手順](neo4j.md) に沿って `register → init → publish → verify → search` を実行します。単独検証では `register --url https://localhost:7473` を使います。チーム共有では、社内 DNS 名の HTTPS URL を指定し、担当する操作の資格情報をクライアントの環境変数に設定します。プライベート CA を使う場合は `NEO4J_CA_CERT` に CA 証明書のパスを指定します。

共有前に読み取り専用アカウントで `search` の成功を確認します。同じ権限設定を適用した使い捨ての Enterprise インスタンスで、次のコマンドが書き込み権限エラーになることを確認してください。

```sh
compose exec neo4j cypher-shell -a bolt://127.0.0.1:7687 -d neo4j \
  -u lb_read_user 'CREATE (:LBReadOnlyCheck);'
```

通信エラーの切り分けには `compose ps` と `compose logs --tail 100 neo4j` を使います。

## バックアップと復旧を行う

公開に使った保存済みファイルと公開先の設定を保管します。DB のバックアップには公開リビジョンと要求 ID の確定記録が含まれます。バックアップのコピーはサーバーとは別の保管先に置きます。

### Community Edition のオフラインダンプ

単独検証で使った `compose` 関数と `runtime` を使用します。ダンプ中はサーバーを停止します。

```sh
stamp=$(date -u +%Y%m%dT%H%M%SZ)
mkdir -m 700 "$runtime/backups/$stamp" &&
  compose stop neo4j &&
  compose run --rm --no-deps -T --entrypoint neo4j-admin neo4j \
    database dump neo4j --to-path="/backups/$stamp"

compose up -d --wait --wait-timeout 120
```

`$runtime/backups/$stamp/neo4j.dump` を保管します。[オフラインダンプとロード](https://neo4j.com/docs/operations-manual/current/docker/dump-load/)

### Enterprise Edition のオンラインバックアップ

チーム共有で使った `compose` 関数を使用します。各回の出力先を分けてフルバックアップを取得します。対象 DB が複数ある場合は DB ごとに実施します。

```sh
stamp=$(date -u +%Y%m%dT%H%M%SZ)
compose exec -T --user 7474:7474 neo4j mkdir -m 700 "/backups/$stamp" &&
  compose exec -T --user 7474:7474 neo4j neo4j-admin database backup neo4j \
    --from=127.0.0.1:6362 --type=full --include-metadata=none --to-path="/backups/$stamp"
```

`$LB_NEO4J_BACKUP_DIR/$stamp` の `.backup` ファイルを保管します。利用者とロールの割り当ては、復旧時に再設定できるよう別途記録します。[オンラインバックアップ](https://neo4j.com/docs/operations-manual/current/backup-restore/online-backup/)

### 新しいボリュームに復元する

空のボリュームを使うよう、未使用の Compose プロジェクト名を指定します。使用する `compose` 関数は、バックアップ取得時のエディションとそろえてください。`<バックアップディレクトリ>` または `<バックアップファイル>` を実際のコンテナ内パスに置き換えます。復元試験では、SAN に `localhost` を含む証明書を用意し、そのディレクトリを `restore_tls_dir` に設定します。単独検証用に作成した証明書も使えます。

```sh
# Community Edition
compose -p light-bringer-neo4j-restore run --rm --no-deps -T --entrypoint neo4j-admin neo4j \
  database load neo4j --from-path='<バックアップディレクトリ>'

# Enterprise Edition
compose -p light-bringer-neo4j-restore run --rm --no-deps -T --entrypoint neo4j-admin neo4j \
  database restore neo4j --from-path='<バックアップファイル>'

# 復元したインスタンスを検証用ポートで起動する
LB_NEO4J_BIND_IP=127.0.0.1 LB_NEO4J_HTTPS_PORT=17473 LB_NEO4J_TLS_DIR="$restore_tls_dir" \
  compose -p light-bringer-neo4j-restore up -d --wait --wait-timeout 120
```

Enterprise では利用者とロールを設定し直します。既定の `neo4j` 以外の DB を復元した場合は、起動後に `system` DB で `CREATE DATABASE <DB名>` を実行して登録します。

復元先が別の VM にある場合は、SSH のローカル転送でクライアントの `17473` を VM の `127.0.0.1:17473` につなぎます。復旧試験用に公開先設定をコピーし、`project_id` と `database` を保ったまま `url` を `https://localhost:17473` に変更します。復元試験用の CA 証明書を `NEO4J_CA_CERT` に設定し、`status`、`receipt`、`verify`、`search` で公開状態を照合します。

古いバックアップを使った場合は、公開を一時停止して未確定の要求を照合します。バックアップより後に確定した公開は、保存していた入力ファイルから復元できます。対象版を確認し、現在の期待リビジョンと新しい要求 ID で公開し直します。照合と権限確認を終えてから共有用の接続先を切り替えます。[バックアップからの復元](https://neo4j.com/docs/operations-manual/current/backup-restore/restore-backup/)
