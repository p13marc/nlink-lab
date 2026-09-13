//! `nlink-lab lsp` — Language Server Protocol server for NLL (issue #56).
//!
//! Speaks LSP over stdio, so every editor drives it the same way:
//! `nlink-lab lsp`. [`analyze`] is the diagnostic pipeline, [`locate`]
//! maps validator locations back to source spans, [`position`] converts
//! byte offsets to UTF-16 positions and [`symbols`] answers the
//! navigation requests off the token stream.
//!
//! Note for anyone adding output to this module: stdout **is** the
//! protocol. Diagnostics go through the client, logs go to stderr (see
//! the writer switch in `main.rs`), and nothing here may `println!`.

mod analyze;
mod locate;
mod position;
#[cfg(test)]
mod protocol_tests;
mod symbols;

use std::collections::HashMap;
use std::path::PathBuf;

use analyze::Analysis;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer, LspService, Server, jsonrpc};

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Accepted and ignored: stdio is the only transport. Editors that
    /// always pass `--stdio` work without special-casing.
    #[arg(long)]
    pub stdio: bool,
}

pub async fn run(_ctx: &Ctx, _args: Args) -> nlink_lab::Result<()> {
    serve(tokio::io::stdin(), tokio::io::stdout()).await;
    Ok(())
}

/// Run the server over one pair of streams. Returns when the client
/// disconnects (stdin EOF) or sends `exit`.
async fn serve<I, O>(input: I, output: O)
where
    I: tokio::io::AsyncRead + Unpin,
    O: tokio::io::AsyncWrite,
{
    let (service, socket) = LspService::new(Backend::new);
    Server::new(input, output, socket).serve(service).await;
}

