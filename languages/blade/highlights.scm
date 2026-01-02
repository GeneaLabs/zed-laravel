; HTML
(tag_name) @tag
(doctype) @tag
(attribute_name) @attribute
(attribute_value) @string
(comment) @comment

[
  "\""
  "'"
] @string

"=" @punctuation.delimiter

[
  "<"
  ">"
  "<!"
  "</"
  "/>"
] @punctuation.bracket

; Blade directives - these are the actual AST node types
; (keyword rule gets aliased to directive in the AST)
(directive) @function
(directive_start) @function
(directive_end) @function

; Directives used as HTML attributes
(attribute (directive) @attribute)

; Blade echo delimiters
[
  "{{"
  "}}"
  "{!!"
  "!!}"
] @punctuation.bracket

; Parentheses in directives
[
  "("
  ")"
] @punctuation.bracket
