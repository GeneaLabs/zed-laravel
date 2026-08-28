# ℹ️ Hover

[← Back to README](../README.md)

Hover any recognised pattern to get an Intelephense-style summary card — a header, the relevant source snippet, and a clickable link to the file it resolves to. No need to jump away from your current line to remember what a view, route, or config key points at.

```php
return view('users.profile');
//          ^^^^^^^^^^^^^^^ hover →  resources/views/users/profile.blade.php
//                                   @props([...]) declaration + click-to-open link

$url = route('users.show', $user);
//           ^^^^^^^^^^^^ hover →  Route::get('/users/{user}', ...)->name('users.show')
//                                 verb · URI · controller@action · click-to-open link

$tz = config('app.timezone');
//           ^^^^^^^^^^^^^^ hover →  'UTC'   (the resolved value, from config/app.php)
```

```blade
{{ $user->email }}
{{-- ^^^^^ hover →  App\Models\User::$email, its PHPDoc summary, and the declaration --}}
```

**Component members in Blade** — `$this->member` in a template backed by a component class (Livewire in any format, class-based Volt, a Filament `$view`-property page) gets a card with the member's kind, the backing class, the full declaration header, and a click-to-open link. This card is emitted unconditionally in Blade: Intelephense cannot resolve `$this` inside a template's PHP context, so there is no PHP-tooling card to defer to.

```blade
{{ $this->getCalculatedEndDateForDisplay() }}
{{--      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ hover →  App\Livewire\ContractPage::getCalculatedEndDateForDisplay()
                                                   public function getCalculatedEndDateForDisplay(): ?string
                                                   + click-to-open link to the declaration --}}

{{ $this->uploadedFile }}
{{--      ^^^^^^^^^^^^ hover →  App\Livewire\ContractPage::$uploadedFile
                                public ?TemporaryUploadedFile $uploadedFile --}}
```

**Eloquent magic members** get semantic cards explaining what the magic actually is — the classification, the declaring class, and the method source that backs it:

```php
$user->posts
//     ^^^^^ hover →  Eloquent relationship — `posts` on `App\Models\User`
//                    public function posts() { return $this->hasMany(Post::class); }
//                    (the body reveals the target model)

User::active()
//    ^^^^^^ hover →  Eloquent scope — `active` on `App\Models\User`
//                    the scopeActive() query body

$user->email
//     ^^^^^ hover →  Database column — `email` on `App\Models\User`
//                    Type `string` (cast-aware: migrations first, live DB as fallback)

User::whereEmail($value)
//   ^^^^^^^^^^ hover →  Dynamic finder — the email column's type + migration link
```

Scopes, accessors, relationships, columns, and dynamic finders are all covered — property-form (`$user->posts`, `$model->full_name`) and call-form (`->active()`, `User::whereEmail()`) alike, including through builder chains (`User::query()->active()`), `self::` / `static::` calls, and `$query->active()` inside scope bodies. When the receiver's type had to be inferred rather than proven, the card says so (*receiver type inferred*). Plain properties and plain method calls Intelephense already understands get **no card** — duplicating its hover would just add noise.

**Artisan command strings** show the declaring `Command` class and its `$signature`:

```php
Artisan::call('emails:send');
//             ^^^^^^^^^^^ hover →  App\Console\Commands\SendEmails
//                                  protected $signature = 'emails:send {--queue}'
```

**Laravel helper functions** get a card on the **function name itself** (not just the string argument) — a Laravel-aware one-line synopsis plus a source link:

```php
route('home');
// ^^^^^ hover →  route — Generate a URL for a named route.
//                (link into vendor's helpers.php, or the laravel.com docs)

config('app.name');
// ^^^^^^ hover →  config — Get / set the value of a configuration variable.
```

This is a deliberately **curated allow-list** of seven helpers — `route`, `view`, `config`, `auth`, `app`, `session`, `cache` — chosen because their framework docblock is thin or generic, so a Laravel-aware synopsis adds value over what Intelephense already shows. That narrow set *is* the dedup policy: every other helper (`bcrypt`, `abort`, `collect`, `str`, …) is simply never indexed, so we never emit a duplicate card next to Intelephense's — no runtime detection needed. The source link points into the vendored framework `helpers.php` when it's present under the workspace root, and falls back to the canonical `laravel.com/docs/helpers` anchor otherwise. (The string *argument* still hovers as before — `route('home')`'s `'home'` resolves to the route definition; the two spans are independent.)

Hover also works the other way round, inside a `.env*` buffer: put the cursor
on a key's name and the card shows its effective value, the file that
declaration won from (`.env` outranks `.env.local`, which outranks
`.env.example`), and how many `env('KEY')` call sites consume it. A key nothing
consumes still gets a card, reading `0 references`. A commented-out declaration
says so, as does a key no `.env*` file defines — and that one keeps its
consumer count, since a stale or mistyped key is exactly when the call sites
matter.

```
APP_NAME=Acme
^^^^^^^^ hover →  APP_NAME
                  Acme
                  .env
                  3 references
```

**Hovered patterns:** views, Blade components (anonymous *and* class-backed), Livewire components, routes, config keys, env vars, translations (including `vendor::namespace.key`), middleware aliases, container bindings, assets (`asset()`, `Vite::asset()`, `mix()`, `public_path()`, …), `url()`, Blade variables, Eloquent magic members, Artisan command strings, and curated Laravel helper-function identifiers. The bottom-line source path renders as a `file://` link, so the whole card is click-to-open in any LSP client that supports markdown links.

Class-backed components and Livewire components show the `class Foo extends Component` signature and link to the PHP class; anonymous components fall back to the `@props([...])` declaration from the `.blade.php` template. Patterns without a meaningful target (directives, controller actions, Pennant features) stay silent rather than showing an empty card.
