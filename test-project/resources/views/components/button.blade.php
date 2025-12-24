<button {{ $attributes->merge(['type' => 'button', 'class' => 'btn']) }}>
    {{ $slot }}
</button>