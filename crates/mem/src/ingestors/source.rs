use crate::ingest::{DerivedRecord, FileMeta, IngestInput, IngestPlugin, IngestPluginManifest};
use crate::node::{Node, Symbol};
use anyhow::{Context, Result, bail};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

const MAX_SYMBOL_BODY_BYTES: usize = 4_000;
const TRUNCATION_MARKER: &str = "\n...[body truncated by indexer]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Python,
    Rust,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Language {
    fn from_path(path: &str) -> Option<Self> {
        let ext = Path::new(path).extension()?.to_str()?;
        match ext {
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "js" | "jsx" => Some(Self::JavaScript),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }

    fn tree_sitter_language(self) -> tree_sitter::Language {
        // tree-sitter 0.23+ grammars expose a `LANGUAGE: LanguageFn` constant
        // instead of a `language()` fn; convert with `.into()`.
        match self {
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript | Self::Tsx => "typescript",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Other,
}

impl SymbolKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Other => "other",
        }
    }
}

pub struct SourceIndexPlugin;

impl Default for SourceIndexPlugin {
    fn default() -> Self {
        Self
    }
}

impl IngestPlugin for SourceIndexPlugin {
    fn manifest(&self) -> IngestPluginManifest {
        IngestPluginManifest {
            name: "source-index",
            label: "Source-code symbol indexer",
            media_types: &[
                "text/rust",
                "text/x-python",
                "text/javascript",
                "text/typescript",
            ],
            extensions: &[".rs", ".py", ".js", ".jsx", ".ts", ".tsx"],
        }
    }

    fn accepts(&self, file: &FileMeta) -> bool {
        Language::from_path(&file.path).is_some()
    }

    fn derive(&self, input: &IngestInput) -> Result<Vec<DerivedRecord>> {
        let bytes = input.bytes.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "source-index plugin requires file bytes for {}",
                input.file.path
            )
        })?;

        let Some(language) = Language::from_path(&input.file.path) else {
            return Ok(vec![]);
        };
        let source = std::str::from_utf8(bytes).with_context(|| {
            format!("source-index requires UTF-8 source for {}", input.file.path)
        })?;

        let mut parser = Parser::new();
        let grammar = language.tree_sitter_language();
        parser
            .set_language(&grammar)
            .with_context(|| format!("failed to load grammar for {}", language.as_str()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {}", input.file.path))?;
        let root = tree.root_node();
        if root.has_error() {
            bail!("tree-sitter reported syntax errors in {}", input.file.path);
        }

        let mut symbols = Vec::new();
        walk_top_level(language, root, source, &input.file.path, &mut symbols);

        Ok(symbols
            .into_iter()
            .map(|symbol| DerivedRecord {
                node: Node::Symbol(symbol),
                edges: vec![],
            })
            .collect())
    }
}

fn walk_top_level(
    language: Language,
    node: TsNode<'_>,
    source: &str,
    rel_path: &str,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some((symbol, kind)) = build_symbol(language, child, source, rel_path) {
            let is_container = matches!(
                kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Trait
            ) || child.kind() == "impl_item";
            symbols.push(symbol);
            if is_container {
                walk_inner(language, child, source, rel_path, symbols);
            }
        }
    }
}

fn walk_inner(
    language: Language,
    node: TsNode<'_>,
    source: &str,
    rel_path: &str,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "block" | "declaration_list" | "class_body" => {
                let mut inner = child.walk();
                for inner_child in child.children(&mut inner) {
                    if let Some((mut symbol, kind)) =
                        build_symbol(language, inner_child, source, rel_path)
                    {
                        if matches!(kind, SymbolKind::Function) {
                            symbol.kind = SymbolKind::Method.as_str().to_string();
                        }
                        symbols.push(symbol);
                    }
                }
            }
            _ => {}
        }
    }
}

