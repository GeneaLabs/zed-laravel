# Diagnostic Severity Guide

## 🎯 Overview

This guide explains the diagnostic severity levels used in the Laravel LSP extension and the reasoning behind each level. Understanding these levels helps developers prioritize fixes and understand the impact of missing resources.

---

## 📊 Severity Levels

### 🔴 ERROR
**Visual:** Red squiggle underline  
**When to use:** Code that will definitely break at runtime  
**User action:** Must fix before deploying

### 🟡 WARNING
**Visual:** Yellow squiggle underline  
**When to use:** Code that will likely cause problems  
**User action:** Should fix soon

### 🔵 INFORMATION
**Visual:** Blue information icon/underline  
**When to use:** Helpful hints or non-critical issues  
**User action:** Optional - review when convenient

---

## 🗂️ Laravel LSP Diagnostic Rules

### Translation Keys

#### ❌ ERROR: Missing Dotted-Key Translation File
```php
// ❌ ERROR: File lang/en/messages.php not found
__('messages.welcome')
trans('auth.failed')
```

**Severity:** `ERROR`

**Reason:**
- Laravel will throw a runtime exception if the translation file doesn't exist
- Dotted notation explicitly references a file: `messages.welcome` → `lang/en/messages.php`
- This WILL break the application at runtime

**Message:**
```
Translation file not found for key 'messages.welcome'
Expected at: lang/en/messages.php or resources/lang/en/messages.php
```

**How to fix:**
1. Create the missing translation file
2. Add the translation key to an existing file
3. Use a different key that exists

---

#### ℹ️ INFO: Missing JSON Translation File
```php
// ℹ️ INFO: File lang/en.json not found
__('Welcome to our app')
__('Confirm')
```

**Severity:** `INFORMATION`

**Reason:**
- Laravel will fallback to displaying the key itself if no JSON file exists
- This won't break the application - just shows the English text
- Text keys are often used for simple strings that don't need translation
- Developer might intentionally skip creating JSON file for English-only apps

**Message:**
```
Translation file not found for key 'Welcome to our app'
Create lang/en.json or resources/lang/en.json to add this translation
```

**How to fix:**
1. Create `lang/en.json` with the translations
2. Leave as-is if English-only app (the text will display correctly)

---

### Middleware

#### ❌ ERROR: Middleware Class File Missing
```php
// ❌ ERROR: Middleware in config but class file missing
Route::middleware('auth')->get('/test', ...);
->middleware('verified')
```

**Severity:** `ERROR`

**Reason:**
- Middleware is defined in configuration
- But the middleware class file doesn't exist
- Laravel will throw a runtime exception when trying to load the class
- Application will crash when this route is accessed

**Message:**
```
Middleware class file not found for 'auth'
Expected at: app/Http/Middleware/Authenticate.php
Class: App\Http\Middleware\Authenticate
```

**How to fix:**
1. Create the missing middleware class file
2. Verify the file path in configuration is correct
3. Check if file was deleted or moved

---

#### ℹ️ INFO: Middleware Not in Configuration
```php
// ℹ️ INFO: Middleware alias not found in config
Route::middleware('custom')->get('/test', ...);
->middleware('unregistered')
```

**Severity:** `INFORMATION`

**Reason:**
- Middleware might be a framework middleware (e.g., `web`, `api`, `auth`)
- Could be defined in a package
- Might be registered dynamically
- Config file might not be parsed correctly yet
- Won't necessarily break at runtime if middleware exists elsewhere

**Message:**
```
Middleware alias 'custom' not found in configuration files
Check bootstrap/app.php or app/Http/Kernel.php
```

**How to fix:**
1. Register the middleware alias in `bootstrap/app.php` (Laravel 11+)
2. Register in `app/Http/Kernel.php` (Laravel 10 and below)
3. Verify it's not a typo
4. Ignore if it's a framework or package middleware

---

### Environment Variables

#### ⚠️ WARNING: Missing Env Variable (No Fallback)
```php
// ⚠️ WARNING: Variable not in .env and no fallback
env('MISSING_VAR')
env('API_KEY')
```

**Severity:** `WARNING`

**Reason:**
- No fallback value provided - will return `null`
- Likely to cause issues if the variable is used without null checking
- Should be defined in `.env` file
- Not necessarily a runtime error (code might handle null)

**Message:**
```
Environment variable 'MISSING_VAR' not found in .env files and has no fallback
Define it in .env, .env.example, or .env.local
```

**How to fix:**
1. Add the variable to `.env` file
2. Add a fallback: `env('MISSING_VAR', 'default')`
3. Add null handling in code

---

#### ℹ️ INFO: Missing Env Variable (Has Fallback)
```php
// ℹ️ INFO: Variable not in .env but has safe fallback
env('DEBUG_MODE', false)
env('CACHE_DRIVER', 'file')
```

**Severity:** `INFORMATION`

**Reason:**
- Fallback value will be used - no runtime error
- Application will work correctly with default value
- Good practice to document in `.env.example`
- Not urgent to fix

**Message:**
```
Environment variable 'DEBUG_MODE' not found in .env files (using fallback value)
```

**How to fix:**
1. Add to `.env` if you want to override the default
2. Document in `.env.example`
3. Leave as-is if default is acceptable

---

### Views

