<?php

use Illuminate\Support\Facades\Route;
use Laravel\Folio\Folio;

// ============================================================================
// Examples of Route::view() and Volt::route()
// ============================================================================
// Both methods are shortcuts for returning a view without a controller
// The second argument should have go-to-definition support
// Missing views show ERROR severity (red squiggle) because routes will 404

// Basic route with view
Route::view('/home', 'welcome');

// Nested view path
Route::view('/about', 'pages.about');

// Route with parameters (view is still second argument)
Route::view('/contact', 'contact.form', ['company' => 'Acme Corp']);

// Route with name
Route::view('/privacy', 'legal.privacy')->name('privacy');

// Mixed with other route types
Route::get('/users', [UserController::class, 'index']);
Route::view('/terms', 'legal.terms');
Route::post('/contact', [ContactController::class, 'store']);

// Deeply nested view
Route::view('/documentation', 'docs.getting-started.installation');

// Using double quotes
Route::view("/help", "support.help-center");

// ============================================================================
// Volt::route() examples (Laravel Volt - Single File Components)
// ============================================================================
// Volt allows you to define routes directly to Volt components
// Missing views show ERROR severity (same as Route::view)

Volt::route('/dashboard', 'volt.dashboard');
Volt::route('/profile', 'volt.user.profile');
Volt::route('/settings', "volt.settings");

// ============================================================================
// ERROR vs WARNING severity demonstration
// ============================================================================

// ❌ ERROR: Missing Route::view() - will cause 404 at runtime
Route::view('/missing-route', 'this.does.not.exist');

// ❌ ERROR: Missing Volt::route() - will cause 404 at runtime
Volt::route('/missing-volt', 'volt.missing.component');

// ⚠️ WARNING: Missing regular view() - might not break immediately
function showMissing() {
    return view('might.be.conditional');  // WARNING severity (yellow)
}

// ⚠️ WARNING: Missing View::make() - might not break immediately
function displayMissing() {
    return View::make('could.be.dynamic');  // WARNING severity (yellow)
}

// ============================================================================
// TESTS:
// ============================================================================
// 1. Cmd+Click on 'welcome' -> should jump to resources/views/welcome.blade.php
// 2. Cmd+Click on 'pages.about' -> should jump to resources/views/pages/about.blade.php
// 3. Cmd+Click on 'volt.dashboard' -> should jump to resources/views/volt/dashboard.blade.php
// 4. Save file -> should show RED squiggles on:
//    - 'this.does.not.exist' (ERROR)
//    - 'volt.missing.component' (ERROR)
// 5. Save file -> should show YELLOW squiggles on:
//    - 'might.be.conditional' (WARNING)
//    - 'could.be.dynamic' (WARNING)

// These should NOT be detected (first argument is the route path, not view):
// - '/home'
// - '/about'
// - '/contact'
// - '/dashboard'

// ============================================================================
// Regular view() calls should still work (WARNING if missing)
// ============================================================================
function show() {
    return view('users.profile');
}

// View::make() should still work (WARNING if missing)
function display() {
    return View::make('admin.dashboard');
}