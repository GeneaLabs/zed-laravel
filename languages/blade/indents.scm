(directive_start) @indent.begin

(directive_end) @indent.end


; html
(start_tag ">" @end) @indent
(self_closing_tag "/>" @end) @indent

(element
  (start_tag) @start
  (end_tag)? @end) @indent
