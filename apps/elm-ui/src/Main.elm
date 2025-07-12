module Main exposing (main)

import Browser
import Html exposing (Html, div, text)
import Http
import Json.Decode as Decode
import Api exposing (Node, Msg, init, update, subscriptions, view)

-- DOMAIN ───────────────────────────────────────────────

type alias Node =
    { id : Int
    , label : String
    }

nodeDecoder : Decode.Decoder Node
nodeDecoder =
    Decode.map2 Node
        (Decode.field "id" Decode.int)
        (Decode.field "label" Decode.string)


-- MODEL ────────────────────────────────────────────────

type Status
    = Loading
    | Success Node
    | Failure String

type alias Model =
    { status : Status }

init : () -> ( Model, Cmd Msg )
init _ =
    ( { status = Loading }
    , getNode 1
    )


-- UPDATE ───────────────────────────────────────────────

type Msg
    = GotNode (Result Http.Error Node)

update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotNode (Ok node) ->
            ( { status = Success node }, Cmd.none )

        GotNode (Err err) ->
            ( { status = Failure (Debug.toString err) }, Cmd.none )


-- VIEW ─────────────────────────────────────────────────

view : Model -> Html Msg
view model =
    case model.status of
        Loading ->
            text "Loading…"

        Success node ->
            div [] [ text ("label: " ++ node.label) ]

        Failure err ->
            div [] [ text ("Error: " ++ err) ]


-- SUBSCRIPTIONS ────────────────────────────────────────

subscriptions : Model -> Sub Msg
subscriptions _ =
    Sub.none


-- HTTP ─────────────────────────────────────────────────

getNode : Int -> Cmd Msg
getNode id =
    Http.get
        { url = "/api/node/" ++ String.fromInt id
        , expect = Http.expectJson GotNode nodeDecoder
        }


-- MAIN ─────────────────────────────────────────────────

main : Program () Model Msg
main =
    Browser.element
        { init = init
        , update = update
        , subscriptions = subscriptions
        , view = view
        }
