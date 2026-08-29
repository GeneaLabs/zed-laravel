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

**IMPORTANT - Binary for Local Development:**
The `.zed/settings.json` configures Zed to use the local build directly:
```json
{
  "lsp": {
    "laravel-lsp": {
      "binary": {
        "path": "laravel-lsp/target/release/laravel-lsp"
      }
    }
  }
}
```

Development workflow:
```bash
cargo build --release
# Then in Zed: Cmd+Shift+P → "zed: reload extensions"
```

No copying or symlinks needed - Zed reads the binary path from settings.

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

## LSP Architecture (laravel-lsp/)

### Core Components

| File | Purpose |
|------|---------|
| `main.rs` | LSP server, request handlers, Backend trait impl |
| `salsa_impl.rs` | Salsa incremental computation actor |
| `queries.rs` | Tree-sitter queries for pattern extraction |
| `parser.rs` | PHP and Blade tree-sitter parsing |
| `config.rs` | Laravel project configuration discovery |
| `env_parser.rs` | .env file parsing |
| `service_provider_analyzer.rs` | Middleware/binding extraction |
| `middleware_parser.rs` | Kernel.php and bootstrap/app.php parsing |

### Salsa Actor Pattern

The LSP uses a dedicated thread for Salsa incremental computation to avoid lifetime issues with async code:

```
┌─────────────────┐     oneshot channel     ┌─────────────────┐
│  LSP Handlers   │ ──────────────────────► │   SalsaActor    │
│  (async/await)  │ ◄────────────────────── │ (dedicated      │
│                 │        response         │  thread)        │
└─────────────────┘                         └─────────────────┘
```

**Key pattern for adding new Salsa features:**
1. Add `#[salsa::input]` type in `salsa_impl.rs`
2. Add data transfer type (no lifetimes) for async boundary
3. Add `SalsaRequest` variant with oneshot sender
4. Add `SalsaHandle` method (async interface)
5. Add handler method in `SalsaActor`
6. Add helper method in `main.rs` to register data

### Salsa Components

| Component | Input Type | Data Transfer Type | Purpose |
|-----------|------------|-------------------|---------|
| File Patterns | `SourceFile` | `ParsedPatternsData` | Cached parsed patterns per file |
| Config | `ConfigFile` | `LaravelConfigData` | Project configuration |
| Project Files | `ProjectFiles` | `ViewReferenceLocationData` | Reference finding across project |
| Service Providers | `ServiceProviderFile` | `MiddlewareRegistrationData`, `BindingRegistrationData` | Middleware/binding lookups |
| Env Variables | `EnvFile` | `EnvVariableData` | Environment variable lookups |
| Translations | `LangFile`, `LangDir` | `ResolvedTranslationData` | Translation key resolution + locale discovery |

### Important Conventions

- **Data transfer types**: Use `*Data` suffix (e.g., `EnvVariableData`) for types crossing async boundaries
- **Salsa inputs**: Use `#[salsa::input]` for source data, store in `HashMap` for O(1) lookup
- **Registration pattern**: Call `register_*_with_salsa()` after successful parsing
- **Fallback pattern**: Use Salsa cache first, fall back to direct computation if unavailable
- **Priority merging** (service providers): Framework=0, Package=1, Module=2, App=3 (higher wins).
  On an equal-priority tie the module listed later in `modules.paths` wins, then the later
  provider in lexicographic path order

### Position Indexing Convention

All positions are **0-based** throughout the stack:
- Tree-sitter `Point`: row/column are 0-based
- LSP `Position`: line/character are 0-based
- All match structs: row/column/end_column are 0-based

**Key fields in match structs:**

| Field | Points to |
|-------|-----------|
| `column` | Start of entire pattern (e.g., `@` in `@include`) |
| `end_column` | End of entire pattern (e.g., after `)` in `@include('x')`) |
| `string_column` | Start of **content** inside quotes (first char after quote) |
| `string_end_column` | End of content (position one past last char, before closing quote) |

**Rule**: Never manually calculate string positions in `main.rs`. Use `string_column`/`string_end_column` from Salsa.

### Cache Invalidation Architecture (CRITICAL)

**All file-derived features MUST use Salsa incremental computation:**

```
did_change(file) → Debounce 250ms → Update Salsa input → Queries recompute → UI updates
```

**Rules:**
1. **Never bypass Salsa** - All file parsing goes through Salsa inputs
2. **Update on edit, not just save** - Wire `did_change` to Salsa (debounced)
3. **Salsa handles invalidation** - Don't manually track what needs recomputing
4. **Pure query functions** - Queries derive from inputs, no side effects

**Pattern Types (all extracted via Salsa queries):**

| Pattern | Example | Extracted From | Target |
|---------|---------|----------------|--------|
| Views | `view('welcome')` | SourceFile | `resources/views/*.blade.php` |
| Blade Components | `<x-button>` | SourceFile (Blade tags + PHP string literals) | `resources/views/components/*.blade.php` |
| Blade Directives | `@include('partial')` | SourceFile | `resources/views/*.blade.php` |
| Livewire | `<livewire:counter>` | SourceFile | `app/Livewire/*.php` |
| Translations | `__('messages.key')` | SourceFile | `lang/*/*.php` |
| Assets | `asset('css/app.css')` | SourceFile | `public/*` |
| Vite | `@vite('resources/js/app.js')` | SourceFile | `resources/*` |
| Routes | `route('home')` | SourceFile | Route name in `routes/*.php` |
| Config | `config('app.name')` | SourceFile | `config/*.php` |
| Env | `env('APP_NAME')` | SourceFile | `.env` |
| Translations | `__('validation.required')` | LangFile / LangDir | `lang/*/*.php`, `lang/*.json` |
| Classes | `use App\Support\Foo;`, `@use('App\Support\Foo')` | SourceFile (PHP `use` clauses, Blade `@use` directives, Volt front matter) | `app/**/Foo.php` |
| Middleware | `->middleware('auth')` | SourceFile | Alias in registry |
| Bindings | `app('cache')` | SourceFile | Binding in registry |

