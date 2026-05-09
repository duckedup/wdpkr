//! Per-language tree-sitter configurations.
//!
//! Each language defines which node types are symbols, containers,
//! comments, and imports. The walker in `tree_sitter.rs` uses these
//! configs to extract chunks generically.

use tree_sitter_language::LanguageFn;

pub struct LanguageConfig {
    pub ts_language: LanguageFn,
    pub symbol_types: &'static [&'static str],
    pub container_types: &'static [&'static str],
    pub comment_types: &'static [&'static str],
    pub import_types: &'static [&'static str],
}

pub fn get_config(language: &str) -> Option<LanguageConfig> {
    match language {
        "rust" => Some(rust()),
        "go" => Some(go()),
        "typescript" => Some(typescript()),
        "javascript" => Some(javascript()),
        "python" => Some(python()),
        "java" => Some(java()),
        "c" | "cpp" => Some(cpp()),
        "csharp" => Some(csharp()),
        _ => None,
    }
}

fn rust() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_rust::LANGUAGE,
        symbol_types: &[
            "function_item",
            "struct_item",
            "enum_item",
            "trait_item",
            "mod_item",
            "const_item",
            "static_item",
            "type_item",
            "impl_item",
        ],
        container_types: &["impl_item"],
        comment_types: &["line_comment", "block_comment"],
        import_types: &["use_declaration"],
    }
}

fn go() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_go::LANGUAGE,
        symbol_types: &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
            "const_declaration",
            "var_declaration",
        ],
        container_types: &[],
        comment_types: &["comment"],
        import_types: &["import_declaration"],
    }
}

fn typescript() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        symbol_types: &[
            "function_declaration",
            "class_declaration",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "export_statement",
            "method_definition",
        ],
        container_types: &["class_declaration"],
        comment_types: &["comment"],
        import_types: &["import_statement"],
    }
}

fn javascript() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_javascript::LANGUAGE,
        symbol_types: &[
            "function_declaration",
            "class_declaration",
            "export_statement",
            "method_definition",
            "lexical_declaration",
        ],
        container_types: &["class_declaration"],
        comment_types: &["comment"],
        import_types: &["import_statement"],
    }
}

fn python() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_python::LANGUAGE,
        symbol_types: &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
        container_types: &["class_definition"],
        comment_types: &["comment"],
        import_types: &["import_statement", "import_from_statement"],
    }
}

fn java() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_java::LANGUAGE,
        symbol_types: &[
            "class_declaration",
            "method_declaration",
            "interface_declaration",
            "enum_declaration",
            "constructor_declaration",
        ],
        container_types: &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
        ],
        comment_types: &["line_comment", "block_comment"],
        import_types: &["import_declaration"],
    }
}

fn cpp() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_cpp::LANGUAGE,
        symbol_types: &[
            "function_definition",
            "class_specifier",
            "struct_specifier",
            "enum_specifier",
            "namespace_definition",
            "declaration",
        ],
        container_types: &[
            "class_specifier",
            "struct_specifier",
            "namespace_definition",
        ],
        comment_types: &["comment"],
        import_types: &["preproc_include"],
    }
}

fn csharp() -> LanguageConfig {
    LanguageConfig {
        ts_language: tree_sitter_c_sharp::LANGUAGE,
        symbol_types: &[
            "class_declaration",
            "method_declaration",
            "interface_declaration",
            "struct_declaration",
            "enum_declaration",
            "constructor_declaration",
        ],
        container_types: &[
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
        ],
        comment_types: &["comment"],
        import_types: &["using_directive"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_languages_have_configs() {
        for lang in [
            "rust",
            "go",
            "typescript",
            "javascript",
            "python",
            "java",
            "c",
            "cpp",
            "csharp",
        ] {
            assert!(get_config(lang).is_some(), "missing config for {lang}");
        }
    }

    #[test]
    fn unknown_language_returns_none() {
        assert!(get_config("brainfuck").is_none());
        assert!(get_config("svelte").is_none());
    }

    #[test]
    fn rust_config_includes_expected_types() {
        let cfg = get_config("rust").unwrap();
        assert!(cfg.symbol_types.contains(&"function_item"));
        assert!(cfg.symbol_types.contains(&"struct_item"));
        assert!(cfg.symbol_types.contains(&"impl_item"));
        assert!(cfg.container_types.contains(&"impl_item"));
        assert!(cfg.import_types.contains(&"use_declaration"));
    }
}
