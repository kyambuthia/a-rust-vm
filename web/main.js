const wasmPath = "../api/wasm";
const i32Min = -2147483648;
const i32Max = 2147483647;

const terminalWindow = document.querySelector("#terminal-window");
const terminalOutput = document.querySelector("#terminal-output");
const terminalForm = document.querySelector("#terminal-form");
const terminalInput = document.querySelector("#terminal-input");
const expandButton = document.querySelector("#expand-terminal");
const runtimeBadge = document.querySelector("#runtime-badge");
const runtimeLabel = document.querySelector("#runtime-label");
const gatewayBadge = document.querySelector("#gateway-badge");
const gatewayLabel = document.querySelector("#gateway-label");
const railGatewayCopy = document.querySelector("#rail-gateway-copy");
const fileInput = document.querySelector("#file-input");
const emptyState = document.querySelector("#empty-state");
const railFilesList = document.querySelector("#rail-files-list");
const railFilesEmpty = document.querySelector("#rail-files-empty");
const railJobsList = document.querySelector("#rail-jobs-list");
const railJobsEmpty = document.querySelector("#rail-jobs-empty");
const quickHelp = document.querySelector("#quick-help");
const quickFiles = document.querySelector("#quick-files");
const quickJobs = document.querySelector("#quick-jobs");
const quickUpload = document.querySelector("#quick-upload");
const quickClear = document.querySelector("#quick-clear");
const railFilesRefresh = document.querySelector("#rail-files-refresh");
const railJobsRefresh = document.querySelector("#rail-jobs-refresh");
const copyInstall = document.querySelector("#copy-install");

const maxUploadBytes = 1024 * 1024;

const state = {
  program: null,
  agentBusy: false,
  history: [],
  historyIndex: -1,
  draft: "",
};

function setGatewayState(tone, label) {
  if (!gatewayBadge || !gatewayLabel) return;
  gatewayBadge.dataset.state = tone;
  gatewayLabel.textContent = label;
  if (railGatewayCopy) {
    railGatewayCopy.textContent = label === "model gateway online"
      ? "Model gateway online — agent tools and file helpers are available. VM tools always work."
      : label === "model gateway unavailable"
        ? "Model gateway unavailable; VM tools still work. Retry /ask later or use local VM commands."
        : label;
    railGatewayCopy.dataset.tone = tone === "ready" ? "ok" : tone === "error" || tone === "unavailable" ? "error" : "";
  }
}

function syncEmptyState() {
  if (!emptyState) return;
  const hasOutput = terminalOutput.children.length > 0;
  emptyState.classList.toggle("hidden", hasOutput);
}

function writeLine(text, kind = "muted") {
  const line = document.createElement("div");
  const labelMap = {
    command: "cmd",
    result: "out",
    muted: "info",
    tool: "tool",
    error: "error",
  };
  line.className = `terminal-line ${kind}`;
  line.dataset.kind = labelMap[kind] ?? kind;
  line.setAttribute("aria-label", `${labelMap[kind] ?? kind}: ${text}`);
  line.textContent = text;
  terminalOutput.append(line);
  terminalOutput.scrollTop = terminalOutput.scrollHeight;
  syncEmptyState();
}

function printWelcome() {
  writeLine("a/rvm v0.1.0 · Run /help for commands", "muted");
}

async function printSystemInfo() {
  try {
    const response = await fetch("../api/v1/system");
    if (!response.ok) throw new Error(`status ${response.status}`);
    const info = await response.json();
    if (info.model_gateway?.configured) {
      setGatewayState("ready", "model gateway online");
    } else {
      setGatewayState("unavailable", "model gateway unavailable");
    }
  } catch (error) {
    writeLine(`[platform warning] system API unavailable: ${error.message}`, "error");
    writeLine("[gateway] model gateway unavailable; VM tools still work", "muted");
    setGatewayState("unavailable", "model gateway unavailable");
  }
}

function renderFiles(files) {
  if (!railFilesList || !railFilesEmpty) return;
  railFilesList.replaceChildren();
  if (!files.length) {
    railFilesEmpty.classList.remove("hidden");
    return;
  }
  railFilesEmpty.classList.add("hidden");
  for (const file of files.slice(0, 8)) {
    const item = document.createElement("li");
    const name = document.createElement("strong");
    name.textContent = file.path;
    const meta = document.createElement("span");
    meta.textContent = `${file.bytes} bytes`;
    item.append(name, meta);
    railFilesList.append(item);
  }
}

