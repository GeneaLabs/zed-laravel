<?php

// Test file to verify Laravel extension is loaded
// Open this file in Zed and check the logs

// These are examples of what we'll support in future phases:

// Phase 2: View references
$view1 = view('welcome');
$view2 = view('users.profile');
$view3 = view('admin.dashboard.index');

// Phase 3: Route references  
$url1 = route('home');
$url2 = route('users.show', $user);

// Phase 4: Config references
$appName = config('app.name');
$timezone = config('app.timezone');

// Phase 5: Blade components (will be in .blade.php files)
// <x-button type="primary">Click me</x-button>
// <x-forms.input name="email" />

// Phase 6: Livewire components (will be in .blade.php files)
// <livewire:user-profile :user="$user" />
// <livewire:search-posts />

// Testing that the extension loads when opening PHP files
echo "Laravel Extension Test File";