### backend.roc ---------------------------------------------------------------
app [
    Model,
    init!,
    respond!,
] {
    web:
        platform
        "https://github.com/roc-lang/basic-webserver/releases/download/0.12.0/Q4h_In-sz1BqAvlpmCsBHhEJnn_YvfRRMiNACB_fBbk.tar.br",
}

import web.Stdout
import web.Http exposing [Request, Response]
import web.Utc
import Inspect
import Str
import List

# -- 型 -----------------------------------------------------------------------

Model : {}
Node : { id : I64, label : Str }

# -- 初期化 -------------------------------------------------------------------

init! : {} => Result Model []
init! = |_| Ok({})

# -- サンプルデータ ------------------------------------------------------------

nodes : List Node
nodes = [
    { id: 1, label: "Hello" },
    { id: 2, label: "World" },
]

nodeToJson : Node -> Str
nodeToJson = |n|
    Str.join_with(
        [
            "{\"id\":",
            Inspect.to_str(n.id),
            ",\"label\":\"",
            n.label,
            "\"}",
        ],
        "", # 区切りは空文字
    )

nodesJson : Str
nodesJson =
    Str.join_with(
        [
            "[",
            Str.join_with(List.map(nodes, nodeToJson), ","),
            "]",
        ],
        "", # 区切りは空文字
    )

# -- リクエストハンドラ --------------------------------------------------------
# Result Response [ServerErr Str]_
respond! :
    Request,
    Model
    => Result Response [ServerErr Str]_
respond! = |req, _|
    # datetime = Utc.to_iso_8601(Utc.now!({}))

    # ログ出力結果を捨てる（←★ここ）
    # _ = Stdout.line!("${datetime} ${Inspect.to_str(req.method)} ${req.uri}")?

    when (req.method, req.uri) is
        (GET, "/api/graph/nodes") ->
            Ok(
                {
                    status: 200,
                    headers: [("Content-Type", "application/json")],
                    body: Str.to_utf8(nodesJson),
                },
            )

        _ ->
            Ok(
                {
                    status: 404,
                    headers: [],
                    body: Str.to_utf8("Not Found"),
                },
            )