struct Backend {
    client: Client,
    docs: tokio::sync::RwLock<HashMap<Uri, Analysis>>,
    /// First workspace folder from `initialize`. Used as the `import`
    /// base directory for documents that have no path of their own.
    root: tokio::sync::RwLock<Option<PathBuf>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            docs: Default::default(),
            root: Default::default(),
        }
    }

    /// The directory `import` statements resolve against, expressed as a
    /// path *inside* it (only the parent is ever used, and the file is
    /// never read).
    async fn doc_path(&self, uri: &Uri) -> Option<PathBuf> {
        if let Some(p) = uri.to_file_path() {
            return Some(p.into_owned());
        }
        let root = self.root.read().await.clone()?;
        Some(root.join("«buffer».nll"))
    }

    /// Re-analyze a document and publish its diagnostics.
    async fn refresh(&self, uri: Uri, text: String, version: Option<i32>) {
        let path = self.doc_path(&uri).await;
        let analysis = analyze::analyze(&text, path.as_deref());
        let diagnostics = analysis.diagnostics.clone();
        // The guard must not be held across the await below.
        self.docs.write().await.insert(uri.clone(), analysis);
        self.client
            .publish_diagnostics(uri, diagnostics, version)
            .await;
    }

    async fn with_doc<T>(&self, uri: &Uri, f: impl FnOnce(&Analysis) -> T) -> Option<T> {
        self.docs.read().await.get(uri).map(f)
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        let folder = params
            .workspace_folders
            .as_ref()
            .and_then(|f| f.first())
            .and_then(|f| f.uri.to_file_path())
            .map(|p| p.into_owned());
        *self.root.write().await = folder.or_else(|| std::env::current_dir().ok());
        Ok(InitializeResult {
            // Not the LSP `positionEncoding` (which stays at its UTF-16
            // default, matching `position.rs`) — this is the older
            // clangd-style extension, which we do not implement.
            offset_encoding: None,
            server_info: Some(ServerInfo {
                name: "nlink-lab lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                // Full sync: an analysis is a few hundred microseconds on
                // real topologies, so incremental sync buys nothing and
                // costs a patch-application bug surface.
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![":".into(), "$".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "nlink-lab lsp ready")
            .await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.refresh(doc.uri, doc.text, Some(doc.version)).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        // FULL sync: the last change carries the whole document.
        if let Some(change) = params.content_changes.pop() {
            self.refresh(
                params.text_document.uri,
                change.text,
                Some(params.text_document.version),
            )
            .await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        // Imported modules may have changed on disk, so re-analyze even
        // when the client does not send the text back.
        let uri = params.text_document.uri;
        let text = match params.text {
            Some(text) => Some(text),
            None => self.with_doc(&uri, |a| a.text.clone()).await,
        };
        if let Some(text) = text {
            self.refresh(uri, text, None).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.write().await.remove(&uri);
        // Otherwise the editor keeps showing the last diagnostics.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let symbols = self
            .with_doc(&params.text_document.uri, document_symbols)
            .await
            .unwrap_or_default();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let base = self.doc_path(&uri).await;
        let target = self
            .with_doc(&uri, |a| definition_at(a, pos, base.as_deref()))
            .await
            .flatten();
        Ok(target.map(|t| match t {
            Target::Here(range) => GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range,
            }),
            Target::File(other) => GotoDefinitionResponse::Scalar(Location {
                uri: other,
                range: Range::default(),
            }),
        }))
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        Ok(self.with_doc(&uri, |a| hover_at(a, pos)).await.flatten())
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let items = self
            .with_doc(&uri, |a| completions_at(a, pos))
            .await
            .unwrap_or_default();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        Ok(self
            .with_doc(&params.text_document.uri, format_document)
            .await
            .flatten())
    }
}

/// Where a definition lives.
enum Target {
    Here(Range),
    File(Uri),
}

/// `textDocument/documentSymbol`: every declaration in the buffer.
// `DocumentSymbol::deprecated` is itself deprecated, and the struct has
// no non-exhaustive constructor.
#[allow(deprecated)]
fn document_symbols(a: &Analysis) -> Vec<DocumentSymbol> {
    symbols::definitions(&a.tokens)
        .into_iter()
        .map(|def| {
            let kind = match def.kind {
                symbols::DefKind::Lab | symbols::DefKind::Site => SymbolKind::NAMESPACE,
                symbols::DefKind::Node => SymbolKind::CLASS,
                symbols::DefKind::Profile => SymbolKind::INTERFACE,
                symbols::DefKind::Network => SymbolKind::MODULE,
                symbols::DefKind::Pool => SymbolKind::ENUM,
                symbols::DefKind::Let | symbols::DefKind::Param => SymbolKind::VARIABLE,
            };
            let detail = a.topology.as_ref().and_then(|t| {
                let node = t.nodes.get(&def.name)?;
                if !node.profiles.is_empty() {
                    Some(node.profiles.join(", "))
                } else {
                    node.image.clone()
                }
            });
            DocumentSymbol {
                name: def.name,
                detail: detail.or_else(|| Some(def.kind.label().to_string())),
                kind,
                tags: None,
                deprecated: None,
                range: a.index.range(def.full_span),
                selection_range: a.index.range(def.name_span),
                children: None,
            }
        })
        .collect()
}

/// `textDocument/definition`.
fn definition_at(a: &Analysis, pos: Position, base: Option<&std::path::Path>) -> Option<Target> {
    use nlink_lab::parser::nll::lexer::Token;
    let offset = a.index.offset(pos);
    let i = symbols::token_at(&a.tokens, offset)?;
    let defs = symbols::definitions(&a.tokens);
    let find = |kind: symbols::DefKind, name: &str| {
        defs.iter()
            .find(|d| d.kind == kind && d.name == name)
            .map(|d| Target::Here(a.index.range(d.name_span.clone())))
    };

    // `r1:eth0` -> the `node r1` declaration.
    if let Some((node, _iface)) = symbols::endpoint_at(&a.tokens, i)
        && let Some(t) = find(symbols::DefKind::Node, &node)
    {
        return Some(t);
    }
    // `node x : router` -> the `profile router` declaration.
    if symbols::is_profile_ref(&a.tokens, i)
        && let Token::Ident(name) = &a.tokens[i].token
        && let Some(t) = find(symbols::DefKind::Profile, name)
    {
        return Some(t);
    }
    // `import "mod.nll" as m` -> the imported file.
    if let Token::String(s) = &a.tokens[i].token
        && matches!(
            i.checked_sub(1).map(|p| &a.tokens[p].token),
            Some(Token::Import)
        )
        && let Some(dir) = base.and_then(|p| p.parent())
        && let Some(uri) = Uri::from_file_path(dir.join(s))
    {
        return Some(Target::File(uri));
    }
    // `${spine1.eth1}` -> the node it names.
    if let Token::Interp(raw) = &a.tokens[i].token {
        let inner = raw.trim_start_matches("${").trim_end_matches('}');
        let name = inner.split('.').next().unwrap_or(inner);
        if let Some(t) = find(symbols::DefKind::Node, name) {
            return Some(t);
        }
    }
    // Any other identifier that happens to name a declaration: network
    // members, `reach a b`, `impair a -- b`, pool and variable uses.
    if let Token::Ident(name) = &a.tokens[i].token {
        return defs
            .iter()
            .find(|d| &d.name == name && d.name_span != a.tokens[i].span)
            .map(|d| Target::Here(a.index.range(d.name_span.clone())));
    }
    None
}

/// `textDocument/hover`.
fn hover_at(a: &Analysis, pos: Position) -> Option<Hover> {
    let offset = a.index.offset(pos);
    let i = symbols::token_at(&a.tokens, offset)?;
    let word = symbols::word(&a.tokens[i].token)?;
    let range = Some(a.index.range(a.tokens[i].span.clone()));

    if let Some(topo) = &a.topology {
        if let Some(node) = topo.nodes.get(&word) {
            let mut md = format!("**node `{word}`**");
            if !node.profiles.is_empty() {
                md.push_str(&format!(" · profiles: `{}`", node.profiles.join("`, `")));
            }
            if let Some(image) = &node.image {
                md.push_str(&format!(" · image: `{image}`"));
            }
            let addrs = nlink_lab::ipmap::collect_node_addrs(topo);
            if let Some(list) = addrs.get(&word).filter(|l| !l.is_empty()) {
                md.push_str("\n\n| iface | address |\n|---|---|\n");
                for a in list {
                    md.push_str(&format!("| `{}` | `{}` |\n", a.iface, a.cidr));
                }
            }
            if !node.routes.is_empty() {
                md.push_str(&format!("\n{} route(s)\n", node.routes.len()));
            }
            return Some(markdown(md, range));
        }
        if topo.profiles.contains_key(&word) {
            let users: Vec<_> = topo
                .nodes
                .iter()
                .filter(|(_, n)| n.profiles.contains(&word))
                .map(|(name, _)| format!("`{name}`"))
                .collect();
            let md = if users.is_empty() {
                format!("**profile `{word}`** — not used by any node")
            } else {
                format!("**profile `{word}`** — used by {}", users.join(", "))
            };
            return Some(markdown(md, range));
        }
        if let Some(net) = topo.networks.get(&word) {
            let mut md = format!("**network `{word}`**");
            if let Some(subnet) = &net.subnet {
                md.push_str(&format!(" · subnet `{subnet}`"));
            }
            md.push_str(&format!(" · {} member(s)", net.members.len()));
            return Some(markdown(md, range));
        }
    }

    // Keyword help — also the fallback while the buffer does not parse.
    let help = symbols::STATEMENT_KEYWORDS
        .iter()
        .find(|(kw, _)| *kw == word)
        .map(|(_, help)| (*help).to_string())?;
    Some(markdown(help, range))
}

fn markdown(value: String, range: Option<Range>) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range,
    }
}

/// `textDocument/completion`.
fn completions_at(a: &Analysis, pos: Position) -> Vec<CompletionItem> {
    use nlink_lab::parser::nll::lexer::Token;
    let offset = a.index.offset(pos);
    // The token *before* the cursor decides the context.
    let prev = a
        .tokens
        .iter()
        .rposition(|t| t.span.end <= offset)
        .filter(|_| !a.tokens.is_empty());

    let item = |label: String, kind: CompletionItemKind, doc: Option<String>| CompletionItem {
        label,
        kind: Some(kind),
        documentation: doc.map(Documentation::String),
        ..Default::default()
    };
    let names = |kind: symbols::DefKind| -> Vec<String> {
        symbols::definitions(&a.tokens)
            .into_iter()
            .filter(|d| d.kind == kind)
            .map(|d| d.name)
            .collect()
    };

    if let Some(p) = prev {
        // `r1:` -> that node's interfaces.
        if matches!(a.tokens[p].token, Token::Colon)
            && let Some(Token::Ident(node)) = p.checked_sub(1).map(|k| &a.tokens[k].token)
        {
            if let Some(n) = a.topology.as_ref().and_then(|t| t.nodes.get(node)) {
                let mut out: Vec<_> = n
                    .interfaces
                    .keys()
                    .map(|i| item(i.clone(), CompletionItemKind::FIELD, None))
                    .collect();
                if out.is_empty() {
                    out.push(item("eth0".into(), CompletionItemKind::FIELD, None));
                }
                return out;
            }
            // Unknown node after a colon inside a `node` head: profiles.
            return names(symbols::DefKind::Profile)
                .into_iter()
                .map(|n| item(n, CompletionItemKind::INTERFACE, None))
                .collect();
        }
        // Inside a block: that block's properties plus the names in scope.
        // A cursor right after the opening brace is inside *that* block,
        // so look from one past it (the lexer collapses the newlines that
        // would otherwise follow).
        let inside = if matches!(a.tokens[p].token, Token::LBrace) {
            p + 1
        } else {
            p
        };
        if let Some(block) = symbols::enclosing_block_kw(&a.tokens, inside) {
            let mut out: Vec<_> = symbols::BLOCK_KEYWORDS
                .iter()
                .find(|(kw, _)| *kw == block)
                .map(|(_, words)| {
                    words
                        .iter()
                        .map(|w| item((*w).to_string(), CompletionItemKind::PROPERTY, None))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for kind in [
                symbols::DefKind::Node,
                symbols::DefKind::Network,
                symbols::DefKind::Pool,
            ] {
                out.extend(
                    names(kind)
                        .into_iter()
                        .map(|n| item(n, CompletionItemKind::VALUE, None)),
                );
            }
            for kind in [symbols::DefKind::Let, symbols::DefKind::Param] {
                out.extend(
                    names(kind)
                        .into_iter()
                        .map(|n| item(format!("${{{n}}}"), CompletionItemKind::VARIABLE, None)),
                );
            }
            return out;
        }
    }

    // Statement position.
    symbols::STATEMENT_KEYWORDS
        .iter()
        .map(|(kw, doc)| {
            item(
                (*kw).to_string(),
                CompletionItemKind::KEYWORD,
                Some((*doc).to_string()),
            )
        })
        .collect()
}

/// `textDocument/formatting` — the same token-level formatter
/// `nlink-lab fmt` uses, so the editor and CI cannot disagree.
fn format_document(a: &Analysis) -> Option<Vec<TextEdit>> {
    let formatted = nlink_lab::fmt::format(&a.text).ok()?;
    if formatted == a.text {
        return None;
    }
    Some(vec![TextEdit {
        range: a.index.full_range(),
        new_text: formatted,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = concat!(
        "lab \"t\"\n",
        "profile router { forward ipv4 }\n",
        "node r1 : router\n",
        "node h1\n",
        "network core { subnet 10.9.0.0/24 members [r1:eth1] }\n",
        "link r1:eth0 -- h1:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
        "validate { reach r1 h1 }\n",
    );

    fn doc(src: &str) -> Analysis {
        analyze::analyze(src, None)
    }

    /// Position of the byte offset of the `n`-th occurrence of `needle`.
    fn pos_of(a: &Analysis, needle: &str) -> Position {
        a.index.position(a.text.find(needle).expect(needle))
    }

    #[test]
    fn document_symbols_cover_every_declaration() {
        let a = doc(SRC);
        let syms = document_symbols(&a);
        let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["t", "router", "r1", "h1", "core"]);
        let r1 = syms.iter().find(|s| s.name == "r1").unwrap();
        assert_eq!(r1.kind, SymbolKind::CLASS);
        assert_eq!(r1.detail.as_deref(), Some("router"));
        // The selection range must sit inside the full range.
        assert!(r1.range.start.line <= r1.selection_range.start.line);
    }

    #[test]
    fn definition_of_an_endpoint_jumps_to_the_node() {
        let a = doc(SRC);
        let pos = pos_of(&a, "link r1:eth0");
        let pos = Position::new(pos.line, pos.character + 5); // on `r1`
        let target = definition_at(&a, pos, None).expect("a definition");
        let Target::Here(range) = target else {
            panic!("expected a same-file target")
        };
        // `node r1` is on line 2 (0-based).
        assert_eq!(range.start.line, 2);
    }

    #[test]
    fn definition_of_a_profile_reference_jumps_to_the_profile() {
        let a = doc(SRC);
        let at = a.text.find(": router").unwrap() + 2;
        let target = definition_at(&a, a.index.position(at), None).expect("a definition");
        let Target::Here(range) = target else {
            panic!("expected a same-file target")
        };
        assert_eq!(range.start.line, 1);
    }

    #[test]
    fn definition_of_an_import_points_at_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.nll"), "node m\n").unwrap();
        let src = "lab \"t\"\nimport \"mod.nll\" as m\n";
        let a = analyze::analyze(src, Some(&dir.path().join("buf.nll")));
        let at = src.find("\"mod.nll\"").unwrap() + 1;
        let target = definition_at(&a, a.index.position(at), Some(&dir.path().join("buf.nll")))
            .expect("a definition");
        let Target::File(uri) = target else {
            panic!("expected another file")
        };
        assert!(uri.as_str().ends_with("mod.nll"), "{uri:?}");
    }

    #[test]
    fn definition_on_a_keyword_is_none() {
        let a = doc(SRC);
        assert!(definition_at(&a, pos_of(&a, "forward"), None).is_none());
    }

    #[test]
    fn hover_on_a_node_lists_its_addresses() {
        let a = doc(SRC);
        let at = a.text.find("link r1:eth0").unwrap() + 5;
        let h = hover_at(&a, a.index.position(at)).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markdown")
        };
        assert!(md.value.contains("**node `r1`**"), "{}", md.value);
        assert!(md.value.contains("10.0.0.1/24"), "{}", md.value);
        assert!(md.value.contains("router"), "{}", md.value);
    }

    #[test]
    fn hover_on_a_profile_lists_its_users() {
        let a = doc(SRC);
        let at = a.text.find("profile router").unwrap() + 8;
        let h = hover_at(&a, a.index.position(at)).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markdown")
        };
        assert!(md.value.contains("profile `router`"), "{}", md.value);
        assert!(md.value.contains("`r1`"), "{}", md.value);
    }

    #[test]
    fn hover_on_a_keyword_shows_syntax_help() {
        let a = doc(SRC);
        let h = hover_at(&a, pos_of(&a, "network core")).expect("hover");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markdown")
        };
        assert!(md.value.contains("network NAME"), "{}", md.value);
    }

    #[test]
    fn hover_works_on_a_buffer_that_does_not_parse() {
        let a = doc("lab \"t\"\nnode a\nlink a:eth0 -- ghost:eth0\n");
        let h = hover_at(&a, Position::new(1, 0)).expect("hover on `node`");
        let HoverContents::Markup(md) = h.contents else {
            panic!("expected markdown")
        };
        assert!(md.value.contains("node NAME"), "{}", md.value);
    }

    #[test]
    fn completion_at_statement_position_offers_keywords() {
        let a = doc(SRC);
        let items = completions_at(&a, Position::new(3, 0));
        assert!(items.iter().any(|i| i.label == "node"));
        assert!(
            items
                .iter()
                .all(|i| i.kind == Some(CompletionItemKind::KEYWORD))
        );
    }

    #[test]
    fn completion_after_a_node_colon_offers_interfaces() {
        let a = doc(SRC);
        let at = a.text.find("link r1:eth0").unwrap() + 8; // just after the colon
        let items = completions_at(&a, a.index.position(at));
        assert!(
            items.iter().any(|i| i.label == "eth0"),
            "{:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn completion_inside_a_block_offers_properties_and_names() {
        let a = doc(SRC);
        let at = a.text.find("subnet").unwrap();
        let items = completions_at(&a, a.index.position(at + 6));
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"members"), "{labels:?}");
        assert!(labels.contains(&"r1"), "{labels:?}");
    }

    #[test]
    fn completion_works_when_the_buffer_does_not_parse() {
        let a = doc("lab \"t\"\nnode a {\n  \n");
        let items = completions_at(&a, Position::new(2, 2));
        assert!(items.iter().any(|i| i.label == "forward"), "{items:?}");
    }

    #[test]
    fn formatting_returns_one_whole_document_edit() {
        let a = doc("lab \"t\"\nnode   a{forward ipv4}\n");
        let edits = format_document(&a).expect("an edit");
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("node a { forward ipv4 }"));
        assert_eq!(edits[0].range.start, Position::new(0, 0));
    }

    #[test]
    fn formatting_a_clean_document_is_a_no_op() {
        let a = doc(SRC);
        assert!(format_document(&a).is_none());
    }

    #[test]
    fn formatting_a_broken_document_is_a_no_op() {
        let a = doc("lab \"t\"\n@\n");
        assert!(format_document(&a).is_none());
    }
}
