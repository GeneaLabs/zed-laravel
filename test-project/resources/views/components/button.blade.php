@props([
    'type' => 'button',
    'variant' => 'primary',
    'size' => 'md',
    'disabled' => false,
    'href' => null,
    'wire:click' => null,
])

@php
    $classes = 'btn btn-' . $variant . ' btn-' . $size;
    
    $variantClasses = [
        'primary' => 'bg-blue-600 hover:bg-blue-700 text-white',
        'secondary' => 'bg-gray-600 hover:bg-gray-700 text-white',
        'success' => 'bg-green-600 hover:bg-green-700 text-white',
        'danger' => 'bg-red-600 hover:bg-red-700 text-white',
        'warning' => 'bg-yellow-500 hover:bg-yellow-600 text-black',
        'info' => 'bg-cyan-600 hover:bg-cyan-700 text-white',
        'light' => 'bg-gray-100 hover:bg-gray-200 text-gray-800',
        'dark' => 'bg-gray-800 hover:bg-gray-900 text-white',
        'link' => 'text-blue-600 hover:text-blue-800 underline',
    ];
    
    $sizeClasses = [
        'xs' => 'px-2 py-1 text-xs',
        'sm' => 'px-3 py-1.5 text-sm',
        'md' => 'px-4 py-2 text-base',
        'lg' => 'px-6 py-3 text-lg',
        'xl' => 'px-8 py-4 text-xl',
    ];
    
    $buttonClasses = implode(' ', [
        'inline-flex items-center justify-center',
        'font-medium rounded-md',
        'transition-colors duration-150',
        'focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-blue-500',
        $variantClasses[$variant] ?? $variantClasses['primary'],
        $sizeClasses[$size] ?? $sizeClasses['md'],
        $disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer',
        $attributes->get('class', ''),
    ]);
@endphp

{{-- Button Component - Test file for Laravel Extension --}}
{{-- This component is referenced by <x-button> in Blade files --}}
{{-- Located at: resources/views/components/button.blade.php --}}

@if($href)
    <a 
        href="{{ $href }}"
        class="{{ $buttonClasses }}"
        @if($disabled) 
            tabindex="-1" 
            aria-disabled="true"
            onclick="event.preventDefault();"
        @endif
        {{ $attributes->except(['class', 'type', 'variant', 'size', 'disabled', 'href']) }}
    >
        {{ $slot }}
    </a>
@else
    <button
        type="{{ $type }}"
        class="{{ $buttonClasses }}"
        @if($disabled) disabled @endif
        @if($attributes->has('wire:click'))
            wire:click="{{ $attributes->get('wire:click') }}"
        @endif
        {{ $attributes->except(['class', 'type', 'variant', 'size', 'disabled', 'wire:click']) }}
    >
        {{ $slot }}
    </button>
@endif