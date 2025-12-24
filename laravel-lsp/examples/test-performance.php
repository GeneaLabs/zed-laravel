<?php

declare(strict_types=1);

namespace App\Http\Controllers;

use Illuminate\Http\Request;
use Illuminate\Support\Facades\View;
use Illuminate\Support\Facades\Config;

/**
 * Test file to demonstrate Laravel LSP performance optimizations
 * 
 * This file contains various Laravel patterns that will be cached and optimized:
 * - Environment variables
 * - Configuration calls
 * - View calls
 * - Middleware references
 * - Translation calls
 * - Container bindings
 * - Asset calls
 */
class TestPerformanceController extends Controller
{
    public function index(Request $request)
    {
        // Environment variables - will be cached with values
        $appName = env('APP_NAME', 'Laravel');
        $appDebug = env('APP_DEBUG', false);
        $dbHost = env('DB_HOST', 'localhost');
        $cacheDriver = env('CACHE_DRIVER', 'file');
        
        // Configuration calls - will show file existence
        $timezone = config('app.timezone');
        $locale = config('app.locale', 'en');
        $dbConnection = config('database.default');
        $mailDriver = config('mail.default');
        $sessionLifetime = config('session.lifetime');
        
        // View calls - will show Blade file locations
        $welcomeView = view('welcome');
        $dashboardView = View::make('dashboard.index');
        $profileView = view('profile.show', compact('request'));
        $adminView = view('admin.users.index');
        
        // Translation calls - will show language file locations
        $welcomeMessage = __('Welcome to our application');
        $loginText = trans('auth.login');
        $validationErrors = __('validation.required');
        $customMessage = trans('messages.user.created');
        
        // Container bindings - will show service locations
        $userService = app('UserService');
        $paymentGateway = app(\App\Services\PaymentGateway::class);
        $cache = app('cache');
        $logger = resolve('log');
        
        // Asset calls - will show asset file locations
        $cssAsset = asset('css/app.css');
        $jsAsset = asset('js/app.js');
        $imageAsset = asset('images/logo.png');
        $publicFile = public_path('uploads/avatar.jpg');
        
        return response()->json([
            'app' => [
                'name' => $appName,
                'debug' => $appDebug,
                'timezone' => $timezone,
                'locale' => $locale,
            ],
            'database' => [
                'host' => $dbHost,
                'connection' => $dbConnection,
            ],
            'cache' => [
                'driver' => $cacheDriver,
            ],
            'mail' => [
                'driver' => $mailDriver,
            ],
            'session' => [
                'lifetime' => $sessionLifetime,
            ],
            'messages' => [
                'welcome' => $welcomeMessage,
                'login' => $loginText,
                'validation' => $validationErrors,
                'custom' => $customMessage,
            ],
            'assets' => [
                'css' => $cssAsset,
                'js' => $jsAsset,
                'image' => $imageAsset,
                'upload' => $publicFile,
            ],
        ]);
    }
    
    public function middleware()
    {
        // Middleware patterns - will show middleware class locations
        return $this->middleware('auth')
                    ->middleware('verified')
                    ->middleware('throttle:api')
                    ->middleware('role:admin')
                    ->middleware('permission:manage-users');
    }
    
    public function morePatterns()
    {
        // More complex patterns for testing
        $nestedConfig = config('services.stripe.key', config('app.fallback_key'));
        $conditionalEnv = env('FEATURE_FLAG_' . strtoupper('new_ui'), false);
        $dynamicView = view("emails.{$this->getTemplateName()}", [
            'user' => auth()->user(),
            'data' => request()->all(),
        ]);
        
        // Array of translations
        $messages = [
            'success' => __('messages.success'),
            'error' => trans('messages.error'),
            'warning' => __('messages.warning'),
        ];
        
        // Multiple asset types
        $assets = [
            'styles' => [
                asset('css/bootstrap.css'),
                asset('css/custom.css'),
            ],
            'scripts' => [
                asset('js/jquery.js'),
                asset('js/bootstrap.js'),
                asset('js/custom.js'),
            ],
            'images' => [
                asset('images/hero-bg.jpg'),
                asset('images/testimonial-1.jpg'),
                asset('images/testimonial-2.jpg'),
            ],
        ];
        
        return compact('nestedConfig', 'conditionalEnv', 'dynamicView', 'messages', 'assets');
    }
    
    private function getTemplateName(): string
    {
        return config('mail.template', 'default');
    }
}