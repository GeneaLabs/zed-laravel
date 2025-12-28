# Laravel for Zed

A language server extension that brings Laravel intelligence to [Zed](https://zed.dev). Click through to Blade views, Livewire components, config files, and more.

## Installation

### From Zed Extensions (Coming Soon)

Search for "Laravel" in Zed's extension panel.

### From Source

```bash
git clone https://github.com/GeneaLabs/zed-laravel.git
cd zed-laravel
cargo build --release
```

Then in Zed: `Cmd+Shift+P` → "zed: install dev extension" → select the `zed-laravel` directory.

---

## Implemented Features

### Views

**Go-to-definition** for view references. Click to open the Blade file.

```php
// Click on 'users.profile' to open resources/views/users/profile.blade.php
return view('users.profile', ['user' => $user]);

// Also works with View facade
View::make('dashboard');

// And route view definitions
Route::view('/welcome', 'welcome');
```

### Blade Components

**Go-to-definition** for `<x-*>` component tags.

```blade
{{-- Click on 'button' to open app/View/Components/Button.php --}}
<x-button type="submit">Save</x-button>

{{-- Nested components work too --}}
<x-forms.input name="email" />

{{-- Opens app/View/Components/Forms/Input.php --}}
```

### Livewire Components

**Go-to-definition** for Livewire tags and directives.

```blade
{{-- Click to open app/Livewire/UserProfile.php --}}
<livewire:user-profile :user="$user" />

{{-- Nested namespaces --}}
<livewire:admin.dashboard />

{{-- Directive syntax --}}
@livewire('counter')
```

### Blade Directives

**Go-to-definition** for layout and include directives.

```blade
{{-- Click to open resources/views/layouts/app.blade.php --}}
@extends('layouts.app')

{{-- Click to open the partial --}}
@include('partials.header')

{{-- Section and slot references --}}
@section('content')
@slot('title')
```

### Configuration

**Go-to-definition** for config keys. Opens the config file.

```php
// Click 'app.name' to open config/app.php
$appName = config('app.name');

// Nested keys
$driver = config('database.default');
$mailHost = config('mail.mailers.smtp.host');
```

### Environment Variables

**Go-to-definition** for env() calls. Opens `.env` at the variable location.

```php
// Click 'APP_NAME' to jump to .env
$name = env('APP_NAME', 'Laravel');

// Works with any env variable
$debug = env('APP_DEBUG', false);
$dbHost = env('DB_HOST', '127.0.0.1');
```

### Routes

**Go-to-definition** for named routes. Finds the route definition.

```php
// Click 'users.show' to find where it's defined in routes/*.php
$url = route('users.show', $user);

// Works with redirect helpers
return redirect()->route('dashboard');
return to_route('login');

// And URL generation
URL::route('home');
Route::has('admin.panel');
```

### Translations

**Go-to-definition** for translation keys. Opens the language file.

```php
// Click 'auth.failed' to open lang/en/auth.php
$message = __('auth.failed');

// Alternative helpers
trans('messages.welcome');
trans_choice('items.count', 5);
Lang::get('validation.required');
```

```blade
{{-- Works in Blade too --}}
{{ __('Welcome to our app') }}
@lang('messages.greeting')
```

### Middleware

**Go-to-definition** for middleware aliases. Opens the middleware class.

```php
// Click 'auth' to open app/Http/Middleware/Authenticate.php
Route::middleware('auth')->group(function () {
    // ...
});

// Array syntax
Route::middleware(['auth', 'verified'])->get('/dashboard', ...);

// Chained
Route::get('/profile', ...)->middleware('auth');
```

### Service Container Bindings

**Go-to-definition** for app() and resolve() calls.

```php
// Click to find where 'cache' is bound
$cache = app('cache');

// Class-based resolution
$payment = app(PaymentGateway::class);
$service = resolve(UserService::class);
```

### Assets

**Go-to-definition** for asset helpers. Opens the file if it exists.

```php
// Click to open public/css/app.css
$css = asset('css/app.css');

// Mix assets
$js = mix('js/app.js');
```

```blade
{{-- Vite assets - each path is clickable --}}
@vite(['resources/css/app.css', 'resources/js/app.js'])

<link href="{{ asset('favicon.ico') }}">
<script src="{{ mix('js/vendor.js') }}"></script>
```

### Path Helpers

**Go-to-definition** for Laravel path helpers.

```php
// Each opens the referenced file/directory
$public = public_path('assets/logo.png');
$storage = storage_path('logs/laravel.log');
$app = app_path('Models/User.php');
$base = base_path('routes/api.php');
$database = database_path('seeders/UserSeeder.php');
$resource = resource_path('views/welcome.blade.php');
$config = config_path('app.php');
$lang = lang_path('en/messages.php');
```

---

## Diagnostics

Real-time validation with inline warnings and errors.

### Missing Views

```php
// ERROR: View file not found
return view('users.missing');  // ← Red underline

// Shows: "View file not found: 'users.missing'
//        Expected at: resources/views/users/missing.blade.php"
```

```blade
{{-- WARNING: Missing layout --}}
@extends('layouts.missing')

{{-- WARNING: Missing partial --}}
@include('partials.undefined')
```

### Missing Blade Components

```blade
{{-- WARNING: Component class not found --}}
<x-undefined-component />

{{-- Shows: "Blade component not found: 'undefined-component'
            Expected at: app/View/Components/UndefinedComponent.php" --}}
```

### Missing Livewire Components

```blade
{{-- WARNING: Livewire class not found --}}
<livewire:missing-component />

{{-- Shows: "Livewire component not found: 'missing-component'
            Expected at: app/Livewire/MissingComponent.php" --}}
```

### Undefined Environment Variables

```php
// WARNING: No fallback, will return null
$key = env('UNDEFINED_VAR');

// INFO: Has fallback, safe to use
$key = env('UNDEFINED_VAR', 'default');
```

### Invalid Middleware

```php
// ERROR: Middleware not found
Route::middleware('undefined-middleware')->group(...);

// Shows: "Middleware 'undefined-middleware' not found
//        Expected at: app/Http/Middleware/UndefinedMiddleware.php
//
//        Create the middleware or add an alias in bootstrap/app.php"
```

### Missing Translations

```php
// ERROR: Translation file not found
$msg = __('undefined.key');
```

```blade
{{-- ERROR: Translation not found --}}
{{ __('missing.translation') }}

{{-- WARNING: @lang directive --}}
@lang('undefined.message')
```

### Undefined Container Bindings

```php
// ERROR: Binding not found
$service = app('undefined-service');

// Shows: "Container binding 'undefined-service' not found
//
//        Define this binding in a service provider's register() method"
```

### Missing Assets

```php
// ERROR: Asset file not found
$css = asset('css/missing.css');
$js = mix('js/undefined.js');
```

```blade
{{-- WARNING: Vite asset not found --}}
@vite(['resources/css/missing.css'])

{{-- Shows: "Asset file not found: 'resources/css/missing.css'
            Expected at: /path/to/project/resources/css/missing.css
            Helper: @vite()" --}}
```

---

## Quick Actions

Instant file creation from diagnostic warnings. Click the lightbulb or press `Cmd+.` to see available fixes.

### Create Missing Views

```php
// Diagnostic: View file not found: 'users.profile'
return view('users.profile');
// Quick Action: "Create view: users.profile"
// Creates: resources/views/users/profile.blade.php
```

### Create Missing Components

```blade
{{-- Diagnostic: Blade component not found: 'button' --}}
<x-button>Click me</x-button>

{{-- Quick Actions:
     1. "Create component: button"
        → resources/views/components/button.blade.php

     2. "Create component with class: button"
        → resources/views/components/button.blade.php
        → app/View/Components/Button.php
--}}
```

### Create Missing Livewire Components

```blade
{{-- Diagnostic: Livewire component not found: 'counter' --}}
<livewire:counter />

{{-- Quick Action: "Create Livewire: counter"
     Creates both:
     → app/Livewire/Counter.php
     → resources/views/livewire/counter.blade.php
--}}
```

### Create Missing Middleware

```php
// Diagnostic: Middleware 'custom' not found
Route::middleware('custom')->group(...);
// Quick Action: "Create middleware: custom"
// Creates: app/Http/Middleware/Custom.php
```

### Create Missing Translations

```php
// Diagnostic: Translation not found: 'messages.welcome'
__('messages.welcome');

// If file exists: "Add translation: messages.welcome"
// If file missing: "Create translation: messages.welcome"
// Creates/updates: lang/en/messages.php
```

### Create Missing Config

```php
// Diagnostic: Config not found: 'custom.setting'
config('custom.setting');

// If file exists: "Add config: custom.setting"
// If file missing: "Create config: custom.setting"
// Creates/updates: config/custom.php
```

### Create Missing Environment Variables

```php
// Diagnostic: Environment variable 'CUSTOM_KEY' not found
env('CUSTOM_KEY');

// Quick Actions:
// - "Add env var: CUSTOM_KEY" (if .env exists)
// - "Create .env with CUSTOM_KEY" (if .env missing)
// - "Copy .env.example to .env" (if .env.example exists)
```

---

## Planned Features

### Auto-Completion

- [ ] Route names when typing `route('...')`
- [ ] Config keys when typing `config('...')`
- [ ] Translation keys when typing `__('...')`
- [ ] Blade component names when typing `<x-...`
- [ ] Validation rules
- [ ] Eloquent model fields and relationships

### Code Lens

- [ ] Reference counts above Blade views ("3 references")
- [ ] Click to show all usages

### InertiaJS Support

- [ ] Go-to-definition for `Inertia::render('Page')`
- [ ] Component path resolution

### Hover Information

- [ ] Show actual `.env` values on hover
- [ ] Show config values on hover
- [ ] Links to Laravel documentation

---

## Requirements

- [Zed Editor](https://zed.dev)
- A Laravel project

The extension automatically detects Laravel projects by looking for `composer.json` with Laravel dependencies.

---

## Configuration

No configuration required. The extension automatically discovers:

- View paths from `config/view.php`
- Component namespaces from `composer.json`
- Middleware aliases from `app/Http/Kernel.php` or `bootstrap/app.php`
- Service bindings from service providers

---

## Performance

Built with performance in mind:

- **Instant responses** - Go-to-definition in 2-15ms
- **No typing lag** - File parsing is debounced
- **Incremental updates** - Only re-parses changed files
- **Query caching** - Tree-sitter queries compiled once

---

## Development

```bash
# Build the LSP server
cd laravel-lsp && cargo build --release

# Run tests
cargo test

# Build for release
./build.sh
```

### Project Structure

```
zed-laravel/
├── src/                    # Zed extension (Rust → WASM)
├── laravel-lsp/            # Language server (Rust)
│   ├── src/
│   │   ├── main.rs         # LSP handlers
│   │   ├── queries.rs      # Pattern extraction
│   │   ├── parser.rs       # Tree-sitter parsing
│   │   └── config.rs       # Laravel project discovery
│   └── queries/            # Tree-sitter query files
└── extension.toml          # Extension manifest
```

---

## Contributing

Contributions welcome! Areas of interest:

- New Laravel pattern support (Inertia, Folio, etc.)
- Auto-completion implementation
- Diagnostics for common mistakes
- Performance improvements

---

## License

MIT
