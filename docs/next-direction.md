Decision: start roadmap milestone M1 ("Wire the extensibility that already
exists") with slice M1.1, an `arvm` configuration file.

Rationale: M0 is done. CI checks every push, failed turns keep their context,
and `agent.rs` and `main.rs` are split into modules. MCP servers and default
permission rules are still only reachable through library code or repeated
`--rule` flags. A configuration file is the shared input that M1.2 (MCP tools
in `arvm` runs) and M1.4 (subagent budgets) both need.

Plan (one small slice):

1. A `config` library module that parses one configuration file:

   ```json
   {
     "rules": ["deny run_command:rm"],
     "mcpServers": {
       "files": { "command": "mcp-files", "args": ["--root", "."] }
     }
   }
   ```

   Unknown fields, invalid rules, and invalid server entries are errors, so a
   typo never silently drops a rule.
2. Two scopes: the user config at `<config dir>/config.json` (the directory
   honours `A_RVM_CONFIG_DIR` and `XDG_CONFIG_HOME`) and the workspace config
   at `.arvm/config.json` in the working directory. A missing file is empty.
3. The workspace config can only tighten permissions. It may declare `ask`
   and `deny` rules; an `allow` rule there is an error, because a cloned
   repository must not be able to approve its own commands.
4. Rule precedence, first match wins: `--rule` flags, workspace config, user
   config, then rules saved with `arvm permission add`.
5. `arvm config show [--json]` prints both paths, whether each file exists,
   and the rules and MCP servers each declares.

Next slices in M1: register configured MCP tools in `agent`/`ask` runs behind
the approval flow (workspace-declared servers need explicit user approval
before they start), add `arvm mcp list|tools`, then keep one server process
per session.

Explicitly out of this slice: starting MCP servers, subagent budgets, model
selection in the config file, a `--config` flag, and any change to the
browser host.
