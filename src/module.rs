//! Validated multi-function programs for the next A/RVM execution format.
//!
//! The legacy assembly VM remains available through crate::program. A module
//! is validated as a whole before its instructions can be executed.

use std::{cell::Cell, collections::BTreeMap, fmt, mem::size_of, rc::Rc};

pub const MODULE_VERSION: u16 = 2;
pub const MAX_FUNCTIONS: usize = 1_024;
pub const MAX_MODULE_INSTRUCTIONS: usize = 65_536;
pub const MAX_LOCALS_PER_FUNCTION: usize = 256;
pub const MAX_OPERAND_STACK: usize = 1_024;
pub const MAX_CALL_FRAMES: usize = 128;
pub const MAX_LIVE_SLOTS: usize = 16_384;
pub const MAX_MODULE_STEPS: usize = 1_000_000;
pub const MAX_MODULE_VALIDATION_VISITS: usize = 1_000_000;
pub const MAX_MODULE_CONSTANT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MODULE_CONSTANTS: usize = 65_536;
pub const MAX_MODULE_HEAP_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_VALUE_DEPTH: usize = 64;
// Deterministic logical charges. They cover payloads and conservative object
// overhead, but are not a measurement of the host allocator's resident memory.
const HEAP_OBJECT_CHARGE: usize = size_of::<HeapObject>();
const MAP_ENTRY_CHARGE: usize = size_of::<String>() + size_of::<Value>() + 4 * size_of::<usize>();

/// An owned value at the module boundary. Collections are ordered by key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedValue {
    Null,
    Bool(bool),
    Integer(i64),
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<OwnedValue>),
    Map(BTreeMap<String, OwnedValue>),
}

/// A cheap VM value. Cloning a heap value shares its immutable contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Heap(HeapValue),
}

/// An opaque reference whose allocation stays live while it is held.
#[derive(Debug, Clone)]
pub struct HeapValue(Rc<HeapObject>);

impl PartialEq for HeapValue {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for HeapValue {}

#[derive(Debug)]
struct HeapObject {
    data: HeapData,
    charge: usize,
    depth: usize,
    usage: Rc<Cell<usize>>,
}

impl Drop for HeapObject {
    fn drop(&mut self) {
        self.usage.set(self.usage.get() - self.charge);
    }
}

#[derive(Debug, Clone)]
enum HeapData {
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

fn value_charge(value: &OwnedValue, depth: usize) -> Result<usize, ModuleVmError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ModuleVmError::ValueDepthLimit);
    }
    match value {
        OwnedValue::Null | OwnedValue::Bool(_) | OwnedValue::Integer(_) => Ok(0),
        OwnedValue::Text(text) => HEAP_OBJECT_CHARGE
            .checked_add(text.len())
            .ok_or(ModuleVmError::HeapLimit),
        OwnedValue::Bytes(bytes) => HEAP_OBJECT_CHARGE
            .checked_add(bytes.len())
            .ok_or(ModuleVmError::HeapLimit),
        OwnedValue::List(items) => {
            let mut total = HEAP_OBJECT_CHARGE
                .checked_add(
                    items
                        .len()
                        .checked_mul(size_of::<Value>())
                        .ok_or(ModuleVmError::HeapLimit)?,
                )
                .ok_or(ModuleVmError::HeapLimit)?;
            for item in items {
                total = total
                    .checked_add(value_charge(item, depth + 1)?)
                    .ok_or(ModuleVmError::HeapLimit)?;
            }
            Ok(total)
        }
        OwnedValue::Map(entries) => {
            let mut total = HEAP_OBJECT_CHARGE;
            for (key, item) in entries {
                total = total
                    .checked_add(MAP_ENTRY_CHARGE)
                    .and_then(|charge| charge.checked_add(key.len()))
                    .ok_or(ModuleVmError::HeapLimit)?;
                total = total
                    .checked_add(value_charge(item, depth + 1)?)
                    .ok_or(ModuleVmError::HeapLimit)?;
            }
            Ok(total)
        }
    }
}

/// An instruction in a validated module function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Const(i64),
    ConstValue(usize),
    Input(usize),
    LocalGet(usize),
    LocalTake(usize),
    LocalSet(usize),
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Lt,
    ListNew,
    ListPush,
    ListGet,
    ListLen,
    MapNew,
    MapInsert,
    MapGet,
    Jump(usize),
    JumpIfZero(usize),
    Call(usize),
    Return,
}

/// A function body and its fixed local-slot layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    arity: usize,
    local_count: usize,
    code: Vec<Op>,
}

impl Function {
    pub fn new(arity: usize, local_count: usize, code: Vec<Op>) -> Self {
        Self {
            arity,
            local_count,
            code,
        }
    }

    pub fn arity(&self) -> usize {
        self.arity
    }

    pub fn local_count(&self) -> usize {
        self.local_count
    }

    pub fn code(&self) -> &[Op] {
        &self.code
    }
}

