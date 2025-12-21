; Tree-sitter query for detecting Laravel Blade patterns
;
; This matches Blade-specific syntax like:
;   - <x-component> tags (Blade components)
;   - <livewire:component> tags
;   - @livewire('component') directives
;
; Note: The exact node names depend on the tree-sitter-blade grammar
; We may need to adjust these based on actual grammar structure

; ============================================================================
; Pattern 1: <x-component-name> Blade component tags
; ============================================================================
; Matches: <x-button>
;          <x-forms.input>
;          <x-layouts.app>
;
; Blade components use <x-*> syntax
; We need to match the opening tag and extract the component name

; Match opening tags like <x-button>
(element
  (start_tag
    (tag_name) @tag_name)
  (#match? @tag_name "^x-"))

; Also match self-closing tags like <x-button />
(self_closing_tag
  (tag_name) @tag_name
  (#match? @tag_name "^x-"))

; ============================================================================
; Pattern 2: <livewire:component-name> Livewire component tags
; ============================================================================
; Matches: <livewire:user-profile>
;          <livewire:admin.dashboard>
;
; Livewire components can be used with tag syntax

(element
  (start_tag
    (tag_name) @tag_name)
  (#match? @tag_name "^livewire:"))

(self_closing_tag
  (tag_name) @tag_name
  (#match? @tag_name "^livewire:"))

; ============================================================================
; Pattern 3: Blade Directives (all types)
; ============================================================================
; Matches: @extends('layout')
;          @section('content')
;          @foreach($items as $item)
;          @customDirective('args')
;
; The Blade grammar has three directive node types:
; - directive: Single directives like @extends, @include
; - directive_start: Block-starting directives like @section, @foreach, @if
; - directive_end: Block-ending directives like @endsection, @endforeach
;
; IMPORTANT: The directive nodes include the @ symbol as part of the node text.
; The capture should include the entire directive from @ through the directive name.
; For example: "@extends" is captured as a single node from byte 0 (the @) to byte 8 (after 's')

; Capture all single directives (includes @ symbol in the node)
(directive) @directive

; Capture block-starting directives (includes @ symbol in the node)
(directive_start) @directive

; Note: We don't capture directive_end (like @endif, @endsection)
; because they're closers, not definitions to navigate to

; ============================================================================
; Pattern 4: @include('view.name') directive (for future)
; ============================================================================
; Matches: @include('partials.header')
;
; (directive
;   (directive_name) @directive_name
;   (directive_argument
;     (string) @view_name)
;   (#eq? @directive_name "@include"))
