; html
(tag_name) @tag
(doctype) @tag.doctype
(attribute_name) @attribute
[
  "\""
  "'"
  (attribute_value)
] @string
(comment) @comment

"=" @punctuation.delimiter.html

[
  "<"
  ">"
  "<!"
  "</"
  "/>"
] @punctuation.bracket.html

; blade
(directive) @tag
(directive_start) @tag
(directive_end) @tag
(comment) @comment
(attribute (directive) @attribute)
(keyword) @function
[
  "{{"
  "}}"
  "{!!"
  "!!}"
  "("
  ")"
] @punctuation.bracket
