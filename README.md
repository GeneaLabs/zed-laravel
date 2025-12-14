# Zed Laravel Extension

A Zed editor extension that provides Laravel development support, similar to the Laravel VSCode extension. This extension is written in Rust and provides features like clickable "go-to-definition" for Blade templates, Livewire components, and Flux components.

> **Learning Project**: This is a learning project where we're building a real extension while learning Rust. The code includes extensive comments explaining Rust concepts!

## Roadmap
### Goto Linking
This will allow you to navigate to the referenced resource, but will also show a warning if the resource cannot be found.
- [x] Blade Directives
- [x] Blade Components
- [x] Livewire Components
- [x] Views
- [ ] Routes
- [ ] Configs
- [ ] Middleware
- [ ] Translations
- [ ] App Bindings
- [ ] Assets
- [ ] Env Variables
- [ ] Inertia Pages

### Auto Completion
Provides autocompletion for those not provided by Intelephense and will warn when using values that cannot be found.
- [ ] Blade Directives
- [ ] Blade Components
- [ ] Livewire Components
- [ ] Flux Components
- [ ] Views
- [ ] Routes
- [ ] Configs
- [ ] Middleware
- [ ] Translations
- [ ] App Bindings
- [ ] Assets
- [ ] Env Variables
- [ ] Inertia Pages
- [ ] Validation Rules
- [ ] Eloquent (database fields, relationships, sub-queries)

### Tooltips
Provides basic documentation references for default elements.
- [ ] Stock Blade Directives
- [ ] Stock Blade Components
- [ ] Stock Livewire Components
- [ ] Stock Flux Components
- [ ] Stock Configs
- [ ] Stock Middleware
- [ ] Stock Env Variables
- [ ] Validation
- [ ] Eloquent

### Syntax Highlighting
Provide comprehensive syntax highlighting for Laravel-related workflows.
- Blade
- Livewire
- AlpineJS
