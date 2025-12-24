@extends('layouts.app')

@section('title', 'Performance Test - ' . config('app.name'))

@section('content')
<div class="container">
    {{-- Environment variables will be cached with hover values --}}
    <h1>{{ env('APP_NAME', 'Laravel Application') }}</h1>
    <p class="lead">Debug Mode: {{ env('APP_DEBUG') ? 'Enabled' : 'Disabled' }}</p>
    
    {{-- Configuration calls will show file existence status --}}
    <div class="app-config">
        <h2>Application Configuration</h2>
        <ul>
            <li>Timezone: {{ config('app.timezone') }}</li>
            <li>Locale: {{ config('app.locale') }}</li>
            <li>URL: {{ config('app.url') }}</li>
            <li>Environment: {{ config('app.env') }}</li>
        </ul>
    </div>
    
    {{-- Database configuration --}}
    <div class="database-config">
        <h2>Database Settings</h2>
        <p>Connection: {{ config('database.default') }}</p>
        <p>Host: {{ env('DB_HOST', 'localhost') }}</p>
        <p>Port: {{ env('DB_PORT', '3306') }}</p>
    </div>
    
    {{-- Blade components will be clickable --}}
    <div class="components-section">
        <h2>UI Components</h2>
        <x-alert type="success" dismissible>
            Welcome to the performance test page!
        </x-alert>
        
        <x-card title="User Profile" class="mt-4">
            <x-slot:header>
                <x-button variant="primary">Edit Profile</x-button>
            </x-slot:header>
            
            <p>This demonstrates component navigation.</p>
            
            <x-slot:footer>
                <x-button variant="secondary">Cancel</x-button>
                <x-button variant="primary">Save Changes</x-button>
            </x-slot:footer>
        </x-card>
        
        <x-forms.input 
            name="email" 
            label="Email Address"
            :value="old('email')"
            type="email"
            required
        />
        
        <x-navigation.breadcrumb :items="$breadcrumbs" />
    </div>
    
    {{-- Livewire components will be clickable --}}
    <div class="livewire-section">
        <h2>Interactive Components</h2>
        <livewire:user-profile :user="$user" />
        <livewire:shopping-cart />
        <livewire:notification-center />
        <livewire:search.advanced-filter />
        <livewire:forms.contact-form />
    </div>
    
    {{-- Directives will be parsed and clickable --}}
    <div class="directives-section">
        <h2>Template Logic</h2>
        
        @auth
            <p>Welcome back, {{ auth()->user()->name }}!</p>
            
            @can('admin-panel')
                <a href="{{ route('admin.dashboard') }}">Admin Panel</a>
            @endcan
            
            @hasrole('moderator')
                <x-moderator-tools />
            @endhasrole
            
        @else
            <p>Please <a href="{{ route('login') }}">login</a> to continue.</p>
        @endauth
        
        @guest
            <x-guest-welcome />
        @endguest
        
        @verified
            <p>Your email is verified!</p>
        @else
            <x-email-verification-notice />
        @endverified
        
        @production
            <!-- Production-only content -->
            <script>
                // Analytics code
                gtag('config', '{{ config("services.google.analytics_id") }}');
            </script>
        @endproduction
        
        @env('local')
            <div class="debug-toolbar">
                <p>Running in local environment</p>
                <p>Debug: {{ config('app.debug') ? 'ON' : 'OFF' }}</p>
            </div>
        @endenv
    </div>
    
    {{-- Conditional sections --}}
    @if(config('features.dark_mode'))
        <div class="theme-toggle">
            <x-theme-switcher />
        </div>
    @endif
    
    @unless(config('app.maintenance_mode'))
        <div class="main-content">
            {{-- Include other views --}}
            @include('partials.sidebar')
            @include('partials.notifications')
            @includeIf('partials.premium-features')
            @includeWhen(auth()->check(), 'partials.user-menu')
            @includeUnless(request()->is('admin/*'), 'partials.public-footer')
        </div>
    @endunless
    
    {{-- Loops and iteration --}}
    <div class="data-lists">
        <h3>Dynamic Content</h3>
        
        @forelse($users as $user)
            <div class="user-card">
                <h4>{{ $user->name }}</h4>
                <p>{{ $user->email }}</p>
                
                @foreach($user->roles as $role)
                    <span class="badge">{{ $role->name }}</span>
                @endforeach
                
                @for($i = 1; $i <= $user->rating; $i++)
                    <span class="star">★</span>
                @endfor
            </div>
        @empty
            <p>No users found.</p>
        @endforelse
        
        @while($condition)
            <p>This will loop while condition is true</p>
        @endwhile
    </div>
    
    {{-- Translation calls will show language file locations --}}
    <div class="translations">
        <h2>{{ __('Multilingual Content') }}</h2>
        <p>{{ trans('welcome.message') }}</p>
        <p>{{ __('validation.required') }}</p>
        <p>{{ trans_choice('messages.items', $count) }}</p>
        <p>@lang('auth.failed')</p>
        
        @choice('There is one apple|There are many apples', $appleCount)
    </div>
    
    {{-- Asset directives will be clickable --}}
    @push('styles')
        <link href="{{ asset('css/custom.css') }}" rel="stylesheet">
        <link href="{{ mix('css/app.css') }}" rel="stylesheet">
    @endpush
    
    @push('scripts')
        <script src="{{ asset('js/custom.js') }}"></script>
        <script src="{{ mix('js/app.js') }}"></script>
    @endpush
    
    {{-- Vite assets will be individually clickable --}}
    @vite(['resources/css/app.css', 'resources/js/app.js'])
    @vite(['resources/css/admin.css', 'resources/js/admin.js', 'resources/js/charts.js'])
    
    {{-- Stack directives --}}
    @stack('custom-styles')
    @stack('page-scripts')
    
    {{-- Session and CSRF --}}
    @csrf
    @method('PUT')
    
    <form method="POST" action="{{ route('profile.update') }}">
        @csrf
        @method('PATCH')
        
        <input type="text" name="name" value="{{ old('name', $user->name) }}">
        
        @error('name')
            <span class="error">{{ $message }}</span>
        @enderror
        
        <button type="submit">{{ __('Update Profile') }}</button>
    </form>
    
    {{-- JSON data for JavaScript --}}
    <script>
        window.appConfig = @json([
            'apiUrl' => config('app.api_url'),
            'locale' => app()->getLocale(),
            'user' => auth()->user(),
            'csrf' => csrf_token(),
        ]);
        
        window.translations = {
            'save': @json(__('Save')),
            'cancel': @json(__('Cancel')),
            'delete': @json(__('Delete')),
        };
    </script>
    
    {{-- Debugging in non-production --}}
    @dump($debugData)
    @dd($criticalError)
    
    {{-- Custom directives --}}
    @datetime($user->created_at)
    @money($product->price)
    @markdown($post->content)
    
    {{-- Route generation --}}
    <nav>
        <a href="{{ route('home') }}">Home</a>
        <a href="{{ route('about') }}">About</a>
        <a href="{{ route('contact') }}">Contact</a>
        <a href="{{ url('/privacy') }}">Privacy Policy</a>
        <a href="{{ secure_url('/terms') }}">Terms of Service</a>
    </nav>
</div>

{{-- Section for additional CSS --}}
@section('additional-css')
    <style>
        .performance-test {
            background: {{ config('theme.colors.background', '#ffffff') }};
            color: {{ config('theme.colors.text', '#000000') }};
        }
    </style>
@endsection

{{-- Section for additional JavaScript --}}
@section('additional-js')
    <script>
        console.log('App Environment:', @json(config('app.env')));
        console.log('Debug Mode:', @json(config('app.debug')));
        console.log('API URL:', @json(config('app.api_url')));
    </script>
@endsection
@endsection

{{-- Comments for testing --}}
{{-- 
    This Blade template demonstrates various Laravel patterns that will be optimized:
    
    Performance improvements:
    1. ✅ Query caching - Queries compiled once, reused many times
    2. ✅ Incremental parsing - Only re-parse changed sections
    3. ✅ Two-tier debouncing - Fast cache updates (50ms), slower diagnostics (200ms)
    4. ✅ Pattern registry - Adding new patterns is now trivial
    5. ✅ Generic pattern matching - All patterns use the same system
    
    Hover over any Laravel pattern to see instant information!
--}}