#### ⚠️ WARNING: Missing View File
```php
// ⚠️ WARNING: View file not found
view('missing.view')
return view('dashboard');
```

**Severity:** `WARNING`

**Reason:**
- Laravel will throw a runtime exception if view doesn't exist
- Will break the application at runtime
- Clear error that needs fixing

**Message:**
```
View file not found: 'dashboard'
Expected at: resources/views/dashboard.blade.php
```

**How to fix:**
1. Create the missing view file
2. Fix the view name typo
3. Check if view is in a different location

---

## 🎨 Visual Guide

### In Your Editor

```
ERROR (red squiggle):
    __('messages.welcome')  // messages.php missing
    ~~~~~~~~~~~~~~~~~~

WARNING (yellow squiggle):
    view('missing')  // View file missing
    ~~~~~~~~~~~~~~~

INFO (blue underline):
    __('Welcome')  // JSON file missing but will fallback
    ~~~~~~~~~~~~~
    
    Route::middleware('custom')  // Middleware not in config
                       ~~~~~~~~
```

---

## 📋 Decision Matrix

Use this matrix to determine severity:

| Will it break at runtime? | Has safe fallback? | Severity | Color |
|---------------------------|-------------------|----------|-------|
| YES ✅ | NO ❌ | ERROR | 🔴 Red |
| PROBABLY ⚠️ | NO ❌ | WARNING | 🟡 Yellow |
| MAYBE ❓ | YES ✅ | INFO | 🔵 Blue |
| NO ❌ | YES ✅ | INFO | 🔵 Blue |
| UNKNOWN ❓ | UNKNOWN ❓ | INFO | 🔵 Blue |

---

## 🔄 Examples by Severity

### 🔴 ERROR Examples
```php
// Translation file missing (dotted keys)
__('messages.welcome')      // ERROR: lang/en/messages.php missing
trans('auth.failed')        // ERROR: lang/en/auth.php missing
@lang('validation.required') // ERROR: lang/en/validation.php missing

// Middleware class file missing
Route::middleware('auth')   // ERROR: Authenticate.php missing (if in config)
->middleware('verified')    // ERROR: EnsureEmailIsVerified.php missing (if in config)
```

### 🟡 WARNING Examples
```php
// View files missing
view('dashboard')           // WARNING: dashboard.blade.php missing
return view('admin.users')  // WARNING: admin/users.blade.php missing

// Env vars without fallback
env('STRIPE_KEY')          // WARNING: Not in .env, no fallback
config('app.custom')       // WARNING (if config file missing)
```

### 🔵 INFO Examples
```php
// JSON translations missing
__('Welcome')              // INFO: en.json missing, will show "Welcome"
__('Click here')           // INFO: en.json missing, will show "Click here"

// Middleware not in config (might be framework middleware)
Route::middleware('web')   // INFO: Not in config, might be built-in
->middleware('api')        // INFO: Not in config, might be built-in
->middleware('custom')     // INFO: Not in config, might be defined elsewhere

// Env vars with fallback
env('DEBUG', false)        // INFO: Not in .env, using false
env('CACHE_TTL', 3600)    // INFO: Not in .env, using 3600
```

---

## 🎯 Best Practices

### For Developers

1. **Fix ERRORs first** - These will break your app
2. **Review WARNINGs soon** - These might cause issues
3. **Check INFOs when convenient** - These are helpful hints

### For Extension Developers

1. **Use ERROR sparingly** - Only for definite runtime failures
2. **Use WARNING for likely issues** - Things that should be fixed
3. **Use INFO generously** - Helpful hints that won't break the app
4. **Provide clear messages** - Explain what's wrong and how to fix it
5. **Include file paths** - Show exactly where to make changes

---

## 📊 Severity Distribution

In a typical Laravel project, you should see:

```
✅ Healthy Project:
   - Errors: 0
   - Warnings: 0-2 (optional configs)
   - Info: 2-5 (helpful hints, framework middleware)

⚠️ Needs Attention:
   - Errors: 1-3 (missing files)
   - Warnings: 3-10 (missing env vars)
   - Info: 5-15 (suggestions)

❌ Critical Issues:
   - Errors: 4+ (broken code)
   - Warnings: 10+ (many issues)
   - Info: Many (lots of suggestions)
```

---

## 🔧 Configuration

If you find the diagnostics too noisy, you can configure Zed to:

1. **Hide INFO diagnostics** - Only show errors and warnings
2. **Disable specific checks** - Turn off translation or middleware checks
3. **Adjust severity** - Customize levels per your needs

*(Configuration options will be added in future versions)*

---

## 📝 Summary Table

| Feature | Missing Resource | Severity | Runtime Impact |
|---------|-----------------|----------|----------------|
| Translation (dotted) | PHP file | ERROR 🔴 | Exception thrown |
| Translation (text) | JSON file | INFO 🔵 | Shows key as text |
| Middleware (in config) | Class file | ERROR 🔴 | Exception thrown |
| Middleware (not in config) | Config entry | INFO 🔵 | May work (framework) |
| View | Blade file | WARNING 🟡 | Exception thrown |
| Env (no fallback) | .env entry | WARNING 🟡 | Returns null |
| Env (has fallback) | .env entry | INFO 🔵 | Uses fallback |

---

**Last Updated:** 2024  
**Version:** 1.0  
**Status:** ✅ Active