/// A complete module that passed structural and control-flow validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    version: u16,
    functions: Vec<Function>,
    entry: usize,
    constants: Vec<OwnedValue>,
    input_count: usize,
    max_operand_stack: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleError {
    pub function: Option<usize>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl ModuleError {
    fn new(
        function: Option<usize>,
        instruction: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            function,
            instruction,
            message: message.into(),
        }
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.function, self.instruction) {
            (Some(function), Some(instruction)) => {
                write!(
                    formatter,
                    "function {function} instruction {instruction}: {}",
                    self.message
                )
            }
            (Some(function), None) => write!(formatter, "function {function}: {}", self.message),
            _ => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for ModuleError {}

impl Module {
    pub fn new(version: u16, functions: Vec<Function>, entry: usize) -> Result<Self, ModuleError> {
        Self::with_values(version, functions, entry, vec![], 0)
    }

    pub fn with_values(
        version: u16,
        functions: Vec<Function>,
        entry: usize,
        constants: Vec<OwnedValue>,
        input_count: usize,
    ) -> Result<Self, ModuleError> {
        if version != MODULE_VERSION {
            return Err(ModuleError::new(
                None,
                None,
                format!("unsupported module version {version}"),
            ));
        }
        if functions.is_empty() || functions.len() > MAX_FUNCTIONS {
            return Err(ModuleError::new(
                None,
                None,
                format!("function count must be between 1 and {MAX_FUNCTIONS}"),
            ));
        }
        if entry >= functions.len() {
            return Err(ModuleError::new(
                None,
                None,
                format!("entry function {entry} is missing"),
            ));
        }
        if functions[entry].arity != 0 {
            return Err(ModuleError::new(
                Some(entry),
                None,
                "entry function must have no parameters",
            ));
        }
        if input_count > MAX_LOCALS_PER_FUNCTION {
            return Err(ModuleError::new(None, None, "too many inputs"));
        }
        if constants.len() > MAX_MODULE_CONSTANTS {
            return Err(ModuleError::new(None, None, "too many constants"));
        }
        let mut constant_bytes = 0usize;
        for (index, value) in constants.iter().enumerate() {
            let charge = value_charge(value, 0).map_err(|error| {
                ModuleError::new(None, None, format!("constant {index}: {error}"))
            })?;
            constant_bytes = constant_bytes.saturating_add(charge);
            if constant_bytes > MAX_MODULE_CONSTANT_BYTES {
                return Err(ModuleError::new(
                    None,
                    None,
                    "module constant limit exceeded",
                ));
            }
        }
        let mut total_instructions = 0usize;
        for (function_index, function) in functions.iter().enumerate() {
            if function.local_count > MAX_LOCALS_PER_FUNCTION
                || function.arity > function.local_count
            {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    format!(
                        "local count must include parameters and be at most {MAX_LOCALS_PER_FUNCTION}"
                    ),
                ));
            }
            if function.code.is_empty() {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    "function is empty",
                ));
            }
            total_instructions = total_instructions.saturating_add(function.code.len());
            if total_instructions > MAX_MODULE_INSTRUCTIONS {
                return Err(ModuleError::new(
                    None,
                    None,
                    format!("module exceeds {MAX_MODULE_INSTRUCTIONS} instructions"),
                ));
            }
            if !matches!(function.code.last(), Some(Op::Return)) {
                return Err(ModuleError::new(
                    Some(function_index),
                    None,
                    "function must end with RETURN",
                ));
            }
            for (ip, op) in function.code.iter().enumerate() {
                match op {
                    Op::ConstValue(index) if *index >= constants.len() => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("constant {index} is missing"),
                        ));
                    }
                    Op::Input(index) if *index >= input_count => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("input {index} is missing"),
                        ));
                    }
                    Op::LocalGet(slot) | Op::LocalTake(slot) | Op::LocalSet(slot)
                        if *slot >= function.local_count =>
                    {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("local slot {slot} is out of range"),
                        ));
                    }
                    Op::Jump(target) | Op::JumpIfZero(target) if *target >= function.code.len() => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("branch target {target} is out of range"),
                        ));
                    }
                    Op::Call(target) if *target >= functions.len() => {
                        return Err(ModuleError::new(
                            Some(function_index),
                            Some(ip),
                            format!("function {target} is missing"),
                        ));
                    }
                    _ => {}
                }
            }
        }
        let mut validation_visits = MAX_MODULE_VALIDATION_VISITS;
        let max_operand_stack = functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                validate_function(index, function, &functions, &mut validation_visits)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            version,
            functions,
            entry,
            constants,
            input_count,
            max_operand_stack,
        })
    }

    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn entry(&self) -> usize {
        self.entry
    }

    pub fn constants(&self) -> &[OwnedValue] {
        &self.constants
    }

    pub fn input_count(&self) -> usize {
        self.input_count
    }

    pub fn max_operand_stack(&self, function: usize) -> Option<usize> {
        self.max_operand_stack.get(function).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Initialized([u64; 4]);

impl Initialized {
    fn parameters(arity: usize) -> Self {
        let mut result = Self([0; 4]);
        for slot in 0..arity {
            result.mark(slot);
        }
        result
    }

    fn mark(&mut self, slot: usize) {
        self.0[slot / 64] |= 1u64 << (slot % 64);
    }

    fn clear(&mut self, slot: usize) {
        self.0[slot / 64] &= !(1u64 << (slot % 64));
    }

    fn contains(self, slot: usize) -> bool {
        self.0[slot / 64] & (1u64 << (slot % 64)) != 0
    }

    fn intersection(self, other: Self) -> Self {
        let mut result = self;
        for (word, other_word) in result.0.iter_mut().zip(other.0) {
            *word &= other_word;
        }
        result
    }
}

fn validate_function(
    index: usize,
    function: &Function,
    functions: &[Function],
    visits_remaining: &mut usize,
) -> Result<usize, ModuleError> {
    // At a control-flow join, local initialization is the intersection of
    // incoming paths. Revisit a successor if that intersection shrinks.
    let mut states: Vec<Option<(usize, Initialized)>> = vec![None; function.code.len()];
    let mut worklist = vec![(0usize, 0usize, Initialized::parameters(function.arity))];
    let mut max_stack = 0usize;
    while let Some((ip, depth, incoming)) = worklist.pop() {
        if *visits_remaining == 0 {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                "module validation visit limit exceeded",
            ));
        }
        *visits_remaining -= 1;
        let merged = match states[ip] {
            Some((known_depth, known)) => {
                if known_depth != depth {
                    return Err(ModuleError::new(
                        Some(index),
                        Some(ip),
                        format!("stack depth at join differs: {known_depth} and {depth}"),
                    ));
                }
                let merged = known.intersection(incoming);
                if merged == known {
                    continue;
                }
                merged
            }
            None => incoming,
        };
        states[ip] = Some((depth, merged));
        let op = function.code[ip];
        let required = match op {
            Op::Const(_)
            | Op::ConstValue(_)
            | Op::Input(_)
            | Op::LocalGet(_)
            | Op::LocalTake(_)
            | Op::Jump(_)
            | Op::ListNew
            | Op::MapNew => 0,
            Op::LocalSet(_) | Op::JumpIfZero(_) | Op::Return | Op::ListLen => 1,
            Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Eq
            | Op::Lt
            | Op::ListPush
            | Op::ListGet
            | Op::MapGet => 2,
            Op::MapInsert => 3,
            Op::Call(target) => functions[target].arity,
        };
        if depth < required {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("instruction needs {required} operands, has {depth}"),
            ));
        }
        if let Op::LocalGet(slot) | Op::LocalTake(slot) = op
            && !merged.contains(slot)
        {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("local slot {slot} may be uninitialized"),
            ));
        }
        if matches!(op, Op::Return) && depth != 1 {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("RETURN needs exactly one operand, has {depth}"),
            ));
        }
        let next_depth = match op {
            Op::Const(_)
            | Op::ConstValue(_)
            | Op::Input(_)
            | Op::LocalGet(_)
            | Op::LocalTake(_)
            | Op::ListNew
            | Op::MapNew => depth + 1,
            Op::LocalSet(_) | Op::JumpIfZero(_) => depth - 1,
            Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Eq
            | Op::Lt
            | Op::ListPush
            | Op::ListGet
            | Op::MapGet => depth - 1,
            Op::MapInsert => depth - 2,
            Op::ListLen => depth,
            Op::Call(_) => depth - required + 1,
            Op::Jump(_) | Op::Return => depth,
        };
        max_stack = max_stack.max(next_depth);
        if max_stack > MAX_OPERAND_STACK {
            return Err(ModuleError::new(
                Some(index),
                Some(ip),
                format!("operand stack exceeds {MAX_OPERAND_STACK} values"),
            ));
        }
        let mut outgoing = merged;
        if let Op::LocalSet(slot) = op {
            outgoing.mark(slot);
        } else if let Op::LocalTake(slot) = op {
            outgoing.clear(slot);
        }
        match op {
            Op::Return => {}
            Op::Jump(target) => worklist.push((target, next_depth, outgoing)),
            Op::JumpIfZero(target) => {
                worklist.push((target, next_depth, outgoing));
                worklist.push((ip + 1, next_depth, outgoing));
            }
            _ => worklist.push((ip + 1, next_depth, outgoing)),
        }
    }
    Ok(max_stack)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleVmError {
    AlreadyFinished,
    OperandStackUnderflow,
    UninitializedLocal,
    DivisionByZero,
    IntegerOverflow,
    TypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    MissingInput,
    InputCountMismatch,
    MissingKey,
    IndexOutOfBounds,
    InvalidReference,
    ValueDepthLimit,
    HeapLimit,
    OperandStackLimit,
    CallFrameLimit,
    LiveSlotLimit,
    StepLimit,
}

