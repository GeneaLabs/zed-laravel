<?php

use Illuminate\Support\Facades\Route;
use Laravel\Fortify\Features;
use Livewire\Volt\Volt;

Route::get('/', function () {
    return view('welcome');
})->name('home');

Route::view('dashboard', 'dashboard')
    ->middleware(['auth', 'verified'])
    ->name('dashboard');

Route::middleware(['auth'])->group(function () {
    Route::redirect('settings', 'settings/profile');

    Volt::route('settings/profile', 'settings.profile')->name('profile.edit');
    Volt::route('settings/password', 'settings.password')->name('user-password.edit');
    Volt::route('settings/appearance', 'settings.appearance')->name('appearance.edit');

    Volt::route('settings/two-factor', 'settings.two-factor')
        ->middleware(
            when(
                Features::canManageTwoFactorAuthentication()
                    && Features::optionEnabled(Features::twoFactorAuthentication(), 'confirmPassword'),
                ['password.confirm'],
                [],
            ),
        )
        ->name('two-factor.show');
});

// Test routes for middleware and translation hover/diagnostics

// INFO diagnostic: middleware not in config
Route::middleware('undefined_middleware')->get('/test-middleware', function () {
    return __('messages.welcome');
});

// ERROR diagnostic: middleware in config but class file missing
Route::middleware('test-missing')->get('/test-missing-class', function () {
    return __('messages.greeting');
});

Route::get('/test-translations', function () {
    return view('welcome', [
        'dotted_key' => __('messages.greeting'),
        'text_key' => __('Welcome to our app'),
        'single_word' => __('Confirm'),
    ]);
});

// Test container binding navigation and diagnostics

// ✅ Valid bindings - no diagnostic, navigate to bound class or registration
app('test'); // Navigate to AppServiceProvider or User class
app('cache'); // Navigate to CacheManager (framework binding)
app('user.service'); // Navigate to AppServiceProvider or UserService class

// ✅ Class references - always valid, navigate to class file
app(\App\Models\User::class); // Navigate to User.php
app(\App\Services\UserService::class); // Navigate to UserService.php (if exists)

// ❌ ERROR diagnostic: binding not found
app('nonexistent'); // Red squiggle - binding not defined
app('my.custom.service'); // Red squiggle - binding not defined
