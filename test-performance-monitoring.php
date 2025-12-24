<?php

declare(strict_types=1);

namespace App\Http\Controllers;

use Illuminate\Http\Request;
use Illuminate\Http\JsonResponse;
use Illuminate\View\View;

class TestController extends Controller
{
    /**
     * Test hover performance on view calls
     */
    public function index(): View
    {
        // Test view() calls for hover performance
        return view('welcome');
    }

    /**
     * Test goto definition performance
     */
    public function show(Request $request): JsonResponse
    {
        // Test config() calls for goto definition performance
        $appName = config('app.name');
        $dbConnection = config('database.default');
        
        // Test env() calls for performance monitoring
        $debug = env('APP_DEBUG', false);
        $url = env('APP_URL', 'http://localhost');
        
        return response()->json([
            'app_name' => $appName,
            'db_connection' => $dbConnection,
            'debug' => $debug,
            'url' => $url,
        ]);
    }

    /**
     * Test multiple Laravel patterns in one method
     */
    public function complexMethod(): View
    {
        // Multiple view calls to test cache performance
        $header = view('components.header');
        $sidebar = view('components.sidebar');
        $footer = view('components.footer');
        
        // Config calls
        $timezone = config('app.timezone');
        $locale = config('app.locale');
        
        // Asset calls  
        $cssPath = asset('css/app.css');
        $jsPath = asset('js/app.js');
        
        // Translation calls
        $title = __('Welcome');
        $message = trans('messages.greeting');
        
        return view('dashboard', compact(
            'header', 'sidebar', 'footer',
            'timezone', 'locale', 
            'cssPath', 'jsPath',
            'title', 'message'
        ));
    }
}