# Translation Key Handling - Improved Behavior

## Overview

This document explains how the Laravel LSP handles different types of translation keys, including the improved fallback behavior for when translation files don't exist.

## Translation Key Types

### 1. Dotted Keys (PHP Files)

**Format:** `file.key` or `file.nested.key`

```php
__('messages.welcome')              // → lang/en/messages.php
__('messages.auth.failed')          // → lang/en/messages.php (nested)
__('validation.custom.email.required')  // → lang/en/validation.php (deeply nested)
```

**Behavior:**
- ✅ Navigates to the PHP file based on first segment
- ✅ Works with both `lang/` and `resources/lang/` directories
- ✅ File must exist for navigation to work

---

### 2. Single-Word Keys (JSON or PHP)

**Format:** Single word with no spaces or dots

```php
__('Confirm')
__('Cancel')
trans('Save')
Lang::get('Delete')
```

**Behavior with Improved Fallback:**

1. **First:** Tries to find `lang/en.json` or `resources/lang/en.json`
   - If found → Navigates to JSON file ✅

2. **Fallback:** If JSON not found, tries common PHP files:
   - `lang/en/messages.php`
   - `lang/en/common.php`
   - `lang/en/app.php`
   - If any found → Navigates to first matching PHP file ✅

3. **Result:** No navigation if neither JSON nor common PHP files exist

**Why this fallback?**

Single-word keys can exist in either location:

```json
// lang/en.json
{
    "Confirm": "Confirm",
    "Cancel": "Cancel"
}
```

OR

```php
// lang/en/messages.php (accessed via __('messages.Confirm'))
return [
    'Confirm' => 'Confirm',
    'Cancel' => 'Cancel',
];
```

The fallback ensures navigation works regardless of your project's organization style.

---

### 3. Multi-Word Keys (JSON Files)

**Format:** Phrases with spaces

```php
__('Welcome to our application')
__('Please login to continue')
trans('Your profile has been updated')
```

**Behavior with Improved Navigation:**

1. **First:** Tries to find `lang/en.json` or `resources/lang/en.json`
   - If found → Navigates to JSON file ✅

2. **Improved:** If JSON not found, still navigates to `lang/en.json` location
   - File doesn't exist → User can create it
   - Zed will show file-not-found, allowing quick file creation ✅

**Why always navigate?**

Multi-word keys MUST be in JSON files per Laravel convention. By always navigating to the expected location, users can:
- See where the file should be created
- Quickly create the missing file
- Add the translation key

---

## Examples by Scenario

### Scenario 1: Standard Laravel Project with JSON

```
your-project/
├── lang/
│   ├── en.json          ← Exists
│   └── en/
│       └── messages.php  ← Exists
```

```php
__('Welcome')           // → lang/en.json ✅
__('Confirm')          // → lang/en.json ✅
__('messages.greeting') // → lang/en/messages.php ✅
```

**Result:** All work perfectly ✅

---

### Scenario 2: Project Without JSON File

```
your-project/
└── lang/
    └── en/
        └── messages.php  ← Only PHP files exist
```

```php
__('Confirm')          // → lang/en/messages.php ✅ (fallback)
__('Save')             // → lang/en/messages.php ✅ (fallback)
__('messages.greeting') // → lang/en/messages.php ✅
```

**Single-word keys:** Navigate to `messages.php` via fallback ✅

```php
__('Welcome to our app')  // → lang/en.json (doesn't exist yet)
```

**Multi-word keys:** Navigate to where JSON file should be, user can create it ✅

---

### Scenario 3: Fresh Laravel Project (No Translations Yet)

```
your-project/
└── lang/
    └── .gitkeep
```

```php
__('messages.welcome')    // ❌ No navigation (messages.php doesn't exist)
__('Confirm')            // ❌ No navigation (no JSON or common PHP files)
__('Welcome to our app') // → lang/en.json ✅ (navigate to expected location)
```

**Multi-word advantage:** Even without files, you can navigate to create them!

---

## Decision Flow Chart

