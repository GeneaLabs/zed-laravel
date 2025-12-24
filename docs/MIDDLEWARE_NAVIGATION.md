# Middleware Navigation in Laravel LSP

## Overview

The Laravel LSP extension for Zed provides intelligent goto-definition support for middleware references in your route files. Click on any middleware name to instantly jump to its class definition.

## Supported Syntax

### 1. Single Middleware

```php
Route::middleware('auth')->group(function () {
    // Click on 'auth' to jump to App\Http\Middleware\Authenticate.php
});

Route::get('/dashboard')->middleware('verified');
//                                    ^^^^^^^^^
//                                    Cmd+Click to navigate
```

### 2. Multiple Middleware (Arrays)

```php
Route::middleware(['auth', 'verified'])->group(function () {
    // Click on 'auth' → App\Http\Middleware\Authenticate.php
    // Click on 'verified' → Illuminate\Auth\Middleware\EnsureEmailIsVerified.php
});
```

### 3. Middleware with Parameters

```php
Route::middleware('throttle:60,1')->group(function () {
    // Click on 'throttle:60,1' → Illuminate\Routing\Middleware\ThrottleRequests.php
});

Route::middleware('role:admin,editor')->get('/admin');
//                  ^^^^^^^^^^^^^^^^^^
//                  Navigation works with parameters too
```

### 4. Without Middleware

```php
Route::withoutMiddleware('auth')->get('/public', function () {
    // Click on 'auth' to see the middleware being excluded
});
```

### 5. All Route Methods

Works with all route definition methods:

```php
// Static methods
Route::middleware('auth')->group(...);
Route::middleware('verified')->prefix(...);

// Chained methods  
Route::get('/profile')->middleware('auth');
Route::post('/update')->middleware(['auth', 'verified']);
Route::delete('/account')->withoutMiddleware('csrf');
```

## How to Use

1. **Hover** over a middleware name to see information (coming soon)
2. **Cmd+Click** (Mac) or **Ctrl+Click** (Windows/Linux) to jump to the middleware class
3. **Option+Cmd** (Mac) or **Alt** (Windows/Linux) + hover to preview the definition

## Supported Middleware

### Application Middleware

The extension automatically finds middleware defined in:
- **Laravel 10 and below:** `app/Http/Kernel.php` in the `$middlewareAliases` array
- **Laravel 11+:** `bootstrap/app.php` in the `->alias([...])` configuration

Example from `app/Http/Kernel.php`:
```php
protected $middlewareAliases = [
    'auth' => \App\Http\Middleware\Authenticate::class,
    'guest' => \App\Http\Middleware\RedirectIfAuthenticated::class,
    'verified' => \Illuminate\Auth\Middleware\EnsureEmailIsVerified::class,
];
```

### Framework Middleware

Common Laravel framework middleware is also supported:
- `auth` - Authenticate users
- `auth.basic` - HTTP Basic authentication
- `auth.session` - Session authentication
- `cache.headers` - Cache control headers
- `can` - Authorization middleware
- `guest` - Redirect if authenticated
- `password.confirm` - Require password confirmation
- `signed` - Validate signed routes
- `throttle` - Rate limiting
- `verified` - Email verification

## Custom Middleware

To add navigation support for custom middleware:

1. Register the middleware alias in your configuration:

**Laravel 10 and below** (`app/Http/Kernel.php`):
```php
protected $middlewareAliases = [
    'admin' => \App\Http\Middleware\AdminMiddleware::class,
];
```

**Laravel 11+** (`bootstrap/app.php`):
```php
->withMiddleware(function (Middleware $middleware) {
    $middleware->alias([
        'admin' => \App\Http\Middleware\AdminMiddleware::class,
    ]);
})
```

2. The LSP will automatically pick up the new middleware on restart

## Troubleshooting

### Middleware Not Found

If goto-definition doesn't work for a middleware:

1. **Check middleware is registered** - Verify the alias exists in `Kernel.php` or `bootstrap/app.php`
2. **Restart the LSP** - Reload the Zed window to refresh middleware configuration
3. **Check file exists** - Ensure the middleware class file exists at the expected path

### Framework Middleware

Framework middleware (from `Illuminate\*` namespace) won't navigate to vendor files. This is intentional to avoid navigating outside your project.

### Path Resolution

The extension uses PSR-4 autoloading conventions:
- `App\Http\Middleware\Authenticate` → `app/Http/Middleware/Authenticate.php`
- `App\Middleware\Custom` → `app/Middleware/Custom.php`

## Examples

### Complete Route File Example

```php
<?php

use Illuminate\Support\Facades\Route;

// Public routes
Route::get('/', function () {
    return view('welcome');
});

// Authenticated routes
Route::middleware('auth')->group(function () {
    Route::get('/dashboard', function () {
        return view('dashboard');
    });
    
    // Verified email required
    Route::middleware('verified')->group(function () {
        Route::get('/profile', [ProfileController::class, 'show']);
        Route::post('/profile', [ProfileController::class, 'update']);
    });
});

// API routes with rate limiting
Route::prefix('api')->middleware(['auth', 'throttle:60,1'])->group(function () {
    Route::get('/user', function (Request $request) {
        return $request->user();
    });
});

// Admin routes with multiple middleware
Route::middleware(['auth', 'verified', 'admin'])->group(function () {
    Route::get('/admin', [AdminController::class, 'index']);
});

// Public API without CSRF protection
Route::withoutMiddleware('csrf')->group(function () {
    Route::post('/webhook', [WebhookController::class, 'handle']);
});
```

**Try it out:** Click on any middleware name in the code above to jump to its definition!

## Tips

1. **Quick Navigation** - Use Cmd+Click (Mac) or Ctrl+Click (Windows/Linux) for fastest navigation
2. **Go Back** - After jumping to a middleware, use Cmd+[ (Mac) or Alt+Left (Windows/Linux) to go back
3. **Explore Dependencies** - Once in the middleware file, you can navigate to any dependencies the same way
4. **Learn Laravel** - Great way to explore how Laravel's built-in middleware works

## Related Features

- [View Navigation](./VIEW_NAVIGATION.md) - Jump to Blade views
- [Config Navigation](./CONFIG_NAVIGATION.md) - Navigate to config files
- [Env Navigation](./ENV_NAVIGATION.md) - Jump to environment variable definitions
- [Component Navigation](./COMPONENT_NAVIGATION.md) - Navigate to Blade components

## Feedback

Found a bug or have a suggestion? Please open an issue on the [GitHub repository](https://github.com/yourusername/zed-laravel).