# Laravel Extension Test Project

This directory contains a comprehensive set of test files for manually testing the Laravel Extension's go-to-definition and navigation features in the Zed editor.

## 📁 Project Structure

```
test-project/
├── app/
│   ├── Http/
│   │   └── Controllers/
│   │       └── TestController.php     # Various view() patterns
│   └── Livewire/
│       └── UserProfile.php            # Livewire component
├── resources/
│   └── views/
│       ├── welcome.blade.php          # Simple view
│       ├── users/
│       │   └── profile.blade.php      # Nested view
│       ├── admin/
│       │   └── dashboard/
│       │       └── index.blade.php    # Deeply nested view
│       ├── components/
│       │   ├── button.blade.php       # Blade component
│       │   └── forms/
│       │       └── input.blade.php    # Nested component
│       └── livewire/
│           └── user-profile.blade.php  # Livewire view
├── routes/
│   └── web.php                         # Route definitions
└── config/
    └── app.php                         # Config file
```

## 🧪 Test Cases

### Phase 2: View Navigation
Test clicking on these patterns in `TestController.php`:

| Pattern | Example | Should Navigate To |
|---------|---------|-------------------|
| Simple view | `view('welcome')` | `resources/views/welcome.blade.php` |
| Nested view | `view('users.profile')` | `resources/views/users/profile.blade.php` |
| Deep nesting | `view('admin.dashboard.index')` | `resources/views/admin/dashboard/index.blade.php` |
| With hyphen | `view('user-settings')` | `resources/views/user-settings.blade.php` |

### Phase 3: Pattern Matching
Test these view patterns:
- Views with single quotes: `view('users.index')`
- Views with double quotes: `view("users.index")`
- Views with data: `view('users.show', compact('user'))`
- Chained methods: `view('dashboard')->with('data', $data)`
- Multi-line calls: Complex view calls spanning multiple lines
- View facade: `View::make('users.profile')`

### Phase 4: Blade Components (Future)
Test clicking on these in `.blade.php` files:

| Pattern | Should Navigate To |
|---------|-------------------|
| `<x-button>` | `resources/views/components/button.blade.php` |
| `<x-forms.input>` | `resources/views/components/forms/input.blade.php` |
| `@include('partials.header')` | `resources/views/partials/header.blade.php` |
| `@extends('layouts.app')` | `resources/views/layouts/app.blade.php` |

### Phase 5: Livewire Components (Future)
Test these patterns:

| Pattern | Should Navigate To |
|---------|-------------------|
| `<livewire:user-profile />` | `app/Livewire/UserProfile.php` |
| `@livewire('user-profile')` | `app/Livewire/UserProfile.php` |
| `<livewire:admin.dashboard />` | `app/Livewire/Admin/Dashboard.php` |

### Phase 6: Routes & Config (Future)
Test these patterns:

| Pattern | Should Navigate To |
|---------|-------------------|
| `route('home')` | Route definition in `routes/web.php` |
| `route('users.profile')` | Route definition in `routes/web.php` |
| `config('app.name')` | `config/app.php` → `'name'` key |
| `config('app.timezone')` | `config/app.php` → `'timezone'` key |

## 🎯 How to Test

1. **Open the test project in Zed:**
   ```bash
   cd zed-laravel/test-project
   zed .
   ```

2. **Open `TestController.php`:**
   - Navigate to `app/Http/Controllers/TestController.php`
   - Try clicking on various view names (once the feature is implemented)

3. **Check Navigation:**
   - When you Cmd+Click (Mac) or Ctrl+Click (Linux/Windows) on a view name
   - It should open the corresponding Blade file
   - The cursor should be positioned at the beginning of the file

4. **Test Edge Cases:**
   - Try views that don't exist (should show error or do nothing)
   - Try dynamic view names (variables, concatenations)
   - Try package views with namespaces

## 📊 Feature Completion Checklist

### Phase 2 (Current)
- [x] Parse simple view names
- [x] Parse nested view names
- [x] Convert dots to directory separators
- [x] Add `.blade.php` extension
- [ ] Hook into Zed's go-to-definition API

### Phase 3
- [ ] Regex pattern matching
- [ ] Handle View facade
- [ ] Handle multi-line view calls
- [ ] Parse Blade directives

### Phase 4
- [ ] Tree-sitter Blade parsing
- [ ] Component tag detection
- [ ] Directive parsing

### Phase 5
- [ ] LSP integration
- [ ] Go-to-definition handler
- [ ] Position calculation

### Phase 6
- [ ] Route navigation
- [ ] Config navigation
- [ ] Livewire components
- [ ] Flux components

## 🔍 Verification Methods

### Manual Testing
1. Open files in Zed
2. Attempt to navigate using keyboard shortcuts
3. Verify correct file opens
4. Check cursor position

### Automated Testing
Run the extension tests:
```bash
cd ../  # Back to zed-laravel directory
cargo test
```

Run the demo:
```bash
cargo run --example demo
```

## 🐛 Known Limitations

1. **Dynamic Views**: Views constructed at runtime cannot be resolved
   ```php
   $viewName = 'users.' . $type;
   return view($viewName);  // Cannot resolve
   ```

2. **Package Views**: Namespaced views need special handling
   ```php
   return view('package::view');  // Needs Phase 6
   ```

3. **Conditional Views**: Complex logic makes resolution difficult
   ```php
   return view($condition ? 'view1' : 'view2');  // Hard to resolve
   ```

## 📚 Resources

- [Laravel View Documentation](https://laravel.com/docs/views)
- [Blade Templates Documentation](https://laravel.com/docs/blade)
- [Livewire Documentation](https://livewire.laravel.com)
- [Zed Extension API](https://zed.dev/docs/extensions)

## 💡 Tips for Development

1. **Start Simple**: Test basic view navigation first
2. **Add Complexity Gradually**: Move from simple to nested to dynamic
3. **Log Everything**: Use Zed's logging to debug navigation attempts
4. **Test Edge Cases**: Empty strings, malformed paths, missing files
5. **Consider Performance**: Large projects may have thousands of views

## 🎉 Success Criteria

The extension is working correctly when:
1. ✅ Clicking on `view('welcome')` opens `welcome.blade.php`
2. ✅ Clicking on `view('users.profile')` opens `users/profile.blade.php`
3. ✅ Navigation works with both single and double quotes
4. ✅ Error handling for non-existent views
5. ✅ Performance is acceptable (<100ms response time)

---

Happy testing! 🚀