function renderJobs(jobs) {
  if (!railJobsList || !railJobsEmpty) return;
  railJobsList.replaceChildren();
  if (!jobs.length) {
    railJobsEmpty.classList.remove("hidden");
    return;
  }
  railJobsEmpty.classList.add("hidden");
  for (const job of jobs.slice(0, 6)) {
    const item = document.createElement("li");
    item.dataset.state = job.state;
    const title = document.createElement("strong");
    title.textContent = `job ${job.id} · ${job.state}`;
    const detail = document.createElement("span");
    detail.textContent = job.output_path ?? job.error ?? job.input_path ?? job.executor;
    item.append(title, detail);
    railJobsList.append(item);
  }
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
      renderFiles([]);
      return;
    }
    for (const file of payload.files) {
      writeLine(`[guest] ${file.path} · ${file.bytes} bytes`, "muted");
    }
    renderFiles(payload.files);
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
    await listJobs();
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
    await listJobs();
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
      renderJobs([]);
      return;
    }
    for (const job of payload.jobs) {
      const detail = job.output_path ?? job.error ?? job.input_path;
      writeLine(`[job ${job.id}] ${job.state} · ${job.executor} · ${detail}`, job.state === "failed" ? "error" : "muted");
    }
    renderJobs(payload.jobs);
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
      writeLine(`[tool] ${event.name}`, "tool");
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
  if (program.instructions) {
    return program.instructions.map(instruction => instruction.text);
  }
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
    "-9": "program failed validation or exceeds the instruction limit",
  };

  return messages[code] ?? `VM error (${code})`;
}

