# Container Binding Navigation - Implementation in Progress

**Date**: December 21, 2024  
**Status**: 🚧 Foundation Complete - Ready for Full Implementation  
**Feature**: Navigate from container resolution to binding definition

## Overview

Implementing goto-definition for Laravel service container bindings. This will allow developers to navigate from where they resolve dependencies (using `app()`, `resolve()`, or `App::make()`) to where those bindings are registered in service providers.

## Use Cases

### Use Case 1: Interface to Implementation Navigation

```php
// Somewhere in your code
$gateway = app(PaymentGatewayInterface::class);
//             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//             Click here → jumps to where it's bound

// app/Providers/AppServiceProvider.php
public function register()
{
    $this->app->bind(
        PaymentGatewayInterface::class,  // ← Jumps here
        StripePaymentGateway::class
    );
}
```

### Use Case 2: Concrete Class Resolution

```php
// In a controller
$service = resolve(UserNotificationService::class);
//                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//                 Click here → jumps to singleton binding

// app/Providers/AppServiceProvider.php
public function register()
{
    $this->app->singleton(
        UserNotificationService::class,  // ← Jumps here
        function ($app) {
            return new UserNotificationService(
                $app->make(MailService::class)
            );
        }
    );
}
```

### Use Case 3: Contextual Binding Navigation

```php
// In a controller
public function __construct(LoggerInterface::class)  // ← Want to know which implementation?
{
}

// app/Providers/AppServiceProvider.php
public function register()
{
    $this->app->when(ApiController::class)
        ->needs(LoggerInterface::class)
        ->give(CloudLogger::class);  // ← Shows this is what's used
        
    $this->app->when(WebController::class)
        ->needs(LoggerInterface::class)
        ->give(FileLogger::class);
}
```

## Supported Patterns

### Resolution Patterns (Detection Complete ✅)

Tree-sitter queries implemented for:

1. **Global app() helper**
   ```php
   app(SomeInterface::class)
   app(\App\Contracts\PaymentGateway::class)
   app('some.string.binding')
   ```

2. **resolve() helper**
   ```php
   resolve(ServiceInterface::class)
   resolve(\App\Services\NotificationService::class)
   ```

3. **App facade make()**
   ```php
   App::make(PaymentGateway::class)
   \App::make(\App\Contracts\Something::class)
   ```

4. **App facade makeWith()**
   ```php
   App::makeWith(SomeClass::class, ['param' => 'value'])
   ```

### Binding Patterns (To Implement 🚧)

Need to detect and parse:

1. **Simple bind()**
   ```php
   $this->app->bind(Interface::class, Implementation::class)
   $this->app->bind(Interface::class, function ($app) { ... })
   ```

2. **Singleton bind()**
   ```php
   $this->app->singleton(Interface::class, Implementation::class)
   $this->app->singleton(Interface::class, function ($app) { ... })
   ```

3. **Scoped bind()**
   ```php
   $this->app->scoped(Interface::class, Implementation::class)
   ```

4. **Conditional binding**
   ```php
   $this->app->bindIf(Interface::class, Implementation::class)
   $this->app->singletonIf(Interface::class, Implementation::class)
   ```

5. **Contextual binding**
   ```php
   $this->app->when(Controller::class)
       ->needs(Interface::class)
       ->give(Implementation::class)
   ```

6. **Instance binding**
   ```php
   $this->app->instance(Interface::class, $instance)
   ```

7. **Alias binding**
   ```php
   $this->app->alias(Original::class, 'alias')
   ```

## Implementation Progress

### ✅ Completed

1. **Tree-sitter Queries for Resolution**
   - Added 175 lines of tree-sitter queries to `queries/php.scm`
   - Patterns 14-17 detect all resolution methods
   - Handles both `::class` and string arguments
   - Supports qualified and unqualified class names

2. **BindingMatch Structure**
   - Created `BindingMatch<'a>` struct in `queries.rs`
   - Captures: class name, byte positions, line/column numbers
   - Lifetime-annotated for zero-copy parsing

