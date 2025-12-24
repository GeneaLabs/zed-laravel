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

; ============================================================================
; Pattern 6: Route::middleware('auth') - Static method calls with single middleware
; ============================================================================
; Matches: Route::middleware('auth')
;          Route::withoutMiddleware('verified')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name) - "Route"
;     name: (name) - "middleware" or "withoutMiddleware"
;     arguments: (arguments ...)

; Single-quoted string middleware
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted string middleware
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 7: Route::middleware(['auth', 'web']) - Array of middleware
; ============================================================================
; Matches: Route::middleware(['auth', 'verified'])
;          Route::withoutMiddleware(['guest'])
;
; This captures individual middleware strings within an array argument

; Single-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted strings in array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 8: ->middleware('auth') - Chained method calls with single middleware
; ============================================================================
; Matches: Route::get('/dashboard')->middleware('auth')
;          $route->middleware('verified')
;          ->withoutMiddleware('guest')
;
; AST structure for member call expressions:
;   member_call_expression
;     name: (name) - "middleware" or "withoutMiddleware"
;     arguments: (arguments ...)

; Single-quoted string middleware in chained calls
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (string
        (string_content) @middleware_name)))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted string middleware in chained calls
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (encapsed_string
        (string_content) @middleware_name)))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 9: ->middleware(['auth', 'web']) - Chained method calls with array
; ============================================================================
; Matches: Route::get('/admin')->middleware(['auth', 'verified'])
;          $route->withoutMiddleware(['guest'])

; Single-quoted strings in array (chained)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @middleware_name)))))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; Double-quoted strings in array (chained)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (encapsed_string
            (string_content) @middleware_name)))))
  (#match? @method_name "^(middleware|withoutMiddleware)$"))

; ============================================================================
; Pattern 10: __('translation.key') - Translation helper function
; ============================================================================
; Matches: __('messages.welcome')
;          __("auth.failed")
;
; This is the most common translation helper in Laravel

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "__"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "__"))

; ============================================================================
; Pattern 11: trans('translation.key') - Trans helper function
; ============================================================================
; Matches: trans('messages.welcome')
;          trans("validation.required")

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "trans"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "trans"))

; ============================================================================
; Pattern 12: trans_choice('translation.key', $count) - Pluralization helper
; ============================================================================
; Matches: trans_choice('messages.apples', 10)
;          trans_choice("messages.minutes_ago", $minutes)

; Single-quoted strings - only match first argument (the translation key)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @function_name "trans_choice"))

; Double-quoted strings - only match first argument
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @function_name "trans_choice"))

; ============================================================================
; Pattern 13: Lang::get('translation.key') - Facade method
; ============================================================================
; Matches: Lang::get('messages.welcome')
;          \Lang::get("validation.email")

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#eq? @class_name "Lang")
  (#match? @method_name "^(get|has|choice)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#eq? @class_name "Lang")
  (#match? @method_name "^(get|has|choice)$"))

; Also match fully qualified Lang class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @translation_key)))
  (#match? @class_name ".*Lang$")
  (#match? @method_name "^(get|has|choice)$"))

; Also match fully qualified Lang class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @translation_key)))
  (#match? @class_name ".*Lang$")
  (#match? @method_name "^(get|has|choice)$"))

; ============================================================================
; Pattern 14: app('binding') - Container binding resolution with strings
; ============================================================================
; Matches: app('auth')
;          app("cache")
;          app('App\Contracts\SomeInterface')
;
; This pattern captures container binding resolution using string identifiers

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; ============================================================================
; Pattern 15: app(SomeClass::class) - Container binding with class reference
; ============================================================================
; Matches: app(UserService::class)
;          app(\App\Services\PaymentService::class)
;
; This pattern captures container resolution using ::class constants

; Class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "app")
  (#eq? @constant_name "class"))

; Qualified class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "app")
  (#eq? @constant_name "class"))

; ============================================================================
