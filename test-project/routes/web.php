<?php

use Illuminate\Support\Facades\Route;
use App\Http\Controllers\TestController;
use App\Http\Controllers\UserController;
use App\Http\Controllers\Admin\DashboardController;
use App\Http\Controllers\Api\ApiController;
use App\Livewire\UserProfile;
use App\Livewire\SearchUsers;

/*
|--------------------------------------------------------------------------
| Web Routes - Test file for Laravel Extension
|--------------------------------------------------------------------------
|
| This file contains various route patterns to test the go-to-definition
| feature of the Laravel extension. Click on route names in PHP files
| to navigate to their definitions here.
|
*/

// Simple routes with closures
Route::get('/', function () {
    return view('welcome');
})->name('home');

Route::get('/about', function () {
    return view('about');
})->name('about');

// Routes with controller methods
Route::get('/test', [TestController::class, 'index'])->name('test.index');
Route::get('/test/simple', [TestController::class, 'simpleViews'])->name('test.simple');
Route::get('/test/dynamic', [TestController::class, 'dynamicViews'])->name('test.dynamic');
Route::get('/test/all', [TestController::class, 'testAllPatterns'])->name('test.all');

// User routes with parameters
Route::get('/users', [UserController::class, 'index'])->name('users.index');
Route::get('/users/{user}', [UserController::class, 'show'])->name('users.show');
Route::get('/users/{user}/edit', [UserController::class, 'edit'])->name('users.edit');
Route::post('/users/{user}', [UserController::class, 'update'])->name('users.update');
Route::delete('/users/{user}', [UserController::class, 'destroy'])->name('users.destroy');

// Resource routes (generates multiple routes)
Route::resource('posts', PostController::class);
Route::resource('comments', CommentController::class)->only(['index', 'store', 'destroy']);
Route::resource('tags', TagController::class)->except(['create', 'edit']);

// Route groups with middleware
Route::middleware(['auth'])->group(function () {
    Route::get('/dashboard', function () {
        return view('dashboard');
    })->name('dashboard');
    
    Route::get('/profile', function () {
        return view('users.profile');
    })->name('profile');
    
    Route::get('/settings', function () {
        return view('users.settings');
    })->name('settings');
});

// Admin routes with prefix and namespace
Route::prefix('admin')->name('admin.')->middleware(['auth', 'admin'])->group(function () {
    Route::get('/', [DashboardController::class, 'index'])->name('index');
    Route::get('/dashboard', [DashboardController::class, 'dashboard'])->name('dashboard.index');
    Route::get('/dashboard/stats', [DashboardController::class, 'stats'])->name('dashboard.stats');
    Route::get('/dashboard/widgets/chart', [DashboardController::class, 'chartWidget'])->name('dashboard.widgets.chart');
    
    // Admin user management
    Route::get('/users', [Admin\UserController::class, 'index'])->name('users.index');
    Route::get('/users/create', [Admin\UserController::class, 'create'])->name('users.create');
    Route::post('/users', [Admin\UserController::class, 'store'])->name('users.store');
    Route::get('/users/{user}/edit', [Admin\UserController::class, 'edit'])->name('users.edit');
    
    // Admin products
    Route::get('/products', [Admin\ProductController::class, 'index'])->name('products.index');
    Route::get('/products/create', [Admin\ProductController::class, 'create'])->name('products.create');
    
    // Admin orders
    Route::get('/orders', [Admin\OrderController::class, 'index'])->name('orders.index');
    
    // Admin reports
    Route::get('/reports/generate', [Admin\ReportController::class, 'generate'])->name('reports.generate');
});

// API routes (usually in api.php but included here for testing)
Route::prefix('api')->name('api.')->middleware(['api'])->group(function () {
    Route::get('/users', [ApiController::class, 'users'])->name('users.index');
    Route::get('/users/{user}', [ApiController::class, 'user'])->name('users.show');
    Route::post('/users', [ApiController::class, 'createUser'])->name('users.store');
    Route::put('/users/{user}', [ApiController::class, 'updateUser'])->name('users.update');
    Route::delete('/users/{user}', [ApiController::class, 'deleteUser'])->name('users.destroy');
});

// Routes with multiple HTTP verbs
Route::match(['get', 'post'], '/search', function () {
    return view('search');
})->name('search');

Route::any('/webhook', function () {
    return response()->json(['status' => 'ok']);
})->name('webhook');

// Redirect routes
Route::redirect('/home', '/', 301);
Route::permanentRedirect('/old-about', '/about');

// View routes (direct view rendering)
Route::view('/terms', 'legal.terms')->name('terms');
Route::view('/privacy', 'legal.privacy', ['updated' => '2024-01-01'])->name('privacy');

// Fallback route (404 handler)
Route::fallback(function () {
    return view('errors.404');
});

// Routes with regular expression constraints
Route::get('/user/{id}', function ($id) {
    return "User ID: $id";
})->where('id', '[0-9]+')->name('user.byId');

Route::get('/post/{slug}', function ($slug) {
    return view('posts.show', ['slug' => $slug]);
})->where('slug', '[a-z0-9\-]+')->name('post.bySlug');

// Routes with multiple parameters
Route::get('/category/{category}/post/{post}', function ($category, $post) {
    return view('posts.show', compact('category', 'post'));
})->name('category.post');

// Subdomain routing (if applicable)
Route::domain('{subdomain}.example.com')->group(function () {
    Route::get('/', function ($subdomain) {
        return view('subdomains.home', ['subdomain' => $subdomain]);
    })->name('subdomain.home');
});

// Named route groups
Route::name('blog.')->prefix('blog')->group(function () {
    Route::get('/', function () {
        return view('blog.index');
    })->name('index');
    
    Route::get('/post/{post}', function ($post) {
        return view('blog.post', ['post' => $post]);
    })->name('post');
    
    Route::get('/category/{category}', function ($category) {
        return view('blog.category', ['category' => $category]);
    })->name('category');
});

// Livewire routes (full-page components)
Route::get('/livewire/profile', UserProfile::class)->name('livewire.profile');
Route::get('/livewire/search', SearchUsers::class)->name('livewire.search');

// Routes with model binding
Route::get('/users/{user:username}', function (App\Models\User $user) {
    return view('users.profile', ['user' => $user]);
})->name('users.byUsername');

// Signed routes
Route::get('/unsubscribe/{user}', function (App\Models\User $user) {
    return view('unsubscribe', ['user' => $user]);
})->name('unsubscribe')->middleware('signed');

// Rate limited routes
Route::middleware(['throttle:api'])->group(function () {
    Route::post('/api/login', [ApiController::class, 'login'])->name('api.login');
    Route::post('/api/register', [ApiController::class, 'register'])->name('api.register');
});

// Routes with custom middleware
Route::middleware(['verified', 'subscription'])->group(function () {
    Route::get('/premium', function () {
        return view('premium.index');
    })->name('premium.index');
    
    Route::get('/premium/features', function () {
        return view('premium.features');
    })->name('premium.features');
});

// Special test routes for edge cases
Route::get('/test-hyphen-route', function () {
    return view('test.hyphen-view');
})->name('test.hyphen-route');

Route::get('/test/2024/report', function () {
    return view('reports.2024.annual');
})->name('test.report.2024');

// Catch-all route (must be last)
Route::get('/{any}', function ($any) {
    return view('pages.dynamic', ['page' => $any]);
})->where('any', '.*')->name('catch.all');