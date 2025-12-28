<?php

use Illuminate\Support\Facades\Route;

/*
|--------------------------------------------------------------------------
| Asset Navigation Test Routes
|--------------------------------------------------------------------------
|
| This file tests Laravel LSP's asset and path helper navigation feature.
| You should be able to Cmd+Click on any path string to navigate to the file.
|
*/

// ============================================================================
// asset() - Navigate to public/ directory
// ============================================================================

Route::get('/test-assets', function () {
    // These should navigate to public/images/logo.png
    $logo = asset('images/logo.png');
    $icon = asset("images/favicon.ico");

    // CSS and JS assets
    $css = asset('css/app.css');
    $js = asset('js/app.js');

    return view('welcome', compact('logo', 'icon', 'css', 'js'));
});

// ============================================================================
// Vite::asset() - Navigate to resources/ directory
// ============================================================================

Route::get('/test-vite', function () {
    // These should navigate to resources/images/
    $logo = \Illuminate\Support\Facades\Vite::asset('resources/images/logo.svg');
    $icon = \Illuminate\Support\Facades\Vite::asset('resources/images/favicon.ico');

    return view('welcome', compact('logo', 'icon'));
});

// ============================================================================
// mix() - Legacy Laravel Mix (navigate to public/)
// ============================================================================

Route::get('/test-mix', function () {
    $css = mix('css/app.css');
    $js = mix('js/app.js');

    return view('welcome', compact('css', 'js'));
});

// ============================================================================
// Path Helpers - Navigate to various Laravel directories
// ============================================================================

Route::get('/test-paths', function () {
    // base_path() - Navigate to project root
    $composer = base_path('composer.json');
    $readme = base_path('README.md');

    // app_path() - Navigate to app/ directory
    $userModel = app_path('Models/User.php');
    $controller = app_path('Http/Controllers/Controller.php');

    // config_path() - Navigate to config/ directory
    $appConfig = config_path('app.php');
    $dbConfig = config_path('database.php');

    // storage_path() - Navigate to storage/ directory
    $logs = storage_path('logs/laravel.log');
    $framework = storage_path('framework/cache');

    // database_path() - Navigate to database/ directory
    $migrations = database_path('migrations');
    $seeders = database_path('seeders/DatabaseSeeder.php');

    // lang_path() - Navigate to lang/ directory (or resources/lang/)
    $messages = lang_path('en/messages.php');
    $validation = lang_path('en/validation.php');

    // resource_path() - Navigate to resources/ directory
    $views = resource_path('views/welcome.blade.php');
    $css = resource_path('css/app.css');
    $js = resource_path('js/app.js');

    // public_path() - Navigate to public/ directory
    $index = public_path('index.php');
    $htaccess = public_path('.htaccess');

    return response()->json([
        'base_path' => $composer,
        'app_path' => $userModel,
        'config_path' => $appConfig,
        'storage_path' => $logs,
        'database_path' => $migrations,
        'lang_path' => $messages,
        'resource_path' => $views,
        'public_path' => $index,
    ]);
});

// ============================================================================
// Complex Examples - Multiple asset types in one route
// ============================================================================

Route::get('/test-complex', function () {
    // Mix of different asset helpers
    $publicLogo = asset('images/logo.png');
    $viteLogo = \Illuminate\Support\Facades\Vite::asset('resources/images/logo.svg');
    $config = config_path('app.php');
    $view = resource_path('views/welcome.blade.php');
    $storage = storage_path('app/public/uploads');

    // Using in return statement
    return response()->file(public_path('index.php'));
});

// ============================================================================
// File Existence Checks
// ============================================================================

Route::get('/test-file-checks', function () {
    // These should all support navigation
    if (file_exists(public_path('index.php'))) {
        $content = file_get_contents(base_path('composer.json'));
    }

    if (is_file(app_path('Models/User.php'))) {
        require_once app_path('Providers/AppServiceProvider.php');
    }

    return response()->json(['status' => 'ok']);
});
