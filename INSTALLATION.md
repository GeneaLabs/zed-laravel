# Installation and Testing Guide - Zed Laravel Extension with Code Lens

This guide will help you install and test the Laravel extension with the new code lens functionality in Zed editor.

## Prerequisites

- Zed editor installed
- Rust toolchain (for building from source)
- A Laravel project for testing

## Installation

### Option 1: Install Development Extension in Zed

1. **Open Zed editor**

2. **Open the command palette** (`Cmd+Shift+P` on macOS, `Ctrl+Shift+P` on Linux/Windows)

3. **Type "install dev extension"** and select it

4. **Navigate to this project directory** (`zed-laravel`)

5. **Select the directory** - Zed will install the extension from source

### Option 2: Manual Installation

1. **Clone and build the extension:**
   ```bash
   git clone <repository-url> zed-laravel
   cd zed-laravel
   cargo build --release
   cd laravel-lsp && cargo build --release && cd ..
   cp laravel-lsp/target/release/laravel-lsp ./laravel-lsp-binary
   cp target/wasm32-wasip2/release/zed_laravel.wasm ./extension.wasm
   ```

2. **Install in Zed:**
   - Open Zed
   - Use "install dev extension" command
   - Select the `zed-laravel` directory

## Verification

### Check Extension is Loaded

1. **Open a Laravel project** in Zed
2. **Open any `.blade.php` file**
3. **Look for "Laravel LSP" messages** in the output panel
4. **You should see messages like:**
   ```
   Laravel LSP: Initializing
   Laravel LSP: Server initialized
   Laravel LSP: Document opened: file:///.../view.blade.php
   ```

### Check LSP Binary

The extension should automatically use the built LSP server. You can verify it's working by:

```bash
# Test the LSP binary directly
./laravel-lsp-binary
# Should start and wait for LSP input (Ctrl+C to exit)
```

## Testing Code Lens Feature

### Quick Test with Test Files

1. **Open the test project:**
   ```bash
   cd test-project
   ```

2. **Open `resources/views/test-view.blade.php` in Zed**

3. **Press `Cmd+.` (macOS) or `Ctrl+.` (Linux/Windows)** anywhere in the file

4. **You should see "X references" in the Quick Actions menu**

5. **Click on the references entry** to see all files that reference this view:
   - `TestController.php` methods
   - `web.php` route closures
   - `test-layout.blade.php` include directive
   - `TestComponent.php` Livewire render method

### Test with Your Own Laravel Project

#### Create Test Files

1. **Create a simple Blade view:**
   ```bash
   # In your Laravel project
   echo '<h1>Test View for Code Lens</h1>' > resources/views/test-code-lens.blade.php
   ```

2. **Add controller reference:**
   ```php
   // In any controller
   public function testCodeLens()
   {
       return view('test-code-lens');
   }
   ```

3. **Add route reference:**
   ```php
   // In routes/web.php
   Route::get('/test-code-lens', function () {
       return view('test-code-lens');
   });
   ```

4. **Add Blade include:**
   ```blade
   {{-- In any other Blade file --}}
   @include('test-code-lens')
   ```

#### Test the Feature

1. **Open `resources/views/test-code-lens.blade.php` in Zed**
2. **Press `Cmd+.` anywhere in the file**
3. **Look for "3 references" (or however many you added)**
4. **Click to navigate to each reference**

## Expected Behavior

### ✅ What Should Work

- **Code lens appears** in Quick Actions menu (`Cmd+.`)
- **Reference count** shows correctly (e.g., "3 references")
- **Reference types** are detected:
  - Controllers: `view('name')` and `View::make('name')`
  - Routes: `return view('name')` in closures
  - Blade templates: `@include('name')` and `@extends('name')`
  - Livewire: `return view('name')` in render methods
- **Navigation works** - clicking references jumps to the correct location
- **Cache updates** - adding/removing references updates the count
- **Performance** - responses are fast after first load