impl fmt::Display for ModuleVmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ModuleVmError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleStep {
    pub function: usize,
    pub instruction_pointer: usize,
    pub instruction: Op,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    function: usize,
    ip: usize,
    locals: Vec<Option<Value>>,
    stack: Vec<Value>,
}

/// An owned debugger view of one call frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSnapshot {
    pub function: usize,
    pub instruction_pointer: usize,
    pub locals: Vec<Option<Value>>,
    pub stack: Vec<Value>,
}

/// A stepwise, bounded executor for a validated module.
#[derive(Debug)]
pub struct ModuleVm<'a> {
    module: &'a Module,
    frames: Vec<Frame>,
    steps: usize,
    result: Option<Value>,
    inputs: Vec<Value>,
    heap_bytes: Rc<Cell<usize>>,
    heap_limit: usize,
}

impl<'a> ModuleVm<'a> {
    pub fn new(module: &'a Module) -> Self {
        let entry = module.entry;
        Self {
            module,
            frames: vec![Frame {
                function: entry,
                ip: 0,
                locals: vec![None; module.functions[entry].local_count],
                stack: Vec::new(),
            }],
            steps: 0,
            result: None,
            inputs: Vec::new(),
            heap_bytes: Rc::new(Cell::new(0)),
            heap_limit: MAX_MODULE_HEAP_BYTES,
        }
    }

    /// Imports inputs into a bounded heap before execution begins.
    pub fn with_inputs(
        module: &'a Module,
        inputs: Vec<OwnedValue>,
        heap_limit: usize,
    ) -> Result<Self, ModuleVmError> {
        if inputs.len() != module.input_count {
            return Err(ModuleVmError::InputCountMismatch);
        }
        if heap_limit > MAX_MODULE_HEAP_BYTES {
            return Err(ModuleVmError::HeapLimit);
        }
        let mut vm = Self::new(module);
        vm.heap_limit = heap_limit;
        let mut charge = 0usize;
        for input in &inputs {
            charge = charge
                .checked_add(value_charge(input, 0)?)
                .ok_or(ModuleVmError::HeapLimit)?;
        }
        vm.reserve_heap(charge)?;
        for input in &inputs {
            let value = vm.import(input);
            vm.inputs.push(value);
        }
        Ok(vm)
    }

    pub fn result(&self) -> Option<Value> {
        self.result.clone()
    }

    /// Guest heap charge for values still reachable from the VM or its callers.
    pub fn heap_bytes(&self) -> usize {
        self.heap_bytes.get()
    }

    pub fn resolve(&self, value: Value) -> Result<OwnedValue, ModuleVmError> {
        self.resolve_at(value, 0)
    }

