; Named imports (`import { A, B } from 'mod'`) and a default import
; (`import Foo from 'mod'`). Namespace imports (`import * as ns`) and
; re-exports are not captured — best-effort, same as every other language.
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name))))

(import_statement
  (import_clause (identifier) @import.name))