```
Translation Key: __('...')
         |
         ├─ Contains '.' (dot)?
         │  └─ YES → Try PHP file (file.key format)
         │      └─ lang/en/{file}.php
         │
         └─ NO → Check for spaces
             │
             ├─ Has spaces?
             │  └─ YES (Multi-word)
             │      ├─ Try lang/en.json
             │      └─ Not found? → Navigate to lang/en.json anyway ✅
             │
             └─ NO (Single word)
                 ├─ Try lang/en.json
                 ├─ Not found? → Try common PHP files
                 │   ├─ lang/en/messages.php
                 │   ├─ lang/en/common.php
                 │   └─ lang/en/app.php
                 └─ Nothing found? → No navigation
```

---

## Best Practices

### When to Use Each Type

**Use Multi-word JSON keys for:**
- UI labels: `"Save Changes"`, `"Cancel"`
- Messages: `"Welcome to our application"`
- Common phrases: `"Please wait..."`

```php
// Good: Clear, self-documenting
return view('dashboard', [
    'welcome' => __('Welcome to your dashboard'),
    'subtitle' => __('Here are your recent activities'),
]);
```

**Use Dotted PHP keys for:**
- Organized translations: `validation.required`, `auth.failed`
- Grouped messages: `emails.welcome.subject`
- Nested structures: `pages.home.sections.hero.title`

```php
// Good: Organized structure
return [
    'title' => __('pages.home.title'),
    'meta' => [
        'description' => __('pages.home.meta.description'),
        'keywords' => __('pages.home.meta.keywords'),
    ],
];
```

**Use Single-word keys for:**
- Simple actions: `"Save"`, `"Cancel"`, `"Delete"`
- Short labels: `"Name"`, `"Email"`, `"Password"`

```php
// Works either way:
__('Save')              // From JSON or messages.php
__('messages.Save')     // Explicit PHP file reference
```

---

## Troubleshooting

### "Navigation doesn't work for `__('Confirm')`"

**Check:**
1. Does `lang/en.json` exist?
2. Do any of these exist?
   - `lang/en/messages.php`
   - `lang/en/common.php`
   - `lang/en/app.php`

**Solution:** Create `lang/en.json` or add the key to an existing PHP file.

---

### "Multi-word translations navigate but file doesn't exist"

This is **expected behavior**! 

**What happens:**
1. Click `__('Welcome to our app')`
2. Zed tries to open `lang/en.json`
3. File doesn't exist → Zed shows error/offers to create

**Solution:** Create the file when prompted, or manually create:

```bash
mkdir -p lang
echo '{}' > lang/en.json
```

Then add your translations:
```json
{
    "Welcome to our app": "Welcome to our app",
    "Please login": "Please login"
}
```

---

### "Single-word key goes to wrong file"

If `__('Confirm')` navigates to `messages.php` but you want JSON:

**Solution:** Create `lang/en.json` - it takes priority over PHP files.

```json
{
    "Confirm": "Confirm",
    "Cancel": "Cancel"
}
```

---

## Migration Guide

### Moving from PHP to JSON

If you have single-word keys in PHP and want to use JSON:

**Before:** `lang/en/messages.php`
```php
return [
    'Save' => 'Save',
    'Cancel' => 'Cancel',
    'Delete' => 'Delete',
];
```

**After:** `lang/en.json`
```json
{
    "Save": "Save",
    "Cancel": "Cancel",
    "Delete": "Delete"
}
```

**Code change:** None! Both work with `__('Save')`

**LSP behavior:** Will now navigate to JSON (preferred) instead of PHP (fallback)

---

## Summary

### Single-Word Keys (`__('Confirm')`)
- ✅ JSON file if exists
- ✅ Falls back to common PHP files
- ❌ No navigation if neither exists

### Multi-Word Keys (`__('Welcome to our app')`)
- ✅ JSON file if exists
- ✅ Still navigates to JSON location even if file doesn't exist
- 🎯 User can create file at expected location

### Dotted Keys (`__('messages.welcome')`)
- ✅ Navigates to PHP file based on first segment
- ❌ No navigation if file doesn't exist

This improved behavior ensures you can always navigate to create missing translations! 🚀