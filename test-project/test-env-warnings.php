<?php

declare(strict_types=1);

// This file tests env() variable warnings in the LSP

// These should NOT show warnings (they exist in .env.example)
$appName = env('APP_NAME', 'Laravel');
$appEnv = env('APP_ENV', 'production');
$appDebug = env('APP_DEBUG', false);

// These SHOULD show warnings (they don't exist in any .env file)
$undefinedVar = env('THIS_VAR_DOES_NOT_EXIST');
$anotherMissing = env('ANOTHER_UNDEFINED_VAR', 'default');
$missingDbHost = env('SOME_RANDOM_CONFIG_VALUE');

// Test with config() - should work fine
$configValue = config('app.name');