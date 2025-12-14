{{-- Admin Dashboard - Test file for Laravel Extension --}}
@extends('layouts.admin')

@section('title', 'Admin Dashboard')

@section('breadcrumbs')
    <nav aria-label="breadcrumb">
        <ol class="breadcrumb">
            <li class="breadcrumb-item"><a href="{{ route('home') }}">Home</a></li>
            <li class="breadcrumb-item"><a href="{{ route('admin.index') }}">Admin</a></li>
            <li class="breadcrumb-item active" aria-current="page">Dashboard</li>
        </ol>
    </nav>
@endsection

@section('content')
<div class="admin-dashboard">
    <h1 class="page-title">Admin Dashboard</h1>
    
    <div class="alert alert-info">
        <strong>Test Navigation:</strong> This is resources/views/admin/dashboard/index.blade.php<br>
        You should arrive here when clicking on view('admin.dashboard.index')
    </div>
    
    {{-- Dashboard Statistics Grid --}}
    <div class="row mb-4">
        <div class="col-md-3">
            @component('components.stat-card')
                @slot('title', 'Total Users')
                @slot('value', $stats['users'] ?? '1,234')
                @slot('icon', 'users')
                @slot('color', 'primary')
            @endcomponent
        </div>
        <div class="col-md-3">
            <x-stat-card 
                title="Revenue"
                :value="$stats['revenue'] ?? '$12,345'"
                icon="dollar-sign"
                color="success"
            />
        </div>
        <div class="col-md-3">
            <x-stat-card 
                title="Orders"
                :value="$stats['orders'] ?? '567'"
                icon="shopping-cart"
                color="warning"
            />
        </div>
        <div class="col-md-3">
            <x-stat-card 
                title="Pending"
                :value="$stats['pending'] ?? '23'"
                icon="clock"
                color="danger"
            />
        </div>
    </div>
    
    {{-- Include various dashboard widgets --}}
    @include('admin.dashboard.widgets.chart')
    @include('admin.dashboard.widgets.recent-activity')
    @include('admin.dashboard.widgets.quick-actions')
    
    {{-- Tabbed Content Area --}}
    <div class="card">
        <div class="card-header">
            <ul class="nav nav-tabs card-header-tabs">
                <li class="nav-item">
                    <a class="nav-link active" href="#users" data-toggle="tab">Users</a>
                </li>
                <li class="nav-item">
                    <a class="nav-link" href="#products" data-toggle="tab">Products</a>
                </li>
                <li class="nav-item">
                    <a class="nav-link" href="#orders" data-toggle="tab">Orders</a>
                </li>
            </ul>
        </div>
        <div class="card-body">
            <div class="tab-content">
                <div class="tab-pane active" id="users">
                    @include('admin.dashboard.tabs.users')
                </div>
                <div class="tab-pane" id="products">
                    @include('admin.dashboard.tabs.products')
                </div>
                <div class="tab-pane" id="orders">
                    @include('admin.dashboard.tabs.orders')
                </div>
            </div>
        </div>
    </div>
    
    {{-- Livewire Components for Real-time Data --}}
    <div class="row mt-4">
        <div class="col-md-6">
            <livewire:admin.user-activity-feed />
        </div>
        <div class="col-md-6">
            <livewire:admin.system-notifications />
        </div>
    </div>
    
    {{-- Flux UI Components Example --}}
    @if(config('app.ui_framework') === 'flux')
        <flux:card class="mt-4">
            <flux:card.header>
                <flux:heading size="lg">System Status</flux:heading>
            </flux:card.header>
            <flux:card.body>
                <flux:badge color="green">All Systems Operational</flux:badge>
                <flux:progress :value="85" class="mt-2" />
            </flux:card.body>
        </flux:card>
    @endif
    
    {{-- Admin Quick Actions --}}
    <div class="quick-actions mt-4">
        <h3>Quick Actions</h3>
        <div class="btn-group" role="group">
            <x-button type="primary" href="{{ route('admin.users.create') }}">
                <x-icon name="user-plus" /> Add User
            </x-button>
            <x-button type="success" href="{{ route('admin.products.create') }}">
                <x-icon name="plus-circle" /> Add Product
            </x-button>
            <x-button type="info" href="{{ route('admin.reports.generate') }}">
                <x-icon name="file-text" /> Generate Report
            </x-button>
        </div>
    </div>
    
    {{-- Complex Component with Named Slots --}}
    <x-admin.panel class="mt-4">
        <x-slot name="title">
            Recent Orders
        </x-slot>
        
        <x-slot name="actions">
            <a href="{{ route('admin.orders.index') }}" class="btn btn-sm btn-outline-primary">
                View All
            </a>
        </x-slot>
        
        <table class="table">
            <thead>
                <tr>
                    <th>Order ID</th>
                    <th>Customer</th>
                    <th>Total</th>
                    <th>Status</th>
                    <th>Actions</th>
                </tr>
            </thead>
            <tbody>
                @foreach($recentOrders ?? [] as $order)
                    @include('admin.orders.row', ['order' => $order])
                @endforeach
            </tbody>
        </table>
    </x-admin.panel>
    
    {{-- Permission-based Content --}}
    @can('manage-users')
        <div class="alert alert-warning mt-4">
            <strong>Admin Notice:</strong> You have full user management permissions.
        </div>
    @endcan
    
    @cannot('edit-settings')
        <div class="alert alert-info mt-4">
            You don't have permission to edit system settings.
        </div>
    @endcannot
</div>
@endsection

@push('styles')
<link rel="stylesheet" href="{{ asset('css/admin-dashboard.css') }}">
<style>
    .admin-dashboard {
        padding: 20px;
    }
    .page-title {
        margin-bottom: 30px;
        color: #333;
    }
    .quick-actions {
        background: #f8f9fa;
        padding: 20px;
        border-radius: 8px;
    }
</style>
@endpush

@push('scripts')
<script src="{{ asset('js/chart.js') }}"></script>
<script>
    document.addEventListener('DOMContentLoaded', function() {
        console.log('Admin dashboard loaded');
        // Initialize dashboard charts
        initDashboardCharts();
    });
    
    function initDashboardCharts() {
        // Chart initialization code here
    }
</script>
@endpush

@section('modals')
    @include('admin.modals.confirm-delete')
    @include('admin.modals.export-data')
@endsection