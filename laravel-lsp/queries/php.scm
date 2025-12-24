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
; Pattern 3: Route::view('/path', 'view.name') - Route view registration
; ============================================================================
; Matches: Route::view('/home', 'welcome')
;          Route::view('/about', 'pages.about')
;
; AST structure for static method calls:
;   scoped_call_expression
;     scope: (name) - "Route"
;     name: (name) - "view"
;     arguments: (arguments ...)
;
; IMPORTANT: We capture the SECOND argument (the view name), not the first (route path)

; Single-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (string
        (string_content) @route_view_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "view"))

; Double-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (encapsed_string
        (string_content) @route_view_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "view"))

; ============================================================================
; Pattern 4: Volt::route('/path', 'view.name') - Volt route registration
; ============================================================================
; Matches: Volt::route('/home', 'welcome')
;          Volt::route('/about', 'pages.about')
;
; Same as Route::view() - captures the SECOND argument (view name)

; Single-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (string
        (string_content) @route_view_name)))
  (#eq? @class_name "Volt")
  (#eq? @method_name "route"))

; Double-quoted view name (second argument)
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument)
    (argument
      (encapsed_string
        (string_content) @route_view_name)))
  (#eq? @class_name "Volt")
  (#eq? @method_name "route"))

; ============================================================================
; Pattern 5: env('VAR_NAME') or env('VAR_NAME', 'default') function calls
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
; Pattern 6: config('config.key') function calls
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
; Pattern 7: route('route.name') function calls
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
; Pattern 8: Route::middleware('auth') - Static method calls with single middleware
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
; Pattern 9: Route::middleware(['auth', 'web']) - Array of middleware
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
; Pattern 10: ->middleware('auth') - Chained method calls with single middleware
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
; Pattern 11: ->middleware(['auth', 'web']) - Chained method calls with array
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
; Pattern 12: __('translation.key') - Translation helper function
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
; Pattern 13: trans('translation.key') - Trans helper function
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
; Pattern 14: trans_choice('translation.key', $count) - Pluralization helper
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
; Pattern 15: Lang::get('translation.key') - Facade method
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
; Pattern 16: app('binding') - Container binding resolution with strings
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
; Pattern 17: app(SomeClass::class) - Container binding with class reference
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
; Pattern 13: Asset and Path Helpers
; ============================================================================
; Matches: asset('images/logo.png')
;          public_path('index.php')
;          base_path('composer.json')
;          app_path('Models/User.php')
;          storage_path('logs/laravel.log')
;          database_path('seeders/UserSeeder.php')
;          lang_path('en/messages.php')
;          config_path('app.php')
;          resource_path('views/welcome.blade.php')
;          mix('css/app.css')
;          Vite::asset('resources/images/logo.svg')

; asset() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @asset_path)))
  (#eq? @function_name "asset"))

; asset() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @asset_path)))
  (#eq? @function_name "asset"))

; public_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @public_path)))
  (#eq? @function_name "public_path"))

; public_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @public_path)))
  (#eq? @function_name "public_path"))

; base_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @base_path)))
  (#eq? @function_name "base_path"))

; base_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @base_path)))
  (#eq? @function_name "base_path"))

; app_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @app_path)))
  (#eq? @function_name "app_path"))

; app_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @app_path)))
  (#eq? @function_name "app_path"))

; storage_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @storage_path)))
  (#eq? @function_name "storage_path"))

; storage_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @storage_path)))
  (#eq? @function_name "storage_path"))

; database_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @database_path)))
  (#eq? @function_name "database_path"))

; database_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @database_path)))
  (#eq? @function_name "database_path"))

; lang_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @lang_path)))
  (#eq? @function_name "lang_path"))

; lang_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @lang_path)))
  (#eq? @function_name "lang_path"))

; config_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_path)))
  (#eq? @function_name "config_path"))

; config_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_path)))
  (#eq? @function_name "config_path"))

; resource_path() - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @resource_path)))
  (#eq? @function_name "resource_path"))

; resource_path() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @resource_path)))
  (#eq? @function_name "resource_path"))

; mix() - single quotes (legacy Laravel Mix)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @mix_path)))
  (#eq? @function_name "mix"))

; mix() - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @mix_path)))
  (#eq? @function_name "mix"))

; Vite::asset() - single quotes
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @vite_asset_path)))
  (#eq? @class_name "Vite")
  (#eq? @method_name "asset"))

; Vite::asset() - double quotes
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @vite_asset_path)))
  (#eq? @class_name "Vite")
  (#eq? @method_name "asset"))

; ============================================================================
