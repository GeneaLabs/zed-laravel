<?php

declare(strict_types=1);

namespace App\Http\Controllers;

use Illuminate\Http\JsonResponse;

class EnvTestController extends Controller
{
    /**
     * Test various env() and config() patterns
     */
    public function index(): JsonResponse
    {
        // Test env() calls with single quotes
        $appName = env('APP_NAME', 'Laravel');
        $appEnv = env('APP_ENV', 'production');
        
        // Test env() calls with double quotes
        $appDebug = env("APP_DEBUG", false);
        $appUrl = env("APP_URL", "http://localhost");
        
        // Test env() without default
        $appKey = env('APP_KEY');
        
        // Test config() calls with single quotes
        $configName = config('app.name');
        $configEnv = config('app.env');
        
        // Test config() calls with double quotes
        $configDebug = config("app.debug");
        $configUrl = config("app.url");
        
        // Test nested config keys
        $dbHost = config('database.connections.mysql.host');
        $dbDatabase = config("database.connections.mysql.database");
        
        // Test config with default value
        $customConfig = config('custom.setting', 'default-value');
        
        return response()->json([
            'env' => [
                'app_name' => $appName,
                'app_env' => $appEnv,
                'app_debug' => $appDebug,
                'app_url' => $appUrl,
                'app_key' => $appKey,
            ],
            'config' => [
                'name' => $configName,
                'env' => $configEnv,
                'debug' => $configDebug,
                'url' => $configUrl,
                'db_host' => $dbHost,
                'db_database' => $dbDatabase,
                'custom' => $customConfig,
            ],
        ]);
    }
}