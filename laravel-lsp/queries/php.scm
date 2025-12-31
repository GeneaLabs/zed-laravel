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
; Pattern 6b: Config::get('key'), Config::string('key'), etc. - Facade methods
; ============================================================================
; Matches: Config::get('app.name')
;          Config::string('app.name')
;          Config::integer('app.timeout')
;          Config::boolean('app.debug')
;          Config::array('app.providers')
;
; This captures config key access via the Config facade

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#eq? @class_name "Config")
  (#match? @method_name "^(get|string|integer|boolean|array|set|has)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#eq? @class_name "Config")
  (#match? @method_name "^(get|string|integer|boolean|array|set|has)$"))

; Also match fully qualified Config class - single quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @config_key)))
  (#match? @class_name ".*Config$")
  (#match? @method_name "^(get|string|integer|boolean|array|set|has)$"))

; Also match fully qualified Config class - double quotes
(scoped_call_expression
  scope: (qualified_name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @config_key)))
  (#match? @class_name ".*Config$")
  (#match? @method_name "^(get|string|integer|boolean|array|set|has)$"))

; ============================================================================
; Pattern 7: route('route.name') function calls
; ============================================================================
; Matches: route('home')
;          route('admin.dashboard')
;          route("user.profile", ['id' => 1])
;
; Captures route name for navigation to route definition

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @function_name "route"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @function_name "route"))

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
;          Lang::has('messages.welcome')
;          Lang::hasForLocale('messages.welcome', 'es')
;          Lang::choice('messages.apples', 10)
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
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

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
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

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
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

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
  (#match? @method_name "^(get|has|hasForLocale|choice)$"))

; ============================================================================
; Pattern 16: app('binding') / resolve('binding') - Container binding resolution
; ============================================================================
; Matches: app('auth'), resolve('auth')
;          app("cache"), resolve("cache")
;          app('App\Contracts\SomeInterface')
;
; This pattern captures container binding resolution using string identifiers

; app() - Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; app() - Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @function_name "app"))

; resolve() - Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @binding_name)))
  (#eq? @function_name "resolve"))

; resolve() - Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @binding_name)))
  (#eq? @function_name "resolve"))

; ============================================================================
; Pattern 17: app(SomeClass::class) / resolve(Class::class) - Container binding with class reference
; ============================================================================
; Matches: app(UserService::class), resolve(UserService::class)
;          app(\App\Services\PaymentService::class)
;
; This pattern captures container resolution using ::class constants

; app() - Class name with ::class
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

; app() - Qualified class name with ::class
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

; resolve() - Class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "resolve")
  (#eq? @constant_name "class"))

; resolve() - Qualified class name with ::class
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (class_constant_access_expression
        (qualified_name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "resolve")
  (#eq? @constant_name "class"))

; ============================================================================
; Pattern 12b: Route::group(['middleware' => 'auth'], ...) - Group with middleware in options array
; ============================================================================
; Matches: Route::group(['middleware' => 'auth'], function() {...})
;          Route::group(['middleware' => ['auth', 'verified']], function() {...})
;
; This captures middleware specified in the options array of Route::group()

; Single middleware string in options array - single quotes key, single quotes value
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Single middleware string in options array - single quotes key, double quotes value
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (encapsed_string
            (string_content) @middleware_name)))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Array of middleware in options array - single quotes in nested array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (array_creation_expression
            (array_element_initializer
              (string
                (string_content) @middleware_name)))))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

; Array of middleware in options array - double quotes in nested array
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    (argument
      (array_creation_expression
        (array_element_initializer
          (string
            (string_content) @_key)
          (array_creation_expression
            (array_element_initializer
              (encapsed_string
                (string_content) @middleware_name)))))))
  (#eq? @class_name "Route")
  (#eq? @method_name "group")
  (#eq? @_key "middleware"))

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
; Pattern 18: url('path') - URL helper function
; ============================================================================
; Matches: url('home')
;          url('/admin/dashboard')
;          url("api/users")
;
; Captures URL path for navigation to public files or route definitions

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @url_path)))
  (#eq? @function_name "url"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @url_path)))
  (#eq? @function_name "url"))

; ============================================================================
; Pattern 19: action('Controller@method') - Controller action URLs
; ============================================================================
; Matches: action('UserController@show')
;          action('App\Http\Controllers\AdminController@index')
;          action([UserController::class, 'show'])
;
; Captures controller action for navigation

; String syntax - single quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @action_name)))
  (#eq? @function_name "action"))

; String syntax - double quotes
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @action_name)))
  (#eq? @function_name "action"))

; ============================================================================
; Pattern 20: redirect()->route('name') - Redirect to named route
; ============================================================================
; Matches: redirect()->route('home')
;          redirect()->route('user.profile', ['id' => 1])
;
; This captures the route name from redirect chains

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "route"))

; Double-quoted strings (already captures route_name like the function)
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "route"))

; ============================================================================
; Pattern 21: to_route('name') - Laravel 9+ redirect helper
; ============================================================================
; Matches: to_route('home')
;          to_route('user.profile', ['id' => 1])
;
; This is a shorthand for redirect()->route()

; Single-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @function_name "to_route"))

; Double-quoted strings
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @function_name "to_route"))

; ============================================================================
; Pattern 22: Route::has('name') - Check if named route exists
; ============================================================================
; Matches: Route::has('admin.dashboard')
;          Route::has('user.profile')
;
; Used to check if a named route exists before generating URLs

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "has"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#eq? @method_name "has"))

; ============================================================================
; Pattern 23: URL::route('name') - Generate URL to named route
; ============================================================================
; Matches: URL::route('home')
;          URL::route('user.profile', ['id' => 1])
;
; Alternative to route() helper for generating URLs

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "URL")
  (#eq? @method_name "route"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "URL")
  (#eq? @method_name "route"))

; ============================================================================
; Pattern 24: Route::is('name') / Route::currentRouteNamed('name')
; ============================================================================
; Matches: Route::is('admin.*')
;          Route::is('user.profile')
;          Route::currentRouteNamed('dashboard')
;
; Used to check if the current route matches a pattern

; Single-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(is|currentRouteNamed)$"))

; Double-quoted strings
(scoped_call_expression
  scope: (name) @class_name
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @class_name "Route")
  (#match? @method_name "^(is|currentRouteNamed)$"))

; ============================================================================
; Pattern 25: $request->routeIs('name') - Request route checking
; ============================================================================
; Matches: $request->routeIs('profile')
;          $request->routeIs('admin.*')
;
; Check if the current request matches a route name pattern

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "routeIs"))

; Double-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "routeIs"))

; ============================================================================
; Pattern 26: $request->route()->named('name') - Check if route matches name
; ============================================================================
; Matches: $request->route()->named('profile')
;          $request->route()->named('admin.dashboard')
;
; Check if the current route has a specific name

; Single-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (string
        (string_content) @route_name)))
  (#eq? @method_name "named"))

; Double-quoted strings
(member_call_expression
  name: (name) @method_name
  arguments: (arguments
    .
    (argument
      (encapsed_string
        (string_content) @route_name)))
  (#eq? @method_name "named"))
