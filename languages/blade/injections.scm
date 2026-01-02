((php_only) @injection.content
  (#set! injection.language "php_only"))

((parameter) @injection.content
    (#set! injection.include-children)
    (#set! injection.language "php_only"))

; Livewire attributes
; <div wire:click="baz++">
(attribute
  (attribute_name) @_attr
    (#any-of? @_attr "wire:model"
      "wire:click"
      "wire:stream"
      "wire:text"
      "wire:show")
  (quoted_attribute_value
    (attribute_value) @injection.content)
  (#set! injection.language "javascript"))


; AlpineJS attributes
; <div x-data="{ foo: 'bar' }" x-init="baz()">
(attribute
  (attribute_name) @_attr
    (#match? @_attr "^x-[a-z]+")
    (#not-any-of? @_attr "x-teleport" "x-ref" "x-transition")
  (quoted_attribute_value
    (attribute_value) @injection.content)
  (#set! injection.language "javascript"))
; <div :foo="bar" @click="baz()">
(attribute
  (attribute_name) @_attr
    (#match? @_attr "^[:@][a-z]+")
  (quoted_attribute_value
    (attribute_value) @injection.content)
  (#set! injection.language "javascript"))

; Blade escaped JS attributes
; <x-foo ::bar="baz" />
(element
  (_
    (tag_name) @_tag
      (#match? @_tag "^x-[a-z]+")
  (attribute
    (attribute_name) @_attr
      (#match? @_attr "^::[a-z]+")
    (quoted_attribute_value
      (attribute_value) @injection.content)
    (#set! injection.language "javascript"))))

; Blade PHP attributes
; <x-foo :bar="$baz" />
(element
  (_
    (tag_name) @_tag
      (#match? @_tag "^x-[a-z]+")
    (attribute
      (attribute_name) @_attr
        (#match? @_attr "^:[a-z]+")
      (quoted_attribute_value
        (attribute_value) @injection.content)
      (#set! injection.language "php_only"))))


; html
(script_element
  (raw_text) @injection.content
  (#set! injection.language "javascript"))

(style_element
  (raw_text) @injection.content
  (#set! injection.language "css"))

(attribute
    (attribute_name) @_attribute_name (#match? @_attribute_name "^style$")
    (quoted_attribute_value (attribute_value) @injection.content)
    (#set! injection.language "css"))