    fn resolve_at(&self, value: Value, depth: usize) -> Result<OwnedValue, ModuleVmError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(ModuleVmError::ValueDepthLimit);
        }
        match value {
            Value::Null => Ok(OwnedValue::Null),
            Value::Bool(value) => Ok(OwnedValue::Bool(value)),
            Value::Integer(value) => Ok(OwnedValue::Integer(value)),
            Value::Heap(reference) => match &reference.0.data {
                HeapData::Text(text) => Ok(OwnedValue::Text(text.clone())),
                HeapData::Bytes(bytes) => Ok(OwnedValue::Bytes(bytes.clone())),
                HeapData::List(items) => items
                    .iter()
                    .map(|item| self.resolve_at(item.clone(), depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(OwnedValue::List),
                HeapData::Map(entries) => entries
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), self.resolve_at(item.clone(), depth + 1)?)))
                    .collect::<Result<BTreeMap<_, _>, _>>()
                    .map(OwnedValue::Map),
            },
        }
    }

    fn reserve_heap(&self, charge: usize) -> Result<(), ModuleVmError> {
        if self
            .heap_bytes
            .get()
            .checked_add(charge)
            .is_none_or(|total| total > self.heap_limit)
        {
            return Err(ModuleVmError::HeapLimit);
        }
        Ok(())
    }

    fn allocate(&self, data: HeapData, charge: usize, depth: usize) -> Value {
        self.heap_bytes.set(self.heap_bytes.get() + charge);
        Value::Heap(HeapValue(Rc::new(HeapObject {
            data,
            charge,
            depth,
            usage: self.heap_bytes.clone(),
        })))
    }

    fn value_depth(&self, value: &Value) -> usize {
        match value {
            Value::Heap(reference) => reference.0.depth,
            _ => 0,
        }
    }

    // The entire tree is preflighted by the caller. Immutable heap edges point
    // only to earlier objects, so collections cannot form cycles.
    fn import(&mut self, value: &OwnedValue) -> Value {
        match value {
            OwnedValue::Null => Value::Null,
            OwnedValue::Bool(value) => Value::Bool(*value),
            OwnedValue::Integer(value) => Value::Integer(*value),
            OwnedValue::Text(text) => self.allocate(
                HeapData::Text(text.clone()),
                HEAP_OBJECT_CHARGE + text.len(),
                0,
            ),
            OwnedValue::Bytes(bytes) => self.allocate(
                HeapData::Bytes(bytes.clone()),
                HEAP_OBJECT_CHARGE + bytes.len(),
                0,
            ),
            OwnedValue::List(items) => {
                let values: Vec<_> = items.iter().map(|item| self.import(item)).collect();
                let depth = values
                    .iter()
                    .map(|value| self.value_depth(value) + 1)
                    .max()
                    .unwrap_or(0);
                self.allocate(
                    HeapData::List(values),
                    HEAP_OBJECT_CHARGE + items.len() * size_of::<Value>(),
                    depth,
                )
            }
            OwnedValue::Map(entries) => {
                let values: BTreeMap<_, _> = entries
                    .iter()
                    .map(|(key, item)| (key.clone(), self.import(item)))
                    .collect();
                let depth = values
                    .values()
                    .map(|value| self.value_depth(value) + 1)
                    .max()
                    .unwrap_or(0);
                let charge = HEAP_OBJECT_CHARGE
                    + entries
                        .keys()
                        .map(|key| MAP_ENTRY_CHARGE + key.len())
                        .sum::<usize>();
                self.allocate(HeapData::Map(values), charge, depth)
            }
        }
    }

    pub fn steps(&self) -> usize {
        self.steps
    }

    pub fn frame_depth(&self) -> usize {
        self.frames.len()
    }

    pub fn frames(&self) -> Vec<FrameSnapshot> {
        self.frames
            .iter()
            .map(|frame| FrameSnapshot {
                function: frame.function,
                instruction_pointer: frame.ip,
                locals: frame.locals.clone(),
                stack: frame.stack.clone(),
            })
            .collect()
    }

    pub fn run(&mut self) -> Result<Value, ModuleVmError> {
        while self.result.is_none() {
            self.step()?;
        }
        Ok(self.result.clone().expect("result checked above"))
    }

    pub fn step(&mut self) -> Result<ModuleStep, ModuleVmError> {
        if self.result.is_some() {
            return Err(ModuleVmError::AlreadyFinished);
        }
        if self.steps >= MAX_MODULE_STEPS {
            return Err(ModuleVmError::StepLimit);
        }
        let frame_index = self.frames.len() - 1;
        let function = self.frames[frame_index].function;
        let instruction_pointer = self.frames[frame_index].ip;
        let instruction = self.module.functions[function].code[instruction_pointer];
        match instruction {
            Op::Const(value) => {
                self.check_push()?;
                self.frames[frame_index].stack.push(Value::Integer(value));
                self.frames[frame_index].ip += 1;
            }
            Op::ConstValue(index) => {
                self.check_push()?;
                let constant = &self.module.constants[index];
                self.reserve_heap(value_charge(constant, 0)?)?;
                let value = self.import(constant);
                self.frames[frame_index].stack.push(value);
                self.frames[frame_index].ip += 1;
            }
            Op::Input(index) => {
                self.check_push()?;
                let value = self
                    .inputs
                    .get(index)
                    .ok_or(ModuleVmError::MissingInput)?
                    .clone();
                self.frames[frame_index].stack.push(value);
                self.frames[frame_index].ip += 1;
            }
            Op::LocalGet(slot) => {
                self.check_push()?;
                let value = self.frames[frame_index].locals[slot]
                    .clone()
                    .ok_or(ModuleVmError::UninitializedLocal)?;
                self.frames[frame_index].stack.push(value);
                self.frames[frame_index].ip += 1;
            }
            Op::LocalTake(slot) => {
                self.check_push()?;
                let frame = &mut self.frames[frame_index];
                let value = frame.locals[slot]
                    .take()
                    .ok_or(ModuleVmError::UninitializedLocal)?;
                frame.stack.push(value);
                frame.ip += 1;
            }
            Op::LocalSet(slot) => {
                let frame = &mut self.frames[frame_index];
                let value = frame
                    .stack
                    .pop()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?;
                frame.locals[slot] = Some(value);
                frame.ip += 1;
            }
            Op::Add => self.binary_int(|a, b| a.checked_add(b))?,
            Op::Sub => self.binary_int(|a, b| a.checked_sub(b))?,
            Op::Mul => self.binary_int(|a, b| a.checked_mul(b))?,
            Op::Div => {
                let stack = &self.frames[frame_index].stack;
                if stack.len() < 2 {
                    return Err(ModuleVmError::OperandStackUnderflow);
                }
                if self.integer(&stack[stack.len() - 1])? == 0 {
                    return Err(ModuleVmError::DivisionByZero);
                }
                self.binary_int(|a, b| a.checked_div(b))?;
            }
            Op::Eq => {
                let [lhs, rhs] = self.operands::<2>()?;
                if matches!(lhs, Value::Heap(_)) || matches!(rhs, Value::Heap(_)) {
                    return Err(ModuleVmError::TypeMismatch {
                        expected: "scalar",
                        actual: "heap value",
                    });
                }
                self.replace_operands(2, Value::Bool(lhs == rhs));
            }
            Op::Lt => {
                let [lhs, rhs] = self.operands::<2>()?;
                let value = Value::Bool(self.integer(lhs)? < self.integer(rhs)?);
                self.replace_operands(2, value);
            }
            Op::ListNew => self.new_collection(HeapData::List(Vec::new()))?,
            Op::MapNew => self.new_collection(HeapData::Map(BTreeMap::new()))?,
            Op::ListPush => self.list_push()?,
            Op::ListGet => self.list_get()?,
            Op::ListLen => self.list_len()?,
            Op::MapInsert => self.map_insert()?,
            Op::MapGet => self.map_get()?,
            Op::Jump(target) => self.frames[frame_index].ip = target,
            Op::JumpIfZero(target) => {
                let condition = self.frames[frame_index]
                    .stack
                    .last()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?;
                let is_zero = match condition {
                    Value::Integer(value) => *value == 0,
                    Value::Bool(value) => !*value,
                    _ => {
                        return Err(ModuleVmError::TypeMismatch {
                            expected: "integer or bool",
                            actual: self.kind(condition),
                        });
                    }
                };
                let frame = &mut self.frames[frame_index];
                frame.stack.pop();
                frame.ip = if is_zero { target } else { frame.ip + 1 };
            }
            Op::Call(target) => {
                let callee = &self.module.functions[target];
                let arity = callee.arity;
                let caller = &self.frames[frame_index];
                let stack_len = caller.stack.len();
                if stack_len < arity {
                    return Err(ModuleVmError::OperandStackUnderflow);
                }
                if self.frames.len() >= MAX_CALL_FRAMES {
                    return Err(ModuleVmError::CallFrameLimit);
                }
                let live = self.live_slots();
                if live - arity + callee.local_count > MAX_LIVE_SLOTS {
                    return Err(ModuleVmError::LiveSlotLimit);
                }
                let args = caller.stack[stack_len - arity..].to_vec();
                let mut locals = vec![None; callee.local_count];
                for (slot, value) in args.into_iter().enumerate() {
                    locals[slot] = Some(value);
                }
                self.frames[frame_index].stack.truncate(stack_len - arity);
                self.frames[frame_index].ip += 1;
                self.frames.push(Frame {
                    function: target,
                    ip: 0,
                    locals,
                    stack: Vec::new(),
                });
            }
            Op::Return => {
                let value = self.frames[frame_index]
                    .stack
                    .last()
                    .ok_or(ModuleVmError::OperandStackUnderflow)?
                    .clone();
                if frame_index == 0 {
                    self.result = Some(value);
                } else {
                    let caller = &self.frames[frame_index - 1];
                    if caller.stack.len() >= MAX_OPERAND_STACK {
                        return Err(ModuleVmError::OperandStackLimit);
                    }
                    self.frames.pop();
                    self.frames[frame_index - 1].stack.push(value);
                }
            }
        }
        self.steps += 1;
        Ok(ModuleStep {
            function,
            instruction_pointer,
            instruction,
            result: self.result.clone(),
        })
    }

    fn live_slots(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.locals.len() + frame.stack.len())
            .sum()
    }

    fn check_push(&self) -> Result<(), ModuleVmError> {
        let frame = self.frames.last().expect("running VM has a frame");
        if frame.stack.len() >= MAX_OPERAND_STACK {
            return Err(ModuleVmError::OperandStackLimit);
        }
        if self.live_slots() >= MAX_LIVE_SLOTS {
            return Err(ModuleVmError::LiveSlotLimit);
        }
        Ok(())
    }

    fn kind(&self, value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Integer(_) => "integer",
            Value::Heap(reference) => match &reference.0.data {
                HeapData::Text(_) => "text",
                HeapData::Bytes(_) => "bytes",
                HeapData::List(_) => "list",
                HeapData::Map(_) => "map",
            },
        }
    }

    fn integer(&self, value: &Value) -> Result<i64, ModuleVmError> {
        match value {
            Value::Integer(value) => Ok(*value),
            other => Err(ModuleVmError::TypeMismatch {
                expected: "integer",
                actual: self.kind(other),
            }),
        }
    }

    fn operands<const N: usize>(&self) -> Result<[&Value; N], ModuleVmError> {
        let stack = &self.frames.last().expect("running VM has a frame").stack;
        let start = stack
            .len()
            .checked_sub(N)
            .ok_or(ModuleVmError::OperandStackUnderflow)?;
        Ok(std::array::from_fn(|index| &stack[start + index]))
    }

    fn replace_operands(&mut self, count: usize, result: Value) {
        let frame = self.frames.last_mut().expect("running VM has a frame");
        frame.stack.truncate(frame.stack.len() - count);
        frame.stack.push(result);
        frame.ip += 1;
    }

    fn object<'b>(
        &self,
        value: &'b Value,
        expected: &'static str,
    ) -> Result<&'b HeapData, ModuleVmError> {
        match value {
            Value::Heap(reference) => Ok(&reference.0.data),
            other => Err(ModuleVmError::TypeMismatch {
                expected,
                actual: self.kind(other),
            }),
        }
    }

    fn text<'b>(&self, value: &'b Value) -> Result<&'b str, ModuleVmError> {
        match self.object(value, "text")? {
            HeapData::Text(text) => Ok(text),
            _ => Err(ModuleVmError::TypeMismatch {
                expected: "text",
                actual: self.kind(value),
            }),
        }
    }

    fn list<'b>(&self, value: &'b Value) -> Result<&'b [Value], ModuleVmError> {
        match self.object(value, "list")? {
            HeapData::List(items) => Ok(items),
            _ => Err(ModuleVmError::TypeMismatch {
                expected: "list",
                actual: self.kind(value),
            }),
        }
    }

    fn map<'b>(&self, value: &'b Value) -> Result<&'b BTreeMap<String, Value>, ModuleVmError> {
        match self.object(value, "map")? {
            HeapData::Map(entries) => Ok(entries),
            _ => Err(ModuleVmError::TypeMismatch {
                expected: "map",
                actual: self.kind(value),
            }),
        }
    }

    fn new_collection(&mut self, object: HeapData) -> Result<(), ModuleVmError> {
        self.check_push()?;
        self.reserve_heap(HEAP_OBJECT_CHARGE)?;
        let value = self.allocate(object, HEAP_OBJECT_CHARGE, 0);
        let frame = self.frames.last_mut().expect("running VM has a frame");
        frame.stack.push(value);
        frame.ip += 1;
        Ok(())
    }

    fn list_push(&mut self) -> Result<(), ModuleVmError> {
        let (unique, charge, depth) = {
            let [list, value] = self.operands::<2>()?;
            let items = self.list(list)?;
            let count = items.len().checked_add(1).ok_or(ModuleVmError::HeapLimit)?;
            let charge = HEAP_OBJECT_CHARGE
                .checked_add(
                    count
                        .checked_mul(size_of::<Value>())
                        .ok_or(ModuleVmError::HeapLimit)?,
                )
                .ok_or(ModuleVmError::HeapLimit)?;
            let depth = self
                .value_depth(list)
                .max(self.value_depth(value).saturating_add(1));
            if depth > MAX_VALUE_DEPTH {
                return Err(ModuleVmError::ValueDepthLimit);
            }
            let unique =
                matches!(list, Value::Heap(reference) if Rc::strong_count(&reference.0) == 1);
            self.reserve_heap(if unique { size_of::<Value>() } else { charge })?;
            (unique, charge, depth)
        };
        let frame = self.frames.last_mut().expect("running VM has a frame");
        let value = frame.stack.pop().expect("validated operand count");
        let list = frame.stack.pop().expect("validated operand count");
        let Value::Heap(mut reference) = list else {
            unreachable!("validated list")
        };
        let result = if unique {
            let object = Rc::get_mut(&mut reference.0).expect("unique list");
            let HeapData::List(items) = &mut object.data else {
                unreachable!("validated list")
            };
            items.push(value);
            object.charge += size_of::<Value>();
            object.depth = depth;
            object.usage.set(object.usage.get() + size_of::<Value>());
            Value::Heap(reference)
        } else {
            let HeapData::List(items) = &reference.0.data else {
                unreachable!("validated list")
            };
            let mut new_items = Vec::with_capacity(items.len() + 1);
            new_items.extend_from_slice(items);
            new_items.push(value);
            self.allocate(HeapData::List(new_items), charge, depth)
        };
        let frame = self.frames.last_mut().expect("running VM has a frame");
        frame.stack.push(result);
        frame.ip += 1;
        Ok(())
    }

    fn list_get(&mut self) -> Result<(), ModuleVmError> {
        let [list, index] = self.operands::<2>()?;
        let index =
            usize::try_from(self.integer(index)?).map_err(|_| ModuleVmError::IndexOutOfBounds)?;
        let value = self
            .list(list)?
            .get(index)
            .ok_or(ModuleVmError::IndexOutOfBounds)?
            .clone();
        self.replace_operands(2, value);
        Ok(())
    }

    fn list_len(&mut self) -> Result<(), ModuleVmError> {
        let [list] = self.operands::<1>()?;
        let count =
            i64::try_from(self.list(list)?.len()).map_err(|_| ModuleVmError::IntegerOverflow)?;
        self.replace_operands(1, Value::Integer(count));
        Ok(())
    }

    fn map_insert(&mut self) -> Result<(), ModuleVmError> {
        let (key, unique, charge, delta, depth) = {
            let [map, key, value] = self.operands::<3>()?;
            let key = self.text(key)?.to_owned();
            let entries = self.map(map)?;
            let delta = if entries.contains_key(&key) {
                0
            } else {
                MAP_ENTRY_CHARGE
                    .checked_add(key.len())
                    .ok_or(ModuleVmError::HeapLimit)?
            };
            let Value::Heap(reference) = map else {
                unreachable!("validated map")
            };
            let charge = reference
                .0
                .charge
                .checked_add(delta)
                .ok_or(ModuleVmError::HeapLimit)?;
            let new_depth = self.value_depth(value) + 1;
            let depth = match entries.get(&key) {
                Some(old)
                    if self.value_depth(old) + 1 == reference.0.depth
                        && new_depth < reference.0.depth =>
                {
                    entries
                        .iter()
                        .filter(|(existing, _)| *existing != &key)
                        .map(|(_, value)| self.value_depth(value) + 1)
                        .max()
                        .unwrap_or(0)
                        .max(new_depth)
                }
                _ => reference.0.depth.max(new_depth),
            };
            if depth > MAX_VALUE_DEPTH {
                return Err(ModuleVmError::ValueDepthLimit);
            }
            let unique = Rc::strong_count(&reference.0) == 1;
            self.reserve_heap(if unique { delta } else { charge })?;
            (key, unique, charge, delta, depth)
        };
        let frame = self.frames.last_mut().expect("running VM has a frame");
        let value = frame.stack.pop().expect("validated operand count");
        frame.stack.pop().expect("validated operand count");
        let map = frame.stack.pop().expect("validated operand count");
        let Value::Heap(mut reference) = map else {
            unreachable!("validated map")
        };
        let result = if unique {
            let object = Rc::get_mut(&mut reference.0).expect("unique map");
            let HeapData::Map(entries) = &mut object.data else {
                unreachable!("validated map")
            };
            entries.insert(key, value);
            object.charge += delta;
            object.depth = depth;
            object.usage.set(object.usage.get() + delta);
            Value::Heap(reference)
        } else {
            let HeapData::Map(entries) = &reference.0.data else {
                unreachable!("validated map")
            };
            let mut new_entries = entries.clone();
            new_entries.insert(key, value);
            self.allocate(HeapData::Map(new_entries), charge, depth)
        };
        let frame = self.frames.last_mut().expect("running VM has a frame");
        frame.stack.push(result);
        frame.ip += 1;
        Ok(())
    }

    fn map_get(&mut self) -> Result<(), ModuleVmError> {
        let [map, key] = self.operands::<2>()?;
        let key = self.text(key)?;
        let value = self
            .map(map)?
            .get(key)
            .ok_or(ModuleVmError::MissingKey)?
            .clone();
        self.replace_operands(2, value);
        Ok(())
    }

    fn binary_int(
        &mut self,
        operation: impl FnOnce(i64, i64) -> Option<i64>,
    ) -> Result<(), ModuleVmError> {
        let [lhs, rhs] = self.operands::<2>()?;
        let value = operation(self.integer(lhs)?, self.integer(rhs)?)
            .ok_or(ModuleVmError::IntegerOverflow)?;
        self.replace_operands(2, Value::Integer(value));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(arity: usize, locals: usize, code: Vec<Op>) -> Function {
        Function::new(arity, locals, code)
    }

    fn module(functions: Vec<Function>) -> Module {
        Module::new(MODULE_VERSION, functions, 0).unwrap()
    }

    #[test]
    fn executes_recursive_calls_with_local_parameters() {
        let program = module(vec![
            function(0, 0, vec![Op::Const(5), Op::Call(1), Op::Return]),
            function(
                1,
                1,
                vec![
                    Op::LocalGet(0),
                    Op::Const(2),
                    Op::Lt,
                    Op::JumpIfZero(6),
                    Op::Const(1),
                    Op::Return,
                    Op::LocalGet(0),
                    Op::LocalGet(0),
                    Op::Const(1),
                    Op::Sub,
                    Op::Call(1),
                    Op::Mul,
                    Op::Return,
                ],
            ),
        ]);
        assert_eq!(program.max_operand_stack(1), Some(3));
        let mut vm = ModuleVm::new(&program);
        assert_eq!(vm.step().unwrap().instruction, Op::Const(5));
        assert_eq!(vm.step().unwrap().instruction, Op::Call(1));
        assert_eq!(vm.frames().len(), 2);
        assert_eq!(vm.frames()[1].locals, vec![Some(Value::Integer(5))]);
        assert_eq!(vm.run(), Ok(Value::Integer(120)));
        assert_eq!(vm.result(), Some(Value::Integer(120)));
        assert_eq!(vm.frame_depth(), 1);
        assert_eq!(vm.step(), Err(ModuleVmError::AlreadyFinished));
    }

    #[test]
    fn locals_are_scoped_to_each_call_frame() {
        let program = module(vec![
            function(
                0,
                1,
                vec![
                    Op::Const(9),
                    Op::LocalSet(0),
                    Op::Const(3),
                    Op::Call(1),
                    Op::LocalGet(0),
                    Op::Add,
                    Op::Return,
                ],
            ),
            function(
                1,
                1,
                vec![Op::LocalGet(0), Op::Const(2), Op::Mul, Op::Return],
            ),
        ]);
        assert_eq!(ModuleVm::new(&program).run(), Ok(Value::Integer(15)));
    }

    #[test]
    fn rejects_uninitialized_locals_on_any_branch() {
        let error = Module::new(
            MODULE_VERSION,
            vec![function(
                0,
                1,
                vec![
                    Op::Const(0),
                    Op::JumpIfZero(4),
                    Op::Const(1),
                    Op::LocalSet(0),
                    Op::LocalGet(0),
                    Op::Return,
                ],
            )],
            0,
        )
        .unwrap_err();
        assert_eq!(error.function, Some(0));
        assert_eq!(error.instruction, Some(4));
        assert!(error.message.contains("uninitialized"));
    }

    #[test]
    fn accepts_locals_initialized_on_both_branches() {
        let program = module(vec![function(
            0,
            1,
            vec![
                Op::Const(0),
                Op::JumpIfZero(5),
                Op::Const(7),
                Op::LocalSet(0),
                Op::Jump(7),
                Op::Const(8),
                Op::LocalSet(0),
                Op::LocalGet(0),
                Op::Return,
            ],
        )]);
        assert_eq!(ModuleVm::new(&program).run(), Ok(Value::Integer(8)));
    }

    #[test]
    fn rejects_inconsistent_stack_depth_and_invalid_structure() {
        let error = Module::new(
            MODULE_VERSION,
            vec![function(
                0,
                0,
                vec![
                    Op::Const(0),
                    Op::JumpIfZero(4),
                    Op::Jump(5),
                    Op::Const(99),
                    Op::Const(1),
                    Op::Const(2),
                    Op::Return,
                ],
            )],
            0,
        )
        .unwrap_err();
        assert!(error.message.contains("stack depth at join"));

        let invalid_jump = Module::new(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::Jump(2), Op::Return])],
            0,
        )
        .unwrap_err();
        assert!(invalid_jump.message.contains("branch target"));

        let invalid_call = Module::new(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::Call(1), Op::Return])],
            0,
        )
        .unwrap_err();
        assert!(invalid_call.message.contains("missing"));

        assert!(Module::new(MODULE_VERSION + 1, vec![], 0).is_err());
    }

    #[test]
    fn runtime_traps_leave_the_current_instruction_unchanged() {
        let program = module(vec![function(
            0,
            0,
            vec![Op::Const(9), Op::Const(0), Op::Div, Op::Return],
        )]);
        let mut vm = ModuleVm::new(&program);
        assert_eq!(vm.step().unwrap().instruction, Op::Const(9));
        assert_eq!(vm.step().unwrap().instruction, Op::Const(0));
        let frames = vm.frames.clone();
        let steps = vm.steps();
        assert_eq!(vm.step(), Err(ModuleVmError::DivisionByZero));
        assert_eq!(vm.frames, frames);
        assert_eq!(vm.steps(), steps);
        assert_eq!(vm.step(), Err(ModuleVmError::DivisionByZero));

        let overflow = module(vec![function(
            0,
            0,
            vec![Op::Const(i64::MAX), Op::Const(1), Op::Add, Op::Return],
        )]);
        assert_eq!(
            ModuleVm::new(&overflow).run(),
            Err(ModuleVmError::IntegerOverflow)
        );
    }

    #[test]
    fn recursion_is_bounded_by_frames_and_live_slots() {
        let recursive = module(vec![function(0, 0, vec![Op::Call(0), Op::Return])]);
        assert_eq!(
            ModuleVm::new(&recursive).run(),
            Err(ModuleVmError::CallFrameLimit)
        );

        let wide = module(vec![function(
            0,
            MAX_LOCALS_PER_FUNCTION,
            vec![Op::Call(0), Op::Return],
        )]);
        assert_eq!(
            ModuleVm::new(&wide).run(),
            Err(ModuleVmError::LiveSlotLimit)
        );
    }

    #[test]
    fn generated_module_inputs_validate_without_panicking() {
        let mut state = 0x7265_6c65_6173_6573_u64;
        for _ in 0..512 {
            let mut code = Vec::new();
            for _ in 0..16 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let op = match state % 12 {
                    0 => Op::Const(state as i64),
                    1 => Op::LocalGet((state >> 8) as usize % 4),
                    2 => Op::LocalSet((state >> 8) as usize % 4),
                    3 => Op::Add,
                    4 => Op::Sub,
                    5 => Op::Mul,
                    6 => Op::Div,
                    7 => Op::Eq,
                    8 => Op::Lt,
                    9 => Op::Jump((state >> 8) as usize % 20),
                    10 => Op::JumpIfZero((state >> 8) as usize % 20),
                    _ => Op::Call((state >> 8) as usize % 3),
                };
                code.push(op);
            }
            code.push(Op::Return);
            let _ = Module::new(MODULE_VERSION, vec![function(0, 2, code)], 0);
        }
    }

    #[test]
    fn validation_has_an_explicit_work_budget() {
        let functions = vec![function(0, 0, vec![Op::Const(1), Op::Return])];
        let mut visits = 0;
        let error = validate_function(0, &functions[0], &functions, &mut visits).unwrap_err();
        assert!(error.message.contains("visit limit"));
    }

    #[test]
    fn transforms_nested_records_with_bounded_memory() {
        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(
                0,
                6,
                vec![
                    Op::Input(0), // records
                    Op::LocalSet(0),
                    Op::Const(0), // index
                    Op::LocalSet(1),
                    Op::Const(0), // total
                    Op::LocalSet(2),
                    Op::ListNew, // doubled scores
                    Op::LocalSet(3),
                    Op::LocalGet(1), // loop condition
                    Op::LocalGet(0),
                    Op::ListLen,
                    Op::Lt,
                    Op::JumpIfZero(36),
                    Op::LocalGet(0), // record = records[index]
                    Op::LocalGet(1),
                    Op::ListGet,
                    Op::LocalSet(4),
                    Op::LocalGet(4), // score = record["score"]
                    Op::ConstValue(0),
                    Op::MapGet,
                    Op::LocalSet(5),
                    Op::LocalGet(2), // total += score
                    Op::LocalGet(5),
                    Op::Add,
                    Op::LocalSet(2),
                    Op::LocalTake(3), // doubled.push(score * 2)
                    Op::LocalGet(5),
                    Op::Const(2),
                    Op::Mul,
                    Op::ListPush,
                    Op::LocalSet(3),
                    Op::LocalGet(1), // index += 1
                    Op::Const(1),
                    Op::Add,
                    Op::LocalSet(1),
                    Op::Jump(8),
                    Op::MapNew, // result
                    Op::ConstValue(1),
                    Op::LocalGet(1),
                    Op::MapInsert,
                    Op::ConstValue(2),
                    Op::LocalGet(2),
                    Op::MapInsert,
                    Op::ConstValue(3),
                    Op::LocalGet(3),
                    Op::MapInsert,
                    Op::Return,
                ],
            )],
            0,
            ["score", "count", "total", "doubled"]
                .map(|text| OwnedValue::Text(text.into()))
                .to_vec(),
            1,
        )
        .unwrap();
        let records = OwnedValue::List(
            (0..256)
                .map(|score| {
                    OwnedValue::Map(BTreeMap::from([(
                        "score".into(),
                        OwnedValue::Integer(score),
                    )]))
                })
                .collect(),
        );
        let expected = OwnedValue::Map(BTreeMap::from([
            ("count".into(), OwnedValue::Integer(256)),
            ("total".into(), OwnedValue::Integer(32_640)),
            (
                "doubled".into(),
                OwnedValue::List(
                    (0..256)
                        .map(|score| OwnedValue::Integer(score * 2))
                        .collect(),
                ),
            ),
        ]));
        let mut first = ModuleVm::with_inputs(&program, vec![records.clone()], 128 * 1024).unwrap();
        let result = first.run().unwrap();
        assert_eq!(first.resolve(result), Ok(expected.clone()));
        assert!(first.heap_bytes() <= 128 * 1024);
        let mut second = ModuleVm::with_inputs(&program, vec![records], 128 * 1024).unwrap();
        let result = second.run().unwrap();
        assert_eq!(second.resolve(result), Ok(expected));
        assert_eq!(first.steps(), second.steps());
        assert_eq!(first.heap_bytes(), second.heap_bytes());
    }

    #[test]
    fn typed_and_memory_traps_do_not_change_the_instruction_state() {
        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(
                0,
                0,
                vec![Op::ConstValue(0), Op::Const(1), Op::Add, Op::Return],
            )],
            0,
            vec![OwnedValue::Bool(true)],
            0,
        )
        .unwrap();
        let mut vm = ModuleVm::new(&program);
        vm.step().unwrap();
        vm.step().unwrap();
        let before = vm.frames();
        let bytes = vm.heap_bytes();
        let steps = vm.steps();
        assert_eq!(
            vm.step(),
            Err(ModuleVmError::TypeMismatch {
                expected: "integer",
                actual: "bool"
            })
        );
        assert_eq!(vm.frames(), before);
        assert_eq!(vm.heap_bytes(), bytes);
        assert_eq!(vm.steps(), steps);

        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::ConstValue(0), Op::Return])],
            0,
            vec![OwnedValue::Text("large".into())],
            0,
        )
        .unwrap();
        let mut vm = ModuleVm::with_inputs(&program, vec![], HEAP_OBJECT_CHARGE + 4).unwrap();
        let before = vm.frames();
        assert_eq!(vm.step(), Err(ModuleVmError::HeapLimit));
        assert_eq!(vm.frames(), before);
        assert_eq!(vm.heap_bytes(), 0);
        assert_eq!(vm.steps(), 0);

        let program = module(vec![function(
            0,
            0,
            vec![Op::ListNew, Op::Const(7), Op::ListPush, Op::Return],
        )]);
        let mut vm = ModuleVm::with_inputs(&program, vec![], HEAP_OBJECT_CHARGE).unwrap();
        vm.step().unwrap();
        vm.step().unwrap();
        let before = vm.frames();
        assert_eq!(vm.step(), Err(ModuleVmError::HeapLimit));
        assert_eq!(vm.frames(), before);
        assert_eq!(vm.heap_bytes(), HEAP_OBJECT_CHARGE);
        assert_eq!(vm.steps(), 2);
    }

    #[test]
    fn round_trips_boundary_value_types() {
        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::Input(0), Op::Return])],
            0,
            vec![],
            1,
        )
        .unwrap();
        let value = OwnedValue::Map(BTreeMap::from([
            ("null".into(), OwnedValue::Null),
            ("bool".into(), OwnedValue::Bool(true)),
            ("integer".into(), OwnedValue::Integer(-9)),
            ("text".into(), OwnedValue::Text("hello".into())),
            ("bytes".into(), OwnedValue::Bytes(vec![0, 255])),
            (
                "list".into(),
                OwnedValue::List(vec![OwnedValue::Null, OwnedValue::Bool(false)]),
            ),
        ]));
        let mut vm = ModuleVm::with_inputs(&program, vec![value.clone()], 1_024).unwrap();
        let result = vm.run().unwrap();
        assert_eq!(vm.resolve(result), Ok(value));
        assert!(vm.heap_bytes() > 0);
    }

    #[test]
    fn releases_unreferenced_values_but_keeps_debugger_values_live() {
        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(
                0,
                1,
                vec![
                    Op::ConstValue(0),
                    Op::LocalSet(0),
                    Op::ConstValue(1),
                    Op::LocalSet(0),
                    Op::Const(0),
                    Op::Return,
                ],
            )],
            0,
            vec![
                OwnedValue::Text("first".into()),
                OwnedValue::Text("second".into()),
            ],
            0,
        )
        .unwrap();
        let mut vm = ModuleVm::new(&program);
        vm.step().unwrap();
        vm.step().unwrap();
        let snapshot = vm.frames();
        let first_charge = vm.heap_bytes();
        vm.step().unwrap();
        vm.step().unwrap();
        assert!(vm.heap_bytes() > first_charge);
        assert_eq!(
            vm.resolve(snapshot[0].locals[0].clone().unwrap()),
            Ok(OwnedValue::Text("first".into()))
        );
        drop(snapshot);
        assert_eq!(vm.heap_bytes(), HEAP_OBJECT_CHARGE + "second".len());
    }

    #[test]
    fn builds_large_list_with_linear_heap_charge() {
        let program = module(vec![function(
            0,
            2,
            vec![
                Op::ListNew,
                Op::LocalSet(0),
                Op::Const(0),
                Op::LocalSet(1),
                Op::LocalGet(1),
                Op::Const(10_000),
                Op::Lt,
                Op::JumpIfZero(17),
                Op::LocalTake(0),
                Op::LocalGet(1),
                Op::ListPush,
                Op::LocalSet(0),
                Op::LocalGet(1),
                Op::Const(1),
                Op::Add,
                Op::LocalSet(1),
                Op::Jump(4),
                Op::LocalTake(0),
                Op::Return,
            ],
        )]);
        let mut vm = ModuleVm::with_inputs(&program, vec![], 256 * 1024).unwrap();
        let result = vm.run().unwrap();
        let OwnedValue::List(values) = vm.resolve(result).unwrap() else {
            panic!("expected list")
        };
        assert_eq!(values.len(), 10_000);
        assert_eq!(values.first(), Some(&OwnedValue::Integer(0)));
        assert_eq!(values.last(), Some(&OwnedValue::Integer(9_999)));
        assert_eq!(
            vm.heap_bytes(),
            HEAP_OBJECT_CHARGE + 10_000 * size_of::<Value>()
        );
    }

    #[test]
    fn shared_collections_keep_their_previous_values() {
        let list_program = module(vec![function(
            0,
            1,
            vec![
                Op::ListNew,
                Op::LocalSet(0),
                Op::LocalGet(0),
                Op::Const(7),
                Op::ListPush,
                Op::Return,
            ],
        )]);
        let mut vm = ModuleVm::new(&list_program);
        let result = vm.run().unwrap();
        assert_eq!(
            vm.resolve(vm.frames()[0].locals[0].clone().unwrap()),
            Ok(OwnedValue::List(vec![]))
        );
        assert_eq!(
            vm.resolve(result),
            Ok(OwnedValue::List(vec![OwnedValue::Integer(7)]))
        );

        let map_program = Module::with_values(
            MODULE_VERSION,
            vec![function(
                0,
                1,
                vec![
                    Op::MapNew,
                    Op::LocalSet(0),
                    Op::LocalGet(0),
                    Op::ConstValue(0),
                    Op::Const(7),
                    Op::MapInsert,
                    Op::Return,
                ],
            )],
            0,
            vec![OwnedValue::Text("key".into())],
            0,
        )
        .unwrap();
        let mut vm = ModuleVm::new(&map_program);
        let result = vm.run().unwrap();
        assert_eq!(
            vm.resolve(vm.frames()[0].locals[0].clone().unwrap()),
            Ok(OwnedValue::Map(BTreeMap::new()))
        );
        assert_eq!(
            vm.resolve(result),
            Ok(OwnedValue::Map(BTreeMap::from([(
                "key".into(),
                OwnedValue::Integer(7)
            )])))
        );
    }

    #[test]
    fn local_take_requires_reinitialization() {
        let error = Module::new(
            MODULE_VERSION,
            vec![function(
                0,
                1,
                vec![
                    Op::Const(1),
                    Op::LocalSet(0),
                    Op::LocalTake(0),
                    Op::LocalTake(0),
                    Op::Return,
                ],
            )],
            0,
        )
        .unwrap_err();
        assert_eq!(error.instruction, Some(3));
        assert!(error.message.contains("uninitialized"));
    }

    #[test]
    fn validates_constants_inputs_and_collection_bounds() {
        let missing = Module::with_values(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::ConstValue(1), Op::Return])],
            0,
            vec![OwnedValue::Null],
            0,
        )
        .unwrap_err();
        assert_eq!(missing.instruction, Some(0));

        let program = Module::with_values(
            MODULE_VERSION,
            vec![function(
                0,
                0,
                vec![Op::Input(0), Op::Const(-1), Op::ListGet, Op::Return],
            )],
            0,
            vec![],
            1,
        )
        .unwrap();
        assert_eq!(
            ModuleVm::with_inputs(&program, vec![], 128).unwrap_err(),
            ModuleVmError::InputCountMismatch
        );
        let mut vm = ModuleVm::with_inputs(&program, vec![OwnedValue::List(vec![])], 128).unwrap();
        vm.step().unwrap();
        vm.step().unwrap();
        let before = vm.frames();
        assert_eq!(vm.step(), Err(ModuleVmError::IndexOutOfBounds));
        assert_eq!(vm.frames(), before);

        let mut deep = OwnedValue::Null;
        for _ in 0..=MAX_VALUE_DEPTH {
            deep = OwnedValue::List(vec![deep]);
        }
        let error = Module::with_values(
            MODULE_VERSION,
            vec![function(0, 0, vec![Op::ConstValue(0), Op::Return])],
            0,
            vec![deep],
            0,
        )
        .unwrap_err();
        assert!(error.message.contains("ValueDepthLimit"));
    }
}
