const wasmPath = "../target/wasm32-unknown-unknown/debug/a_rust_vm.wasm";
const i32Min = -2147483648;
const i32Max = 2147483647;

const terminalWindow = document.querySelector("#terminal-window");
const terminalOutput = document.querySelector("#terminal-output");
const terminalForm = document.querySelector("#terminal-form");
const terminalInput = document.querySelector("#terminal-input");
const expandButton = document.querySelector("#expand-terminal");
const runtimeBadge = document.querySelector("#runtime-badge");
const runtimeLabel = document.querySelector("#runtime-label");
const fileInput = document.querySelector("#file-input");

const maxUploadBytes = 1024 * 1024;

const state = {
  program: null,
  agentBusy: false,
};

function writeLine(text, kind = "muted") {
  const line = document.createElement("div");
  line.className = `terminal-line ${kind}`;
  line.textContent = text;
  terminalOutput.append(line);
  terminalOutput.scrollTop = terminalOutput.scrollHeight;
}

function printWelcome() {
  writeLine("a/rvm v0.1.0 · Run /help for commands");
  writeLine("[ready] wasm runtime online", "result");
  writeLine("[hint] ask anything · upload files · /load 2 + 3", "muted");
}

async function uploadFiles(files) {
  for (const file of files) {
    if (file.size > maxUploadBytes) {
      writeLine(`[upload error] ${file.name} exceeds the 1 MiB limit`, "error");
      continue;
    }

    try {
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
      const response = await fetch("../api/upload", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ name: file.name, bytes }),
      });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) {
        throw new Error(payload.error ?? `upload failed: ${response.status}`);
      }
      writeLine(`[uploaded] ${payload.path} · ${payload.bytes} bytes`, "result");
      await listGuestFiles();
    } catch (error) {
      writeLine(`[upload error] ${file.name}: ${error.message}`, "error");
    }
  }
}

async function listGuestFiles() {
  try {
    const response = await fetch("../api/files");
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(payload.error ?? `file listing failed: ${response.status}`);
    }
    if (!payload.files?.length) {
      writeLine("[guest] no uploaded files", "muted");
      return;
    }
    for (const file of payload.files) {
      writeLine(`[guest] ${file.path} · ${file.bytes} bytes`, "muted");
    }
  } catch (error) {
    writeLine(`[guest files error] ${error.message}`, "error");
  }
}

async function tabulateFile(value) {
  const file = value.trim();
  if (!file) {
    writeLine("[error] usage: /tabulate <uploaded filename>", "error");
    return;
  }
  const path = file.startsWith("/") ? file : `/workspace/uploads/${file}`;
  try {
    const response = await fetch("../api/tabulate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ path }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(payload.error ?? `tabulation failed: ${response.status}`);
    }
    const columns = payload.table.columns
      .map(column => `${column.name}: ${column.non_empty}/${payload.table.rows} present, ${column.numeric} numeric`)
      .join(" · ");
    writeLine(`[job ${payload.job.id}] tabulated ${payload.table.source_path}`, "result");
    writeLine(`[table] ${payload.table.rows} rows · ${payload.table.columns.length} columns · ${payload.table.delimiter.toUpperCase()}`, "result");
    writeLine(`[columns] ${columns || "no columns"}`, "muted");
    writeLine(`[output] ${payload.table.output_path}`, "muted");
  } catch (error) {
    writeLine(`[tabulate error] ${error.message}`, "error");
  }
}

async function inspectPdf(value) {
  const file = value.trim();
  if (!file) {
    writeLine("[error] usage: /pdf <uploaded filename>", "error");
    return;
  }
  const path = file.startsWith("/") ? file : `/workspace/uploads/${file}`;
  try {
    const response = await fetch("../api/pdf/inspect", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ path }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(payload.error ?? `PDF inspection failed: ${response.status}`);
    }
    writeLine(`[job ${payload.job.id}] inspected ${payload.pdf.source_path}`, "result");
    writeLine(`[pdf] version ${payload.pdf.version} · ${payload.pdf.bytes} bytes · ${payload.pdf.page_objects_estimate} page objects estimated`, "result");
    writeLine(`[output] ${payload.pdf.output_path}`, "muted");
    writeLine("[pdf] text extraction requires a dedicated parser sandbox and is not enabled yet", "muted");
  } catch (error) {
    writeLine(`[pdf error] ${error.message}`, "error");
  }
}

