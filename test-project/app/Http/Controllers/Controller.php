<?php

namespace App\Http\Controllers;

abstract class Controller
{
    function __construct()
    {
        Config::get('app.cipher');
        route('t');

        throw new \Exception('Not implemented');
    }
    //
}