3. **Query Function**
   - Implemented `find_binding_calls()` in `queries.rs`
   - Cleans up class names (removes `\` prefix and `::class` suffix)
   - Returns vector of all binding resolution calls
   - Integrated with existing query compilation system

4. **Module Integration**
   - Exported `BindingMatch` and `find_binding_calls` from queries module
   - Added to main.rs imports
   - Ready for use in goto-definition handler

### 🚧 In Progress / TODO

1. **Enhanced ServiceProviderRegistry**
   - [ ] Add `BindingRegistration` struct with source location
   - [ ] Track binding file path and line number
   - [ ] Store binding type (bind, singleton, scoped, etc.)
   - [ ] Store implementation class or closure info

2. **Service Provider Parsing**
   - [ ] Add tree-sitter queries for `$this->app->bind()` patterns
   - [ ] Parse all binding variants (bind, singleton, scoped, etc.)
   - [ ] Extract contextual bindings (when/needs/give)
   - [ ] Handle closure bindings (show closure location)
   - [ ] Parse instance and alias bindings

3. **Goto-Definition Implementation**
   - [ ] Add `create_binding_location()` method
   - [ ] Look up binding in ServiceProviderRegistry
   - [ ] Navigate to binding definition line
   - [ ] Handle multiple bindings (show all, pick most specific)
   - [ ] Handle contextual bindings (context-aware navigation)

4. **Diagnostics**
   - [ ] Detect unresolvable bindings
   - [ ] Warn about missing bindings
   - [ ] Info for auto-resolved concrete classes
   - [ ] Show contextual binding hints

5. **Hover Information**
   - [ ] Show bound implementation
   - [ ] Show binding type (singleton, scoped, etc.)
   - [ ] Show where binding is registered
   - [ ] Show resolved class documentation

## Technical Architecture

### Data Flow

```
1. User clicks on app(SomeInterface::class)
   ↓
2. find_binding_calls() finds the resolution
   ↓
3. create_binding_location() looks up in registry
   ↓
4. ServiceProviderRegistry returns binding info
   ↓
5. Navigate to binding definition file:line
```

### ServiceProviderRegistry Enhancement

Current structure:
```rust
pub struct ServiceProviderRegistry {
    pub bindings: HashMap<String, String>,  // interface -> implementation
    pub singletons: HashMap<String, String>,
    // ...
}
```

Enhanced structure needed:
```rust
pub struct ServiceProviderRegistry {
    pub bindings: HashMap<String, BindingRegistration>,
    pub singletons: HashMap<String, BindingRegistration>,
    pub scoped: HashMap<String, BindingRegistration>,
    pub contextual: Vec<ContextualBinding>,
    // ...
}

pub struct BindingRegistration {
    pub interface: String,
    pub implementation: String,  // or "Closure" for closures
    pub source_file: PathBuf,
    pub source_line: usize,
    pub binding_type: BindingType,
    pub is_closure: bool,
}

pub enum BindingType {
    Bind,
    Singleton,
    Scoped,
    Instance,
    Alias,
}

pub struct ContextualBinding {
    pub when: String,      // Controller class
    pub needs: String,     // Interface
    pub give: String,      // Implementation
    pub source_file: PathBuf,
    pub source_line: usize,
}
```

## Example Scenarios

### Scenario 1: Simple Interface Binding

**Code:**
```php
// Controller.php
$gateway = app(PaymentGatewayInterface::class);
```

**Binding:**
```php
// AppServiceProvider.php:15
$this->app->bind(
    PaymentGatewayInterface::class,
    StripePaymentGateway::class
);
```

**Expected Behavior:**
- Click on `PaymentGatewayInterface::class` in controller
- Jump to line 15 of `AppServiceProvider.php`
- Cursor on the `PaymentGatewayInterface::class` line in binding

### Scenario 2: Singleton with Closure

**Code:**
```php
$cache = resolve(CacheManager::class);
```

**Binding:**
```php
// CacheServiceProvider.php:22
$this->app->singleton(CacheManager::class, function ($app) {
    return new CacheManager($app->make('config'));
});
```

**Expected Behavior:**
- Click on `CacheManager::class`
- Jump to line 22 of `CacheServiceProvider.php`
- Show hover: "Singleton (Closure)" with file location

### Scenario 3: Contextual Binding

**Code:**
```php
// ApiController.php
public function __construct(LoggerInterface $logger) {}
```

**Binding:**
```php
// LogServiceProvider.php:30
$this->app->when(ApiController::class)
    ->needs(LoggerInterface::class)
    ->give(CloudLogger::class);
```

**Expected Behavior:**
- Click on `LoggerInterface` type hint
- Jump to line 31 (the ->needs line)
- Show context: "For ApiController, resolves to CloudLogger"

## Next Steps

1. **Create BindingRegistration struct** in service_provider_analyzer.rs
2. **Add tree-sitter queries** for bind() patterns in service providers
3. **Parse service providers** to extract all bindings with locations
4. **Implement create_binding_location()** in main.rs
5. **Add to goto-definition** handler
6. **Test** with real Laravel projects
7. **Add diagnostics** for missing bindings
8. **Add hover** information

## Testing Strategy

### Unit Tests
- [ ] Test bind() parsing from service providers
- [ ] Test singleton() parsing
- [ ] Test contextual binding parsing
- [ ] Test find_binding_calls() with various syntaxes

### Integration Tests
- [ ] Real Laravel project navigation
- [ ] Framework bindings (Illuminate\*)
- [ ] Package bindings (vendor packages)
- [ ] Contextual binding resolution

### Edge Cases
- [ ] Multiple bindings for same interface
- [ ] Overridden bindings (later takes precedence)
- [ ] Closure bindings (show closure location)
- [ ] Auto-resolved concrete classes (no explicit binding)

## Related Features

This feature complements:
- ✅ Middleware navigation
- ✅ Config navigation
- ✅ Translation navigation
- ✅ Environment variable navigation
- 🚧 Route navigation (future)
- 🚧 Event/listener navigation (future)

## References

- Laravel Container Documentation: https://laravel.com/docs/12.x/container
- Service Container Binding: https://laravel.com/docs/12.x/container#binding
- Contextual Binding: https://laravel.com/docs/12.x/container#contextual-binding

---

**Status**: Foundation complete, ready for full implementation. Tree-sitter queries and data structures in place.