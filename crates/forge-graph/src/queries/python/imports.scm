; Last identifier segment of `import foo.bar` and `from foo import bar`.
; Aliases (`as x`), relative imports (`from . import x`), and star imports
; are not captured — same best-effort scope as every other language here.
(import_statement
  name: (dotted_name (identifier) @import.name .))

(import_from_statement
  name: (dotted_name (identifier) @import.name .))
