{{-- User Profile Page - Test file for Laravel Extension --}}
@extends('layouts.app')

@section('title', 'User Profile')

@section('content')
<div class="container mx-auto px-4">
    <h1 class="text-3xl font-bold mb-6">User Profile</h1>
    
    <p class="text-gray-600 mb-4">
        This is resources/views/users/profile.blade.php<br>
        Navigate here by clicking on view('users.profile') in PHP files!
    </p>
    
    {{-- User info card using components --}}
    <x-card class="mb-6">
        <x-slot name="header">
            <h2 class="text-xl font-semibold">{{ $user->name ?? 'John Doe' }}</h2>
        </x-slot>
        
        <div class="space-y-4">
            <x-forms.input 
                label="Email"
                name="email" 
                value="{{ $user->email ?? 'john@example.com' }}"
                readonly
            />
            
            <x-forms.input 
                label="Username"
                name="username" 
                value="{{ $user->username ?? 'johndoe' }}"
                readonly
            />
            
            {{-- Include a partial for user stats --}}
            @include('users.partials.stats', ['user' => $user ?? null])
        </div>
        
        <x-slot name="footer">
            <x-button type="primary" wire:click="editProfile">
                Edit Profile
            </x-button>
            <x-button type="secondary" onclick="window.history.back()">
                Go Back
            </x-button>
        </x-slot>
    </x-card>
    
    {{-- Livewire component for user activity --}}
    <div class="mb-6">
        <h3 class="text-lg font-semibold mb-3">Recent Activity</h3>
        @livewire('user-activity', ['userId' => $user->id ?? 1])
    </div>
    
    {{-- Conditional content --}}
    @auth
        @if(auth()->id() === ($user->id ?? null))
            <div class="bg-blue-100 border-l-4 border-blue-500 p-4 mb-6">
                <p class="text-blue-700">This is your profile!</p>
            </div>
        @endif
    @endauth
    
    {{-- Loop through user posts --}}
    <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
        @forelse($posts ?? [] as $post)
            @include('posts.card', ['post' => $post])
        @empty
            <p class="text-gray-500 col-span-2">No posts yet.</p>
        @endforelse
    </div>
    
    {{-- Using components with different syntax --}}
    @component('components.modal')
        @slot('title')
            Delete Account
        @endslot
        
        @slot('content')
            Are you sure you want to delete your account?
        @endslot
        
        @slot('footer')
            <x-button type="danger">Delete</x-button>
            <x-button type="secondary">Cancel</x-button>
        @endslot
    @endcomponent
    
    {{-- Flux UI components example --}}
    <flux:tabs class="mt-8">
        <flux:tab name="posts" label="Posts">
            @include('users.tabs.posts')
        </flux:tab>
        <flux:tab name="comments" label="Comments">
            @include('users.tabs.comments')
        </flux:tab>
        <flux:tab name="settings" label="Settings">
            @include('users.tabs.settings')
        </flux:tab>
    </flux:tabs>
</div>
@endsection

@section('sidebar')
    @include('users.sidebar', ['user' => $user ?? null])
@endsection

@push('styles')
<style>
    /* Profile-specific styles */
    .profile-avatar {
        width: 100px;
        height: 100px;
        border-radius: 50%;
    }
</style>
@endpush

@push('scripts')
<script>
    // Profile page JavaScript
    document.addEventListener('DOMContentLoaded', function() {
        console.log('Profile page loaded for user: {{ $user->id ?? "unknown" }}');
    });
</script>
@endpush