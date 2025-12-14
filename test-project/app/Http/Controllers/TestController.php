<?php

namespace App\Http\Controllers;

use App\Models\User;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\View;

/**
 * Test Controller for Laravel Extension Development
 * 
 * This file contains various patterns of view() calls
 * to test the go-to-definition feature
 */
class TestController extends Controller
{
    /**
     * Simple view references - Phase 2
     * Click on these view names to navigate to the Blade files
     */
    public function simpleViews()
    {
        // Single-word view
        return view('welcome');
        
        // Nested view with dots
        return view('users.profile');
        
        // Deeply nested view
        return view('admin.dashboard.index');
        
        // With double quotes
        return view("users.settings");
    }
    
    /**
     * Views with data - Phase 2
     */
    public function viewsWithData()
    {
        // View with compact
        $users = User::all();
        return view('users.index', compact('users'));
        
        // View with array
        return view('users.show', [
            'user' => User::find(1),
            'posts' => []
        ]);
        
        // View with with() method
        return view('users.edit')->with('user', User::find(1));
        
        // Chained with methods
        return view('admin.dashboard.stats')
            ->with('total', 100)
            ->with('active', 50);
    }
    
    /**
     * Conditional and dynamic views - Phase 3
     */
    public function dynamicViews()
    {
        // Conditional view
        if (auth()->check()) {
            return view('users.dashboard');
        } else {
            return view('guests.welcome');
        }
        
        // View in ternary
        $view = auth()->check() 
            ? view('users.home') 
            : view('public.home');
            
        // View exists check
        if (View::exists('custom.template')) {
            return view('custom.template');
        }
        
        // First available view
        return view()->first([
            'custom.dashboard',
            'users.dashboard',
            'dashboard'
        ]);
    }
    
    /**
     * View facade usage - Phase 3
     */
    public function facadeViews()
    {
        // Using View facade
        return View::make('users.profile');
        
        // Facade with data
        return View::make('users.list', ['users' => User::all()]);
        
        // Check existence with facade
        if (View::exists('special.promo')) {
            return View::make('special.promo');
        }
    }
    
    /**
     * Response and redirect with views
     */
    public function responseViews()
    {
        // Response with view
        return response()->view('errors.404', [], 404);
        
        // Redirect with view
        return redirect()->route('home')->with('message', 'Success!');
    }
    
    /**
     * Multi-line view calls - Edge cases for Phase 3
     */
    public function multiLineViews()
    {
        // Multi-line view call
        return view(
            'emails.order.confirmation',
            [
                'order' => $order,
                'user' => $user
            ]
        );
        
        // Complex multi-line
        return view('reports.annual')
            ->with('year', 2024)
            ->with('data', $this->getReportData())
            ->with('format', 'pdf');
    }
    
    /**
     * Views with namespaces/packages - Phase 6
     */
    public function packageViews()
    {
        // Package view (namespace::view)
        return view('admin::users.index');
        
        // Vendor package view
        return view('vendor.package.component');
        
        // Another package format
        return view('livewire::modal');
    }
    
    /**
     * Edge cases and special patterns
     */
    public function edgeCases()
    {
        // View with special characters
        return view('emails.user-welcome');
        
        // View with numbers
        return view('reports.2024.quarterly');
        
        // View name in variable (harder to detect)
        $viewName = 'users.profile';
        return view($viewName);
        
        // Concatenated view name (very hard to detect)
        $prefix = 'admin';
        return view($prefix . '.dashboard');
        
        // View with UTF-8 characters in comments
        // This should navigate to résumé.blade.php
        return view('documents.resume');
    }
    
    /**
     * Component and include directives (for Blade files)
     * These would appear in Blade files, not PHP
     */
    public function componentExamples()
    {
        // These comments show patterns we'll find in Blade files:
        // @include('partials.header')
        // @include('partials.footer')
        // @extends('layouts.app')
        // @component('components.alert')
        // <x-button type="primary" />
        // <x-forms.input name="email" />
        // <livewire:user-profile />
        // <flux:button variant="primary" />
        
        return view('components.showcase');
    }
    
    /**
     * Config and route references - Phase 6
     */
    public function configAndRoutes()
    {
        // Config references
        $appName = config('app.name');
        $timezone = config('app.timezone');
        $dbConnection = config('database.default');
        
        // Route references
        $url = route('users.profile', $user);
        $home = route('home');
        $api = route('api.users.index');
        
        return view('test.config-routes');
    }
    
    /**
     * Method to test all patterns at once
     */
    public function testAllPatterns()
    {
        // This method references multiple views for testing
        $views = [
            'welcome',                      // Simple
            'users.index',                   // Nested
            'admin.dashboard.widgets.chart', // Deeply nested
            'emails.order-confirmation',     // With hyphen
            'reports.2024.q1',              // With numbers
        ];
        
        foreach ($views as $view) {
            if (View::exists($view)) {
                echo "✅ Found: $view\n";
            } else {
                echo "❌ Missing: $view\n";
            }
        }
        
        return view('test.results');
    }
}