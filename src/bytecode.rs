use crate::{
    Result, Value,
    builtin::{Builtin, Global},
    compilation::{Buffer, Name, Table},
    syntax::{
        self, Argument, ArgumentKind, Block, CallForm, Expr, Node, ParamKind, Statement, Stmt,
        Target,
    },
};
use std::collections::HashMap;

mod calls;
mod errors;
mod namespaces;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    TryBegin(usize),
    TryBody,
    TryEnd,
    EnsureEnd,
    Retry,
    RaiseStart(Option<(usize, Option<usize>)>, usize),
    RaiseValue,
    Raise(u8),
    InitNamespace(usize),
    UnboundClass(usize),
    BindIvar(usize, usize),
    NamespaceSelf(usize),
    NamespaceConstant(usize, usize),
    NamespaceConstantAddress(usize, usize),
    NamespaceVariable(usize, bool),
    NamespaceStore(usize),
    NamespaceAddress(usize, bool),
    AmbientValue(usize, usize),
    AmbientAddress(usize, usize),
    FileValue(usize, usize),
    FileAddress(usize, usize),
    RootAddress(usize, usize),
    PrepareMember(CallSite, bool),
    StoreDeclaration(usize),
    Regex(usize, u8),
    TypeShadowed(usize, usize),
    Normalize(usize, usize),
    Declaration(usize),
    Global(usize),
    GlobalReceiver(usize, bool),
    StoreGlobal(usize),
    ResolveGlobalCall(usize),
    AddressGlobal(usize),
    Integer(usize, u32),
    Constant(usize),
    Nil,
    Load(usize),
    LoadOptional(usize, usize),
    ReceiverBound(usize, usize),
    Unbound(usize),
    NonCallable(usize),
    Bind(usize, usize),
    BindEnd,
    Declare(usize),
    Shadow(usize),
    BlockArg(usize, bool),
    Attach(usize),
    BlockGiven(bool, bool),
    CheckBlock,
    Yield(usize),
    Store(usize),
    Pop,
    Dup,
    Unary(&'static str),
    Binary(&'static str),
    Shovel(CallSite),
    AddStore(usize),
    Array(usize),
    TextStart,
    TextPart,
    TextEnd(bool),
    Hash(usize),
    RangeStart,
    Range(bool, bool, bool),
    Index(usize),
    AddressLocal(usize),
    AddressBound(usize, usize),
    AddressValue,
    AddressIndex(usize),
    AddressTarget(usize, bool),
    AddressMember(CallSite),
    AddressNamespaceField(CallSite),
    AddressMemberTarget(CallSite, bool),
    AddressStore,
    AddressDrop,
    Mutate(CallSite, usize),
    Extract(Selection),
    CaseCompare(bool, bool),
    LoopStart {
        iterable: bool,
        expression: bool,
        next: usize,
        end: usize,
    },
    LoopTest,
    IterNext,
    LoopBody,
    LoopEnd,
    LoopGuard(bool),
    Break(bool),
    Next(bool),
    Call(usize, usize),
    AutoCall(usize),
    Host(usize, usize),
    HostValue(usize),
    Method(CallSite, usize),
    Arguments,
    RootCall(usize, bool),
    ForwardArguments,
    ResolveCall(usize, usize, bool),
    CallName(usize, usize),
    CallValue,
    CallMember(CallSite),
    Bypass(usize),
    BypassEnd(usize),
    Argument(ArgumentOp),
    Invoke(Invocation),
    InvokeRoot(Invocation),
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
    JumpNil(usize),
    AddressJumpNil(usize, bool),
    Return,
    Finish,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ArgumentOp {
    Positional,
    Splat,
    Keyword(usize),
    KeywordSplat,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Invocation {
    ImplicitMember(usize, usize),
    Builtin(Builtin),
    Function(usize),
    Host(usize),
    Member(CallSite, bool),
    NonCallable,
    Resolved,
}

#[derive(Debug)]
pub(crate) struct Parameter {
    pub name: String,
    pub kind: ParamKind,
    pub default: bool,
    pub slot: usize,
    pub ty: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSite {
    pub name: usize,
    pub method: Option<Method>,
    pub auto: bool,
    pub parenthesized: bool,
    pub scope: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Selection {
    At(usize),
    Rest {
        leading: usize,
        trailing: usize,
    },
    Tail {
        leading: usize,
        trailing: usize,
        index: usize,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Method {
    Length,
    Size,
    At,
    Slice,
    ByteSlice,
    GetByte,
    First,
    Last,
    ToArray,
    Cover,
    ExcludeEnd,
    Empty,
    Reverse,
    Take,
    Drop,
    Compact,
    Uniq,
    Flatten,
    Chunk,
    Window,
    Zip,
    Transpose,
    ToHash,
    Fetch,
    ValuesAt,
    Dig,
    Key,
    HasValue,
    RemapKeys,
    Except,
    Member,
    Abs,
    Even,
    Odd,
    Ord,
    Chr,
    Bytes,
    Chars,
    Lines,
    Codepoints,
    StartWith,
    EndWith,
    IsNil,
    Itself,
    ByteSize,
    Include,
    Index,
    Rindex,
    Split,
    Join,
    Push,
    Prepend,
    Pop,
    Shift,
    Delete,
    Insert,
    Clear,
    Fill,
    Store,
    Replace,
    Dup,
    Sum,
    Keys,
    Values,
    ToString,
    ToInt,
    ToFloat,
}
impl Method {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "length" => Self::Length,
            "size" => Self::Size,
            "at" => Self::At,
            "slice" => Self::Slice,
            "byteslice" => Self::ByteSlice,
            "getbyte" => Self::GetByte,
            "first" => Self::First,
            "last" => Self::Last,
            "to_a" => Self::ToArray,
            "cover?" => Self::Cover,
            "member?" => Self::Member,
            "exclude_end?" => Self::ExcludeEnd,
            "empty?" => Self::Empty,
            "reverse" => Self::Reverse,
            "take" => Self::Take,
            "drop" => Self::Drop,
            "compact" => Self::Compact,
            "uniq" => Self::Uniq,
            "flatten" => Self::Flatten,
            "chunk" => Self::Chunk,
            "window" => Self::Window,
            "zip" => Self::Zip,
            "transpose" => Self::Transpose,
            "to_h" => Self::ToHash,
            "fetch" => Self::Fetch,
            "values_at" => Self::ValuesAt,
            "dig" => Self::Dig,
            "key?" | "has_key?" => Self::Key,
            "value?" | "has_value?" => Self::HasValue,
            "remap_keys" => Self::RemapKeys,
            "except" => Self::Except,
            "abs" => Self::Abs,
            "even?" => Self::Even,
            "odd?" => Self::Odd,
            "ord" => Self::Ord,
            "chr" => Self::Chr,
            "bytes" => Self::Bytes,
            "chars" => Self::Chars,
            "lines" => Self::Lines,
            "codepoints" => Self::Codepoints,
            "start_with?" => Self::StartWith,
            "end_with?" => Self::EndWith,
            "nil?" => Self::IsNil,
            "itself" => Self::Itself,
            "bytesize" => Self::ByteSize,
            "include?" => Self::Include,
            "index" | "find_index" => Self::Index,
            "rindex" => Self::Rindex,
            "split" => Self::Split,
            "join" => Self::Join,
            "push" | "append" => Self::Push,
            "prepend" | "unshift" => Self::Prepend,
            "pop" => Self::Pop,
            "shift" => Self::Shift,
            "delete" => Self::Delete,
            "insert" => Self::Insert,
            "clear" => Self::Clear,
            "fill" => Self::Fill,
            "store" => Self::Store,
            "replace" => Self::Replace,
            "dup" => Self::Dup,
            "sum" => Self::Sum,
            "keys" => Self::Keys,
            "values" => Self::Values,
            "to_s" | "string" => Self::ToString,
            "to_i" => Self::ToInt,
            "to_f" => Self::ToFloat,
            _ => return None,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct Function {
    pub offset: u32,
    pub private: bool,
    pub locations: Vec<u32>,
    pub trace_name: std::sync::Arc<str>,
    pub instance: bool,
    pub accessor: Option<(String, bool)>,
    pub namespace: Option<usize>,
    pub initializer: bool,
    pub name: String,
    pub params: Vec<Parameter>,
    pub binds_parameters: bool,
    pub plain: bool,
    pub locals: usize,
    pub code: Vec<Op>,
    pub captures: Vec<Option<Capture>>,
    pub block_arity: usize,
    pub local_names: Vec<String>,
    pub return_type: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capture {
    pub depth: usize,
    pub slot: usize,
}
#[derive(Debug)]
pub(crate) struct Program {
    pub file: bool,
    pub owner: std::sync::Weak<crate::code::Code>,
    pub handlers: Vec<errors::TrySpec>,
    pub source: crate::source::Source,
    pub namespaces: Vec<std::sync::Arc<crate::namespace::Definition>>,
    pub type_guards: Vec<Vec<String>>,
    pub types: Vec<crate::types::Type>,
    pub declarations: Vec<Value>,
    pub enum_definitions: std::sync::Arc<[std::sync::Arc<crate::enums::Definition>]>,
    pub declaration_names: HashMap<String, usize>,
    pub globals: Vec<(Global, Value)>,
    pub functions: Vec<Function>,
    pub constants: Vec<Value>,
    pub names: HashMap<String, usize>,
    pub hosts: Vec<String>,
    pub members: Vec<String>,
}

pub(crate) fn compile(
    source: &str,
    hosts: Vec<String>,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    compile_mode(source, hosts, false, work)
}

pub(crate) fn compile_file(
    source: &str,
    hosts: Vec<String>,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    compile_mode(source, hosts, true, work)
}

fn compile_mode(
    source: &str,
    hosts: Vec<String>,
    file: bool,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    let parsed = syntax::parse(source, work)?;
    let mut defs = parsed.functions;
    let mut contexts = Buffer::with_capacity(work, defs.len())?;
    for _ in 0..defs.len() {
        contexts.push(work, (None, false, false))?;
    }
    let mut names = HashMap::new();
    for (i, definition) in defs.iter().enumerate() {
        work.bytes(definition.name.len())?;
        names.insert(definition.name.as_str().to_owned(), i);
    }
    let mut declarations = Vec::new();
    let mut declaration_names = HashMap::new();
    for (name, members) in parsed.enums {
        work.bytes(name.len())?;
        for member in &members {
            work.bytes(member.len())?;
        }
        if declaration_names.contains_key(name.as_str())
            || names.get(name.as_str()).is_some_and(|&index| index != 0)
        {
            return Err(syntax::unsupported(work, "duplicate top-level declaration"));
        }
        declaration_names.insert(name.as_str().to_owned(), declarations.len());
        declarations.push(crate::enums::compile(
            name.into_string(),
            members
                .into_iter()
                .map(crate::compilation::Name::into_string),
            work,
        )?);
    }
    let enum_definitions = declarations
        .iter()
        .filter_map(|value| match &value.0 {
            crate::value::Kind::Enum(enumeration) => Some(enumeration.definition.clone()),
            _ => None,
        })
        .collect();
    let mut program = Program {
        file,
        owner: std::sync::Weak::new(),
        handlers: Vec::new(),
        source: crate::source::Source::compile(source, work)?,
        namespaces: Vec::new(),
        type_guards: Vec::new(),
        types: Vec::new(),
        declarations,
        enum_definitions,
        declaration_names,
        globals: Vec::new(),
        functions: Vec::new(),
        constants: Vec::new(),
        names,
        hosts,
        members: Vec::new(),
    };
    for module in parsed.modules {
        program.register_module(module, "", &mut defs, &mut contexts, work)?;
    }
    program.functions = (0..defs.len()).map(|_| Function::default()).collect();
    for (index, def) in defs.into_iter().enumerate() {
        work.bytes(def.name.len())?;
        let mut c = Compiler {
            work,
            namespace: contexts[index].0,
            instance: contexts[index].2,
            program: &mut program,
            locals: Table::new(),
            slots: 0,
            code: Vec::new(),
            locations: Vec::new(),
            offset: def.offset,
            parameters: Table::new(),
            loop_bindings: Buffer::new(),
            outer: Buffer::new(),
            reads: Table::new(),
            assigned: Table::new(),
        };
        let binds_parameters = def
            .params
            .iter()
            .any(|p| p.default.is_some() || p.ty.is_some());
        let plain = !binds_parameters && def.params.iter().all(|p| p.kind == ParamKind::Positional);
        let mut params = Vec::new();
        for (i, param) in def.params.iter().enumerate() {
            work.bytes(param.name.len())?;
            let ty = param.ty.as_ref().map(|ty| c.annotation(ty)).transpose()?;
            let bind = binds_parameters.then(|| c.emit(Op::Bind(i, 0)));
            if let Some(value) = &param.default {
                c.declare_expr(value)?;
                c.expr(value)?;
                if let Some(ty) = ty {
                    let label = c.program.constants.len();
                    c.program
                        .constants
                        .push(Value::bytes(param.name.as_bytes()));
                    c.emit(Op::Normalize(ty, label));
                }
            }
            let slot = c.slot(&param.name)?;
            if param.default.is_some() {
                c.emit(Op::Store(slot));
                c.emit(Op::Pop);
            }
            if let Some(bind) = bind {
                c.patch(bind, c.code.len());
            }
            if c.instance {
                if let Some(name) = &param.ivar {
                    let name = c.call_site(name, false).name;
                    c.emit(Op::BindIvar(name, slot));
                }
            }
            c.parameters.insert(work, param.name.clone(), ())?;
            params.push(Parameter {
                name: param.name.as_str().to_owned(),
                kind: param.kind,
                default: param.default.is_some(),
                slot,
                ty,
            });
        }
        if binds_parameters {
            c.emit(Op::BindEnd);
        }
        c.declare(&def.body)?;
        c.block(&def.body)?;
        let finish = c.emit(Op::Finish);
        c.locations[finish] = def.body.last().map_or(def.offset, |stmt| stmt.offset);
        let return_type = def
            .return_type
            .as_ref()
            .map(|ty| c.annotation(ty))
            .transpose()?;
        debug_assert_eq!(c.code.len(), c.locations.len());
        let function = Function {
            offset: def.offset,
            private: def.private,
            locations: c.locations,
            trace_name: def.name.rsplit(['.', '#']).next().unwrap().into(),
            instance: contexts[index].2,
            accessor: def
                .accessor
                .map(|(name, setter)| (name.into_string(), setter)),
            namespace: contexts[index].0,
            initializer: contexts[index].1,
            name: def.name.into_string(),
            params,
            binds_parameters,
            plain,
            locals: c.slots,
            local_names: local_names(&c.locals, c.slots, work)?,
            code: c.code,
            captures: Vec::new(),
            block_arity: 0,
            return_type,
        };
        program.functions[index] = function;
    }
    work.checkpoint()?;
    Ok(program)
}

fn local_names(
    locals: &Table<usize>,
    slots: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Vec<String>> {
    work.charge(slots)?;
    let mut names = vec![String::new(); slots];
    for (name, &index) in locals.iter(work)? {
        work.bytes(name.len())?;
        names[index] = name.as_str().to_owned();
    }
    Ok(names)
}

fn expanded(args: &[Argument]) -> bool {
    args.iter()
        .any(|a| !matches!(a.kind, ArgumentKind::Positional))
}

struct Compiler<'a> {
    work: &'a dyn crate::compilation::Work,
    instance: bool,
    namespace: Option<usize>,
    program: &'a mut Program,
    locals: Table<usize>,
    slots: usize,
    code: Vec<Op>,
    locations: Vec<u32>,
    offset: u32,
    parameters: Table<()>,
    loop_bindings: Buffer<Buffer<usize>>,
    outer: Buffer<Table<usize>>,
    reads: Table<()>,
    assigned: Table<()>,
}
impl Compiler<'_> {
    fn slot(&mut self, name: &Name) -> Result<usize> {
        if let Some(&slot) = self.locals.get(self.work, name)? {
            return Ok(slot);
        }
        let slot = self.slots;
        self.locals.insert(self.work, name.clone(), slot)?;
        self.slots += 1;
        Ok(slot)
    }
    fn outer_binding(&self, name: &str) -> Result<Option<Capture>> {
        for (depth, scope) in self.outer.iter().enumerate() {
            if let Some(&slot) = scope.get(self.work, name)? {
                return Ok(Some(Capture { depth, slot }));
            }
        }
        Ok(None)
    }
    fn capture_name(&mut self, name: &Name) -> Result<()> {
        if self.outer_binding(name)?.is_some() {
            self.slot(name)?;
        }
        Ok(())
    }
    fn declare(&mut self, body: &[Stmt]) -> Result<()> {
        self.work.charge(1)?;
        for stmt in body {
            self.work.charge(1)?;
            match &stmt.node {
                Statement::Raise(value, message) => {
                    for value in value.iter().chain(message) {
                        self.declare_expr(value)?;
                    }
                }
                Statement::Module(_) | Statement::UnboundClass(_) | Statement::Retry => (),
                Statement::Expr(e) => self.declare_expr(e)?,
                Statement::Assign(target, _, value) => {
                    self.declare_target(target)?;
                    self.declare_expr(value)?;
                }
                Statement::If(cond, yes, no) => {
                    self.declare_expr(cond)?;
                    self.declare(yes)?;
                    self.declare(no)?;
                }
                Statement::While(cond, body) => {
                    self.declare_expr(cond)?;
                    self.declare(body)?;
                }
                Statement::For(target, iterable, body) => {
                    self.declare_target(target)?;
                    self.declare_expr(iterable)?;
                    self.declare(body)?;
                }
                Statement::Return(e) | Statement::Break(e) | Statement::Next(e) => {
                    if let Some(e) = e {
                        self.declare_expr(e)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn declare_target(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        match target {
            Target::Typed(target, _) => self.declare_target(target)?,
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                if !self.namespace_binding(name)?
                    && (!self.outer.is_empty() || self.global_binding(name)?.is_none())
                {
                    self.slot(name)?;
                }
                self.assigned.insert(self.work, name.clone(), ())?;
            }
            Target::Value(e) => self.declare_expr(e)?,
            Target::Tuple(parts) => {
                for (part, _) in parts {
                    if let Some(part) = part {
                        self.declare_target(part)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn declare_expr(&mut self, e: &Expr) -> Result<()> {
        self.work.charge(1)?;
        match &e.node {
            Node::Try(attempt) => {
                self.declare(&attempt.body)?;
                for rescue in &attempt.rescues {
                    self.declare(&rescue.body)?;
                }
                self.declare(&attempt.alternate)?;
                self.declare(&attempt.ensure)?;
            }
            Node::Regex(..) => (),
            Node::Shape(_, fallback, _) => {
                if let Some(fallback) = fallback {
                    self.declare_expr(fallback)?;
                }
            }
            Node::Literal(_) | Node::Integer(_) | Node::BigInteger(..) => (),
            Node::Var(name) => {
                self.reads.insert(self.work, name.clone(), ())?;
                self.capture_name(name)?;
            }
            Node::Array(values) | Node::Yield(values) | Node::Template(values, _) => {
                for value in values {
                    self.declare_expr(value)?;
                }
            }
            Node::Call(name, args, _) => {
                if name != "it" {
                    self.reads.insert(self.work, name.clone(), ())?;
                }
                self.capture_name(name)?;
                for arg in args {
                    self.declare_expr(&arg.value)?;
                }
            }
            Node::BlockCall(call, _) => self.declare_expr(call)?,
            Node::Hash(entries) => {
                for (_, value) in entries {
                    self.declare_expr(value)?;
                }
            }
            Node::Unary(_, value) | Node::Member(value, _) | Node::SafeMember(value, _) => {
                self.declare_expr(value)?
            }
            Node::Binary(_, a, b) => {
                self.declare_expr(a)?;
                self.declare_expr(b)?;
            }
            Node::Range(a, b, _) => {
                if let Some(e) = a {
                    self.declare_expr(e)?;
                }
                if let Some(e) = b {
                    self.declare_expr(e)?;
                }
            }
            Node::Conditional(a, b, c) => {
                self.declare_expr(a)?;
                self.declare_expr(b)?;
                self.declare_expr(c)?;
            }
            Node::Case(target, clauses, alternate) => {
                if let Some(e) = target {
                    self.declare_expr(e)?;
                }
                for clause in clauses {
                    for (e, _) in &clause.values {
                        self.declare_expr(e)?;
                    }
                    self.declare_expr(&clause.result)?;
                }
                if let Some(e) = alternate {
                    self.declare_expr(e)?;
                }
            }
            Node::Loop(stmt) => self.declare(std::slice::from_ref(stmt.as_ref()))?,
            Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
                self.declare_expr(recv)?;
                for arg in args {
                    self.declare_expr(&arg.value)?;
                }
            }
            Node::ComputedCall(recv, args) => {
                self.declare_callee(recv)?;
                for arg in args {
                    self.declare_expr(&arg.value)?;
                }
            }
            Node::Scope(recv, _, args) => {
                self.declare_expr(recv)?;
                for arg in args.iter().flatten() {
                    self.declare_expr(&arg.value)?;
                }
            }
            Node::Index(recv, args) => {
                self.declare_expr(recv)?;
                for arg in args {
                    self.declare_expr(arg)?;
                }
            }
        }
        Ok(())
    }
    // As with a named call, `it` in callee position, including either branch of a
    // rescue modifier callee, names a function rather than the implicit parameter.
    fn declare_callee(&mut self, callee: &Expr) -> Result<()> {
        self.work.charge(1)?;
        match &callee.node {
            Node::Var(name) if name == "it" => self.capture_name(name),
            Node::Try(attempt) if attempt.modifier => {
                let rescues = attempt.rescues.iter().flat_map(|rescue| rescue.body.iter());
                for stmt in attempt.body.iter().chain(rescues) {
                    if let Statement::Expr(expr) = &stmt.node {
                        self.declare_callee(expr)?;
                    } else {
                        self.declare(std::slice::from_ref(stmt))?;
                    }
                }
                self.declare(&attempt.alternate)?;
                self.declare(&attempt.ensure)
            }
            _ => self.declare_expr(callee),
        }
    }
    fn emit(&mut self, op: Op) -> usize {
        let pos = self.code.len();
        self.code.push(op);
        self.locations.push(self.offset);
        pos
    }
    fn patch(&mut self, pos: usize, target: usize) {
        match &mut self.code[pos] {
            Op::RaiseStart(_, n)
            | Op::Jump(n)
            | Op::JumpFalse(n)
            | Op::JumpTrue(n)
            | Op::JumpNil(n)
            | Op::AddressJumpNil(n, _)
            | Op::Bind(_, n)
            | Op::NamespaceConstant(_, n)
            | Op::NamespaceConstantAddress(_, n)
            | Op::FileValue(_, n)
            | Op::FileAddress(_, n)
            | Op::RootAddress(_, n)
            | Op::AmbientValue(_, n)
            | Op::AmbientAddress(_, n)
            | Op::TypeShadowed(_, n)
            | Op::AddressBound(_, n)
            | Op::ReceiverBound(_, n) => *n = target,
            _ => unreachable!(),
        }
    }
    fn constant(&mut self, v: Value) {
        let n = self.program.constants.len();
        self.program.constants.push(v);
        self.emit(Op::Constant(n));
    }
    fn integer_literal(&mut self, text: Value, radix: u32) {
        let n = self.program.constants.len();
        self.program.constants.push(text);
        self.emit(Op::Integer(n, radix));
    }
    fn block(&mut self, body: &[Stmt]) -> Result<()> {
        self.work.charge(1)?;
        if body.is_empty() {
            self.emit(Op::Nil);
        }
        for (i, stmt) in body.iter().enumerate() {
            if i > 0 {
                self.emit(Op::Pop);
            }
            self.stmt(stmt, false)?;
        }
        Ok(())
    }
    fn stmt(&mut self, stmt: &Stmt, expression: bool) -> Result<()> {
        self.work.charge(1)?;
        let previous = std::mem::replace(&mut self.offset, stmt.offset);
        let result = self.statement_at(stmt, expression);
        self.offset = previous;
        result
    }
    fn statement_at(&mut self, stmt: &Stmt, expression: bool) -> Result<()> {
        self.work.charge(1)?;
        if let Statement::Assign(target, _, _) = &stmt.node {
            let mut names = Buffer::new();
            target_names(target, &mut names, self.work)?;
            for name in names {
                if self.program.declaration_names.contains_key(name.as_str()) {
                    continue;
                }
                if let Some(&slot) = self.locals.get(self.work, name)? {
                    self.emit(Op::Declare(slot));
                }
            }
        }
        self.statement(stmt, expression)?;
        if !expression
            && matches!(
                stmt.node,
                Statement::If(..) | Statement::While(..) | Statement::For(..)
            )
        {
            for slot in self.statement_bindings(std::slice::from_ref(stmt))? {
                self.emit(Op::Declare(slot));
            }
        }
        Ok(())
    }
    fn assignment_rhs(&mut self, target: &Target, values: &[&Expr]) -> Result<()> {
        self.work.charge(1)?;
        let mut names = Buffer::new();
        target_names(target, &mut names, self.work)?;
        let mut calls = Table::new();
        for value in values {
            call_names(value, &mut calls, self.work)?;
        }
        let mut seen = Table::new();
        let mut slots = Buffer::new();
        for name in names {
            if let Some(&slot) = self.locals.get(self.work, name)? {
                if calls.contains(self.work, name)?
                    && seen.insert(self.work, name.clone(), ())?.is_none()
                {
                    slots.push(self.work, slot)?;
                }
            }
        }
        drop(calls);
        drop(seen);
        for &slot in &slots {
            self.emit(Op::Bypass(slot));
        }
        for value in values {
            self.expr(value)?;
        }
        if !slots.is_empty() {
            self.emit(Op::BypassEnd(slots.len()));
        }
        Ok(())
    }
    fn host_position(&self, name: &str) -> Result<Option<usize>> {
        for (index, host) in self.program.hosts.iter().enumerate() {
            self.work
                .bytes(name.len().min(host.len()).saturating_add(1))?;
            if host == name {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
    fn declaration_slot(&self, name: &str) -> Result<Option<usize>> {
        Ok(
            if self.namespace_binding(name)?
                || self.program.declaration_names.contains_key(name)
                || (Global::parse(name).is_some()
                    && !self.program.names.contains_key(name)
                    && self.host_position(name)?.is_none())
            {
                None
            } else {
                self.locals.get(self.work, name)?.copied()
            },
        )
    }
    fn statement_bindings(&self, body: &[Stmt]) -> Result<Buffer<usize>> {
        let mut names = Buffer::new();
        statement_names(body, &mut names, self.work)?;
        let mut seen = Table::new();
        let mut slots = Buffer::new();
        for name in names {
            self.work.bytes(name.len())?;
            if seen.insert(self.work, name.clone(), ())?.is_none() {
                if let Some(slot) = self.declaration_slot(name)? {
                    slots.push(self.work, slot)?;
                }
            }
        }
        Ok(slots)
    }
    fn loop_body(&mut self, body: &[Stmt]) -> Result<()> {
        self.work.charge(1)?;
        self.loop_bindings
            .push(self.work, self.statement_bindings(body)?)?;
        self.block(body)?;
        self.loop_bindings.pop();
        Ok(())
    }
    fn statement(&mut self, stmt: &Stmt, expression: bool) -> Result<()> {
        self.work.charge(1)?;
        match &stmt.node {
            Statement::Raise(value, message) => self.raise(value.as_deref(), message.as_deref())?,
            Statement::Retry => {
                self.emit(Op::Retry);
            }
            Statement::UnboundClass(name) => {
                let name = self.call_site(name, false).name;
                self.emit(Op::UnboundClass(name));
            }
            Statement::Module(name) => {
                self.initialize_namespaces(name)?;
                self.emit(Op::Nil);
            }
            Statement::Expr(e) => self.expr(e)?,
            Statement::Assign(target, op, rhs) => {
                let binding_target = target;
                let binary = match *op {
                    "+=" => Some("+"),
                    "-=" => Some("-"),
                    "*=" => Some("*"),
                    "/=" => Some("/"),
                    "%=" => Some("%"),
                    "**=" => Some("**"),
                    _ => None,
                };
                let Target::Value(target) = target else {
                    self.assignment_rhs(target, &[rhs])?;
                    self.assign_value(target)?;
                    return Ok(());
                };
                match &target.node {
                    Node::Var(name) => {
                        if self.namespace_binding(name)? {
                            self.namespace_assignment(name, binding_target, target, op, rhs)?;
                            return Ok(());
                        }
                        if let Some(global) = self.global_binding(name)? {
                            if matches!(*op, "||=" | "&&=") {
                                self.emit(Op::Global(global));
                                self.emit(Op::Dup);
                                let skip = self.emit(if *op == "||=" {
                                    Op::JumpTrue(0)
                                } else {
                                    Op::JumpFalse(0)
                                });
                                self.emit(Op::Pop);
                                self.assignment_rhs(binding_target, &[rhs])?;
                                self.emit(Op::StoreGlobal(global));
                                self.patch(skip, self.code.len());
                                return Ok(());
                            }
                            if binary.is_some() {
                                self.emit(Op::Global(global));
                            }
                            self.assignment_rhs(binding_target, &[rhs])?;
                            if let Some(op) = binary {
                                self.emit(Op::Binary(op));
                            }
                            self.emit(Op::StoreGlobal(global));
                            return Ok(());
                        }
                        let slot = self.slot(name)?;
                        if matches!(*op, "||=" | "&&=") {
                            self.expr(target)?;
                            self.emit(Op::Dup);
                            let skip = self.emit(if *op == "||=" {
                                Op::JumpTrue(0)
                            } else {
                                Op::JumpFalse(0)
                            });
                            self.emit(Op::Pop);
                            self.assignment_rhs(binding_target, &[rhs])?;
                            self.emit(Op::Store(slot));
                            self.patch(skip, self.code.len());
                            return Ok(());
                        }
                        if binary.is_none() && !self.program.file {
                            if let Node::Binary("+", left, right) = &rhs.node {
                                self.assignment_rhs(binding_target, &[left, right])?;
                                let instruction = self.emit(Op::AddStore(slot));
                                self.locations[instruction] = rhs.offset;
                                return Ok(());
                            }
                        }
                        if binary.is_some() {
                            self.expr(target)?;
                        }
                        self.assignment_rhs(binding_target, &[rhs])?;
                        if binary == Some("+") && !self.program.file {
                            self.emit(Op::AddStore(slot));
                            return Ok(());
                        }
                        if let Some(op) = binary {
                            self.emit(Op::Binary(op));
                        }
                        self.emit(Op::Store(slot));
                    }
                    Node::Index(..) | Node::Member(..) => {
                        if binary.is_none() && !matches!(*op, "||=" | "&&=") {
                            self.assignment_rhs(binding_target, &[rhs])?;
                            self.address_target(target, false)?;
                            self.emit(Op::AddressStore);
                        } else {
                            self.address_target(target, true)?;
                            if matches!(*op, "||=" | "&&=") {
                                self.emit(Op::Dup);
                                let skip = self.emit(if *op == "||=" {
                                    Op::JumpTrue(0)
                                } else {
                                    Op::JumpFalse(0)
                                });
                                self.emit(Op::Pop);
                                self.assignment_rhs(binding_target, &[rhs])?;
                                self.emit(Op::AddressStore);
                                let end = self.emit(Op::Jump(0));
                                self.patch(skip, self.code.len());
                                self.emit(Op::AddressDrop);
                                self.patch(end, self.code.len());
                            } else {
                                self.assignment_rhs(binding_target, &[rhs])?;
                                self.emit(Op::Binary(binary.unwrap()));
                                self.emit(Op::AddressStore);
                            }
                        }
                    }
                    _ => return Err(syntax::unsupported(self.work, "invalid assignment target")),
                }
            }
            Statement::If(cond, yes, no) => {
                self.expr(cond)?;
                let branch = self.emit(Op::JumpFalse(0));
                self.block(yes)?;
                let done = self.emit(Op::Jump(0));
                self.patch(branch, self.code.len());
                self.block(no)?;
                self.patch(done, self.code.len());
            }
            Statement::While(cond, body) => {
                let mark = self.emit(Op::LoopStart {
                    iterable: false,
                    expression,
                    next: 0,
                    end: 0,
                });
                let next = self.code.len();
                self.expr(cond)?;
                self.emit(Op::LoopTest);
                self.loop_body(body)?;
                self.emit(Op::LoopBody);
                let end = self.emit(Op::LoopEnd);
                self.code[mark] = Op::LoopStart {
                    iterable: false,
                    expression,
                    next,
                    end,
                };
            }
            Statement::For(target, iterable, body) => {
                self.expr(iterable)?;
                let mut names = Buffer::new();
                target_names(target, &mut names, self.work)?;
                for name in names {
                    if let Some(&slot) = self.locals.get(self.work, name)? {
                        self.emit(Op::Declare(slot));
                    }
                }
                let mark = self.emit(Op::LoopStart {
                    iterable: true,
                    expression,
                    next: 0,
                    end: 0,
                });
                let next = self.emit(Op::IterNext);
                self.assign_value(target)?;
                self.emit(Op::Pop);
                self.loop_body(body)?;
                self.emit(Op::LoopBody);
                let end = self.emit(Op::LoopEnd);
                self.code[mark] = Op::LoopStart {
                    iterable: true,
                    expression,
                    next,
                    end,
                };
            }
            Statement::Return(value) => {
                if let Some(e) = value {
                    self.expr(e)?;
                } else {
                    self.emit(Op::Nil);
                }
                self.emit(Op::Return);
            }
            Statement::Break(value) => {
                if let Some(value) = value {
                    if self.loop_bindings.is_empty() && self.outer.is_empty() {
                        self.emit(Op::LoopGuard(true));
                    }
                    self.expr(value)?;
                }
                self.emit(Op::Break(value.is_some()));
            }
            Statement::Next(value) => {
                if let Some(value) = value {
                    if self.loop_bindings.is_empty() && self.outer.is_empty() {
                        self.emit(Op::LoopGuard(false));
                    }
                    self.expr(value)?;
                }
                if let Some(bindings) = self.loop_bindings.last() {
                    for &slot in bindings {
                        self.work.charge(1)?;
                        self.code.push(Op::Declare(slot));
                        self.locations.push(self.offset);
                    }
                }
                self.emit(Op::Next(value.is_some()));
            }
        }
        Ok(())
    }
    fn initialize_namespaces(&mut self, name: &str) -> Result<()> {
        for index in 0..self.program.namespaces.len() {
            self.work.charge(1)?;
            let module = &self.program.namespaces[index];
            self.work.bytes(name.len().min(module.name.len()))?;
            if module.body.is_some()
                && module
                    .name
                    .strip_prefix(name)
                    .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with("::"))
            {
                self.emit(Op::InitNamespace(module.index));
            }
        }
        Ok(())
    }
    fn assign_value(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        let offset = target.offset().unwrap_or(self.offset);
        let previous = std::mem::replace(&mut self.offset, offset);
        let result = self.assign_value_at(target);
        self.offset = previous;
        result
    }
    fn assign_value_at(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        match target {
            Target::Typed(target, ty) => {
                let ty = self.annotation(ty)?;
                let mut text = Buffer::new();
                target_label(target, &mut text, self.work)?;
                if text.is_empty() {
                    text.extend_from_slice(self.work, b"destructured value")?;
                }
                let label = self.program.constants.len();
                self.program.constants.push(Value::bytes(&*text));
                drop(text);
                self.emit(Op::Normalize(ty, label));
                self.assign_value(target)?;
            }
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                if self.namespace_binding(name)? {
                    self.store_namespace_name(name);
                } else if let Some(global) = self.global_binding(name)? {
                    self.emit(Op::StoreGlobal(global));
                } else {
                    let slot = self.slot(name)?;
                    self.emit(Op::Store(slot));
                }
            }
            Target::Value(
                target @ Expr {
                    node: Node::Index(..) | Node::Member(..),
                    ..
                },
            ) => {
                self.address_target(target, false)?;
                self.emit(Op::AddressStore);
            }
            Target::Tuple(parts) => {
                let rest = parts.iter().position(|(_, rest)| *rest);
                for (i, (part, _)) in parts.iter().enumerate() {
                    let Some(part) = part else {
                        continue;
                    };
                    let select = match rest {
                        Some(pos) if i == pos => Selection::Rest {
                            leading: pos,
                            trailing: parts.len() - pos - 1,
                        },
                        Some(pos) if i > pos => Selection::Tail {
                            leading: pos,
                            trailing: parts.len() - pos - 1,
                            index: i - pos - 1,
                        },
                        _ => Selection::At(i),
                    };
                    self.emit(Op::Extract(select));
                    self.assign_value(part)?;
                    self.emit(Op::Pop);
                }
            }
            _ => return Err(syntax::unsupported(self.work, "invalid assignment target")),
        }
        Ok(())
    }
    fn expr(&mut self, e: &Expr) -> Result<()> {
        self.work.charge(1)?;
        let previous = std::mem::replace(&mut self.offset, e.offset);
        let result = self.expression(e);
        self.offset = previous;
        result
    }
    fn expression(&mut self, e: &Expr) -> Result<()> {
        self.work.charge(1)?;
        match &e.node {
            Node::Try(attempt) => self.attempt(attempt, false)?,
            Node::Regex(pattern, flags) => {
                self.work.bytes(pattern.len())?;
                let index = self.program.constants.len();
                self.program.constants.push(pattern.compiler_constant());
                self.emit(Op::Regex(index, *flags));
            }
            Node::Shape(ty, fallback, names) => {
                return self.shape_expression(ty, fallback.as_deref(), names);
            }
            Node::Integer(n) => {
                if let Ok(n) = i64::try_from(*n) {
                    self.constant(Value::int(n));
                } else {
                    let mut digits = [0; 20];
                    self.integer_literal(Value::bytes(decimal_digits(*n, &mut digits)), 10);
                }
            }
            Node::BigInteger(text, radix) => self.integer_literal(text.compiler_constant(), *radix),
            Node::Unary("-", value) if matches!(value.node, Node::Integer(n) if n == i64::MAX as u64 + 1) =>
            {
                self.constant(Value::int(i64::MIN));
            }
            Node::Literal(v) => self.constant(v.compiler_constant()),
            Node::Var(name) if name.starts_with('@') => {
                let name = self.call_site(name, false).name;
                self.emit(Op::NamespaceVariable(name, true));
            }
            Node::Var(name) if name == "self" && self.namespace.is_some() => {
                self.emit(Op::NamespaceSelf(self.namespace.unwrap()));
            }
            Node::Var(name) if name == "block_given?" => {
                self.emit(Op::BlockGiven(false, false));
            }
            Node::Var(name) => {
                return self.variable_expression(name);
            }
            Node::Array(values) => {
                for v in values {
                    self.expr(v)?;
                }
                self.emit(Op::Array(values.len()));
            }
            Node::Template(parts, symbol) => {
                self.emit(Op::TextStart);
                for part in parts {
                    self.expr(part)?;
                    let instruction = self.emit(Op::TextPart);
                    self.locations[instruction] = part.offset;
                }
                self.emit(Op::TextEnd(*symbol));
            }
            Node::Hash(values) => {
                for (k, v) in values {
                    self.constant(k.compiler_constant());
                    self.expr(v)?;
                }
                self.emit(Op::Hash(values.len()));
            }
            Node::Unary(op, v) => {
                self.expr(v)?;
                self.emit(Op::Unary(op));
            }
            Node::Range(start, end, exclusive) => {
                if let Some(start) = start {
                    self.expr(start)?;
                    if end.is_some() {
                        // The start converts before the end runs, as in Go.
                        self.emit(Op::RangeStart);
                    }
                }
                if let Some(end) = end {
                    self.expr(end)?;
                }
                self.emit(Op::Range(start.is_some(), end.is_some(), *exclusive));
            }
            Node::Conditional(cond, yes, no) => {
                self.expr(cond)?;
                let branch = self.emit(Op::JumpFalse(0));
                self.expr(yes)?;
                let done = self.emit(Op::Jump(0));
                self.patch(branch, self.code.len());
                self.expr(no)?;
                self.patch(done, self.code.len());
            }
            Node::Loop(stmt) => self.stmt(stmt, true)?,
            Node::Case(target, clauses, alternate) => {
                return self.case_expression(target.as_deref(), clauses, alternate.as_deref());
            }
            Node::Binary("<<", a, b) => {
                self.address(a)?;
                self.expr(b)?;
                let site = self.call_site("push", false);
                self.emit(Op::Shovel(site));
            }
            Node::Binary(op, a, b) => {
                self.expr(a)?;
                if matches!(*op, "&&" | "||") {
                    self.emit(Op::Dup);
                    let jump = self.emit(if *op == "&&" {
                        Op::JumpFalse(0)
                    } else {
                        Op::JumpTrue(0)
                    });
                    self.emit(Op::Pop);
                    self.expr(b)?;
                    self.patch(jump, self.code.len());
                } else {
                    self.expr(b)?;
                    self.emit(Op::Binary(op));
                }
            }
            Node::Call(name, args, _) if name == "block_given?" => {
                self.emit(Op::BlockGiven(!args.is_empty(), false));
            }
            Node::Yield(args) => {
                self.emit(Op::CheckBlock);
                for arg in args {
                    self.expr(arg)?;
                }
                self.emit(Op::Yield(args.len()));
            }
            Node::BlockCall(call, block) => self.block_call(call, block)?,
            Node::ComputedCall(call, args) => self.computed_call(call, args, None)?,
            Node::Call(name, args, form) => self.named_call(name, args, *form)?,
            Node::Member(recv, name) | Node::SafeMember(recv, name) => {
                self.member_call(
                    recv,
                    name,
                    &[],
                    CallForm::Auto,
                    None,
                    matches!(e.node, Node::SafeMember(..)),
                )?;
            }
            Node::Scope(recv, name, args) => self.scoped_call(recv, name, args.as_deref(), None)?,
            Node::Method(recv, name, args, form) | Node::SafeMethod(recv, name, args, form) => {
                self.member_call(
                    recv,
                    name,
                    args,
                    *form,
                    None,
                    matches!(e.node, Node::SafeMethod(..)),
                )?;
            }
            Node::Index(value, index) => {
                self.expr(value)?;
                for index in index {
                    self.expr(index)?;
                }
                self.emit(Op::Index(index.len()));
            }
        }
        Ok(())
    }
    fn shape_expression(
        &mut self,
        ty: &crate::compilation::Type,
        fallback: Option<&Expr>,
        names: &[crate::compilation::Name],
    ) -> Result<()> {
        self.work.names(names)?;
        let guard = fallback.map(|_| {
            let index = self.program.type_guards.len();
            self.program
                .type_guards
                .push(names.iter().map(|name| name.as_str().to_owned()).collect());
            self.emit(Op::TypeShadowed(index, 0))
        });
        self.constant(crate::shapes::compile(ty.compile(self.work)?));
        if let Some(fallback) = fallback {
            let done = self.emit(Op::Jump(0));
            self.patch(guard.unwrap(), self.code.len());
            self.expr(fallback)?;
            self.patch(done, self.code.len());
        }
        Ok(())
    }
    fn variable_expression(&mut self, name: &str) -> Result<()> {
        let global = self.global(name);
        let constant = (self.namespace.is_some()
            && name.chars().next().is_some_and(syntax::unicode::upper)
            && !self.locals.contains(self.work, name)?)
        .then(|| {
            let name = self.call_site(name, false).name;
            self.emit(Op::NamespaceConstant(name, 0))
        });
        let ambient =
            (self.namespace.is_some() && !self.locals.contains(self.work, name)?).then(|| {
                let name = self.call_site(name, false).name;
                self.emit(Op::AmbientValue(name, 0))
            });
        let file = (self.program.file && !self.locals.contains(self.work, name)?).then(|| {
            let name = self.call_site(name, false).name;
            self.emit(Op::FileValue(name, 0))
        });
        if let Some(&slot) = self.locals.get(self.work, name)? {
            if !self.parameters.contains(self.work, name)? {
                let name = self.call_site(name, false).name;
                self.emit(Op::LoadOptional(slot, name));
            } else {
                self.emit(Op::Load(slot));
            }
        } else if let Some(&index) = self.program.declaration_names.get(name) {
            self.emit(Op::Declaration(index));
        } else if let Some(&fun) = self.program.names.get(name) {
            self.emit(Op::AutoCall(fun));
        } else if let Some(host) = self.host_position(name)? {
            self.emit(Op::HostValue(host));
        } else if let Some(global) = global {
            self.emit(Op::Global(global));
        } else {
            let site = self.call_site(name, false);
            self.emit(Op::Unbound(site.name));
        }
        if let Some(constant) = constant {
            self.patch(constant, self.code.len());
        }
        if let Some(ambient) = ambient {
            self.patch(ambient, self.code.len());
        }
        if let Some(file) = file {
            self.patch(file, self.code.len());
        }
        Ok(())
    }
    fn case_expression(
        &mut self,
        target: Option<&Expr>,
        clauses: &[syntax::When],
        alternate: Option<&Expr>,
    ) -> Result<()> {
        if let Some(target) = target {
            self.expr(target)?;
        }
        let mut completed = Buffer::new();
        for clause in clauses {
            let mut matches = Buffer::new();
            for (value, splat) in &clause.values {
                if target.is_some() {
                    self.emit(Op::Dup);
                }
                self.expr(value)?;
                self.emit(Op::CaseCompare(target.is_some(), *splat));
                matches.push(self.work, self.emit(Op::JumpTrue(0)))?;
            }
            let next = self.emit(Op::Jump(0));
            for matched in matches {
                self.patch(matched, self.code.len());
            }
            if target.is_some() {
                self.emit(Op::Pop);
            }
            self.expr(&clause.result)?;
            completed.push(self.work, self.emit(Op::Jump(0)))?;
            self.patch(next, self.code.len());
        }
        if target.is_some() {
            self.emit(Op::Pop);
        }
        if let Some(alternate) = alternate {
            self.expr(alternate)?;
        } else {
            self.emit(Op::Nil);
        }
        for completed in completed {
            self.patch(completed, self.code.len());
        }
        Ok(())
    }
    fn call_site(&mut self, name: &str, auto: bool) -> CallSite {
        let index = self.program.members.len();
        self.program.members.push(name.to_owned());
        CallSite {
            name: index,
            method: Method::parse(name),
            auto,
            parenthesized: !auto,
            scope: false,
        }
    }
    fn annotation(&mut self, ty: &crate::compilation::Type) -> Result<usize> {
        let index = self.program.types.len();
        self.program.types.push(ty.compile(self.work)?);
        Ok(index)
    }
    fn global(&mut self, name: &str) -> Option<usize> {
        if self.program.declaration_names.contains_key(name) {
            return None;
        }
        let namespace = Global::parse(name)?;
        if let Some(index) = self
            .program
            .globals
            .iter()
            .position(|(kind, _)| *kind == namespace)
        {
            return Some(index);
        }
        let index = self.program.globals.len();
        self.program.globals.push((namespace, namespace.value()));
        Some(index)
    }
    fn member_call(
        &mut self,
        receiver: &Expr,
        name: &str,
        args: &[Argument],
        form: CallForm,
        block: Option<usize>,
        safe: bool,
    ) -> Result<()> {
        self.work.charge(1)?;
        let forwarding = crate::members::forwarding::supported(name);
        let mutating = mutating_member(name) || forwarding;
        if mutating {
            self.address(receiver)?;
        } else {
            self.member_receiver(receiver, name != "call")?;
        }
        let skip = safe.then(|| {
            self.emit(if mutating {
                Op::AddressJumpNil(0, true)
            } else {
                Op::JumpNil(0)
            })
        });
        let mut site = self.call_site(name, form == CallForm::Auto);
        site.parenthesized = form == CallForm::Parenthesized;
        if !forwarding && name != "call" && (mutating || form != CallForm::Auto) {
            self.emit(Op::PrepareMember(site, mutating));
        }
        if name == "call" && form != CallForm::Auto {
            self.emit(Op::Arguments);
            self.emit(Op::CallMember(site));
            self.argument_values(args)?;
            if let Some(block) = block {
                self.emit(Op::Attach(block));
            }
            self.emit(Op::Invoke(Invocation::Resolved));
            if let Some(skip) = skip {
                self.patch(skip, self.code.len());
            }
            return Ok(());
        }
        if expanded(args)
            || block.is_some()
            || crate::iteration::method(name)
            || name == "is_type?"
            || forwarding
        {
            if forwarding {
                self.emit(Op::ForwardArguments);
                self.argument_values(args)?;
            } else {
                self.call_arguments(args)?;
            }
            if let Some(block) = block {
                self.emit(Op::Attach(block));
            }
            self.emit(Op::Invoke(Invocation::Member(site, mutating)));
        } else {
            for arg in args {
                self.expr(&arg.value)?;
            }
            self.emit(if mutating {
                Op::Mutate(site, args.len())
            } else {
                Op::Method(site, args.len())
            });
        }
        if let Some(skip) = skip {
            self.patch(skip, self.code.len());
        }
        Ok(())
    }
    fn member_receiver(&mut self, receiver: &Expr, auto: bool) -> Result<()> {
        self.work.charge(1)?;
        if let Node::Var(name) = &receiver.node {
            if let Some(&slot) = self.locals.get(self.work, name.as_str())? {
                let bound = self.emit(Op::ReceiverBound(slot, 0));
                self.expr(receiver)?;
                self.patch(bound, self.code.len());
                return Ok(());
            }
            if let Some(global) = self.global_fallback(name)? {
                self.emit(Op::GlobalReceiver(global, auto));
                return Ok(());
            }
        }
        self.expr(receiver)
    }
    fn scoped_call(
        &mut self,
        receiver: &Expr,
        name: &str,
        args: Option<&[Argument]>,
        block: Option<usize>,
    ) -> Result<()> {
        self.work.charge(1)?;
        self.expr(receiver)?;
        let mut site = self.call_site(name, args.is_none() && block.is_none());
        site.scope = true;
        let args = args.unwrap_or(&[]);
        if expanded(args) || block.is_some() {
            self.call_arguments(args)?;
            if let Some(block) = block {
                self.emit(Op::Attach(block));
            }
            self.emit(Op::Invoke(Invocation::Member(site, false)));
        } else {
            for arg in args {
                self.expr(&arg.value)?;
            }
            self.emit(Op::Method(site, args.len()));
        }
        Ok(())
    }
    fn block_call(&mut self, call: &Expr, block: &Block) -> Result<()> {
        self.work.charge(1)?;
        let function = self.compile_block(block)?;
        let (name, args, form) = match &call.node {
            Node::Var(name) => (name.as_str(), &[][..], CallForm::Bare),
            Node::Call(name, args, form) => (name.as_str(), args.as_slice(), *form),
            Node::Member(receiver, name) | Node::SafeMember(receiver, name) => {
                return self.member_call(
                    receiver,
                    name,
                    &[],
                    CallForm::Bare,
                    Some(function),
                    matches!(call.node, Node::SafeMember(..)),
                );
            }
            Node::Method(receiver, name, args, form)
            | Node::SafeMethod(receiver, name, args, form) => {
                return self.member_call(
                    receiver,
                    name,
                    args,
                    *form,
                    Some(function),
                    matches!(call.node, Node::SafeMethod(..)),
                );
            }
            Node::Scope(receiver, name, args) => {
                return self.scoped_call(receiver, name, args.as_deref(), Some(function));
            }
            Node::ComputedCall(call, args) => {
                return self.computed_call(call, args, Some(function));
            }
            _ => {
                return self.computed_call(call, &[], Some(function));
            }
        };
        if name == "block_given?" {
            self.emit(Op::BlockGiven(!args.is_empty(), true));
            return Ok(());
        }
        if self.program.file || self.namespace.is_some() {
            self.global(name);
            let slot = self
                .locals
                .get(self.work, name)?
                .copied()
                .unwrap_or(usize::MAX);
            let name = self.call_site(name, false).name;
            self.emit(Op::ResolveCall(slot, name, form == CallForm::Parenthesized));
            self.argument_values(args)?;
            self.emit(Op::Attach(function));
            self.emit(Op::Invoke(Invocation::Resolved));
            return Ok(());
        }
        let target = if let Some(&slot) = self.locals.get(self.work, name)? {
            let name = self.call_site(name, false).name;
            self.emit(Op::ResolveCall(slot, name, form == CallForm::Parenthesized));
            Invocation::Resolved
        } else if let Some(global) = self.global_binding(name)? {
            self.emit(Op::ResolveGlobalCall(global));
            Invocation::Resolved
        } else {
            let target = if self.program.declaration_names.contains_key(name) {
                Invocation::NonCallable
            } else if let Some(&function) = self.program.names.get(name) {
                Invocation::Function(function)
            } else if let Some(host) = self.host_position(name)? {
                Invocation::Host(host)
            } else {
                let site = self.call_site(name, false);
                self.emit(Op::ResolveCall(
                    usize::MAX,
                    site.name,
                    form == CallForm::Parenthesized,
                ));
                self.argument_values(args)?;
                self.emit(Op::Attach(function));
                self.emit(Op::Invoke(Invocation::Resolved));
                return Ok(());
            };
            let name = self.call_site(name, false).name;
            self.emit(Op::RootCall(name, true));
            target
        };
        self.argument_values(args)?;
        self.emit(Op::Attach(function));
        self.emit(Op::InvokeRoot(target));
        Ok(())
    }
    fn compile_block(&mut self, block: &Block) -> Result<usize> {
        self.work.charge(1)?;
        let mut block_arity = block.params.len();
        let mut outer = Buffer::new();
        for scope in std::iter::once(&self.locals).chain(&self.outer) {
            outer.push(self.work, scope.copy(self.work)?)?;
        }
        let mut child = Compiler {
            work: self.work,
            instance: self.instance,
            namespace: self.namespace,
            program: self.program,
            locals: Table::new(),
            slots: 0,
            code: Vec::new(),
            locations: Vec::new(),
            offset: self.offset,
            parameters: Table::new(),
            loop_bindings: Buffer::new(),
            outer,
            reads: Table::new(),
            assigned: Table::new(),
        };
        child.declare(&block.body)?;
        for target in &block.params {
            child.declare_target(target)?;
            let mut names = Buffer::new();
            target_names(target, &mut names, self.work)?;
            for name in names {
                let slot = child.slot(name)?;
                child.parameters.insert(self.work, name.clone(), ())?;
                child.emit(Op::Shadow(slot));
            }
        }
        for (index, target) in block.params.iter().enumerate() {
            child.emit(Op::BlockArg(index, block.params.len() > 1));
            child.assign_value(target)?;
            child.emit(Op::Pop);
        }
        if block.implicit {
            let candidates = ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| (name, index))
                .chain(block.infer_it.then_some(("it", 0)));
            for (name, index) in candidates {
                if child.reads.contains(child.work, name)?
                    && !child.assigned.contains(child.work, name)?
                {
                    block_arity = block_arity.max(index + 1);
                    let name = Name::new(self.work, name)?;
                    let slot = child.slot(&name)?;
                    child.parameters.insert(self.work, name, ())?;
                    child.emit(Op::Shadow(slot));
                    child.emit(Op::BlockArg(index, false));
                    child.emit(Op::Store(slot));
                    child.emit(Op::Pop);
                }
            }
        }
        child.block(&block.body)?;
        let finish = child.emit(Op::Finish);
        child.locations[finish] = block.body.last().map_or(self.offset, |stmt| stmt.offset);
        debug_assert_eq!(child.code.len(), child.locations.len());
        let mut captures = vec![None; child.slots];
        for (name, &slot) in child.locals.iter(self.work)? {
            self.work.bytes(name.len())?;
            if name.starts_with('\0') {
                continue;
            }
            captures[slot] = child.outer_binding(name)?;
        }
        let function = Function {
            offset: self.offset,
            locations: child.locations,
            trace_name: "<block>".into(),
            instance: self.instance,
            namespace: self.namespace,
            name: "<block>".into(),
            locals: child.slots,
            local_names: local_names(&child.locals, child.slots, self.work)?,
            code: child.code,
            captures,
            block_arity,
            ..Function::default()
        };
        let index = self.program.functions.len();
        self.program.functions.push(function);
        Ok(index)
    }
    fn call_arguments(&mut self, args: &[Argument]) -> Result<()> {
        self.work.charge(1)?;
        self.emit(Op::Arguments);
        self.argument_values(args)
    }
    fn argument_values(&mut self, args: &[Argument]) -> Result<()> {
        self.work.charge(1)?;
        for arg in args {
            self.expr(&arg.value)?;
            let kind = match &arg.kind {
                ArgumentKind::Positional => ArgumentOp::Positional,
                ArgumentKind::Splat => ArgumentOp::Splat,
                ArgumentKind::Keyword(name) => {
                    ArgumentOp::Keyword(self.call_site(name, false).name)
                }
                ArgumentKind::KeywordSplat => ArgumentOp::KeywordSplat,
            };
            self.emit(Op::Argument(kind));
        }
        Ok(())
    }
    fn address_target(&mut self, target: &Expr, read: bool) -> Result<()> {
        self.work.charge(1)?;
        let previous = std::mem::replace(&mut self.offset, target.offset);
        let result = self.address_target_at(target, read);
        self.offset = previous;
        result
    }
    fn address_target_at(&mut self, target: &Expr, read: bool) -> Result<()> {
        self.work.charge(1)?;
        match &target.node {
            Node::Index(receiver, indices) => {
                self.assignment_address(receiver)?;
                for index in indices {
                    self.expr(index)?;
                }
                self.emit(Op::AddressTarget(indices.len(), read));
            }
            Node::Member(receiver, name) => {
                self.assignment_address(receiver)?;
                let site = self.call_site(name, true);
                self.emit(Op::AddressMemberTarget(site, read));
            }
            _ => return Err(syntax::unsupported(self.work, "invalid assignment target")),
        }
        Ok(())
    }
    fn global_binding(&mut self, name: &str) -> Result<Option<usize>> {
        Ok(
            if self.locals.contains(self.work, name)? || self.outer_binding(name)?.is_some() {
                None
            } else {
                self.global_fallback(name)?
            },
        )
    }
    fn global_fallback(&mut self, name: &str) -> Result<Option<usize>> {
        Ok(
            if self.program.names.contains_key(name) || self.host_position(name)?.is_some() {
                None
            } else {
                self.global(name)
            },
        )
    }
    fn address(&mut self, receiver: &Expr) -> Result<()> {
        self.address_root(receiver, false)
    }
    /// Addresses `receiver`. An assignment root in an instance method sets `constant`
    /// so that it writes an existing class constant in place, unless a bound local
    /// of the same name takes precedence.
    pub(super) fn address_root(&mut self, receiver: &Expr, constant: bool) -> Result<()> {
        self.work.charge(1)?;
        let previous = std::mem::replace(&mut self.offset, receiver.offset);
        let result = self.address_at(receiver, constant);
        self.offset = previous;
        result
    }
    fn address_at(&mut self, receiver: &Expr, constant: bool) -> Result<()> {
        self.work.charge(1)?;
        let constant = match &receiver.node {
            Node::Var(name)
                if constant
                    && self.instance
                    && self.namespace.is_some()
                    && name.chars().next().is_some_and(syntax::unicode::upper)
                    && !self.parameters.contains(self.work, name.as_str())? =>
            {
                Some(self.call_site(name, false).name)
            }
            _ => None,
        };
        let local = match &receiver.node {
            Node::Var(name) => self.locals.contains(self.work, name.as_str())?,
            _ => false,
        };
        let early = constant
            .filter(|_| !local)
            .map(|name| self.emit(Op::NamespaceConstantAddress(name, 0)));
        let root = if let Node::Var(name) = &receiver.node {
            if !name.starts_with('@') && !self.locals.contains(self.work, name.as_str())? {
                let name = self.call_site(name, false).name;
                Some(self.emit(Op::RootAddress(name, 0)))
            } else {
                None
            }
        } else {
            None
        };
        let file = if self.program.file {
            if let Node::Var(name) = &receiver.node {
                if !name.starts_with('@') && !self.parameters.contains(self.work, name.as_str())? {
                    let name = self.call_site(name, false).name;
                    Some(self.emit(Op::FileAddress(name, 0)))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        match &receiver.node {
            Node::Var(name) if name.starts_with('@') => {
                let name = self.call_site(name, false).name;
                self.emit(Op::NamespaceAddress(name, true));
            }
            Node::Var(name) if self.locals.contains(self.work, name.as_str())? => {
                let slot = *self.locals.get(self.work, name)?.unwrap();
                if self.parameters.contains(self.work, name.as_str())? {
                    self.emit(Op::AddressLocal(slot));
                } else {
                    let bound = self.emit(Op::AddressBound(slot, 0));
                    let constant =
                        constant.map(|name| self.emit(Op::NamespaceConstantAddress(name, 0)));
                    let ambient = self.namespace.map(|_| {
                        let name = self.call_site(name, false).name;
                        self.emit(Op::AmbientAddress(name, 0))
                    });
                    if let Some(global) = self.global_fallback(name)? {
                        self.emit(Op::AddressGlobal(global));
                    } else {
                        self.expr(receiver)?;
                        self.emit(Op::AddressValue);
                    }
                    self.patch(bound, self.code.len());
                    for jump in constant.into_iter().chain(ambient) {
                        self.patch(jump, self.code.len());
                    }
                }
            }
            Node::Var(name) if self.namespace.is_some() => {
                let index = self.call_site(name, false).name;
                let ambient = self.emit(Op::AmbientAddress(index, 0));
                if self.global_fallback(name)?.is_some() {
                    self.emit(Op::NamespaceAddress(index, false));
                } else {
                    self.expr(receiver)?;
                    self.emit(Op::AddressValue);
                }
                self.patch(ambient, self.code.len());
            }
            Node::Var(name) if self.global_fallback(name)?.is_some() => {
                let global = self.global_fallback(name)?.unwrap();
                self.emit(Op::AddressGlobal(global));
            }
            Node::Member(root, name) | Node::SafeMember(root, name) => {
                self.address(root)?;
                let skip = matches!(receiver.node, Node::SafeMember(..))
                    .then(|| self.emit(Op::AddressJumpNil(0, false)));
                let site = self.call_site(name, true);
                self.emit(Op::AddressMember(site));
                if let Some(skip) = skip {
                    self.patch(skip, self.code.len());
                }
            }
            Node::Index(root, indices) => {
                self.address(root)?;
                for index in indices {
                    self.expr(index)?;
                }
                self.emit(Op::AddressIndex(indices.len()));
            }
            _ => {
                self.expr(receiver)?;
                self.emit(Op::AddressValue);
            }
        }
        for jump in [early, root, file].into_iter().flatten() {
            self.patch(jump, self.code.len());
        }
        Ok(())
    }
}

fn call_names(
    expr: &Expr,
    names: &mut Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    match &expr.node {
        Node::Try(attempt) => {
            block_call_names(&attempt.body, names, work)?;
            for rescue in &attempt.rescues {
                block_call_names(&rescue.body, names, work)?;
            }
            block_call_names(&attempt.alternate, names, work)?;
            block_call_names(&attempt.ensure, names, work)?;
        }
        Node::Shape(_, fallback, _) => {
            if let Some(fallback) = fallback {
                call_names(fallback, names, work)?;
            }
        }
        Node::Regex(..)
        | Node::Literal(_)
        | Node::Integer(_)
        | Node::BigInteger(..)
        | Node::Var(_) => (),
        Node::Call(name, args, _) => {
            names.insert(work, name.clone(), ())?;
            for arg in args {
                call_names(&arg.value, names, work)?;
            }
        }
        Node::BlockCall(call, block) => {
            if let Node::Var(name) = &call.node {
                names.insert(work, name.clone(), ())?;
            }
            call_names(call, names, work)?;
            block_call_names(&block.body, names, work)?;
        }
        Node::Array(items) | Node::Yield(items) | Node::Template(items, _) => {
            for item in items {
                call_names(item, names, work)?;
            }
        }
        Node::Hash(entries) => {
            for (_, item) in entries {
                call_names(item, names, work)?;
            }
        }
        Node::Unary(_, value) | Node::Member(value, _) | Node::SafeMember(value, _) => {
            call_names(value, names, work)?
        }
        Node::Binary(_, a, b) => {
            call_names(a, names, work)?;
            call_names(b, names, work)?;
        }
        Node::Conditional(a, b, c) => {
            call_names(a, names, work)?;
            call_names(b, names, work)?;
            call_names(c, names, work)?;
        }
        Node::Range(a, b, _) => {
            for item in a.iter().chain(b) {
                call_names(item, names, work)?;
            }
        }
        Node::Case(target, clauses, alternate) => {
            for item in target.iter().chain(alternate) {
                call_names(item, names, work)?;
            }
            for clause in clauses {
                for (item, _) in &clause.values {
                    call_names(item, names, work)?;
                }
                call_names(&clause.result, names, work)?;
            }
        }
        Node::Loop(stmt) => block_call_names(std::slice::from_ref(stmt), names, work)?,
        Node::Method(receiver, _, args, _)
        | Node::SafeMethod(receiver, _, args, _)
        | Node::ComputedCall(receiver, args) => {
            call_names(receiver, names, work)?;
            for arg in args {
                call_names(&arg.value, names, work)?;
            }
        }
        Node::Scope(receiver, _, args) => {
            call_names(receiver, names, work)?;
            for arg in args.iter().flatten() {
                call_names(&arg.value, names, work)?;
            }
        }
        Node::Index(receiver, args) => {
            call_names(receiver, names, work)?;
            for arg in args {
                call_names(arg, names, work)?;
            }
        }
    }
    Ok(())
}

fn target_call_names(
    target: &Target,
    names: &mut Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    match target {
        Target::Typed(target, _) => target_call_names(target, names, work)?,
        Target::Value(expr) => call_names(expr, names, work)?,
        Target::Tuple(parts) => {
            for (part, _) in parts {
                work.charge(1)?;
                if let Some(part) = part {
                    target_call_names(part, names, work)?;
                }
            }
        }
    }
    Ok(())
}

fn block_call_names(
    body: &[Stmt],
    names: &mut Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    for stmt in body {
        work.charge(1)?;
        match &stmt.node {
            Statement::Module(_) | Statement::UnboundClass(_) | Statement::Retry => (),
            Statement::Raise(value, message) => {
                for value in value.iter().chain(message) {
                    call_names(value, names, work)?;
                }
            }
            Statement::Expr(expr) => call_names(expr, names, work)?,
            Statement::Assign(target, _, value) => {
                target_call_names(target, names, work)?;
                call_names(value, names, work)?;
            }
            Statement::If(condition, yes, no) => {
                call_names(condition, names, work)?;
                block_call_names(yes, names, work)?;
                block_call_names(no, names, work)?;
            }
            Statement::While(condition, body) => {
                call_names(condition, names, work)?;
                block_call_names(body, names, work)?;
            }
            Statement::For(target, source, body) => {
                target_call_names(target, names, work)?;
                call_names(source, names, work)?;
                block_call_names(body, names, work)?;
            }
            Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
                if let Some(value) = value {
                    call_names(value, names, work)?;
                }
            }
        }
    }
    Ok(())
}

fn target_label(
    target: &Target,
    text: &mut Buffer<u8>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    match target {
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => text.extend_from_slice(work, name.as_bytes())?,
        Target::Typed(target, ty) => {
            target_label(target, text, work)?;
            text.extend_from_slice(work, b": ")?;
            crate::shapes::format(ty, &mut (work, text))?;
        }
        Target::Tuple(parts) => {
            text.push(work, b'(')?;
            for (index, (target, rest)) in parts.iter().enumerate() {
                if index > 0 {
                    text.extend_from_slice(work, b", ")?;
                }
                if *rest {
                    text.push(work, b'*')?;
                }
                if let Some(target) = target {
                    target_label(target, text, work)?;
                }
            }
            text.push(work, b')')?;
        }
        _ => (),
    }
    Ok(())
}

fn decimal_digits(mut value: u64, digits: &mut [u8; 20]) -> &str {
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    std::str::from_utf8(&digits[start..]).unwrap()
}

fn target_names<'a>(
    target: &'a Target,
    names: &mut Buffer<&'a Name>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    match target {
        Target::Typed(target, _) => target_names(target, names, work)?,
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => names.push(work, name)?,
        Target::Tuple(parts) => {
            for (part, _) in parts {
                work.charge(1)?;
                if let Some(part) = part {
                    target_names(part, names, work)?;
                }
            }
        }
        _ => (),
    }
    Ok(())
}

fn statement_names<'a>(
    body: &'a [Stmt],
    names: &mut Buffer<&'a Name>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    work.charge(1)?;
    for stmt in body {
        work.charge(1)?;
        match &stmt.node {
            Statement::Expr(Expr {
                node: Node::Try(attempt),
                ..
            }) => {
                statement_names(&attempt.body, names, work)?;
                for rescue in &attempt.rescues {
                    let mut scoped = Buffer::new();
                    statement_names(&rescue.body, &mut scoped, work)?;
                    for name in scoped {
                        if Some(name) != rescue.binding.as_ref() {
                            names.push(work, name)?;
                        }
                    }
                }
                statement_names(&attempt.alternate, names, work)?;
                statement_names(&attempt.ensure, names, work)?;
            }
            Statement::Assign(target, _, _) => target_names(target, names, work)?,
            Statement::If(_, yes, no) => {
                statement_names(yes, names, work)?;
                statement_names(no, names, work)?;
            }
            Statement::While(_, body) => statement_names(body, names, work)?,
            Statement::For(target, _, body) => {
                target_names(target, names, work)?;
                statement_names(body, names, work)?;
            }
            _ => (),
        }
    }
    Ok(())
}

pub(crate) fn mutating_member(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "append"
            | "prepend"
            | "unshift"
            | "pop"
            | "shift"
            | "delete"
            | "delete_if"
            | "keep_if"
            | "insert"
            | "clear"
            | "fill"
            | "store"
            | "replace"
    )
}
