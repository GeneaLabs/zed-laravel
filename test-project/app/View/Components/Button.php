<?php

declare(strict_types=1);

namespace App\View\Components;

use Illuminate\View\View;

class Button
{
    protected int $test = 0;

    public function render(): View
    {
        return view('components.button')
            ->with([
                'test' => $this->test,
            ]);
    }
}