**File Type → Salsa Input Mapping:**

| File Pattern | Salsa Input | What It Provides |
|--------------|-------------|------------------|
| `*.php`, `*.blade.php` | `SourceFile` | Pattern extraction (views, components, etc.) |
| `bootstrap/app.php`, `Providers/*.php` | `ServiceProviderFile` | Middleware aliases, container bindings |
| `.env`, `.env.*` | `EnvFile` | Environment variable values |
| `config/*.php`, `composer.json` | `ConfigFile` | View paths, namespaces, PSR-4 mappings |
| `lang/**/*.php`, `lang/*.json`, `resources/lang/**` | `LangFile`, `LangDir` | Translation values, locale discovery |

**Target Files (existence only):**
- View files, component files, Livewire classes, translation files, assets
- Tracked via file existence cache with 5-minute TTL
- No Salsa input needed - just check if file exists

**Adding New Features:**
1. Define `#[salsa::input]` for source data
2. Define `#[salsa::tracked]` query function (pure, no side effects)
3. Ensure `did_change` updates the input (automatic via file type detection)
4. Query results are automatically cached and incrementally updated

### Request Flow Example

```
User hovers over view('users.index')
    │
    ▼
Backend::hover() in main.rs
    │
    ▼
salsa.get_parsed_patterns(file_path, content)
    │
    ▼
SalsaActor checks cache, returns ParsedPatternsData
    │
    ▼
Find matching pattern at cursor position
    │
    ▼
Resolve view name to file path using config
    │
    ▼
Return HoverContents with file location
```

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

---
## Session State (2026-08-26)

### `.env` handling — settled (updated 2026-08-28, #337)

Zed classifies **every** env file as Shell Script: bare `.env` via the bash
grammar's `path_suffixes`, and `.env.*` variants via `"Shell Script": [".env.*"]`
in Zed's own `assets/settings/default.json`. The extension therefore attaches to
`.env` through the single `"Shell Script"` entry in `extension.toml`, and the LSP
dispatches env features on **filename**, not on Zed's language classification:
every env feature routes through one shared gate,
`env_key_locator::is_env_file_name` (and its path form `path_is_env_file`),
which matches `.env` exactly or a `.env.` variant prefix. The Salsa ingestion
in `execute_salsa_update` joined that gate last — it read `starts_with(".env")`
until #337, and so admitted `.envrc`, `.environment`, and `.env-backup` as
Laravel env sources. Startup registration is the one env path that does not
consult the gate, because it names its three files outright and classifies
nothing.

Consequences, all verified against primary sources:

- **No language definition is shipped for `.env`, and none should be.** An
  extension language could tie-and-win bare `.env` (bash offers `".env"`), but
  the `.env.*` variants sit at the `UserConfigured` tier that only a user's own
  `file_types` can override. Shipping one would fix `.env` and leave
  `.env.local` shell-linted — a split worse than the uniform status quo.
- **`zarifpour/zed-env` is not recommended, and the `"env"` attach entry was
  dropped in v0.7.4 (#337).** Its `path_suffixes` are a bare `"env"` (loses the
  length tie to `".env"`), so it never claims the files it appears to; and its
  bare `"conf"` / `"example"` / `"local"` / `"test"` entries reclassify
  unrelated files project-wide (e.g. `laravel/sail`'s `supervisord.conf`).
  Because it never claims `.env`, the `"env"` attach entry never fired — it
  bought no capability, while reading to a registry reviewer as this extension
  claiming another extension's language. `extension.toml` now attaches to
  `"PHP"`, `"Blade"`, `"XML"` and `"Shell Script"` only, and its comment block
  carries the written justification for the `"Shell Script"` entry.
- **SC2034 noise is NOT silenced by this extension.** The
  `language_server_additional_workspace_configuration` hook that injected
  `--exclude=SC2034` into bash-language-server shipped in v0.7.2–v0.7.3 and was
  removed entirely in v0.7.4 (#337). It reconfigured a language server this
  extension neither provides nor owns, which Zed's publishing prerequisites
  forbid, and the registry rejected v0.7.3 over it
  (zed-industries/extensions#7370). `src/lib.rs` now writes workspace
  configuration for `laravel-lsp` and nothing else. The manual workarounds in
  `docs/environment.md` are the whole answer by design: document how, do not do
  it to the user's other servers.

### Blade language definition — still open

The `experiment/remove-blade-language` investigation (options A/B/C: contribute
upstream to `bajrangCoder/zed-laravel-blade`, own the language definition without
bundling grammars, or wait for semantic tokens) has **not** been decided. Note
that Zed has since shipped semantic highlighting, so the `semantic_tokens`
setting referenced by option C now exists and is documented in
`docs/configuration.md` for Blade.
