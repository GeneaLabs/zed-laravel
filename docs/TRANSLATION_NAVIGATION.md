# Translation Navigation in Laravel LSP

## Overview

The Laravel LSP extension for Zed provides intelligent goto-definition support for translation strings. Click on any translation key to instantly jump to the translation file where it's defined.

## Supported Functions

### 1. Short Helper `__()`

Most common translation helper in Laravel:

```php
$message = __('messages.welcome');
//          ^^^^^^^^^^^^^^^^^^^
//          Cmd+Click to navigate

$error = __('auth.failed');
```

### 2. Trans Helper

```php
$title = trans('pages.home.title');
//            ^^^^^^^^^^^^^^^^^^^^^
//            Jump to lang/en/pages.php

$description = trans('validation.required');
```

### 3. Trans Choice (Pluralization)

```php
$apples = trans_choice('messages.apples', 10);
//                      ^^^^^^^^^^^^^^^^
//                      Navigate to translation

$time = trans_choice('messages.minutes_ago', $minutes);
```

### 4. Lang Facade

```php
// Get translation
$greeting = Lang::get('messages.greeting');

// Check if exists
if (Lang::has('messages.optional')) {
    // ...
}

// Pluralization
$items = Lang::choice('messages.items', $count);
```

### 5. Blade Templates

Works in Blade files too:

```blade
{{-- Standard output --}}
<h1>{{ __('pages.home.title') }}</h1>

{{-- Trans helper --}}
<p>{{ trans('messages.welcome') }}</p>

{{-- Lang directive --}}
@lang('messages.greeting')

{{-- Pluralization --}}
<span>{{ trans_choice('messages.items', $count) }}</span>
```

## Translation File Types

### PHP Translation Files

Located in `lang/en/*.php` or `resources/lang/en/*.php`:

```php
<?php
// lang/en/messages.php

return [
    'welcome' => 'Welcome to our application!',
    'farewell' => 'Goodbye!',
    
    // Nested translations
    'auth' => [
        'failed' => 'These credentials do not match our records.',
        'throttle' => 'Too many login attempts.',
    ],
];
```

**Key Format:** `file.key` or `file.nested.key`

Examples:
- `__('messages.welcome')` → `lang/en/messages.php` → `['welcome']`
- `__('messages.auth.failed')` → `lang/en/messages.php` → `['auth']['failed']`

### JSON Translation Files

Located in `lang/en.json` or `resources/lang/en.json`:

```json
{
    "Welcome": "Welcome",
    "Login": "Login",
    "Profile": "Profile",
    "Welcome to our application": "Welcome to our application"
}
```

**Key Format:** Plain strings (no dots, or contains spaces)

Examples:
- `__('Welcome')` → `lang/en.json`
- `__('Welcome to our application')` → `lang/en.json`

## How to Use

1. **Hover** over a translation key to see information (coming soon)
2. **Cmd+Click** (Mac) or **Ctrl+Click** (Windows/Linux) to jump to the translation file
3. **Option+Cmd** (Mac) or **Alt** (Windows/Linux) + hover to preview

## Supported Directory Structures

The extension automatically detects your Laravel version:

### Laravel 9+ (Recommended)
```
lang/
├── en/
│   ├── messages.php
│   ├── auth.php
│   └── validation.php
├── en.json
└── es/
    └── ...
```

### Laravel 8 and Below (Legacy)
```
resources/
└── lang/
    ├── en/
    │   ├── messages.php
    │   └── ...
    └── en.json
```

Both structures are supported automatically!

## Complete Example

```php
<?php

namespace App\Http\Controllers;

class HomeController extends Controller
{
    public function index()
    {
        return view('home', [
            // Click on any of these keys to navigate
            'title' => __('pages.home.title'),
            'description' => trans('pages.home.description'),
            'welcome' => __('Welcome to our site'),
        ]);
    }
    
    public function profile()
    {
        $user = auth()->user();
        
        return view('profile', [
            // Pluralization
            'posts_count' => trans_choice('messages.posts', $user->posts()->count()),
            
            // Nested keys
            'validation_error' => __('validation.custom.email.required'),
            
            // Lang facade
            'greeting' => Lang::get('messages.greeting'),
        ]);
    }
}
```

## Blade Template Example

```blade
@extends('layouts.app')

@section('title', __('pages.home.title'))

@section('content')
    <div class="container">
        {{-- Jump to lang/en/messages.php --}}
        <h1>{{ __('messages.welcome') }}</h1>
        
        {{-- Jump to lang/en.json --}}
        <p>{{ __('Welcome to our application') }}</p>
        
        {{-- Nested translation --}}
        <p>{{ trans('pages.home.description') }}</p>
        
        {{-- Pluralization --}}
        <span>{{ trans_choice('messages.items', $count) }}</span>
        
        {{-- Lang directive --}}
        @lang('messages.footer.copyright')
    </div>
@endsection
```

## Common Patterns

### Validation Messages

```php
// app/Http/Requests/StoreUserRequest.php
public function messages()
{
    return [
        'email.required' => __('validation.required'),
        //                   ^^^^^^^^^^^^^^^^^^^^^
        //                   Cmd+Click → lang/en/validation.php
        
        'email.email' => trans('validation.email'),
        'password.min' => __('validation.min.string'),
    ];
}
```

### Flash Messages

