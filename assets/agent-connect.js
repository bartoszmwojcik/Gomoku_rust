// "How to connect an agent": a collapsed card on the creator's waiting screen. The commands and files are built here
// from the MCP address of the server; no credential of any kind is ever part of them.
(() => {
  const { gt } = window.GT;

  const CLIENTS = [
    { id: "claude-code", label: "Claude Code", kind: "command", intro: "runCommandAgent", copy: "copyCommand", check: "claudeCheck" },
    { id: "codex", label: "Codex", kind: "command", intro: "runCommand", copy: "copyCommand", check: "codexCheck" },
    { id: "gemini-cli", label: "Gemini CLI", kind: "command", intro: "runCommand", copy: "copyCommand", check: "geminiCheck", note: "geminiNote" },
    { id: "copilot-cli", label: "GitHub Copilot CLI", kind: "command", intro: "runCommand", copy: "copyCommand", check: "copilotCliCheck" },
    { id: "copilot-vscode", label: "GitHub Copilot (VS Code)", kind: "config", intro: "vscodeIntro", copy: "copyConfig", check: "vscodeCheck" },
    { id: "cursor", label: "Cursor", kind: "config", intro: "cursorIntro", copy: "copyConfig", check: "cursorCheck" },
  ];

  const snippetOf = (client, url) => {
    switch (client.id) {
      case "claude-code": return `claude mcp add --transport http gomoku ${url}`;
      case "codex": return `codex mcp add gomoku --url ${url}`;
      case "gemini-cli": return `gemini mcp add --transport http gomoku ${url}`;
      case "copilot-cli": return `copilot mcp add --transport http gomoku ${url}`;
      case "copilot-vscode": return JSON.stringify({ servers: { gomoku: { type: "http", url } } }, null, 2);
      default: return JSON.stringify({ mcpServers: { gomoku: { url } } }, null, 2);
    }
  };

  /** The card; `setStatus` prints a line in the shared status area (role=status) below the cards. */
  const agentConnectCard = (h, { mcpUrl, code, setStatus }) => {
    let client = CLIENTS[0];
    const body = h("div", null);

    const copyBlock = async (text, block, okKey) => {
      try { await navigator.clipboard.writeText(text); setStatus(gt(okKey)); }
      catch {
        const range = document.createRange();
        range.selectNodeContents(block);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        setStatus(gt("game.room.copy.text-fail"));
      }
    };

    const render = () => {
      const snippet = snippetOf(client, mcpUrl);
      const snippetEl = h("pre", { "data-copy": "snippet", tabindex: "0" }, snippet);
      const codeEl = h("code", { "data-copy": "code", tabindex: "0" }, code);
      const select = h("select", { "data-testid": "agent-client", onchange: (e) => { client = CLIENTS.find((c) => c.id === e.target.value) ?? CLIENTS[0]; setStatus(""); render(); } },
        ...CLIENTS.map((c) => h("option", { value: c.id, selected: c.id === client.id }, c.label)));
      body.replaceChildren(
        h("label", null, gt("game.room.agentClient"), " ", select),
        h("ol", null,
          h("li", null, gt(`game.agentClients.${client.intro}`), snippetEl,
            h("button", { type: "button", "data-testid": "agent-copy-snippet", onclick: () => copyBlock(snippet, snippetEl, client.kind === "command" ? "game.room.copy.command-ok" : "game.room.copy.config-ok") }, gt(`game.agentClients.${client.copy}`))),
          h("li", null, gt(`game.agentClients.${client.check}`)),
          h("li", null, gt("game.room.agentCodeIntro", { code }), " ", codeEl, " ",
            h("button", { type: "button", "data-testid": "agent-copy-code", onclick: () => copyBlock(code, codeEl, "game.room.copy.code-ok") }, gt("game.room.copyCode")))),
        h("dl", null, h("dt", null, gt("game.room.serverAddress")), h("dd", null, h("code", { "data-testid": "agent-mcp-url" }, mcpUrl))),
        h("p", { "data-testid": "agent-note" }, gt("game.room.agentNote"), client.note ? [" ", gt(`game.agentClients.${client.note}`)] : null));
    };
    render();
    return h("details", { class: "card", "data-testid": "agent-card" }, h("summary", null, gt("game.room.agentSummary")), body);
  };

  window.Gomoku = { ...window.Gomoku, agentConnectCard };
})();
