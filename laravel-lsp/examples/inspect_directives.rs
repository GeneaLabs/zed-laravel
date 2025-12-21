/// Quick program to inspect directive nodes in Blade templates
use std::fs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read the test file
    let content = fs::read_to_string("test_directives.blade.php")?;

    // Parse it
    let mut parser = tree_sitter::Parser::new();

    // Get Blade language (simplified - would normally use our parser module)
    extern "C" {
        fn tree_sitter_blade() -> *const tree_sitter::ffi::TSLanguage;
    }

    let language = unsafe { tree_sitter::Language::from_raw(tree_sitter_blade()) };
    parser.set_language(&language)?;

    let tree = parser.parse(&content, None).unwrap();
    let root = tree.root_node();

    println!("=== Blade Directive Inspection ===\n");

    // Walk the tree and find directive nodes
    walk_tree(root, &content, 0);

    Ok(())
}

fn walk_tree(node: tree_sitter::Node, source: &str, depth: usize) {
    let indent = "  ".repeat(depth);
    let kind = node.kind();

    // Print directive-related nodes AND their parents/siblings
    if kind.contains("directive") || kind.contains("parameter") {
        let text = node.utf8_text(source.as_bytes()).unwrap_or("<error>");
        let text_preview = if text.len() > 50 {
            format!("{}...", &text[..50])
        } else {
            text.to_string()
        };

        println!("{}[{}] '{}' ({}:{} to {}:{})",
            indent,
            kind,
            text_preview.replace('\n', "\\n"),
            node.start_position().row,
            node.start_position().column,
            node.end_position().row,
            node.end_position().column
        );

        // Show parent for context
        if let Some(parent) = node.parent() {
            let parent_text = parent.utf8_text(source.as_bytes()).unwrap_or("<error>");
            let parent_preview = if parent_text.len() > 50 {
                format!("{}...", &parent_text[..50])
            } else {
                parent_text.to_string()
            };
            println!("{}  parent: [{}] '{}'",
                indent,
                parent.kind(),
                parent_preview.replace('\n', "\\n")
            );
        }
    }

    // Recurse to children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree(child, source, depth + 1);
    }
}