```php
// In controller
return redirect()->route('dashboard')
    ->with('success', __('messages.profile_updated'));
    //                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    //                 Navigate to translation
```

### Email Subjects

```php
// In Mailable
public function build()
{
    return $this->subject(__('emails.welcome.subject'))
        //                  ^^^^^^^^^^^^^^^^^^^^^^^^^^
        //                  Jump to lang/en/emails.php
                    ->view('emails.welcome');
}
```

## Translation Key Types

### Dotted Keys (PHP Files)

```php
// Format: file.key
__('messages.welcome')          → lang/en/messages.php

// Format: file.nested.key
__('messages.auth.failed')      → lang/en/messages.php → ['auth']['failed']

// Format: file.deeply.nested.key
__('validation.custom.email.required') → lang/en/validation.php
```

### Plain Strings (JSON Files)

```php
// Single word (no dots)
__('Welcome')                   → lang/en.json

// Multiple words (contains spaces)
__('Welcome to our app')        → lang/en.json

// Short phrases
__('Login')                     → lang/en.json
__('Sign Up')                   → lang/en.json
```

## Tips & Tricks

1. **Quick Navigation** - Use Cmd+Click (Mac) or Ctrl+Click (Windows/Linux) for fastest navigation
2. **Go Back** - After jumping to a translation file, use Cmd+[ (Mac) or Alt+Left (Windows/Linux) to return
3. **Organize Translations** - Group related translations in the same file (e.g., all auth messages in `auth.php`)
4. **Use Nested Keys** - For complex features, use nested arrays: `feature.section.key`
5. **JSON for Simple Strings** - Use JSON files for single-word or simple phrase translations
6. **PHP for Structured Data** - Use PHP arrays for organized, multi-level translations

## Troubleshooting

### Translation Not Found

If goto-definition doesn't work:

1. **Check file exists** - Ensure `lang/en/{file}.php` or `lang/en.json` exists
2. **Verify key format** - Make sure the translation key matches the file structure
3. **Check Laravel version** - The extension checks both `lang/` and `resources/lang/`
4. **Restart LSP** - Reload Zed window to refresh file detection

### Wrong File Opened

If it jumps to the wrong location:

- **JSON vs PHP** - Keys without dots go to JSON, keys with dots go to PHP files
- **Locale** - Currently defaults to `en` locale (multi-locale support coming soon)

### Dynamic Keys Don't Work

This is intentional for safety:

```php
// ❌ Won't work (dynamic variable)
$key = 'messages.welcome';
__($key)

// ✅ Works (static string)
__('messages.welcome')
```

## File Organization Best Practices

### Group by Feature

```
lang/en/
├── auth.php          # Authentication messages
├── passwords.php     # Password reset messages
├── validation.php    # Validation error messages
├── messages.php      # General app messages
├── emails.php        # Email subjects and content
├── pages/
│   ├── home.php      # Home page translations
│   └── about.php     # About page translations
└── ...
```

### Use Consistent Naming

```php
// Good: Consistent, organized
__('pages.home.title')
__('pages.home.description')
__('pages.about.title')
__('pages.about.description')

// Avoid: Inconsistent, hard to find
__('home_title')
__('aboutPageDesc')
__('title_for_contact')
```

### Nested Structure Example

```php
// lang/en/pages.php
return [
    'home' => [
        'title' => 'Welcome Home',
        'description' => 'Your dashboard',
        'sections' => [
            'recent' => 'Recent Activity',
            'stats' => 'Your Statistics',
        ],
    ],
    'profile' => [
        'title' => 'Your Profile',
        'edit' => 'Edit Profile',
    ],
];

// Usage
__('pages.home.title')                  // "Welcome Home"
__('pages.home.sections.recent')        // "Recent Activity"
__('pages.profile.edit')                // "Edit Profile"
```

## Related Features

- [View Navigation](./VIEW_NAVIGATION.md) - Jump to Blade views
- [Config Navigation](./CONFIG_NAVIGATION.md) - Navigate to config files
- [Env Navigation](./ENV_NAVIGATION.md) - Jump to environment variables
- [Middleware Navigation](./MIDDLEWARE_NAVIGATION.md) - Navigate to middleware
- [Component Navigation](./COMPONENT_NAVIGATION.md) - Navigate to Blade components

## Examples by Use Case

### Multi-language Application

```php
// Same key, different files
__('messages.welcome')
// → lang/en/messages.php → "Welcome"
// → lang/es/messages.php → "Bienvenido"
// → lang/fr/messages.php → "Bienvenue"

// The extension uses 'en' by default
// (Multi-locale support coming soon)
```

### API Response Messages

```php
return response()->json([
    'message' => __('api.success.user_created'),
    //            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    //            Navigate to lang/en/api.php
    'data' => $user,
]);
```

### Form Labels and Placeholders

```blade
<label>{{ __('forms.labels.email') }}</label>
<input 
    type="email" 
    placeholder="{{ __('forms.placeholders.email') }}"
    {{--              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ --}}
    {{--              Jump to translation file --}}
>
```

## Summary

Translation navigation in Laravel LSP provides:

✅ Support for all Laravel translation helpers  
✅ Both PHP and JSON translation files  
✅ Laravel 9+ and legacy directory structures  
✅ Nested translation keys  
✅ Instant, cached navigation  
✅ Blade template support  

Click any translation key to explore your app's translations! 🚀