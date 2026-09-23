; This grammar does not export a highlight query.
["FROM" "AS" "RUN" "CMD" "LABEL" "EXPOSE" "ENV" "ADD" "COPY"
 "ENTRYPOINT" "VOLUME" "USER" "WORKDIR" "ARG" "ONBUILD"
 "STOPSIGNAL" "HEALTHCHECK" "SHELL"] @keyword
(comment) @comment
[(double_quoted_string) (single_quoted_string) (json_string)] @string
(image_name) @string
(image_tag) @string
(variable) @variable
(expose_port) @number
