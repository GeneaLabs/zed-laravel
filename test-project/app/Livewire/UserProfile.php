<?php

namespace App\Livewire;

use Livewire\Component;
use App\Models\User;
use Livewire\WithFileUploads;

/**
 * User Profile Livewire Component
 *
 * This component is referenced by <livewire:user-profile /> in Blade files
 * Located at: app/Livewire/UserProfile.php
 *
 * Test navigation: Click on "user-profile" in Blade files to navigate here
 */
class UserProfile extends Component
{
    use WithFileUploads;

    // Public properties that can be bound with wire:model
    public $userId;
    public $name;
    public $email;
    public $bio;
    public $avatar;
    public $isEditing = false;
    public $showDeleteConfirmation = false;

    // Validation rules
    protected $rules = [
        'name' => 'required|string|min:3|max:255',
        'email' => 'required|email',
        'bio' => 'nullable|string|max:500',
        'avatar' => 'nullable|image|max:1024', // 1MB Max
    ];

    // Custom validation messages
    protected $messages = [
        'name.required' => 'The name field is required.',
        'name.min' => 'Name must be at least 3 characters.',
        'email.required' => 'Please provide an email address.',
        'email.email' => 'Please provide a valid email address.',
        'avatar.image' => 'The avatar must be an image.',
        'avatar.max' => 'The avatar must not be larger than 1MB.',
    ];

    // Component mount method
    public function mount($userId = null)
    {
        $this->userId = $userId ?? auth()->id();
        $this->loadUser();
    }

    // Load user data
    public function loadUser()
    {
        if ($this->userId) {
            $user = User::find($this->userId);
            if ($user) {
                $this->name = $user->name;
                $this->email = $user->email;
                $this->bio = $user->bio ?? '';
            }
        }
    }

    // Toggle edit mode
    public function toggleEdit()
    {
        $this->isEditing = !$this->isEditing;

        if (!$this->isEditing) {
            // Reset to original values if canceling
            $this->loadUser();
        }
    }

    // Update user profile
    public function updateProfile()
    {
        $this->validate();

        $user = User::find($this->userId);
        if ($user) {
            $user->update([
                'name' => $this->name,
                'email' => $this->email,
                'bio' => $this->bio,
            ]);

            if ($this->avatar) {
                $path = $this->avatar->store('avatars', 'public');
                $user->update(['avatar' => $path]);
            }

            $this->isEditing = false;
            $this->dispatch('profile-updated');
            session()->flash('message', 'Profile updated successfully!');
        }
    }

    // Real-time validation
    public function updated($propertyName)
    {
        $this->validateOnly($propertyName);
    }

    // Delete account confirmation
    public function confirmDelete()
    {
        $this->showDeleteConfirmation = true;
    }

    // Cancel delete
    public function cancelDelete()
    {
        $this->showDeleteConfirmation = false;
    }

    // Delete user account
    public function deleteAccount()
    {
        $user = User::find($this->userId);
        if ($user && auth()->id() === $user->id) {
            $user->delete();
            auth()->logout();
            return redirect()->route('home');
        }

        $this->showDeleteConfirmation = false;
    }

    // Refresh component
    public function refresh()
    {
        $this->loadUser();
    }

    // Listen for events
    protected $listeners = [
        'refreshProfile' => 'refresh',
        'userUpdated' => 'loadUser',
    ];

    // Render the component view
    public function render()
    {
        return view('livewire.user-profile', [
            'user' => User::find($this->userId),
            'canEdit' => auth()->id() === $this->userId,
        ]);
    }

    // Lifecycle hooks
    public function hydrate()
    {
        // Called on subsequent requests
    }

    public function dehydrate()
    {
        // Called before the response is sent to browser
    }

    // Custom methods
    public function uploadAvatar()
    {
        $this->validate([
            'avatar' => 'required|image|max:1024',
        ]);

        $path = $this->avatar->store('avatars', 'public');

        $user = User::find($this->userId);
        if ($user) {
            $user->update(['avatar' => $path]);
            $this->dispatch('avatar-uploaded', ['path' => $path]);
        }
    }
}
