# Container Binding Navigation

## Overview

The Laravel LSP now provides intelligent goto-definition navigation for Laravel's service container bindings. This feature helps you navigate from `app()` calls to their bound implementations or registration locations.

## Features

### 1. Class Reference Navigation (`app(SomeClass::class)`)

When you use `app()` with a class constant, the LSP navigates directly to the class file:

```php
// Click on UserService::class to jump to the class file
app(\App\Services\UserService::class);
app(UserService::class);
```

**Behavior:**
- ✅ Navigates to the class file if it exists
- ✅ Returns empty result (prevents Zed's fallback) if class not found

### 2. String Binding Navigation (`app('string')`)

When you use `app()` with a string identifier, the LSP checks the service provider registry:

```php
// Click on 'cache' to navigate to bound class or registration
app('cache');
app('user.service');
app('auth');
```

**Behavior:**
- ✅ If binding exists: Navigates to the bound concrete class file
- ✅ If class file doesn't exist: Navigates to where the binding is registered in the service provider
- ✅ If no binding found: Returns empty result (prevents Zed's fallback navigation)

### 3. Prevents False Navigation

The LSP explicitly returns an empty array for invalid bindings, preventing Zed's default PHP symbol resolution from navigating to unrelated code:

```php
// This will NOT navigate to Pest's test() function
app('test'); // Returns empty - no navigation
```

**Before this feature:**
- Clicking `app('test')` would navigate to Pest's `test()` function ❌

**After this feature:**
- Clicking `app('test')` does nothing (correct behavior) ✅

## How It Works

### 1. Service Provider Scanning

On initialization, the LSP scans all service providers and builds a registry of container bindings:

```php
// AppServiceProvider.php
public function register(): void
{
    // Detected as: 'test' -> App\Models\User
    $this->app->bind('test', \App\Models\User::class);
    
    // Detected as: 'cache' -> Illuminate\Cache\CacheManager
    $this->app->singleton('cache', \Illuminate\Cache\CacheManager::class);
    
    // Detected as: App\Contracts\PaymentGateway -> App\Services\StripeGateway
    $this->app->bind(\App\Contracts\PaymentGateway::class, \App\Services\StripeGateway::class);
    
    // Detected as: 'user.service' -> App\Services\UserService
    $this->app->alias(\App\Services\UserService::class, 'user.service');
}
```

### 2. Binding Detection

The LSP uses regex patterns to detect these binding methods:
- `bind(abstract, concrete)`
- `singleton(abstract, concrete)`
- `scoped(abstract, concrete)`
- `alias(concrete, alias)`

### 3. Navigation Resolution

When you click on an `app()` call:

```
app('cache')
     ↓
1. Check if 'cache' is registered in service provider
     ↓
2. Found: 'cache' -> Illuminate\Cache\CacheManager
     ↓
3. Try to resolve class file: vendor/laravel/framework/src/Illuminate/Cache/CacheManager.php
     ↓
4. Navigate to class file (or registration location if class not found)
```

## Supported Patterns

### Binding Registration (Detected)

```php
// All these patterns are detected and cached:
$this->app->bind('abstract', Concrete::class);
$this->app->bind(Abstract::class, Concrete::class);
$this->app->bind('abstract', 'ConcreteClass');

$this->app->singleton('abstract', Concrete::class);
$this->app->singleton(Abstract::class, Concrete::class);

$this->app->scoped('abstract', Concrete::class);
$this->app->scoped(Abstract::class, Concrete::class);

$this->app->alias(Concrete::class, 'alias');
$this->app->alias('ConcreteClass', 'alias');

// Closure bindings also detected (navigates to service provider)
$this->app->bind('service', function ($app) {
    return new Service();
});
```

### Container Resolution (Navigable)

```php
// All these patterns trigger goto-definition:
app('string')
app("string")
app(SomeClass::class)
app(\App\Services\SomeClass::class)
```

## Navigation Priority

For string bindings, the LSP tries navigation in this order:

1. **Bound concrete class file** (if resolvable)
   ```
   app('cache') → vendor/laravel/framework/src/Illuminate/Cache/CacheManager.php
   ```

2. **Binding registration location** (if class file not found)
   ```
   app('custom.service') → AppServiceProvider.php (line where bind() was called)
   ```

3. **Empty result** (if binding not found)
   ```
   app('nonexistent') → No navigation (prevents false positives)
   ```

## Registry Caching

The service provider registry is:
- ✅ Built on LSP initialization
- ✅ Cached in memory for fast lookups
- 🔄 Refreshed when service providers change (future enhancement)

## Benefits

### 1. Accurate Navigation
- No more false positives (like navigating to Pest's `test()` function)
- Only navigates when a real binding exists

### 2. Developer Experience
- Jump to implementation quickly
- Discover where bindings are registered
- Understand the container's binding structure

### 3. Type Safety
- Validates that bindings actually exist
- Helps catch typos in binding names

## Examples

### Example 1: Navigate to Bound Class

```php
// In AppServiceProvider.php
$this->app->singleton('cache', \Illuminate\Cache\CacheManager::class);

// In your controller
app('cache'); // Click → jumps to CacheManager.php
```

### Example 2: Navigate to Registration

```php
// In AppServiceProvider.php (line 25)
$this->app->bind('payment', function ($app) {
    return new PaymentService();
});

// In your controller
app('payment'); // Click → jumps to AppServiceProvider.php:25
```

### Example 3: Navigate to Class Directly

```php
// No binding needed - resolves directly
app(\App\Services\UserService::class); // Click → jumps to UserService.php
```

### Example 4: Prevent False Navigation

```php
// Not bound anywhere
app('test'); // Click → nothing happens (correct!)
```

## Current Limitations

### Not Yet Implemented
- ❌ Context-aware bindings (when conditional)
- ❌ Contextual bindings (`$this->app->when()`)
- ❌ Tagged bindings (`$this->app->tag()`)
- ❌ Auto-refresh on service provider changes
- ❌ Line-precise navigation to specific array keys in PHP return arrays

### Works With
- ✅ `bind()`, `singleton()`, `scoped()`, `alias()`
- ✅ String abstracts and class abstracts
- ✅ Fully qualified class names
- ✅ PSR-4 autoloading conventions

## Future Enhancements

1. **Watch service providers** for changes and refresh registry
2. **Parse binding closures** to extract concrete types
3. **Support contextual bindings** (`when()`, `needs()`, `give()`)
4. **Navigate to exact line** in config arrays
5. **Show hover documentation** with binding type and source

## Technical Details

### Tree-Sitter Queries

The LSP uses these tree-sitter queries to detect `app()` calls:

```scheme
; String bindings: app('cache')
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument (string (string_content) @binding_name)))
  (#eq? @function_name "app"))

; Class bindings: app(SomeClass::class)
(function_call_expression
  function: (name) @function_name
  arguments: (arguments
    (argument
      (class_constant_access_expression
        (qualified_name) @binding_class_name
        (name) @constant_name)))
  (#eq? @function_name "app")
  (#eq? @constant_name "class"))
```

### Binding Registry Structure

```rust
pub struct BindingRegistration {
    pub abstract_name: String,        // 'cache' or 'App\Contracts\Interface'
    pub concrete_class: String,       // 'Illuminate\Cache\CacheManager'
    pub file_path: Option<PathBuf>,   // Resolved path to concrete class
    pub binding_type: BindingType,    // Bind, Singleton, Scoped, Alias
    pub registered_in: String,        // Service provider file path
    pub source_file: Option<PathBuf>, // For goto-definition to registration
    pub source_line: Option<usize>,   // Line number in service provider
    pub priority: u8,                 // Framework=0, Package=1, App=2
}
```

### Class Resolution

The LSP resolves classes using PSR-4 conventions:

```
App\Services\UserService → app/Services/UserService.php
Illuminate\Cache\CacheManager → vendor/laravel/framework/src/Illuminate/Cache/CacheManager.php
```

## Testing

Test the feature with these examples in your Laravel project:

```php
// In app/Providers/AppServiceProvider.php
public function register(): void
{
    $this->app->bind('test', \App\Models\User::class);
    $this->app->singleton('cache', \Illuminate\Cache\CacheManager::class);
    $this->app->alias(\App\Services\UserService::class, 'user.service');
}

// In routes/web.php or any controller
app('test');              // Should navigate to AppServiceProvider or User.php
app('cache');             // Should navigate to CacheManager.php
app('user.service');      // Should navigate to UserService.php
app(\App\Models\User::class); // Should navigate to User.php
app('nonexistent');       // Should do nothing (no navigation)
```

## Troubleshooting

### Navigation Not Working

1. **Check service provider is scanned**
   - Only files in `app/Providers/` are scanned by default
   - Package providers are detected via `composer.json`

2. **Check binding syntax**
   - Use `bind()`, `singleton()`, `scoped()`, or `alias()`
   - Regex may not catch complex patterns

3. **Restart LSP**
   - Registry is built on initialization
   - Changes to service providers require LSP restart (for now)

### Navigates to Wrong Location

1. **Multiple bindings**
   - If the same abstract is bound multiple times, priority matters
   - App providers (priority 2) override package providers (priority 1)

2. **Class resolution fails**
   - Falls back to registration location
   - Check PSR-4 autoload configuration

## See Also

- [Laravel Container Documentation](https://laravel.com/docs/11.x/container)
- [Service Provider Documentation](https://laravel.com/docs/11.x/providers)
- [PSR-4 Autoloading](https://www.php-fig.org/psr/psr-4/)