# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Zed editor extension that provides Laravel development support, similar to the Laravel VSCode extension. The extension is written in Rust and aims to provide features such as:

- Clickable "go-to-definition" for Blade templates
- Clickable "go-to-definition" for Livewire components
- Clickable "go-to-definition" for Flux components
- Other Laravel-specific IDE features

**Important**: This is a learning project. The developer is learning Rust while building this extension, so explanations of Rust concepts, providing options, and teaching best practices are essential.

## Development Commands

Zed extensions are typically developed using:

```bash
# Build the extension (assuming standard Rust project)
cargo build

# Run tests
cargo test

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy

# Build for release
cargo build --release
```

## Running Diagnostics (Important for Zed)

When using Claude Code in Zed, it doesn't have direct access to LSP diagnostics. Always run these commands to check for errors:

### Check for Compilation Errors
```bash
cargo check
```
This is the fastest way to check if your code compiles without actually building the binary. Run this frequently while developing.

### See Detailed Compiler Messages
```bash
cargo build
```
This compiles the project and shows all errors and warnings with detailed explanations. The Rust compiler gives very helpful error messages - always read them carefully!

### Run Clippy for Best Practice Lints
```bash
cargo clippy
```
Clippy is Rust's linter that catches common mistakes and suggests more idiomatic code. Very useful when learning Rust!

### Run Tests
```bash
cargo test
```
Runs all tests in the project. Add `-- --nocapture` to see println! output during tests.

### Format Code
```bash
cargo fmt
```
Automatically formats your code according to Rust style guidelines. Run this before committing.

### Install the Extension Locally in Zed
```bash
# Install for local development/testing
zed: install dev extension
```
Use this command within Zed to load your extension for testing.

**Important**: After making changes, always run `cargo check` or `cargo build` to see if your code compiles before proceeding with more changes.

## Zed Extension Architecture

Zed extensions follow the Extension API provided by Zed. Key concepts:

- Extensions are written in Rust (or can use WebAssembly)
- Extensions interact with the Zed editor through the Extension API
- Language features like "go-to-definition" are typically implemented using the Language Server Protocol (LSP)
- Extensions can provide custom language servers or enhance existing ones

## Laravel-Specific Features to Implement

### Go-to-Definition Targets

1. **Blade Components**: `<x-component-name>` → `resources/views/components/component-name.blade.php`
2. **Livewire Components**: `<livewire:component-name>` → `app/Livewire/ComponentName.php`
3. **Flux Components**: `<flux:component>` → Flux component definition
4. **View References**: `view('view.name')` → `resources/views/view/name.blade.php`
5. **Route Names**: `route('route.name')` → route definition in `routes/` files
6. **Config References**: `config('app.name')` → `config/app.php`

## Architecture Notes

- Zed extensions MUST be written in Rust (compiled to WebAssembly)
- JavaScript/TypeScript cannot be used - VSCode extensions cannot be wrapped or ported
- Zed uses tree-sitter for syntax parsing
- May need custom tree-sitter queries for Laravel-specific patterns
- Extensions use the `zed_extension_api` crate and implement the `Extension` trait
- Language features use LSP (Language Server Protocol) integration

## Implementation Plan

This project follows a phased approach designed for learning Rust while building:

### Phase 1: Rust & Zed Extension Basics
**Goal**: Create a minimal working Zed extension

**Learning Focus**:
- Rust project structure (`Cargo.toml`, `src/lib.rs`)
- Basic Rust syntax (structs, traits, macros)
- The `zed_extension_api` crate
- What `impl` means and how traits work
- The `register_extension!` macro
- Rust's ownership model basics

**Deliverable**: Extension that loads in Zed and prints "Hello from Laravel Extension"

### Phase 2: File System Navigation
**Goal**: Given a view name, find the corresponding `.blade.php` file

**Learning Focus**:
- Rust's `String` vs `&str` types
- Working with file paths (`std::path::Path`)
- Result and Option types (error handling)
- Basic pattern matching with `match`
- The `?` operator for error propagation
- Why Rust doesn't have `null`

**Deliverable**: Function that converts `view('users.profile')` → `resources/views/users/profile.blade.php`

### Phase 3: Pattern Matching
**Goal**: Detect Laravel patterns in code using regex

**Learning Focus**:
- Regular expressions in Rust (`regex` crate)
- Iterators and closures
- Borrowing and references (`&` and `&mut`)
- Collections (`Vec`, `HashMap`)
- Iterator methods (`.map()`, `.filter()`, `.collect()`)

**Deliverable**: Function that finds all `view('...')` calls in a file

### Phase 4: Tree-sitter Integration
**Goal**: Parse Blade and PHP files properly using tree-sitter

**Learning Focus**:
- Working with tree-sitter's Rust API
- Tree traversal algorithms
- Lifetimes (what they are and why they matter)
- Memory management and performance
- Rust's zero-cost abstractions

**Deliverable**: Parse `<x-button>` tags from Blade files

### Phase 5: Go-to-Definition
**Goal**: Implement clickable "go-to-definition" for Blade components

**Learning Focus**:
- Zed's LSP integration APIs
- Async Rust (`async`/`await`, `Future` trait)
- More advanced trait usage
- Position/range calculations
- How async works in Rust vs JavaScript

**Deliverable**: Click `<x-button>` and jump to `components/button.blade.php`

### Phase 6: Advanced Features
**Goal**: Extend to Livewire, Flux, routes, config

**Learning Focus**:
- Code organization (modules, workspace structure)
- Advanced error handling
- Testing in Rust (`#[cfg(test)]`)
- Documentation (`///` comments)
- Publishing extensions

**Deliverable**: Full-featured Laravel extension with multiple go-to features

## Teaching Approach

When working on this project:
1. **Explain concepts first** - Explain Rust concepts before implementing them
2. **Provide options** - Present multiple implementation approaches with trade-offs
3. **Write code together** - Explain each line as it's written
4. **Encourage questions** - Answer "why" questions about design decisions
5. **Iterative development** - Build working code first, then refactor to be "more Rusty"
6. **Help with compiler errors** - Rust's compiler is helpful; explain what errors mean

## Resources

- Zed Extension API documentation: https://zed.dev/docs/extensions
- Existing Zed extensions for reference: https://github.com/zed-industries/extensions
- Laravel VSCode extension (for feature reference): https://github.com/amiralizadeh9480/laravel-extra-intellisense