async function listJobs() {
  try {
    const response = await fetch("../api/jobs");
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(payload.error ?? `job listing failed: ${response.status}`);
    }
    if (!payload.jobs?.length) {
      writeLine("[jobs] no jobs", "muted");
      return;
    }
    for (const job of payload.jobs) {
      const detail = job.output_path ?? job.error ?? job.input_path;
      writeLine(`[job ${job.id}] ${job.state} · ${job.executor} · ${detail}`, job.state === "failed" ? "error" : "muted");
    }
  } catch (error) {
    writeLine(`[jobs error] ${error.message}`, "error");
  }
}

fileInput.addEventListener("change", () => {
  const files = Array.from(fileInput.files ?? []);
  fileInput.value = "";
  void uploadFiles(files);
});

function printAgentEvent(event) {
  switch (event.type) {
    case "assistant_text":
      writeLine(event.content, "result");
      break;
    case "assistant_delta":
      writeLine(event.content, "result");
      break;
    case "tool_call":
      writeLine(`[tool] ${event.name}`, "command");
      break;
    case "tool_result":
      writeLine(`[tool result] ${event.content}`, event.is_error ? "error" : "muted");
      break;
    case "permission_requested":
      writeLine(`[permission] ${event.description}`, "error");
      break;
    case "error":
      writeLine(`[error] ${event.message}`, "error");
      break;
    default:
      break;
  }
}

async function askAgent(prompt) {
  state.agentBusy = true;
  terminalInput.disabled = true;
  writeLine("[agent] thinking...", "muted");
  try {
    const response = await fetch("../api/agent", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ prompt }),
    });
    if (!response.ok) {
      const payload = await response.json().catch(() => ({}));
      throw new Error(payload.error ?? `agent request failed: ${response.status}`);
    }

    if (!response.body) {
      throw new Error("agent response did not provide a stream");
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let pending = "";
    while (true) {
      const { value, done } = await reader.read();
      pending += decoder.decode(value ?? new Uint8Array(), { stream: !done });
      const lines = pending.split("\n");
      pending = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) {
          continue;
        }
        const event = JSON.parse(line);
        printAgentEvent(event);
        if (event.type === "permission_requested") {
          await answerApproval(event);
        }
      }
      if (done) {
        break;
      }
    }
    if (pending.trim()) {
      printAgentEvent(JSON.parse(pending));
    }
  } catch (error) {
    writeLine(`[agent error] ${error.message}`, "error");
  } finally {
    state.agentBusy = false;
    terminalInput.disabled = false;
    terminalInput.focus();
  }
}

async function answerApproval(request) {
  const allowed = window.confirm(`Allow ${request.description}?`);
  const response = await fetch("../api/approval", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      id: request.id,
      decision: allowed ? "allow" : "deny",
    }),
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new Error(payload.error ?? `approval request failed: ${response.status}`);
  }
}

function parseExpression(command) {
  const expression = command.replace(/^(?:\/?run|\/?load)\s+/i, "").trim();
  const match = expression.match(/^(-?\d+)\s*(\+|−|\*|×|\/|÷|-)\s*(-?\d+)$/);

  if (!match) {
    return null;
  }

  const [, leftText, symbol, rightText] = match;
  const left = Number(leftText);
  const right = Number(rightText);
  const operationBySymbol = {
    "+": { code: 0, label: "ADD" },
    "−": { code: 1, label: "SUB" },
    "-": { code: 1, label: "SUB" },
    "*": { code: 2, label: "MUL" },
    "×": { code: 2, label: "MUL" },
    "/": { code: 3, label: "DIV" },
    "÷": { code: 3, label: "DIV" },
  };

  return { left, right, operation: operationBySymbol[symbol], symbol };
}

function formatStack(instance) {
  const length = instance.exports.debug_stack_len();

  if (length < 0) {
    return "[]";
  }

  const values = Array.from({ length }, (_, index) => instance.exports.debug_stack_at(index));
  return `[${values.join(", ")}]`;
}

function instructionList(program) {
  return [
    `PUSH ${program.left}`,
    `PUSH ${program.right}`,
    program.operation.label,
    "HALT",
  ];
}

function printProgram(program) {
  instructionList(program).forEach((instruction, index) => {
    writeLine(`${String(index).padStart(2, "0")}  ${instruction}`);
  });
}