fn build_symbol(
    language: Language,
    node: TsNode<'_>,
    source: &str,
    rel_path: &str,
) -> Option<(Symbol, SymbolKind)> {
    let kind = classify(language, node.kind())?;
    let name = symbol_name(node, source);
    let raw_body = source.get(node.start_byte()..node.end_byte())?;
    let signature = raw_body.lines().next().unwrap_or("").trim().to_string();
    let body = truncate_body(raw_body, MAX_SYMBOL_BODY_BYTES);

    Some((
        Symbol {
            path: rel_path.to_string(),
            name,
            kind: kind.as_str().to_string(),
            language: language.as_str().to_string(),
            signature,
            body,
            start_line: (node.start_position().row + 1) as u32,
            end_line: (node.end_position().row + 1) as u32,
        },
        kind,
    ))
}

fn symbol_name(node: TsNode<'_>, source: &str) -> String {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("type"))
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .to_string()
}

fn classify(language: Language, node_kind: &str) -> Option<SymbolKind> {
    match (language, node_kind) {
        (Language::Python, "function_definition") => Some(SymbolKind::Function),
        (Language::Python, "class_definition") => Some(SymbolKind::Class),

        (Language::Rust, "function_item") => Some(SymbolKind::Function),
        (Language::Rust, "struct_item") => Some(SymbolKind::Struct),
        (Language::Rust, "enum_item") => Some(SymbolKind::Enum),
        (Language::Rust, "trait_item") => Some(SymbolKind::Trait),
        (Language::Rust, "impl_item") => Some(SymbolKind::Other),

        (Language::JavaScript, "function_declaration") => Some(SymbolKind::Function),
        (Language::JavaScript, "class_declaration") => Some(SymbolKind::Class),
        (Language::JavaScript, "method_definition") => Some(SymbolKind::Method),

        (Language::TypeScript | Language::Tsx, "function_declaration") => {
            Some(SymbolKind::Function)
        }
        (Language::TypeScript | Language::Tsx, "class_declaration") => Some(SymbolKind::Class),
        (Language::TypeScript | Language::Tsx, "method_definition") => Some(SymbolKind::Method),
        (Language::TypeScript | Language::Tsx, "interface_declaration") => Some(SymbolKind::Trait),

        _ => None,
    }
}

