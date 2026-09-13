// VS Code client for the NLL language server (`nlink-lab lsp`).
//
// The server is the same binary users already have on their PATH, so
// there is nothing to bundle: point `nll.serverPath` at it if it lives
// somewhere unusual.

const { workspace } = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client;

function serverOptions() {
  const command = workspace.getConfiguration('nll').get('serverPath', 'nlink-lab');
  const run = { command, args: ['lsp'], transport: TransportKind.stdio };
  return { run, debug: run };
}

function activate(context) {
  client = new LanguageClient(
    'nll',
    'NLL Language Server',
    serverOptions(),
    {
      documentSelector: [{ scheme: 'file', language: 'nll' }],
      // The server resolves `import` against the document's directory and
      // reads imported modules from disk, so it wants to know when one
      // changes on disk even if it is not open in the editor.
      synchronize: { fileEvents: workspace.createFileSystemWatcher('**/*.nll') },
    },
  );
  context.subscriptions.push(client);
  client.start();
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
