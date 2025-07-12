module [playground]

numCheck : I64, I64 => Str
numCheck = |a, b|
    when (a, b) is
        (1, 2) ->
            Ok("1, 2")

        _ ->
            Ok("other")
