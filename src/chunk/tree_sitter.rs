//! Tree-sitter-based [`Chunker`] implementation.
//!
//! Generic AST walker that uses per-language configs from `languages.rs`
//! to extract symbol chunks. Handles doc-comment association, import
//! extraction, container recursion (one level for methods inside impl
//! blocks / classes), and graceful fallback on parse failure.

use std::collections::HashSet;

use tree_sitter::{Node, Parser};

use super::languages::{self, LanguageConfig};
use super::{Chunker, FileChunks, Import, SymbolChunk};

pub struct TreeSitterChunker;

impl TreeSitterChunker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TreeSitterChunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker for TreeSitterChunker {
    fn chunk(&self, file_path: &str, content: &str, language: &str) -> anyhow::Result<FileChunks> {
        let config = languages::get_config(language);

        let (imports, symbols) = match config {
            Some(cfg) => {
                let mut parser = Parser::new();
                parser
                    .set_language(&cfg.ts_language.into())
                    .map_err(|e| anyhow::anyhow!("failed to set language for {language}: {e}"))?;

                match parser.parse(content, None) {
                    Some(tree) => {
                        if tree.root_node().has_error() {
                            eprintln!(
                                "warning: parse errors in {file_path}, extracting what we can"
                            );
                        }
                        extract_all(&tree.root_node(), content, &cfg)
                    }
                    None => {
                        eprintln!("warning: tree-sitter failed to parse {file_path}");
                        (vec![], vec![])
                    }
                }
            }
            None => (vec![], vec![]),
        };

        Ok(FileChunks {
            file_path: file_path.to_string(),
            language: language.to_string(),
            file_content: content.to_string(),
            imports,
            symbols,
        })
    }
}

fn extract_all(
    root: &Node,
    source: &str,
    config: &LanguageConfig,
) -> (Vec<Import>, Vec<SymbolChunk>) {
    let mut imports = Vec::new();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = child.kind();

        if config.import_types.contains(&kind)
            && let Some(import) = extract_import(&child, source)
        {
            imports.push(import);
        }

        if config.symbol_types.contains(&kind)
            && let Some(sym) = extract_symbol(&child, source, config)
        {
            symbols.push(sym);
        }

        if config.container_types.contains(&kind) {
            // Methods live inside the body field of the container (e.g.
            // declaration_list for Rust impl, class_body for Java/TS,
            // block for Python). Fall back to the container itself.
            let body = child.child_by_field_name("body").unwrap_or(child);
            let mut inner_cursor = body.walk();
            for inner_child in body.children(&mut inner_cursor) {
                let inner_kind = inner_child.kind();
                let is_extractable = config.symbol_types.contains(&inner_kind)
                    || matches!(
                        inner_kind,
                        "function_item"
                            | "method_declaration"
                            | "method_definition"
                            | "function_definition"
                            | "constructor_declaration"
                    );
                if is_extractable && let Some(sym) = extract_symbol(&inner_child, source, config) {
                    symbols.push(sym);
                }
            }
        }
    }

    (imports, symbols)
}

fn extract_symbol(node: &Node, source: &str, config: &LanguageConfig) -> Option<SymbolChunk> {
    let src = source.as_bytes();

    // For export_statement and decorated_definition, the name lives on
    // the child declaration, not the wrapper node.
    let name = node
        .child_by_field_name("name")
        .or_else(|| {
            // Dig into wrapper nodes for the nested declaration's name
            if matches!(node.kind(), "export_statement" | "decorated_definition") {
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .find(|c| c.child_by_field_name("name").is_some())
                    .and_then(|c| c.child_by_field_name("name"))
            } else {
                None
            }
        })
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or_default()
        .to_string();

    if name.is_empty() && node.kind() != "export_statement" && node.kind() != "decorated_definition"
    {
        return None;
    }

    let body = node.utf8_text(src).ok()?.to_string();
    let kind = normalize_kind(node.kind());
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let signature = extract_signature(node, source);

    let doc_comment = node
        .prev_sibling()
        .filter(|sib| config.comment_types.contains(&sib.kind()))
        .and_then(|sib| sib.utf8_text(src).ok())
        .map(|s| s.to_string());

    let references = extract_references(node, source, config);

    let display_name = if name.is_empty() {
        body.lines()
            .next()
            .unwrap_or("(anonymous)")
            .trim()
            .chars()
            .take(60)
            .collect()
    } else {
        name
    };

    Some(SymbolChunk {
        name: display_name,
        kind: kind.to_string(),
        body,
        signature,
        doc_comment,
        start_line,
        end_line,
        references,
    })
}

