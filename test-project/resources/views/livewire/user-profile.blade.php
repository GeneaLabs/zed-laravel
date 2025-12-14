{{-- User Profile Livewire Component View --}}
{{-- This view is rendered by App\Livewire\UserProfile --}}
{{-- Located at: resources/views/livewire/user-profile.blade.php --}}

<div class="user-profile-component">
    {{-- Flash Message --}}
    @if (session()->has('message'))
        <div class="alert alert-success alert-dismissible fade show" role="alert">
            {{ session('message') }}
            <button type="button" class="close" data-dismiss="alert" aria-label="Close">
                <span aria-hidden="true">&times;</span>
            </button>
        </div>
    @endif

    {{-- Profile Card --}}
    <div class="card">
        <div class="card-header d-flex justify-content-between align-items-center">
            <h3>User Profile</h3>
            @if($canEdit)
                <button 
                    wire:click="toggleEdit" 
                    class="btn btn-sm {{ $isEditing ? 'btn-secondary' : 'btn-primary' }}"
                >
                    {{ $isEditing ? 'Cancel' : 'Edit Profile' }}
                </button>
            @endif
        </div>

        <div class="card-body">
            {{-- Profile Display Mode --}}
            @if(!$isEditing)
                <div class="row">
                    <div class="col-md-3">
                        {{-- Avatar --}}
                        <div class="text-center mb-3">
                            @if($user && $user->avatar)
                                <img 
                                    src="{{ Storage::url($user->avatar) }}" 
                                    alt="{{ $name }}"
                                    class="rounded-circle"
                                    style="width: 150px; height: 150px; object-fit: cover;"
                                >
                            @else
                                <div class="bg-secondary rounded-circle d-flex align-items-center justify-content-center" 
                                     style="width: 150px; height: 150px;">
                                    <span class="text-white" style="font-size: 48px;">
                                        {{ substr($name, 0, 1) }}
                                    </span>
                                </div>
                            @endif
                        </div>
                    </div>
                    <div class="col-md-9">
                        <h4>{{ $name }}</h4>
                        <p class="text-muted">{{ $email }}</p>
                        @if($bio)
                            <div class="mt-3">
                                <h5>Bio</h5>
                                <p>{{ $bio }}</p>
                            </div>
                        @endif
                    </div>
                </div>
            @else
                {{-- Edit Mode --}}
                <form wire:submit.prevent="updateProfile">
                    <div class="row">
                        <div class="col-md-3">
                            {{-- Avatar Upload --}}
                            <div class="text-center mb-3">
                                @if($avatar)
                                    <img 
                                        src="{{ $avatar->temporaryUrl() }}" 
                                        alt="New avatar"
                                        class="rounded-circle mb-2"
                                        style="width: 150px; height: 150px; object-fit: cover;"
                                    >
                                @elseif($user && $user->avatar)
                                    <img 
                                        src="{{ Storage::url($user->avatar) }}" 
                                        alt="{{ $name }}"
                                        class="rounded-circle mb-2"
                                        style="width: 150px; height: 150px; object-fit: cover;"
                                    >
                                @else
                                    <div class="bg-secondary rounded-circle d-flex align-items-center justify-content-center mb-2" 
                                         style="width: 150px; height: 150px;">
                                        <span class="text-white" style="font-size: 48px;">
                                            {{ substr($name, 0, 1) }}
                                        </span>
                                    </div>
                                @endif
                                
                                <div class="mb-3">
                                    <label for="avatar" class="btn btn-sm btn-outline-primary">
                                        Change Avatar
                                    </label>
                                    <input 
                                        type="file" 
                                        id="avatar"
                                        wire:model="avatar"
                                        class="d-none"
                                        accept="image/*"
                                    >
                                    @error('avatar')
                                        <div class="text-danger small mt-1">{{ $message }}</div>
                                    @enderror
                                </div>
                                
                                {{-- Upload Progress --}}
                                <div wire:loading wire:target="avatar" class="text-muted small">
                                    Uploading...
                                </div>
                            </div>
                        </div>
                        
                        <div class="col-md-9">
                            {{-- Name Input --}}
                            <div class="mb-3">
                                <label for="name" class="form-label">Name</label>
                                <input 
                                    type="text" 
                                    class="form-control @error('name') is-invalid @enderror" 
                                    id="name"
                                    wire:model.defer="name"
                                    placeholder="Enter your name"
                                >
                                @error('name')
                                    <div class="invalid-feedback">{{ $message }}</div>
                                @enderror
                            </div>
                            
                            {{-- Email Input --}}
                            <div class="mb-3">
                                <label for="email" class="form-label">Email</label>
                                <input 
                                    type="email" 
                                    class="form-control @error('email') is-invalid @enderror" 
                                    id="email"
                                    wire:model.defer="email"
                                    placeholder="Enter your email"
                                >
                                @error('email')
                                    <div class="invalid-feedback">{{ $message }}</div>
                                @enderror
                            </div>
                            
                            {{-- Bio Textarea --}}
                            <div class="mb-3">
                                <label for="bio" class="form-label">Bio</label>
                                <textarea 
                                    class="form-control @error('bio') is-invalid @enderror" 
                                    id="bio"
                                    wire:model.defer="bio"
                                    rows="4"
                                    placeholder="Tell us about yourself..."
                                ></textarea>
                                @error('bio')
                                    <div class="invalid-feedback">{{ $message }}</div>
                                @enderror
                                <div class="form-text">
                                    {{ strlen($bio) }}/500 characters
                                </div>
                            </div>
                            
                            {{-- Form Actions --}}
                            <div class="d-flex justify-content-between">
                                <button 
                                    type="submit" 
                                    class="btn btn-primary"
                                    wire:loading.attr="disabled"
                                >
                                    <span wire:loading.remove wire:target="updateProfile">
                                        Save Changes
                                    </span>
                                    <span wire:loading wire:target="updateProfile">
                                        <span class="spinner-border spinner-border-sm me-2" role="status" aria-hidden="true"></span>
                                        Saving...
                                    </span>
                                </button>
                                
                                <button 
                                    type="button"
                                    wire:click="toggleEdit"
                                    class="btn btn-secondary"
                                >
                                    Cancel
                                </button>
                            </div>
                        </div>
                    </div>
                </form>
            @endif
        </div>
        
        {{-- Card Footer with Actions --}}
        <div class="card-footer">
            <div class="d-flex justify-content-between align-items-center">
                <div>
                    <small class="text-muted">
                        Last updated: {{ $user ? $user->updated_at->diffForHumans() : 'Never' }}
                    </small>
                </div>
                
                @if($canEdit)
                    <button 
                        wire:click="confirmDelete"
                        class="btn btn-sm btn-danger"
                    >
                        Delete Account
                    </button>
                @endif
            </div>
        </div>
    </div>
    
    {{-- Delete Confirmation Modal --}}
    @if($showDeleteConfirmation)
        <div class="modal fade show d-block" tabindex="-1" role="dialog" style="background: rgba(0,0,0,0.5);">
            <div class="modal-dialog" role="document">
                <div class="modal-content">
                    <div class="modal-header">
                        <h5 class="modal-title">Confirm Account Deletion</h5>
                        <button 
                            type="button" 
                            class="close" 
                            wire:click="cancelDelete"
                            aria-label="Close"
                        >
                            <span aria-hidden="true">&times;</span>
                        </button>
                    </div>
                    <div class="modal-body">
                        <p>Are you sure you want to delete your account? This action cannot be undone.</p>
                    </div>
                    <div class="modal-footer">
                        <button 
                            type="button" 
                            class="btn btn-secondary" 
                            wire:click="cancelDelete"
                        >
                            Cancel
                        </button>
                        <button 
                            type="button" 
                            class="btn btn-danger" 
                            wire:click="deleteAccount"
                        >
                            Delete Account
                        </button>
                    </div>
                </div>
            </div>
        </div>
    @endif
    
    {{-- Component Styles --}}
    <style>
        .user-profile-component {
            max-width: 800px;
            margin: 0 auto;
            padding: 20px;
        }
        
        .user-profile-component .card {
            box-shadow: 0 0 10px rgba(0,0,0,0.1);
        }
        
        .user-profile-component .modal.show {
            display: block !important;
        }
    </style>
    
    {{-- Component Scripts --}}
    <script>
        document.addEventListener('livewire:load', function () {
            Livewire.on('profile-updated', function () {
                // Handle profile update event
                console.log('Profile has been updated');
            });
            
            Livewire.on('avatar-uploaded', function (data) {
                // Handle avatar upload event
                console.log('Avatar uploaded:', data.path);
            });
        });
    </script>
</div>