function debugErrorMessage(code) {
  const messages = {
    "-1": "no program is loaded",
    "-2": "program ended without HALT",
    "-3": "stack underflow",
    "-4": "division by zero",
    "-5": "integer overflow",
    "-6": "HALT reached with an empty stack",
    "-7": "the VM is already halted",
    "-8": "unknown operation",
  };

  return messages[code] ?? `VM error (${code})`;
}

function validateProgram(program) {
  if (!Number.isInteger(program.left) || !Number.isInteger(program.right) || program.left < i32Min || program.left > i32Max || program.right < i32Min || program.right > i32Max) {
    return "values must be whole numbers within the i32 range";
  }

  if (program.operation.code === 3 && program.right === 0) {
    return "division by zero is not permitted";
  }

  return null;
}

function loadProgram(program, instance) {
  const validationError = validateProgram(program);

  if (validationError) {
    writeLine(`[error] ${validationError}`, "error");
    return false;
  }

  const result = instance.exports.debug_load_binary(program.left, program.right, program.operation.code);

  if (result !== 0) {
    writeLine(`[error] ${debugErrorMessage(result)}`, "error");
    return false;
  }

  state.program = program;
  writeLine(`[loaded] ${program.left} ${program.symbol} ${program.right} · 4 instructions`, "result");
  return true;
}

function runLoadedProgram(instance) {
  if (!state.program) {
    writeLine("[error] no program is loaded. Try /load 8 * 5", "error");
    return;
  }

  const resetResult = instance.exports.debug_reset();

  if (resetResult !== 0) {
    writeLine(`[error] ${debugErrorMessage(resetResult)}`, "error");
    return;
  }

  writeLine(`[run] ${state.program.left} ${state.program.symbol} ${state.program.right}`, "command");

  const instructions = instructionList(state.program);
  let status = 0;
  let steps = 0;

  while (status === 0 && steps < instructions.length) {
    status = instance.exports.debug_step();
    const stack = formatStack(instance);
    const instruction = instructions[steps] ?? "UNKNOWN";

    if (status < 0) {
      writeLine(`[error] ${debugErrorMessage(status)}`, "error");
      return;
    }

    writeLine(`  ${String(steps).padStart(2, "0")}  ${instruction.padEnd(8)} stack: ${stack}`);
    steps += 1;
  }

  if (status === 1) {
    writeLine(`[result]  ${state.program.left} ${state.program.symbol} ${state.program.right} = ${formatStack(instance).replace(/[\[\]]/g, "")}`, "result");
  }
}

function stepProgram(instance) {
  if (!state.program) {
    writeLine("[error] no program is loaded. Try /load 8 * 5", "error");
    return;
  }

  const pointer = instance.exports.debug_instruction_pointer();
  const instruction = instructionList(state.program)[pointer] ?? "UNKNOWN";
  const status = instance.exports.debug_step();

  if (status < 0) {
    writeLine(`[error] ${debugErrorMessage(status)}`, "error");
    return;
  }

  writeLine(`[step] ip ${pointer} · ${instruction}`, "command");
  writeLine(`[stack] ${formatStack(instance)}`, "result");

  if (status === 1) {
    writeLine("[halt] program complete", "result");
  }
}

