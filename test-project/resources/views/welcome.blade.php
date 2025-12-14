{{-- Welcome Page - Test file for Laravel Extension --}}
@extends('layouts.app')

@section('title', 'Welcome')

@section('content')
<div class="container">
    <h1>Welcome to Laravel Extension Test</h1>
    
    <p>This is the welcome.blade.php file</p>
    <p>When you click on view('welcome') in PHP files, you should navigate here!</p>
    
    {{-- Test various Blade directives --}}
    
    @include('partials.header')
    
    @component('components.alert', ['type' => 'success'])
        This is an alert component
    @endcomponent
    
    {{-- Modern component syntax --}}
    <x-button type="primary">
        Click Me
    </x-button>
    
    <x-forms.input 
        name="email" 
        type="email"
        placeholder="Enter your email"
    />
    
    {{-- Livewire components --}}
    @livewire('user-counter')
    
    <livewire:search-users />
    
    {{-- Flux components (if using Flux UI) --}}
    <flux:button variant="primary">
        Flux Button
    </flux:button>
    
    <flux:card>
        <flux:card.header>
            Card Title
        </flux:card.header>
        <flux:card.body>
            Card content goes here
        </flux:card.body>
    </flux:card>
    
    @include('partials.footer')
</div>
@endsection

@push('scripts')
<script>
    console.log('Welcome page loaded');
</script>
@endpush