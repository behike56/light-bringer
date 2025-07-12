module Api exposing
    ( Node
    , Msg(..)
    , init
    , update
    , subscriptions
    , view        -- Optional: ここではデバッグ用に表示
    )

import Browser
import Html exposing (Html, div, text)
import Http
import Json.Decode as D exposing (Decoder)


-- MODEL ----------------------------------------------------------------------

type alias Node =
    { id : Int
    , label : String
    }


type alias Model =
    { nodes : List Node
    , error : Maybe String
    }


init : () -> ( Model, Cmd Msg )
init _ =
    ( { nodes = [], error = Nothing }
    , fetchNodes
    )


-- UPDATE ---------------------------------------------------------------------

type Msg
    = GotNodes (Result Http.Error (List Node))


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotNodes (Ok nodes) ->
            ( { model | nodes = nodes, error = Nothing }, Cmd.none )

        GotNodes (Err httpErr) ->
            ( { model | error = Just (httpErrorToString httpErr) }, Cmd.none )


-- HTTP -----------------------------------------------------------------------

backendBase : String
backendBase =
    -- Tauri で同一オリジンの場合は "" にして下さい
    "http://localhost:3000"


fetchNodes : Cmd Msg
fetchNodes =
    Http.get
        { url = backendBase ++ "/api/graph/nodes"
        , expect = Http.expectJson GotNodes nodesDecoder
        }


nodesDecoder : Decoder (List Node)
nodesDecoder =
    D.list nodeDecoder


nodeDecoder : Decoder Node
nodeDecoder =
    D.map2 Node
        (D.field "id" D.int)
        (D.field "label" D.string)


httpErrorToString : Http.Error -> String
httpErrorToString err =
    case err of
        Http.NetworkError ->
            "ネットワークエラー"

        Http.Timeout ->
            "タイムアウト"

        Http.BadStatus code ->
            "HTTP ステータスエラー: " ++ String.fromInt code

        Http.BadBody msg ->
            "デコード失敗: " ++ msg

        Http.BadUrl url ->
            "URL が不正: " ++ url


-- VIEW -----------------------------------------------------------------------

view : Model -> Html Msg
view model =
    div []
        [ case model.error of
            Just e ->
                text ("Error: " ++ e)

            Nothing ->
                text <|
                    "Loaded nodes: "
                        ++ String.fromInt (List.length model.nodes)
        ]


-- PROGRAM MAIN ---------------------------------------------------------------

main : Program () Model Msg
main =
    Browser.element
        { init = init
        , update = update
        , subscriptions = subscriptions
        , view = view
        }


subscriptions : Model -> Sub Msg
subscriptions _ =
    Sub.none
