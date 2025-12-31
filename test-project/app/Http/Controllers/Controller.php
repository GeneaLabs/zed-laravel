<?php

namespace App\Http\Controllers;

abstract class Controller
{
    function __construct()
    {
        Config::get('app.cipher');

        throw new \Exception('Not implemented');
    }
    //
}
