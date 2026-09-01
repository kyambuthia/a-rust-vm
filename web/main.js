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
  writeLine("[hint] try: run 2 + 3", "muted");
}

function parseExpression(command) {
  const expression = command.replace(/^run\s+/i, "").trim();
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

function printHelp() {
  writeLine("commands:", "result");
  writeLine("  run <a> <op> <b>   execute bytecode");
  writeLine("  /status             inspect the runtime");
  writeLine("  /clear              clear terminal output");
  writeLine("examples: run 2 + 3 · run 8 * 5 · run 20 / 4");
}

function executeCommand(rawCommand, instance) {
  const command = rawCommand.trim();
  const normalized = command.toLowerCase();

  if (normalized === "/help" || normalized === "help") {
    printHelp();
    return;
  }

  if (normalized === "/status" || normalized === "status") {
    writeLine("[status] wasm online · target wasm32-unknown-unknown", "result");
    writeLine("[status] value stack: i32", "muted");
    return;
  }

  if (normalized === "/clear" || normalized === "clear") {
    terminalOutput.replaceChildren();
    return;
  }

  const parsed = parseExpression(command);

  if (!parsed) {
    writeLine("[error] unknown command. Try /help", "error");
    return;
  }

  const { left, right, operation, symbol } = parsed;

  if (!Number.isInteger(left) || !Number.isInteger(right) || left < i32Min || left > i32Max || right < i32Min || right > i32Max) {
    writeLine("[error] values must be whole numbers within the i32 range", "error");
    return;
  }

  if (operation.code === 3 && right === 0) {
    writeLine("[error] division by zero is not permitted", "error");
    return;
  }

  try {
    const answer = instance.exports.run_binary(left, right, operation.code);
    writeLine(`[program] PUSH ${left} · PUSH ${right} · ${operation.label} · HALT`, "muted");
    writeLine(`[stack]   [${answer}]`, "result");
    writeLine(`[result]  ${left} ${symbol} ${right} = ${answer}`, "result");
  } catch (error) {
    writeLine(`[error] ${error.message}`, "error");
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

  document.querySelectorAll(".command-chip").forEach(button => {
    button.addEventListener("click", () => {
      submitCommand(instance, button.dataset.command);
      terminalInput.focus();
    });
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
