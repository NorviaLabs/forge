; Kotlin grammar does not export a highlight query.
["fun" "val" "var" "class" "interface" "object" "package" "import"
 "return" "if" "else" "when" "for" "while" "in" "is" "as"
 "try" "catch" "finally" "throw"] @keyword
[(line_comment) (block_comment) (shebang)] @comment
[(string_literal) (multiline_string_literal) (character_literal)] @string
[(number_literal) (float_literal)] @number
(identifier) @variable
(user_type (identifier) @type)
(function_declaration (identifier) @function)
(call_expression (identifier) @function.call)
(annotation) @attribute
((identifier) @constant.builtin (#match? @constant.builtin "^(true|false|null)$"))
["=" "+" "-" "*" "/" "==" "!=" "<" ">"] @operator
["(" ")" "{" "}" "[" "]" "," "." ":"] @punctuation