function loadAssembly(source, instance) {
  const opcodeByName = { ADD: 1, SUB: 2, MUL: 3, DIV: 4, HALT: 5 };
  const lines = source.split(/;|\n/).map(line => line.trim()).filter(Boolean);
  const instructions = [];
  for (const line of lines) {
    const fields = line.split(/\s+/);
    const name = fields[0].toUpperCase();
    if (name === "PUSH" && fields.length === 2) {
      const operand = Number(fields[1]);
      if (!Number.isInteger(operand) || operand < i32Min || operand > i32Max) {
        writeLine(`[error] PUSH operand must be an i32: ${fields[1]}`, "error");
        return false;
      }
      instructions.push({ opcode: 0, operand, text: `PUSH ${operand}` });
    } else if (Object.hasOwn(opcodeByName, name) && fields.length === 1) {
      instructions.push({ opcode: opcodeByName[name], operand: 0, text: name });
    } else {
      writeLine(`[error] invalid assembly instruction: ${line}`, "error");
      return false;
    }
  }

  instance.exports.debug_program_begin();
  for (const instruction of instructions) {
    const status = instance.exports.debug_program_push(instruction.opcode, instruction.operand);
    if (status !== 0) {
      writeLine(`[error] ${debugErrorMessage(status)}`, "error");
      return false;
    }
  }
  const status = instance.exports.debug_program_finish();
  if (status !== 0) {
    writeLine(`[error] ${debugErrorMessage(status)}`, "error");
    return false;
  }
  state.program = { instructions, label: "assembly program" };
  writeLine(`[loaded] validated assembly · ${instructions.length} instructions`, "result");
  return true;
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

  const label = state.program.label ?? `${state.program.left} ${state.program.symbol} ${state.program.right}`;
  writeLine(`[run] ${label}`, "command");

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
    writeLine(`[result] ${formatStack(instance).replace(/[\[\]]/g, "")}`, "result");
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
    writeLine("  /asm <instructions>  load ';'-separated assembly");
    writeLine("  /disassemble         inspect loaded bytecode");
    writeLine("  /step                execute one instruction");
    writeLine("  /run                 run the loaded program");
    writeLine("  /stack               inspect the current stack");
    writeLine("  /reset               reset without unloading");
    writeLine("  /clear               clear terminal output");
    writeLine("examples: /load 8 * 5 · /asm PUSH 6; PUSH 7; MUL; HALT");
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
    syncEmptyState();
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

  if (normalized === "/asm" || normalized === "asm") {
    writeLine("[error] usage: /asm PUSH 6; PUSH 7; MUL; HALT", "error");
    return;
  }

  if (normalized.startsWith("/asm ") || normalized.startsWith("asm ")) {
    const prefixLength = normalized.startsWith("/asm ") ? 5 : 4;
    loadAssembly(command.slice(prefixLength), instance);
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

function pushHistory(value) {
  if (!value.trim()) return;
  if (state.history[state.history.length - 1] === value) return;
  state.history.push(value);
  if (state.history.length > 80) state.history.shift();
}

function submitCommand(instance, command) {
  if (state.agentBusy) {
    return;
  }
  const trimmedCommand = command.trim();

  if (!trimmedCommand) {
    return;
  }

  pushHistory(trimmedCommand);
  state.historyIndex = -1;
  state.draft = "";
  writeLine(`› ${trimmedCommand}`, "command");
  executeCommand(trimmedCommand, instance);
}

function insertCommand(text) {
  terminalInput.value = text;
  terminalInput.focus();
}

terminalInput.addEventListener("keydown", event => {
  if (event.key === "Escape") {
    event.preventDefault();
    state.historyIndex = -1;
    terminalInput.value = "";
    return;
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    if (state.history.length === 0) return;
    if (state.historyIndex === -1) {
      state.draft = terminalInput.value;
      state.historyIndex = state.history.length - 1;
    } else if (state.historyIndex > 0) {
      state.historyIndex -= 1;
    }
    terminalInput.value = state.history[state.historyIndex] ?? "";
    requestAnimationFrame(() => terminalInput.setSelectionRange(terminalInput.value.length, terminalInput.value.length));
  } else if (event.key === "ArrowDown") {
    event.preventDefault();
    if (state.historyIndex === -1) return;
    if (state.historyIndex === state.history.length - 1) {
      state.historyIndex = -1;
      terminalInput.value = state.draft;
    } else {
      state.historyIndex += 1;
      terminalInput.value = state.history[state.historyIndex] ?? "";
    }
    requestAnimationFrame(() => terminalInput.setSelectionRange(terminalInput.value.length, terminalInput.value.length));
  }
});

quickHelp?.addEventListener("click", () => {
  insertCommand("/help");
});
quickFiles?.addEventListener("click", () => {
  insertCommand("/files");
  if (terminalInput.value.trim().toLowerCase() === "/files") {
    const form = terminalForm;
    form.requestSubmit();
  }
});
quickJobs?.addEventListener("click", () => {
  insertCommand("/jobs");
  if (terminalInput.value.trim().toLowerCase() === "/jobs") {
    terminalForm.requestSubmit();
  }
});
quickUpload?.addEventListener("click", () => fileInput.click());
quickClear?.addEventListener("click", () => {
  insertCommand("/clear");
  terminalForm.requestSubmit();
  terminalInput.focus();
});
railFilesRefresh?.addEventListener("click", () => {
  void listGuestFiles();
  terminalInput.focus();
});
railJobsRefresh?.addEventListener("click", () => {
  void listJobs();
  terminalInput.focus();
});

copyInstall?.addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText("git clone https://github.com/kyambuthia/a-rust-vm.git");
    copyInstall.textContent = "copied";
    window.setTimeout(() => { copyInstall.textContent = "install"; }, 1600);
  } catch {
    copyInstall.textContent = "copy unavailable";
    window.setTimeout(() => { copyInstall.textContent = "install"; }, 1600);
  }
});

for (const chip of document.querySelectorAll(".chip[data-insert]")) {
  chip.addEventListener("click", () => {
    const value = chip.getAttribute("data-insert") ?? "";
    insertCommand(value);
  });
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
  await printSystemInfo();
  terminalInput.focus();

  terminalForm.addEventListener("submit", event => {
    event.preventDefault();
    submitCommand(instance, terminalInput.value);
    terminalInput.value = "";
  });
} catch (error) {
  runtimeBadge.dataset.state = "error";
  runtimeLabel.textContent = "runtime unavailable";
  setGatewayState("unavailable", "model gateway unavailable");
  writeLine(`[error] ${error.message}`, "error");
}

expandButton?.addEventListener("click", () => {
  const expanded = terminalWindow.classList.toggle("expanded");
  expandButton.setAttribute("aria-expanded", String(expanded));
  expandButton.textContent = expanded ? "collapse ↗" : "expand ↗";
});
