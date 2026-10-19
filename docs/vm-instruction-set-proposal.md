# A/RVM instruction set expansion proposal

## Purpose and current contract

A/RVM currently executes a 16-instruction, signed `i32` stack machine:
constants, checked arithmetic, stack manipulation, comparison, jumps, and
`HALT`. `Program::new` checks branch targets and computes one operand-stack
depth per reachable instruction. `Vm::step` leaves the stack and instruction
pointer unchanged on failure. Direct execution has a step limit and the VM
has a stack limit. The assembly parser and browser debugger also name or
number these instructions.

The next instruction work should make small source programs practical while
keeping that validation and stepping behavior. The agent roadmap already
assigns general-purpose commands to isolated Linux and WASI runners. The
integer VM serves deterministic validation and tracing.

## What other implementations teach us

| Source | Relevant design | Consequence for A/RVM |
| --- | --- | --- |
| [Crafting Interpreters: Local Variables](https://craftinginterpreters.com/local-variables.html) and [Calls and Functions](https://craftinginterpreters.com/calls-and-functions.html) | Locals use indexed slots; each invocation has a frame with its own return location and local window. | Add indexed locals and direct calls before dynamic values or closures. Specify frame bounds and return stack effects. |
| [Lua `lopcodes.h`](https://github.com/lua/lua/blob/master/lopcodes.h) | A mature VM gives instructions explicit operand formats and documents their effects. | Maintain one opcode specification covering operands, stack effects, traps, and validation; do not let the Rust enum, text parser, and browser mapping drift apart. |
| [WebAssembly validation algorithm](https://webassembly.github.io/spec/core/appendix/algorithm.html) | Validation tracks operand-stack types, control flow, and local initialization. | Validate each function and reject reads of locals that are not initialized on every incoming path. |
| [wasm3 interpreter notes](https://github.com/wasm3/wasm3/blob/main/docs/Interpreter.md) | Its author describes a separate translation and dispatch strategy, plus measured speed and space tradeoffs. | Keep the simple interpreter first; benchmark before adding fused instructions or a different dispatch loop. |
| [Wren embedding configuration](https://wren.io/embedding/configuring-the-vm.html) | Host functions and output are explicit callbacks configured by the embedder. | If A/RVM ever adds imports, resolve them through the runner's capabilities, not ambient VM opcodes. |

These references guide the order and contracts of A/RVM's smaller VM.

## Proposed order

### 0. Freeze semantics before adding opcodes

Write a compact instruction table and executable conformance cases for
operands, stack effect, result, trap, and step cost. Preserve the existing
`i32` behavior, including checked overflow, division by zero, zero-based
branch targets, and `HALT` returning the top value. Document an assembly and
bytecode version boundary before expanding the browser's numeric opcode ABI.

### 1. Add locals, then direct functions

Introduce a versioned program format with function metadata: entry, end,
arity, and local count. The initial new instructions are:

| Instruction | Effect |
| --- | --- |
| `LOCAL_GET slot` | Push the initialized `i32` in the current frame's slot. |
| `LOCAL_SET slot` | Pop one `i32` into a slot in the current frame. |
| `CALL function_id` | Consume the function's declared number of arguments and eventually produce one result. |
| `RETURN` | Return exactly one `i32` to the caller. |

Use explicit frames with a return address and indexed local slots. Copy call
arguments into the callee's parameter slots. Keep temporary operands separate
from local slots, so `LOCAL_SET` has an unambiguous stack effect. Calls are by
validated function ID; jumps may only target instructions in the same function.
Only the entry function may `HALT`; called functions must return. A runtime
frame-depth limit, a total live-local-slot limit, and the existing value-stack
and instruction limits remain in force, including through recursion. Initialize
parameter slots from call arguments. All other slots begin uninitialized;
definite-assignment validation rejects a read on any path before a write.

Validate each function's control-flow graph independently. Stack height must
agree at joins, operands cannot underflow, and every reachable function exit
must have a valid `RETURN` or entry `HALT`. A `CALL` has the static stack effect
`1 - arity`; the callee is checked against its own entry stack and return
contract. Recursive call graphs can be valid because runtime limits still
bound their execution. Before executing a fallible instruction, check frame,
stack, slot, and arithmetic limits so an error leaves the complete VM state
unchanged, including frames and locals.

The first useful source-language subset can then compile `let`, assignment,
`if`, `while`, direct function calls, and `return` to this bytecode. A compiler
can lower `>`, `<=`, `>=`, and short-circuit Boolean expressions to existing
comparisons and branches; those do not initially need separate opcodes.

### 2. Add data only for a measured VM workload

If a benchmark needs VM-native data, define a typed `Value` model and bounded
allocation before adding strings, bytes, arrays, or indexing. Specify equality,
encoding, memory accounting, snapshots, and deterministic iteration order at
the same time. A constant pool can reduce repeated literals once those values
exist. `MOD`, bitwise operations, or wider integers can be added independently
when an actual program needs them, with explicit overflow and division rules.

Filesystem, process, network, time, randomness, and model calls belong behind
the runner capability boundary. A future imported function instruction would
need a declared signature, capability check, deterministic replay rule, bounded
output, and a clear distinction between a rejected call and a VM trap.

## Verification and release gates

1. Preserve all existing assembly programs and VM traces as the legacy format.
   Add parser cases and stable disassembly snapshots for new syntax.
2. Test each new instruction's normal path and failure atomicity. Include
   invalid slots, wrong arity, cross-function jumps, uninitialized locals,
   inconsistent stack joins, recursion limits, and resource exhaustion.
3. Compare a small source program's result and trace with a reference evaluator
   for generated inputs. Run fuzz or generated-input tests against the parser
   and validator before accepting untrusted programs.
4. Benchmark loop and call workloads before any dispatch optimization. Record
   validation time, execution steps, wall time, peak value stack, and frames.
5. Update the CLI parser, agent VM tools, browser opcode mapping, trace format,
   debugger display, and any snapshot representation as one compatibility
   review. Keep older programs runnable or provide an explicit migration.

**First implementation slice:** publish the opcode table and program-version
contract, then add `LOCAL_GET` and `LOCAL_SET` with their validator and focused
tests. Direct calls follow in a separate change after local-slot behavior is
stable.
