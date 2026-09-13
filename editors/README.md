# Editor support

Two independent pieces:

- **Syntax highlighting** — a [tree-sitter grammar](tree-sitter-nll/) plus a
  TextMate grammar for [VS Code](vscode-nll/) and a [Zed extension](zed-nll/).
- **Diagnostics and navigation** — `nlink-lab lsp`, a Language Server Protocol
  server over stdio. It reports exactly what `nlink-lab validate` and
  `nlink-lab lint` report (errors, warnings and style hints, each carrying its
  rule id), plus document symbols, go-to-definition, hover and completion, and
  formatting through the same formatter as `nlink-lab fmt`.

See [`docs/cli/lsp.md`](../docs/cli/lsp.md) for what the server provides. The
snippets below are the client side.

## VS Code

The extension in [`vscode-nll/`](vscode-nll/) starts the server itself:

```bash
cd editors/vscode-nll && npm install
# then: F5 in VS Code, or `vsce package` and install the .vsix
```

Set `nll.serverPath` if `nlink-lab` is not on your `PATH`, and
`nll.trace.server` to `verbose` to see the JSON-RPC traffic.

## Neovim (0.11+)

```lua
vim.filetype.add({ extension = { nll = 'nll' } })
vim.lsp.config.nll = {
  cmd = { 'nlink-lab', 'lsp' },
  filetypes = { 'nll' },
  root_markers = { '.git' },
}
vim.lsp.enable('nll')
```

With `nvim-lspconfig` instead, the same `cmd`/`filetypes` pair works through
`vim.lsp.config`.

## Helix

`~/.config/helix/languages.toml`:

```toml
[language-server.nlink-lab]
command = "nlink-lab"
args = ["lsp"]

[[language]]
name = "nll"
scope = "source.nll"
file-types = ["nll"]
roots = [".git"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }
language-servers = ["nlink-lab"]
```

Helix can also use the tree-sitter grammar in [`tree-sitter-nll/`](tree-sitter-nll/)
for highlighting; point `[[grammar]]` at this repository.

## Emacs (eglot)

```elisp
(define-derived-mode nll-mode prog-mode "NLL"
  (setq-local comment-start "# "))
(add-to-list 'auto-mode-alist '("\\.nll\\'" . nll-mode))
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(nll-mode . ("nlink-lab" "lsp"))))
```

## Zed

The extension in [`zed-nll/`](zed-nll/) provides highlighting only. Zed launches
a language server from a compiled WASM extension, not from a declarative
`[language_servers]` entry, so wiring the server there needs a small Rust
extension crate — tracked on
[issue #56](https://git.marcpardo.eu/marcpardo/nlink-lab/issues/56).

## Any other LSP client

Run `nlink-lab lsp`. It speaks LSP 3.17 over stdio, uses UTF-16 positions
(the protocol default), and logs to stderr — stdout carries only JSON-RPC.
