<?php

// Test regular view names
return view('welcome');

// Test hyphenated view names - this should show diagnostic with full highlight
return view('user-profile');
return view('admin-dashboard');

// Test nested with hyphens
return view('layouts.app');
return view('admin.user-settings');

