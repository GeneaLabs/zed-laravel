#!/bin/bash

# Laravel Zed Extension Path Verification Script
# This script verifies that all necessary files and paths are correctly set up

echo "🔍 Laravel Zed Extension Path Verification"
echo "=========================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Base directory (assuming script is run from project root)
BASE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
echo "📁 Base Directory: $BASE_DIR"
echo ""

# Function to check if file exists
check_file() {
    local file=$1
    local description=$2
    
    if [ -f "$file" ]; then
        echo -e "${GREEN}✅ $description${NC}"
        echo "   Path: $file"
    else
        echo -e "${RED}❌ $description${NC}"
        echo "   Missing: $file"
    fi
}

# Function to check if directory exists
check_dir() {
    local dir=$1
    local description=$2
    
    if [ -d "$dir" ]; then
        echo -e "${GREEN}✅ $description${NC}"
        echo "   Path: $dir"
    else
        echo -e "${RED}❌ $description${NC}"
        echo "   Missing: $dir"
    fi
}

# Function to check if file is executable
check_executable() {
    local file=$1
    local description=$2
    
    if [ -x "$file" ]; then
        echo -e "${GREEN}✅ $description (executable)${NC}"
        echo "   Path: $file"
    elif [ -f "$file" ]; then
        echo -e "${YELLOW}⚠️  $description (exists but not executable)${NC}"
        echo "   Path: $file"
        echo "   Fix with: chmod +x $file"
    else
        echo -e "${RED}❌ $description (missing)${NC}"
        echo "   Missing: $file"
    fi
}

echo "📦 Extension Files"
echo "------------------"
check_file "$BASE_DIR/extension.toml" "Extension manifest"
check_file "$BASE_DIR/Cargo.toml" "Cargo configuration"
check_file "$BASE_DIR/src/lib.rs" "Extension source"
echo ""

echo "🔧 LSP Binary"
echo "-------------"
check_executable "$BASE_DIR/laravel-lsp-binary" "LSP binary (copied)"
check_executable "$BASE_DIR/laravel-lsp/target/release/laravel-lsp" "LSP binary (original)"
echo ""

echo "📁 Test Project Structure"
echo "------------------------"
check_dir "$BASE_DIR/test-project" "Test project root"
check_dir "$BASE_DIR/test-project/app/Http/Controllers" "Controllers directory"
check_dir "$BASE_DIR/test-project/resources/views" "Views directory"
check_dir "$BASE_DIR/test-project/app/Livewire" "Livewire directory"
echo ""

echo "📄 Test Files"
echo "-------------"
check_file "$BASE_DIR/test-project/app/Http/Controllers/TestController.php" "Test controller"
check_file "$BASE_DIR/test-project/resources/views/welcome.blade.php" "Welcome view"
check_file "$BASE_DIR/test-project/resources/views/users/profile.blade.php" "User profile view"
check_file "$BASE_DIR/test-project/resources/views/admin/dashboard/index.blade.php" "Admin dashboard view"
check_file "$BASE_DIR/test-project/resources/views/components/button.blade.php" "Button component"
echo ""

echo "🧪 Testing LSP Binary"
echo "--------------------"
if [ -x "$BASE_DIR/laravel-lsp-binary" ]; then
    echo "Testing LSP response..."
    
    # Send initialize request to LSP
    RESPONSE=$(echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}' | "$BASE_DIR/laravel-lsp-binary" 2>/dev/null | head -2 | tail -1)
    
    if [[ $RESPONSE == *"\"result\""* ]]; then
        echo -e "${GREEN}✅ LSP responds correctly${NC}"
    else
        echo -e "${RED}❌ LSP not responding correctly${NC}"
        echo "   Response: $RESPONSE"
    fi
else
    echo -e "${YELLOW}⚠️  Cannot test LSP - binary not executable${NC}"
fi
echo ""

echo "📊 Summary"
echo "----------"

# Count issues
MISSING_COUNT=$(
    {
        [ ! -f "$BASE_DIR/extension.toml" ] && echo "1"
        [ ! -f "$BASE_DIR/laravel-lsp-binary" ] && echo "1"
        [ ! -d "$BASE_DIR/test-project" ] && echo "1"
    } | wc -l
)

if [ "$MISSING_COUNT" -eq "0" ]; then
    echo -e "${GREEN}✅ All critical files are present!${NC}"
    echo ""
    echo "📝 Next Steps:"
    echo "1. Open Zed"
    echo "2. Run: zed: install dev extension"
    echo "3. Select: $BASE_DIR"
    echo "4. Open: $BASE_DIR/test-project"
    echo "5. Test go-to-definition in TestController.php"
else
    echo -e "${RED}❌ Some files are missing. Please fix the issues above.${NC}"
fi