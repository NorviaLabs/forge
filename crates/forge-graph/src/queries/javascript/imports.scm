; Same shapes as TypeScript's imports.scm — JS's grammar uses identical
; node kinds here.
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name))))

(import_statement
  (import_clause (identifier) @import.name))
