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

const state = {
  program: null,
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
  writeLine("[hint] try: /load 2 + 3 · /step", "muted");
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

  const parsed = parseExpression(command);

  if (!parsed) {
    writeLine("[error] unknown command. Try /help", "error");
    return;
  }

  const hasLoaded = loadProgram(parsed, instance);
  const isLoadCommand = /^(?:\/?load)\s+/i.test(command);

  if (hasLoaded && !isLoadCommand) {
    runLoadedProgram(instance);
  }
}

function submitCommand(instance, command) {
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
