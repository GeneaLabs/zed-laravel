<?php

namespace App\Providers;

use Illuminate\Support\ServiceProvider;

class AppServiceProvider extends ServiceProvider
{
    /**
     * Register any application services.
     */
    public function register(): void
    {
        // Test bindings for container resolution
        $this->app->bind('test', \App\Models\User::class);
        $this->app->singleton('cache', \Illuminate\Cache\CacheManager::class);
        $this->app->bind(\App\Contracts\PaymentGateway::class, \App\Services\StripeGateway::class);
        $this->app->singleton(\App\Services\UserService::class);
        $this->app->alias(\App\Services\UserService::class, 'user.service');
    }

    /**
     * Bootstrap any application services.
     */
    public function boot(): void
    {
        //
    }
}