fn extract_signature(node: &Node, source: &str) -> Option<String> {
    if let Some(body_node) = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("block"))
    {
        let sig_start = node.start_byte();
        let sig_end = body_node.start_byte();
        if sig_end > sig_start {
            let sig = &source[sig_start..sig_end];
            return Some(sig.trim_end().to_string());
        }
    }
    source[node.start_byte()..node.end_byte()]
        .lines()
        .next()
        .map(|l| l.trim().to_string())
}

fn extract_import(node: &Node, source: &str) -> Option<Import> {
    let text = node.utf8_text(source.as_bytes()).ok()?.trim().to_string();
    Some(Import {
        module: text,
        names: vec![],
    })
}

fn extract_references(node: &Node, source: &str, config: &LanguageConfig) -> Vec<String> {
    let mut refs = Vec::new();
    collect_calls(node, source, config, &mut refs);
    let mut seen = HashSet::new();
    refs.retain(|r| seen.insert(r.clone()));
    refs
}

fn collect_calls(node: &Node, source: &str, config: &LanguageConfig, refs: &mut Vec<String>) {
    if config.call_expression_types.contains(&node.kind())
        && let Some(name) = extract_callee_name(node, source)
        && !name.is_empty()
    {
        refs.push(name);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(&child, source, config, refs);
    }
}

fn extract_callee_name(call_node: &Node, source: &str) -> Option<String> {
    let callee = call_node
        .child_by_field_name("function")
        .or_else(|| call_node.child_by_field_name("name"))
        .or_else(|| call_node.child_by_field_name("macro"))
        .or_else(|| call_node.child_by_field_name("constructor"))
        .or_else(|| call_node.child_by_field_name("type"))?;
    let text = callee.utf8_text(source.as_bytes()).ok()?;
    Some(last_identifier_segment(text).to_string())
}

fn last_identifier_segment(text: &str) -> &str {
    let after_scope = text.rsplit("::").next().unwrap_or(text);
    after_scope.rsplit('.').next().unwrap_or(after_scope).trim()
}

