@props([
    'type' => 'text',
    'name' => '',
    'id' => null,
    'value' => '',
    'label' => null,
    'placeholder' => null,
    'required' => false,
    'disabled' => false,
    'readonly' => false,
    'error' => null,
    'help' => null,
    'prefix' => null,
    'suffix' => null,
    'wire:model' => null,
])

@php
    $inputId = $id ?? 'input-' . $name . '-' . uniqid();
    $hasError = $error || $errors->has($name);
    
    $inputClasses = implode(' ', [
        'block w-full rounded-md shadow-sm',
        'transition duration-150 ease-in-out',
        $hasError 
            ? 'border-red-300 text-red-900 placeholder-red-300 focus:border-red-500 focus:ring-red-500' 
            : 'border-gray-300 focus:border-blue-500 focus:ring-blue-500',
        $disabled || $readonly ? 'bg-gray-100' : '',
        'sm:text-sm',
        $attributes->get('class', ''),
    ]);
@endphp

{{-- Form Input Component - Test file for Laravel Extension --}}
{{-- This component is referenced by <x-forms.input> in Blade files --}}
{{-- Located at: resources/views/components/forms/input.blade.php --}}

<div class="form-group">
    @if($label)
        <label for="{{ $inputId }}" class="block text-sm font-medium text-gray-700 mb-1">
            {{ $label }}
            @if($required)
                <span class="text-red-500">*</span>
            @endif
        </label>
    @endif
    
    <div class="relative">
        @if($prefix)
            <div class="absolute inset-y-0 left-0 pl-3 flex items-center pointer-events-none">
                <span class="text-gray-500 sm:text-sm">
                    {{ $prefix }}
                </span>
            </div>
        @endif
        
        <input
            type="{{ $type }}"
            name="{{ $name }}"
            id="{{ $inputId }}"
            value="{{ old($name, $value) }}"
            @if($placeholder) placeholder="{{ $placeholder }}" @endif
            @if($required) required @endif
            @if($disabled) disabled @endif
            @if($readonly) readonly @endif
            @if($attributes->has('wire:model'))
                wire:model="{{ $attributes->get('wire:model') }}"
            @endif
            class="{{ $inputClasses }} {{ $prefix ? 'pl-10' : '' }} {{ $suffix ? 'pr-10' : '' }}"
            {{ $attributes->except(['class', 'type', 'name', 'id', 'value', 'placeholder', 'required', 'disabled', 'readonly', 'wire:model']) }}
        />
        
        @if($suffix)
            <div class="absolute inset-y-0 right-0 pr-3 flex items-center pointer-events-none">
                <span class="text-gray-500 sm:text-sm">
                    {{ $suffix }}
                </span>
            </div>
        @endif
        
        @if($hasError)
            <div class="absolute inset-y-0 right-0 pr-3 flex items-center pointer-events-none">
                <svg class="h-5 w-5 text-red-500" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor">
                    <path fill-rule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-7 4a1 1 0 11-2 0 1 1 0 012 0zm-1-9a1 1 0 00-1 1v4a1 1 0 102 0V6a1 1 0 00-1-1z" clip-rule="evenodd" />
                </svg>
            </div>
        @endif
    </div>
    
    @if($hasError)
        <p class="mt-1 text-sm text-red-600">
            {{ $error ?: $errors->first($name) }}
        </p>
    @endif
    
    @if($help)
        <p class="mt-1 text-sm text-gray-500">
            {{ $help }}
        </p>
    @endif
</div>