### ✅ Integration with Existing Features

- **Go-to-definition still works**: Click on `view('users.profile')` → navigate to view
- **Component navigation**: Click on `<x-button>` → navigate to component  
- **Livewire navigation**: Click on `<livewire:profile />` → navigate to class
- **Hover information**: Hover over Laravel constructs for details
- **Diagnostics**: Missing files show yellow squiggles

## Troubleshooting

### No Code Lens Appearing

1. **Check file extension**: Code lens only works on `.blade.php` files
2. **Verify Laravel project**: LSP needs to detect Laravel structure
3. **Check LSP connection**: Look for "Laravel LSP" messages in Zed output
4. **Try restarting Zed**: Sometimes helps with LSP connection issues

### No References Found

1. **Check view name**: View name is extracted from file path
2. **Verify references exist**: Make sure you have actual `view()` calls
3. **Check search patterns**: References must match exact patterns
4. **Try different quote styles**: Test both single and double quotes

### Performance Issues

1. **Large project**: First search may take time, subsequent should be fast
2. **Many files**: Consider if project has unusual structure
3. **Debug output**: Enable LSP debug logging in Zed settings

### LSP Not Starting

1. **Check binary permissions**: `chmod +x laravel-lsp-binary`
2. **Verify binary path**: LSP should be in extension root directory
3. **Check Rust installation**: Binary may need system dependencies
4. **Try rebuilding**: `cargo build --release` in both directories

## Debug Information

### Enable LSP Logging

In Zed settings, enable LSP debug logging to see detailed information:

```json
{
  "lsp": {
    "laravel": {
      "initialization_options": {
        "debug": true
      }
    }
  }
}
```

### Useful Log Messages

Look for these messages in Zed's output panel:

```
Laravel LSP: Providing code lenses for: file:///path/to/view.blade.php
Laravel LSP: Found 3 cached references for view: test-view
Laravel LSP: Searching for references to view: users.profile
Invalidated cache for file: /path/to/controller.php (had 2 view references)
```

## Performance Expectations

### First Load
- **Small project** (< 100 files): ~100-300ms
- **Medium project** (100-500 files): ~300-800ms  
- **Large project** (500+ files): ~800ms-2s

### Cached Results
- **Subsequent requests**: ~1-10ms
- **File changes**: Only affected files re-parsed
- **Memory usage**: ~1-5MB for reference cache

## Supported Patterns

### Controller Patterns
```php
view('view-name')
view("view-name")
View::make('view-name')  
View::make("view-name")
return view('view-name')
```

### Route Patterns
```php
Route::get('/', function () {
    return view('view-name');
});
```

### Blade Patterns
```blade
@extends('view-name')
@include('view-name')
@extends("view-name")  
@include("view-name")
```

### Livewire Patterns
```php
public function render()
{
    return view('view-name');
}
```

## Future Enhancements

The following features are planned for future releases:

- **Command execution**: Implement reference navigation commands
- **Package views**: Support `package::view` syntax
- **Config references**: Track `config('key')` usage
- **Route references**: Track `route('name')` calls
- **Background indexing**: Pre-index files for faster startup
- **File watchers**: Real-time file system change detection

## Support

If you encounter issues:

1. **Check this guide** for common solutions
2. **Enable debug logging** to see detailed information
3. **Test with simple cases** before complex scenarios
4. **Verify LSP is working** with basic Laravel features first
5. **Create minimal reproduction** if reporting issues

## Contributing

To contribute to the code lens feature:

1. **Fork the repository**
2. **Make changes** to LSP code in `laravel-lsp/src/`
3. **Test thoroughly** with various Laravel projects
4. **Update documentation** as needed
5. **Submit pull request** with clear description

The code lens implementation is in:
- `laravel-lsp/src/main.rs` - Main LSP implementation
- Focus on `code_lens()` method and cache system
- Reference finding functions for different file types