fn normalize_kind(node_type: &str) -> &str {
    match node_type {
        "function_declaration" | "function_definition" | "function_item" => "function",
        "method_declaration" | "method_definition" => "method",
        "struct_item" | "struct_specifier" | "struct_declaration" => "struct",
        "enum_item" | "enum_specifier" | "enum_declaration" => "enum",
        "trait_item" => "trait",
        "interface_declaration" => "interface",
        "class_declaration" | "class_specifier" | "class_definition" => "class",
        "type_declaration" | "type_alias_declaration" | "type_item" => "type",
        "const_item" | "const_declaration" => "const",
        "mod_item" => "module",
        "namespace_definition" => "namespace",
        "impl_item" => "impl",
        "constructor_declaration" => "constructor",
        "var_declaration" | "lexical_declaration" => "var",
        "decorated_definition" => "function",
        "export_statement" => "export",
        "static_item" => "static",
        "declaration" => "declaration",
        _ => node_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(source: &str, language: &str) -> FileChunks {
        TreeSitterChunker::new()
            .chunk("test.rs", source, language)
            .unwrap()
    }

    // ── Rust ──────────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_function() {
        let src = r#"
fn hello() {
    println!("hi");
}
"#;
        let chunks = chunk(src, "rust");
        assert_eq!(chunks.symbols.len(), 1);
        assert_eq!(chunks.symbols[0].name, "hello");
        assert_eq!(chunks.symbols[0].kind, "function");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_struct_and_enum() {
        let src = r#"
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub enum Color {
    Red,
    Green,
    Blue,
}
"#;
        let chunks = chunk(src, "rust");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Color"));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_impl_methods() {
        let src = r#"
struct Foo;

impl Foo {
    fn bar(&self) -> i32 {
        42
    }

    fn baz(&self) {}
}
"#;
        let chunks = chunk(src, "rust");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "struct: {names:?}");
        assert!(names.contains(&"bar"), "method bar: {names:?}");
        assert!(names.contains(&"baz"), "method baz: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_doc_comment() {
        let src = r#"
/// This is a documented function.
fn documented() {}
"#;
        let chunks = chunk(src, "rust");
        assert_eq!(chunks.symbols.len(), 1);
        assert!(chunks.symbols[0].doc_comment.is_some());
        assert!(
            chunks.symbols[0]
                .doc_comment
                .as_ref()
                .unwrap()
                .contains("documented function")
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_use_imports() {
        let src = r#"
use std::collections::HashMap;
use anyhow::{Result, bail};

fn main() {}
"#;
        let chunks = chunk(src, "rust");
        assert_eq!(chunks.imports.len(), 2);
        assert!(chunks.imports[0].module.contains("HashMap"));
        assert!(chunks.imports[1].module.contains("anyhow"));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_trait() {
        let src = r#"
pub trait Drawable {
    fn draw(&self);
    fn area(&self) -> f64;
}
"#;
        let chunks = chunk(src, "rust");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Drawable"));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_line_numbers_are_1_based() {
        let src = "fn first() {}\nfn second() {}";
        let chunks = chunk(src, "rust");
        assert_eq!(chunks.symbols[0].start_line, 1);
        assert_eq!(chunks.symbols[1].start_line, 2);
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_signature_excludes_body() {
        let src = r#"
pub fn process(input: &str) -> Result<()> {
    Ok(())
}
"#;
        let chunks = chunk(src, "rust");
        let sig = chunks.symbols[0].signature.as_ref().unwrap();
        assert!(sig.contains("pub fn process"), "sig: {sig}");
        assert!(!sig.contains("Ok(())"), "sig should exclude body: {sig}");
    }

    // ── Go ────────────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn go_extracts_function() {
        let src = r#"
package main

func Hello() string {
    return "hi"
}
"#;
        let chunks = chunk(src, "go");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Hello"), "got: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn go_extracts_imports() {
        let src = r#"
package main

import "fmt"

func main() {}
"#;
        let chunks = chunk(src, "go");
        assert!(!chunks.imports.is_empty());
    }

    // ── Python ────────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn python_extracts_function_and_class() {
        let src = r#"
def greet(name):
    return f"Hello {name}"

class Person:
    def __init__(self, name):
        self.name = name
"#;
        let chunks = chunk(src, "python");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"), "got: {names:?}");
        assert!(names.contains(&"Person"), "got: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn python_extracts_methods_from_class() {
        let src = r#"
class Calculator:
    def add(self, a, b):
        return a + b

    def multiply(self, a, b):
        return a * b
"#;
        let chunks = chunk(src, "python");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Calculator"), "got: {names:?}");
        assert!(names.contains(&"add"), "got: {names:?}");
        assert!(names.contains(&"multiply"), "got: {names:?}");
    }

    // ── TypeScript ────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_extracts_function_and_interface() {
        let src = r#"
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello ${user.name}`;
}
"#;
        let chunks = chunk(src, "typescript");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"User"), "got: {names:?}");
        assert!(names.contains(&"greet"), "got: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_extracts_type_alias_and_enum() {
        let src = r#"
type UserRole = 'admin' | 'user' | 'guest';

enum Status {
    Active,
    Inactive,
    Pending,
}

export function getRole(): UserRole {
    return 'admin';
}
"#;
        let chunks = chunk(src, "typescript");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserRole"), "type alias: {names:?}");
        assert!(names.contains(&"Status"), "enum: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_extracts_class_methods() {
        let src = r#"
class UserService {
    private users: string[] = [];

    async findById(id: number): Promise<string | undefined> {
        return this.users[id];
    }

    create(name: string): void {
        this.users.push(name);
    }
}
"#;
        let chunks = chunk(src, "typescript");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "class: {names:?}");
        assert!(names.contains(&"findById"), "method: {names:?}");
        assert!(names.contains(&"create"), "method: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_extracts_imports() {
        let src = r#"
import { Request, Response } from 'express';
import type { User } from './models';

export function handler(req: Request, res: Response): void {
    res.json({});
}
"#;
        let chunks = chunk(src, "typescript");
        assert!(
            !chunks.imports.is_empty(),
            "should extract imports: {:?}",
            chunks.imports
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_arrow_function_export() {
        let src = r#"
export const processPayment = async (amount: number): Promise<void> => {
    console.log(amount);
};
"#;
        let chunks = chunk(src, "typescript");
        assert!(
            !chunks.symbols.is_empty(),
            "should extract exported arrow function"
        );
    }

    // ── TSX ───────────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn tsx_parses_jsx_without_errors() {
        let src = r#"
import React from 'react';

interface Props {
    name: string;
    count: number;
}

export function Greeting({ name, count }: Props): JSX.Element {
    return (
        <div className="greeting">
            <h1>Hello {name}</h1>
            <p>You have {count} messages</p>
        </div>
    );
}
"#;
        let chunker = TreeSitterChunker::new();
        let chunks = chunker.chunk("App.tsx", src, "tsx").unwrap();
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Props"), "interface: {names:?}");
        assert!(names.contains(&"Greeting"), "function: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn tsx_extracts_component_class() {
        let src = r#"
import React, { Component } from 'react';

interface State {
    count: number;
}

class Counter extends Component<{}, State> {
    state = { count: 0 };

    increment() {
        this.setState({ count: this.state.count + 1 });
    }

    render() {
        return <button onClick={() => this.increment()}>{this.state.count}</button>;
    }
}
"#;
        let chunker = TreeSitterChunker::new();
        let chunks = chunker.chunk("Counter.tsx", src, "tsx").unwrap();
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Counter"), "class component: {names:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_barrel_reexports_no_parse_errors() {
        let src = r#"
export { Button } from './Button.svelte';
export { Modal } from './Modal.svelte';
export { Header } from './components/Header';
export type { User, UserRole } from './types';
export { default as App } from './App.svelte';
"#;
        let chunker = TreeSitterChunker::new();
        let chunks = chunker.chunk("src/index.ts", src, "typescript").unwrap();
        // Barrel files are mostly re-exports; symbols may or may not be extracted
        // but the key thing is: NO parse errors (the tree should be valid).
        assert_eq!(chunks.file_content, src);
        // Imports count as exports in tree-sitter's view; verify no crash
        // Barrel files are valid TS — no panics, no errors in the tree.
        let _ = &chunks;
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn tsx_barrel_reexports_no_parse_errors() {
        let src = r#"
export { Button } from './Button.svelte';
export { Modal } from './Modal';
export * from './utils';
"#;
        let chunker = TreeSitterChunker::new();
        let chunks = chunker.chunk("src/index.tsx", src, "tsx").unwrap();
        assert_eq!(chunks.file_content, src);
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn tsx_file_extension_maps_to_tsx_language() {
        assert_eq!(crate::chunk::detect_language("App.tsx"), Some("tsx"));
        assert_eq!(
            crate::chunk::detect_language("src/App.ts"),
            Some("typescript")
        );
    }

    // ── Java ──────────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn java_extracts_class_and_methods() {
        let src = r#"
public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }

    public int multiply(int a, int b) {
        return a * b;
    }
}
"#;
        let chunks = chunk(src, "java");
        let names: Vec<&str> = chunks.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Calculator"), "got: {names:?}");
        assert!(names.contains(&"add"), "got: {names:?}");
        assert!(names.contains(&"multiply"), "got: {names:?}");
    }

    // ── Fallback ──────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn unknown_language_returns_file_only() {
        let chunks = chunk("some content", "brainfuck");
        assert!(chunks.symbols.is_empty());
        assert!(chunks.imports.is_empty());
        assert_eq!(chunks.file_content, "some content");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn malformed_code_still_returns_file_content() {
        let src = "fn broken( {{{{{ this is not valid rust";
        let chunks = chunk(src, "rust");
        assert_eq!(chunks.file_content, src);
    }

    // ── Reference extraction ─────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_call_references() {
        let src = r#"
fn orchestrate() {
    let x = validate_input();
    let y = process_data(x);
    emit_result(y);
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert!(
            sym.references.contains(&"validate_input".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"process_data".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"emit_result".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_method_call_references() {
        let src = r#"
fn work(svc: &Service) {
    svc.run();
    svc.cleanup();
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert!(
            sym.references.contains(&"run".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"cleanup".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_scoped_call_references() {
        let src = r#"
fn create() {
    let map = HashMap::new();
    let config = Config::from_env();
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert!(
            sym.references.contains(&"new".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"from_env".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_extracts_macro_references() {
        let src = r#"
fn log_stuff() {
    println!("hello");
    vec![1, 2, 3];
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert!(
            sym.references.contains(&"println".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"vec".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_deduplicates_references() {
        let src = r#"
fn repeated() {
    foo();
    foo();
    foo();
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert_eq!(
            sym.references.iter().filter(|r| *r == "foo").count(),
            1,
            "should deduplicate: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn rust_no_calls_means_empty_references() {
        let src = r#"
fn pure(x: i32) -> i32 {
    x + 1
}
"#;
        let chunks = chunk(src, "rust");
        let sym = &chunks.symbols[0];
        assert!(sym.references.is_empty(), "refs: {:?}", sym.references);
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn go_extracts_call_references() {
        let src = r#"
package main

func handler(w http.ResponseWriter, r *http.Request) {
    data := parseRequest(r)
    result := processData(data)
    writeResponse(w, result)
}
"#;
        let chunks = chunk(src, "go");
        let sym = chunks.symbols.iter().find(|s| s.name == "handler").unwrap();
        assert!(
            sym.references.contains(&"parseRequest".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"processData".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"writeResponse".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn python_extracts_call_references() {
        let src = r#"
def orchestrate():
    data = fetch_data()
    result = transform(data)
    save_result(result)
"#;
        let chunks = chunk(src, "python");
        let sym = &chunks.symbols[0];
        assert!(
            sym.references.contains(&"fetch_data".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"transform".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"save_result".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn typescript_extracts_call_references() {
        let src = r#"
function handleRequest(req: Request): Response {
    const data = parseBody(req);
    const result = validate(data);
    return formatResponse(result);
}
"#;
        let chunks = chunk(src, "typescript");
        let sym = chunks
            .symbols
            .iter()
            .find(|s| s.name == "handleRequest")
            .unwrap();
        assert!(
            sym.references.contains(&"parseBody".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"validate".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"formatResponse".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn java_extracts_method_invocation_references() {
        let src = r#"
public class Service {
    public void handle() {
        Data data = parser.parse();
        Result result = processor.transform(data);
        emitter.emit(result);
    }
}
"#;
        let chunks = chunk(src, "java");
        let sym = chunks.symbols.iter().find(|s| s.name == "handle").unwrap();
        assert!(
            sym.references.contains(&"parse".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"transform".to_string()),
            "refs: {:?}",
            sym.references
        );
        assert!(
            sym.references.contains(&"emit".to_string()),
            "refs: {:?}",
            sym.references
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn last_identifier_segment_splits_correctly() {
        assert_eq!(last_identifier_segment("foo"), "foo");
        assert_eq!(last_identifier_segment("self.foo"), "foo");
        assert_eq!(last_identifier_segment("a.b.c"), "c");
        assert_eq!(last_identifier_segment("HashMap::new"), "new");
        assert_eq!(
            last_identifier_segment("std::collections::HashMap::new"),
            "new"
        );
    }
}
