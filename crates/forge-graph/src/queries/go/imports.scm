; Import path as a raw string, e.g. "fmt" or "github.com/foo/bar" — the
; extractor takes the last "/"-delimited segment as the imported name.
; Aliased imports (`f "fmt"`) keep the alias unresolved to the real
; package name; best-effort, same as every other language here.
(import_spec path: (interpreted_string_literal) @import.path)
