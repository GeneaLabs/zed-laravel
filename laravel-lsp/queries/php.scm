; Tree-sitter query for detecting Laravel patterns in PHP files
;
; This file uses tree-sitter query syntax (S-expressions)
; to match patterns in the PHP Abstract Syntax Tree (AST)
;
; Query syntax:
;   (node_type) - matches a node of this type
;   field_name: - matches a named field on the node
;   @capture_name - captures the matched node for later use
;   (#eq? @var "value") - predicate to filter matches
;
; Reference: https://tree-sitter.github.io/tree-sitter/using-parsers#pattern-matching-with-queries

; ============================================================================
; Pattern 1: view('view.name') function calls
; ============================================================================
; Matches: view('users.profile')
;          view("admin.dashboard")
;
; AST structure for function calls:
;   function_call_expression
;     function: (name or qualified_name)
;     arguments: (arguments ...)

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#eq? @function_name "view"))

; Double-quoted strings (encapsed_string)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#eq? @function_name "view"))

; ============================================================================
; Pattern 2: View::make('view.name') static method calls
; ============================================================================
; Matches: View::make('users.profile')
;          \View::make('admin.dashboard')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name)
;     name: (name)
;     arguments: (arguments ...)

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#eq? @class_name "View")
  (#eq? @method_name "make"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#eq? @class_name "View")
  (#eq? @method_name "make"))

; Also match fully qualified View class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @view_name)))
  (#match? @class_name ".*View$")
  (#eq? @method_name "make"))

; Also match fully qualified View class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @view_name)))
  (#match? @class_name ".*View$")
  (#eq? @method_name "make"))

; ============================================================================
; Pattern 3: env('VAR_NAME') or env('VAR_NAME', 'default') function calls
; ============================================================================
; Matches: env('APP_NAME', 'Laravel')
;          env("DB_HOST")
;
; This pattern captures the FIRST argument to env() which is the variable name

; Single-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @env_var)))
  (#eq? @function_name "env"))

; Double-quoted strings (encapsed_string) - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @env_var)))
  (#eq? @function_name "env"))

; ============================================================================
; Pattern 4: config('config.key') function calls
; ============================================================================
; Matches: config('app.name')
;          config("database.connections.mysql.host")
;
; This pattern captures config key access in application code

; Single-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#eq? @function_name "config"))

; Double-quoted strings (encapsed_string) - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#eq? @function_name "config"))

; ============================================================================
; Pattern 5: route('route.name') function calls
; ============================================================================
; For future implementation - route name navigation
;
; (function_call_expression
;   function: (name) @function_name
;   arguments: (arguments
;     (argument
;       (string
;         (string_value) @route_name)))
;   (#eq? @function_name "route"))