function executeCommand(rawCommand, instance) {
  const command = rawCommand.trim();
  const normalized = command.toLowerCase();

  if (normalized === "/help" || normalized === "help") {
    writeLine("commands:", "result");
    writeLine("  /ask <prompt>         ask the server-side LLM agent");
    writeLine("  /files                list files in the guest VM");
    writeLine("  /tabulate <file>      summarize an uploaded CSV or TSV");
    writeLine("  /pdf <file>           validate an uploaded PDF safely");
    writeLine("  /jobs                 list data-processing jobs");
    writeLine("  /load <a> <op> <b>  load a program");
    writeLine("  /disassemble         inspect loaded bytecode");
    writeLine("  /step                execute one instruction");
    writeLine("  /run                 run the loaded program");
    writeLine("  /stack               inspect the current stack");
    writeLine("  /reset               reset without unloading");
    writeLine("  /clear               clear terminal output");
    writeLine("examples: /load 2 + 3 · /load 8 * 5");
    return;
  }

  if (normalized === "/status" || normalized === "status") {
    const loaded = state.program ? "loaded" : "empty";
    const pointer = instance.exports.debug_instruction_pointer();
    writeLine(`[status] wasm online · program: ${loaded} · ip: ${pointer}`, "result");
    writeLine(`[status] stack: ${formatStack(instance)}`);
    return;
  }

  if (normalized === "/files" || normalized === "files") {
    void listGuestFiles();
    return;
  }

  if (normalized === "/jobs" || normalized === "jobs") {
    void listJobs();
    return;
  }

  if (normalized === "/tabulate" || normalized === "tabulate") {
    void tabulateFile("");
    return;
  }

  if (normalized.startsWith("/tabulate ") || normalized.startsWith("tabulate ")) {
    const prefixLength = normalized.startsWith("/tabulate ") ? 11 : 10;
    void tabulateFile(command.slice(prefixLength));
    return;
  }

  if (normalized === "/pdf" || normalized === "pdf") {
    void inspectPdf("");
    return;
  }

  if (normalized.startsWith("/pdf ") || normalized.startsWith("pdf ")) {
    const prefixLength = normalized.startsWith("/pdf ") ? 5 : 4;
    void inspectPdf(command.slice(prefixLength));
    return;
  }

  if (normalized === "/clear" || normalized === "clear") {
    terminalOutput.replaceChildren();
    return;
  }

  if (normalized === "/stack" || normalized === "stack") {
    writeLine(`[stack] ${formatStack(instance)}`, "result");
    return;
  }

  if (normalized === "/reset" || normalized === "reset") {
    const resetResult = instance.exports.debug_reset();
    if (resetResult !== 0) {
      writeLine(`[error] ${debugErrorMessage(resetResult)}`, "error");
      return;
    }
    writeLine("[reset] instruction pointer: 0 · stack: []", "result");
    return;
  }

  if (normalized === "/disassemble" || normalized === "disassemble") {
    if (!state.program) {
      writeLine("[error] no program is loaded. Try /load 8 * 5", "error");
      return;
    }
    printProgram(state.program);
    return;
  }

  if (normalized === "/step" || normalized === "step") {
    stepProgram(instance);
    return;
  }

  if (normalized === "/run" || normalized === "run") {
    runLoadedProgram(instance);
    return;
  }

  if (normalized === "/ask" || normalized === "ask") {
    writeLine("[error] usage: /ask <prompt>", "error");
    return;
  }

  if (normalized.startsWith("/ask ") || normalized.startsWith("ask ")) {
    const prefixLength = normalized.startsWith("/ask ") ? 5 : 4;
    const prompt = command.slice(prefixLength).trim();
    if (prompt) {
      void askAgent(prompt);
    } else {
      writeLine("[error] usage: /ask <prompt>", "error");
    }
    return;
  }

  const parsed = parseExpression(command);

  if (!parsed) {
    if (command.startsWith("/")) {
      writeLine("[error] unknown slash command. Use plain text to ask the agent, or try /help", "error");
    } else {
      void askAgent(command);
    }
    return;
  }

  const hasLoaded = loadProgram(parsed, instance);
  const isLoadCommand = /^(?:\/?load)\s+/i.test(command);

  if (hasLoaded && !isLoadCommand) {
    runLoadedProgram(instance);
  }
}

function submitCommand(instance, command) {
  if (state.agentBusy) {
    return;
  }
  const trimmedCommand = command.trim();

  if (!trimmedCommand) {
    return;
  }

  writeLine(`› ${trimmedCommand}`, "command");
  executeCommand(trimmedCommand, instance);
}

try {
  const response = await fetch(wasmPath);

  if (!response.ok) {
    throw new Error(`Could not load runtime: ${response.status}`);
  }

  const bytes = await response.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  runtimeBadge.dataset.state = "ready";
  runtimeLabel.textContent = "wasm online";
  terminalInput.disabled = false;
  printWelcome();
  terminalInput.focus();

  terminalForm.addEventListener("submit", event => {
    event.preventDefault();
    submitCommand(instance, terminalInput.value);
    terminalInput.value = "";
  });
} catch (error) {
  runtimeBadge.dataset.state = "error";
  runtimeLabel.textContent = "runtime unavailable";
  writeLine(`[error] ${error.message}`, "error");
}

expandButton.addEventListener("click", () => {
  const expanded = terminalWindow.classList.toggle("expanded");
  expandButton.setAttribute("aria-expanded", String(expanded));
  expandButton.textContent = expanded ? "collapse ↗" : "expand ↗";
});
