<?php

namespace App\Http\Controllers;

abstract class Controller
{
    function __construct()
    {
        Config::get('app.cipher');
        __('messages.goodbye');
        $rules = [
            'field' => 'required|array',
            'field1' => 'after:field|exists:users,name',
        ];

        throw new \Exception('Not implemented');
        view('component-test')
            ->with('data', $rules);
    }
    //
}