fn truncate_body(body: &str, max_bytes: usize) -> String {
    if body.len() <= max_bytes {
        return body.to_string();
    }

    let mut cap = max_bytes;
    while cap > 0 && !body.is_char_boundary(cap) {
        cap -= 1;
    }

    let mut out = body[..cap].to_string();
    out.push_str(TRUNCATION_MARKER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cid::compute;
    use crate::ingest::{FileMeta, IngestInput};
    use std::collections::BTreeMap;

    fn input(path: &str, code: impl Into<Vec<u8>>) -> IngestInput {
        let bytes = code.into();
        let cid = compute(&bytes);
        IngestInput {
            file: FileMeta {
                path: path.to_string(),
                size: bytes.len() as u64,
                media_type: None,
                mtime: None,
            },
            file_ref: cid,
            blob: cid,
            bytes: Some(bytes),
        }
    }

    fn symbols(records: Vec<DerivedRecord>) -> Vec<Symbol> {
        records
            .into_iter()
            .map(|record| match record.node {
                Node::Symbol(symbol) => symbol,
                _ => panic!("not a symbol"),
            })
            .collect()
    }

    fn by_name(symbols: &[Symbol]) -> BTreeMap<String, Symbol> {
        symbols
            .iter()
            .map(|symbol| (symbol.name.clone(), symbol.clone()))
            .collect()
    }

    #[test]
    fn rust_top_level_items_are_classified_with_stable_labels() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
            pub fn helper(x: i32) -> i32 { x + 1 }
            pub struct Cache { field: u32 }
            pub enum Mode { Fast }
            pub trait Storage { fn put(&self); }
        "#;
        let records = plugin.derive(&input("main.rs", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["helper"].kind, "function");
        assert_eq!(by_name["Cache"].kind, "struct");
        assert_eq!(by_name["Mode"].kind, "enum");
        assert_eq!(by_name["Storage"].kind, "trait");
        assert!(symbols.iter().all(|symbol| symbol.language == "rust"));
        Ok(())
    }

    #[test]
    fn python_class_methods_are_indexed_as_methods() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
def authenticate(token):
    return verify(token)

class Authenticator:
    def login(self, user):
        return user.token
"#;
        let records = plugin.derive(&input("auth.py", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["authenticate"].kind, "function");
        assert_eq!(by_name["Authenticator"].kind, "class");
        assert_eq!(by_name["login"].kind, "method");
        assert!(symbols.iter().all(|symbol| symbol.language == "python"));
        Ok(())
    }

    #[test]
    fn javascript_classes_and_methods_are_indexed() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
function save(item) {
    return item.id;
}

class Repository {
    load(id) {
        return id;
    }
}
"#;
        let records = plugin.derive(&input("repo.js", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["save"].kind, "function");
        assert_eq!(by_name["Repository"].kind, "class");
        assert_eq!(by_name["load"].kind, "method");
        assert!(symbols.iter().all(|symbol| symbol.language == "javascript"));
        Ok(())
    }

    #[test]
    fn jsx_extension_parses_jsx_syntax() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
function Shell() {
    return <div>ok</div>;
}

class Panel {
    render() {
        return <span />;
    }
}
"#;
        let records = plugin.derive(&input("view.jsx", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["Shell"].kind, "function");
        assert_eq!(by_name["Panel"].kind, "class");
        assert_eq!(by_name["render"].kind, "method");
        assert!(symbols.iter().all(|symbol| symbol.language == "javascript"));
        Ok(())
    }

    #[test]
    fn typescript_interfaces_are_indexed_as_traits() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
interface Api {
    call(): void;
}

function boot(): void {}

class Runner {
    start(): void {}
}
"#;
        let records = plugin.derive(&input("api.ts", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["Api"].kind, "trait");
        assert_eq!(by_name["boot"].kind, "function");
        assert_eq!(by_name["Runner"].kind, "class");
        assert_eq!(by_name["start"].kind, "method");
        assert!(symbols.iter().all(|symbol| symbol.language == "typescript"));
        Ok(())
    }

    #[test]
    fn tsx_extension_parses_typescript_and_jsx_syntax() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = r#"
interface Widget {
    render(): string;
}

function mount(): void {}

class Screen {
    open(): JSX.Element {
        return <section />;
    }
}
"#;
        let records = plugin.derive(&input("view.tsx", code))?;
        let symbols = symbols(records);
        let by_name = by_name(&symbols);

        assert_eq!(by_name["Widget"].kind, "trait");
        assert_eq!(by_name["mount"].kind, "function");
        assert_eq!(by_name["Screen"].kind, "class");
        assert_eq!(by_name["open"].kind, "method");
        assert!(symbols.iter().all(|symbol| symbol.language == "typescript"));
        Ok(())
    }

    #[test]
    fn syntax_errors_are_reported_as_plugin_errors() {
        let plugin = SourceIndexPlugin;
        let err = plugin
            .derive(&input("broken.py", "def stable(:\n    return 2\n"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("tree-sitter reported syntax errors in broken.py"));
    }

    #[test]
    fn body_truncation_preserves_utf8_boundaries() -> Result<()> {
        let plugin = SourceIndexPlugin;
        let code = format!(
            "def long_body():\n    value = \"x{}\"\n    return value\n",
            "é".repeat(2_500)
        );
        let bytes = code.as_bytes().to_vec();
        let symbols = symbols(plugin.derive(&input("long.py", bytes))?);
        let body = &symbols[0].body;

        assert!(body.contains(TRUNCATION_MARKER));
        assert!(body.is_char_boundary(body.len()));
        Ok(())
    }
}
