use crate::{
    Result, Value,
    builtin::{Builtin, Global},
    syntax::{self, Argument, ArgumentKind, Block, Expr, Node, ParamKind, Stmt, Target},
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    Global(usize),
    StoreGlobal(usize),
    ResolveGlobalCall(usize),
    AddressGlobal(usize),
    Integer(usize, u32),
    Constant(usize),
    Nil,
    Load(usize),
    LoadOptional(usize, usize),
    Unbound(usize),
    NonCallable,
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
    AddStore(usize),
    Array(usize),
    TextStart,
    TextPart,
    TextEnd(bool),
    Hash(usize),
    Range(bool, bool, bool),
    Index(usize),
    AddressLocal(usize),
    AddressBound(usize, usize),
    AddressValue,
    AddressIndex(usize),
    AddressTarget(usize, bool),
    AddressMember(CallSite),
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
    Break(bool),
    Next(bool),
    Call(usize, usize),
    AutoCall(usize),
    Host(usize, usize),
    HostValue(usize),
    Method(CallSite, usize),
    Arguments,
    ResolveCall(usize, usize),
    Bypass(usize),
    BypassEnd(usize),
    Argument(ArgumentOp),
    Invoke(Invocation),
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
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
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSite {
    pub name: usize,
    pub method: Option<Method>,
    pub auto: bool,
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
    Codepoints,
    StartWith,
    EndWith,
    IsNil,
    Itself,
    ByteSize,
    Upcase,
    Downcase,
    Include,
    Index,
    Rindex,
    Strip,
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
            "codepoints" => Self::Codepoints,
            "start_with?" => Self::StartWith,
            "end_with?" => Self::EndWith,
            "nil?" => Self::IsNil,
            "itself" => Self::Itself,
            "bytesize" => Self::ByteSize,
            "upcase" => Self::Upcase,
            "downcase" => Self::Downcase,
            "include?" => Self::Include,
            "index" | "find_index" => Self::Index,
            "rindex" => Self::Rindex,
            "strip" => Self::Strip,
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
    pub name: String,
    pub params: Vec<Parameter>,
    pub defaults: bool,
    pub plain: bool,
    pub locals: usize,
    pub code: Vec<Op>,
    pub captures: Vec<Option<Capture>>,
    pub block_arity: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capture {
    pub depth: usize,
    pub slot: usize,
}
#[derive(Debug)]
pub(crate) struct Program {
    pub globals: Vec<(Global, Value)>,
    pub functions: Vec<Function>,
    pub constants: Vec<Value>,
    pub names: HashMap<String, usize>,
    pub hosts: Vec<String>,
    pub members: Vec<String>,
}

pub(crate) fn compile(source: &str, hosts: Vec<String>) -> Result<Program> {
    let defs = syntax::parse(source)?;
    let names = defs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.clone(), i))
        .collect();
    let mut program = Program {
        globals: Vec::new(),
        functions: (0..defs.len()).map(|_| Function::default()).collect(),
        constants: Vec::new(),
        names,
        hosts,
        members: Vec::new(),
    };
    for (index, def) in defs.into_iter().enumerate() {
        let mut c = Compiler {
            program: &mut program,
            locals: HashMap::new(),
            code: Vec::new(),
            parameters: HashSet::new(),
            loop_bindings: Vec::new(),
            outer: Vec::new(),
            reads: HashSet::new(),
            assigned: HashSet::new(),
        };
        let defaults = def.params.iter().any(|p| p.default.is_some());
        let plain = !defaults && def.params.iter().all(|p| p.kind == ParamKind::Positional);
        let mut params = Vec::new();
        for (i, param) in def.params.iter().enumerate() {
            let bind = defaults.then(|| c.emit(Op::Bind(i, 0)));
            if let Some(value) = &param.default {
                c.declare_expr(value);
                c.expr(value)?;
            }
            let slot = c.slot(&param.name);
            if param.default.is_some() {
                c.emit(Op::Store(slot));
                c.emit(Op::Pop);
            }
            if let Some(bind) = bind {
                c.patch(bind, c.code.len());
            }
            c.parameters.insert(param.name.clone());
            params.push(Parameter {
                name: param.name.clone(),
                kind: param.kind,
                default: param.default.is_some(),
                slot,
            });
        }
        if defaults {
            c.emit(Op::BindEnd);
        }
        c.declare(&def.body);
        c.block(&def.body)?;
        c.code.push(Op::Finish);
        let function = Function {
            name: def.name,
            params,
            defaults,
            plain,
            locals: c.locals.len(),
            code: c.code,
            captures: Vec::new(),
            block_arity: 0,
        };
        program.functions[index] = function;
    }
    Ok(program)
}

fn expanded(args: &[Argument]) -> bool {
    args.iter()
        .any(|a| !matches!(a.kind, ArgumentKind::Positional))
}

struct Compiler<'a> {
    program: &'a mut Program,
    locals: HashMap<String, usize>,
    code: Vec<Op>,
    parameters: HashSet<String>,
    loop_bindings: Vec<Vec<usize>>,
    outer: Vec<HashMap<String, usize>>,
    reads: HashSet<String>,
    assigned: HashSet<String>,
}
impl Compiler<'_> {
    fn slot(&mut self, name: &str) -> usize {
        let n = self.locals.len();
        *self.locals.entry(name.to_owned()).or_insert(n)
    }
    fn capture_name(&mut self, name: &str) {
        if self.outer.iter().any(|scope| scope.contains_key(name)) {
            self.slot(name);
        }
    }
    fn declare(&mut self, body: &[Stmt]) {
        for stmt in body {
            match stmt {
                Stmt::Expr(e) => self.declare_expr(e),
                Stmt::Assign(target, _, value) => {
                    self.declare_target(target);
                    self.declare_expr(value);
                }
                Stmt::If(cond, yes, no) => {
                    self.declare_expr(cond);
                    self.declare(yes);
                    self.declare(no);
                }
                Stmt::While(cond, body) => {
                    self.declare_expr(cond);
                    self.declare(body);
                }
                Stmt::For(target, iterable, body) => {
                    self.declare_target(target);
                    self.declare_expr(iterable);
                    self.declare(body);
                }
                Stmt::Return(e) | Stmt::Break(e) | Stmt::Next(e) => {
                    if let Some(e) = e {
                        self.declare_expr(e);
                    }
                }
            }
        }
    }
    fn declare_target(&mut self, target: &Target) {
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                if !self.outer.is_empty() || self.global_binding(name).is_none() {
                    self.slot(name);
                }
                self.assigned.insert(name.clone());
            }
            Target::Value(e) => self.declare_expr(e),
            Target::Tuple(parts) => {
                for (part, _) in parts {
                    if let Some(part) = part {
                        self.declare_target(part);
                    }
                }
            }
        }
    }
    fn declare_expr(&mut self, e: &Expr) {
        match &e.node {
            Node::Literal(_) | Node::Integer(_) | Node::BigInteger(..) => (),
            Node::Var(name) => {
                self.reads.insert(name.clone());
                self.capture_name(name);
            }
            Node::Array(values) | Node::Yield(values) | Node::Template(values, _) => {
                for value in values {
                    self.declare_expr(value);
                }
            }
            Node::Call(name, args) => {
                if name != "it" {
                    self.reads.insert(name.clone());
                }
                self.capture_name(name);
                for arg in args {
                    self.declare_expr(&arg.value);
                }
            }
            Node::BlockCall(call, _) => self.declare_expr(call),
            Node::Hash(entries) => {
                for (_, value) in entries {
                    self.declare_expr(value);
                }
            }
            Node::Unary(_, value) | Node::Member(value, _) => self.declare_expr(value),
            Node::Binary(_, a, b) => {
                self.declare_expr(a);
                self.declare_expr(b);
            }
            Node::Range(a, b, _) => {
                if let Some(e) = a {
                    self.declare_expr(e);
                }
                if let Some(e) = b {
                    self.declare_expr(e);
                }
            }
            Node::Conditional(a, b, c) => {
                self.declare_expr(a);
                self.declare_expr(b);
                self.declare_expr(c);
            }
            Node::Case(target, clauses, alternate) => {
                if let Some(e) = target {
                    self.declare_expr(e);
                }
                for clause in clauses {
                    for (e, _) in &clause.values {
                        self.declare_expr(e);
                    }
                    self.declare_expr(&clause.result);
                }
                if let Some(e) = alternate {
                    self.declare_expr(e);
                }
            }
            Node::Loop(stmt) => self.declare(std::slice::from_ref(stmt.as_ref())),
            Node::Method(recv, _, args) => {
                self.declare_expr(recv);
                for arg in args {
                    self.declare_expr(&arg.value);
                }
            }
            Node::Scope(recv, _, args) => {
                self.declare_expr(recv);
                for arg in args.iter().flatten() {
                    self.declare_expr(&arg.value);
                }
            }
            Node::Index(recv, args) => {
                self.declare_expr(recv);
                for arg in args {
                    self.declare_expr(arg);
                }
            }
        }
    }
    fn emit(&mut self, op: Op) -> usize {
        let pos = self.code.len();
        self.code.push(op);
        pos
    }
    fn patch(&mut self, pos: usize, target: usize) {
        match &mut self.code[pos] {
            Op::Jump(n)
            | Op::JumpFalse(n)
            | Op::JumpTrue(n)
            | Op::Bind(_, n)
            | Op::AddressBound(_, n) => *n = target,
            _ => unreachable!(),
        }
    }
    fn constant(&mut self, v: Value) {
        let n = self.program.constants.len();
        self.program.constants.push(v);
        self.emit(Op::Constant(n));
    }
    fn integer_literal(&mut self, text: &str, radix: u32) {
        let n = self.program.constants.len();
        self.program.constants.push(Value::bytes(text.as_bytes()));
        self.emit(Op::Integer(n, radix));
    }
    fn block(&mut self, body: &[Stmt]) -> Result<()> {
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
        if let Stmt::Assign(target, _, _) = stmt {
            let mut names = Vec::new();
            target_names(target, &mut names);
            for name in names {
                if let Some(&slot) = self.locals.get(name) {
                    self.emit(Op::Declare(slot));
                }
            }
        }
        self.statement(stmt, expression)?;
        if !expression && matches!(stmt, Stmt::If(..) | Stmt::While(..) | Stmt::For(..)) {
            for slot in self.statement_bindings(std::slice::from_ref(stmt)) {
                self.emit(Op::Declare(slot));
            }
        }
        Ok(())
    }
    fn assignment_rhs(&mut self, target: &Target, values: &[&Expr]) -> Result<()> {
        let mut names = Vec::new();
        target_names(target, &mut names);
        let mut calls = HashSet::new();
        for value in values {
            call_names(value, &mut calls);
        }
        let mut seen = HashSet::new();
        let names: Vec<_> = names
            .into_iter()
            .filter(|name| {
                self.locals.contains_key(*name) && calls.contains(name) && seen.insert(*name)
            })
            .collect();
        for name in &names {
            self.emit(Op::Bypass(self.locals[*name]));
        }
        for value in values {
            self.expr(value)?;
        }
        if !names.is_empty() {
            self.emit(Op::BypassEnd(names.len()));
        }
        Ok(())
    }
    fn declaration_slot(&self, name: &str) -> Option<usize> {
        if Global::parse(name).is_some()
            && !self.program.names.contains_key(name)
            && !self.program.hosts.iter().any(|host| host == name)
        {
            None
        } else {
            self.locals.get(name).copied()
        }
    }
    fn statement_bindings(&self, body: &[Stmt]) -> Vec<usize> {
        let mut names = Vec::new();
        statement_names(body, &mut names);
        let mut seen = HashSet::new();
        names
            .into_iter()
            .filter(|name| seen.insert(*name))
            .filter_map(|name| self.declaration_slot(name))
            .collect()
    }
    fn loop_body(&mut self, body: &[Stmt]) -> Result<()> {
        self.loop_bindings.push(self.statement_bindings(body));
        self.block(body)?;
        self.loop_bindings.pop();
        Ok(())
    }
    fn statement(&mut self, stmt: &Stmt, expression: bool) -> Result<()> {
        match stmt {
            Stmt::Expr(e) => self.expr(e)?,
            Stmt::Assign(target, op, rhs) => {
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
                        if let Some(global) = self.global_binding(name) {
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
                        let slot = self.slot(name);
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
                        if binary.is_none() {
                            if let Node::Binary("+", left, right) = &rhs.node {
                                self.assignment_rhs(binding_target, &[left, right])?;
                                self.emit(Op::AddStore(slot));
                                return Ok(());
                            }
                        }
                        if binary.is_some() {
                            self.expr(target)?;
                        }
                        self.assignment_rhs(binding_target, &[rhs])?;
                        if binary == Some("+") {
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
                    _ => return Err(syntax::unsupported("invalid assignment target")),
                }
            }
            Stmt::If(cond, yes, no) => {
                self.expr(cond)?;
                let branch = self.emit(Op::JumpFalse(0));
                self.block(yes)?;
                let done = self.emit(Op::Jump(0));
                self.patch(branch, self.code.len());
                self.block(no)?;
                self.patch(done, self.code.len());
            }
            Stmt::While(cond, body) => {
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
            Stmt::For(target, iterable, body) => {
                self.expr(iterable)?;
                let mut names = Vec::new();
                target_names(target, &mut names);
                for name in names {
                    if let Some(&slot) = self.locals.get(name) {
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
            Stmt::Return(value) => {
                if let Some(e) = value {
                    self.expr(e)?;
                } else {
                    self.emit(Op::Nil);
                }
                self.emit(Op::Return);
            }
            Stmt::Break(value) => {
                if let Some(value) = value {
                    self.expr(value)?;
                }
                self.emit(Op::Break(value.is_some()));
            }
            Stmt::Next(value) => {
                if let Some(value) = value {
                    self.expr(value)?;
                }
                if let Some(bindings) = self.loop_bindings.last() {
                    for &slot in bindings {
                        self.code.push(Op::Declare(slot));
                    }
                }
                self.emit(Op::Next(value.is_some()));
            }
        }
        Ok(())
    }
    fn assign_value(&mut self, target: &Target) -> Result<()> {
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                if let Some(global) = self.global_binding(name) {
                    self.emit(Op::StoreGlobal(global));
                } else {
                    let slot = self.slot(name);
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
            _ => return Err(syntax::unsupported("invalid assignment target")),
        }
        Ok(())
    }
    fn expr(&mut self, e: &Expr) -> Result<()> {
        match &e.node {
            Node::Integer(n) => {
                if let Ok(n) = i64::try_from(*n) {
                    self.constant(Value::int(n));
                } else {
                    self.integer_literal(&n.to_string(), 10);
                }
            }
            Node::BigInteger(text, radix) => self.integer_literal(text, *radix),
            Node::Unary("-", value) if matches!(value.node, Node::Integer(n) if n == i64::MAX as u64 + 1) =>
            {
                self.constant(Value::int(i64::MIN));
            }
            Node::Literal(v) => self.constant(v.clone()),
            Node::Var(name) if name == "block_given?" => {
                self.emit(Op::BlockGiven(false, false));
            }
            Node::Var(name) => {
                let global = self.global(name);
                if let Some(&slot) = self.locals.get(name) {
                    if !self.parameters.contains(name) {
                        let name = self.call_site(name, false).name;
                        self.emit(Op::LoadOptional(slot, name));
                    } else {
                        self.emit(Op::Load(slot));
                    }
                } else if let Some(&fun) = self.program.names.get(name) {
                    self.emit(Op::AutoCall(fun));
                } else if let Some(host) = self.program.hosts.iter().position(|h| h == name) {
                    self.emit(Op::HostValue(host));
                } else if let Some(global) = global {
                    self.emit(Op::Global(global));
                } else {
                    let site = self.call_site(name, false);
                    self.emit(Op::Unbound(site.name));
                }
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
                    self.emit(Op::TextPart);
                }
                self.emit(Op::TextEnd(*symbol));
            }
            Node::Hash(values) => {
                for (k, v) in values {
                    self.constant(Value::bytes(k.clone()));
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
                if let Some(target) = target {
                    self.expr(target)?;
                }
                let mut completed = Vec::new();
                for clause in clauses {
                    let mut matches = Vec::new();
                    for (value, splat) in &clause.values {
                        if target.is_some() {
                            self.emit(Op::Dup);
                        }
                        self.expr(value)?;
                        self.emit(Op::CaseCompare(target.is_some(), *splat));
                        matches.push(self.emit(Op::JumpTrue(0)));
                    }
                    let next = self.emit(Op::Jump(0));
                    for matched in matches {
                        self.patch(matched, self.code.len());
                    }
                    if target.is_some() {
                        self.emit(Op::Pop);
                    }
                    self.expr(&clause.result)?;
                    completed.push(self.emit(Op::Jump(0)));
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
            }
            Node::Binary("<<", a, b) => {
                self.address(a)?;
                self.expr(b)?;
                let site = self.call_site("push", false);
                self.emit(Op::Mutate(site, 1));
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
            Node::Call(name, args) if name == "block_given?" => {
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
            Node::Call(name, args) => {
                self.global(name);
                if let Some(&slot) = self.locals.get(name) {
                    let name = self.call_site(name, false).name;
                    self.emit(Op::ResolveCall(slot, name));
                    self.argument_values(args)?;
                    self.emit(Op::Invoke(Invocation::Resolved));
                    return Ok(());
                }
                let target = if let Some(&fun) = self.program.names.get(name) {
                    Invocation::Function(fun)
                } else if let Some(host) = self.program.hosts.iter().position(|h| h == name) {
                    Invocation::Host(host)
                } else if let Some(global) = self.global(name) {
                    self.emit(Op::ResolveGlobalCall(global));
                    self.argument_values(args)?;
                    self.emit(Op::Invoke(Invocation::Resolved));
                    return Ok(());
                } else {
                    let site = self.call_site(name, false);
                    self.emit(Op::Unbound(site.name));
                    return Ok(());
                };
                if expanded(args) {
                    self.call_arguments(args)?;
                    self.emit(Op::Invoke(target));
                } else {
                    for arg in args {
                        self.expr(&arg.value)?;
                    }
                    self.emit(match target {
                        Invocation::Function(fun) => Op::Call(fun, args.len()),
                        Invocation::Host(host) => Op::Host(host, args.len()),
                        Invocation::NonCallable => Op::NonCallable,
                        _ => unreachable!(),
                    });
                }
            }
            Node::Member(recv, name) => self.member_call(recv, name, &[], true, None)?,
            Node::Scope(recv, name, args) => self.scoped_call(recv, name, args.as_deref(), None)?,
            Node::Method(recv, name, args) => self.member_call(recv, name, args, false, None)?,
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
    fn call_site(&mut self, name: &str, auto: bool) -> CallSite {
        let index = self.program.members.len();
        self.program.members.push(name.to_owned());
        CallSite {
            name: index,
            method: Method::parse(name),
            auto,
            scope: false,
        }
    }
    fn global(&mut self, name: &str) -> Option<usize> {
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
        auto: bool,
        block: Option<usize>,
    ) -> Result<()> {
        let mutating = matches!(
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
        );
        if mutating {
            self.address(receiver)?;
        } else {
            self.expr(receiver)?;
        }
        let site = self.call_site(name, auto);
        if expanded(args) || block.is_some() || crate::iteration::method(name) {
            self.call_arguments(args)?;
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
        Ok(())
    }
    fn scoped_call(
        &mut self,
        receiver: &Expr,
        name: &str,
        args: Option<&[Argument]>,
        block: Option<usize>,
    ) -> Result<()> {
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
        let function = self.compile_block(block)?;
        let (name, args) = match &call.node {
            Node::Var(name) => (name.as_str(), &[][..]),
            Node::Call(name, args) => (name.as_str(), args.as_slice()),
            Node::Member(receiver, name) => {
                return self.member_call(receiver, name, &[], false, Some(function));
            }
            Node::Method(receiver, name, args) => {
                return self.member_call(receiver, name, args, false, Some(function));
            }
            Node::Scope(receiver, name, args) => {
                return self.scoped_call(receiver, name, args.as_deref(), Some(function));
            }
            _ => {
                self.expr(call)?;
                self.emit(Op::Pop);
                self.emit(Op::NonCallable);
                return Ok(());
            }
        };
        if name == "block_given?" {
            self.emit(Op::BlockGiven(!args.is_empty(), true));
            return Ok(());
        }
        let target = if let Some(&slot) = self.locals.get(name) {
            let name = self.call_site(name, false).name;
            self.emit(Op::ResolveCall(slot, name));
            Invocation::Resolved
        } else if let Some(global) = self.global_binding(name) {
            self.emit(Op::ResolveGlobalCall(global));
            Invocation::Resolved
        } else {
            let target = if let Some(&function) = self.program.names.get(name) {
                Invocation::Function(function)
            } else if let Some(host) = self.program.hosts.iter().position(|host| host == name) {
                Invocation::Host(host)
            } else {
                let site = self.call_site(name, false);
                self.emit(Op::Unbound(site.name));
                return Ok(());
            };
            self.emit(Op::Arguments);
            target
        };
        self.argument_values(args)?;
        self.emit(Op::Attach(function));
        self.emit(Op::Invoke(target));
        Ok(())
    }
    fn compile_block(&mut self, block: &Block) -> Result<usize> {
        let mut block_arity = block.params.len();
        let mut outer = vec![self.locals.clone()];
        outer.extend(self.outer.iter().cloned());
        let mut child = Compiler {
            program: self.program,
            locals: HashMap::new(),
            code: Vec::new(),
            parameters: HashSet::new(),
            loop_bindings: Vec::new(),
            outer,
            reads: HashSet::new(),
            assigned: HashSet::new(),
        };
        child.declare(&block.body);
        for target in &block.params {
            child.declare_target(target);
            let mut names = Vec::new();
            target_names(target, &mut names);
            for name in names {
                let slot = child.slot(name);
                child.parameters.insert(name.to_owned());
                child.emit(Op::Shadow(slot));
            }
        }
        for (index, target) in block.params.iter().enumerate() {
            child.emit(Op::BlockArg(index, block.params.len() > 1));
            child.assign_value(target)?;
            child.emit(Op::Pop);
        }
        if block.implicit {
            let candidates = (1..=9)
                .map(|index| (format!("_{index}"), index - 1))
                .chain(block.infer_it.then_some(("it".to_owned(), 0)));
            for (name, index) in candidates {
                if child.reads.contains(&name) && !child.assigned.contains(&name) {
                    block_arity = block_arity.max(index + 1);
                    let slot = child.slot(&name);
                    child.parameters.insert(name);
                    child.emit(Op::Shadow(slot));
                    child.emit(Op::BlockArg(index, false));
                    child.emit(Op::Store(slot));
                    child.emit(Op::Pop);
                }
            }
        }
        child.block(&block.body)?;
        child.emit(Op::Finish);
        let mut captures = vec![None; child.locals.len()];
        for (name, &slot) in &child.locals {
            captures[slot] =
                child.outer.iter().enumerate().find_map(|(depth, scope)| {
                    scope.get(name).map(|&slot| Capture { depth, slot })
                });
        }
        let function = Function {
            name: "<block>".into(),
            locals: child.locals.len(),
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
        self.emit(Op::Arguments);
        self.argument_values(args)
    }
    fn argument_values(&mut self, args: &[Argument]) -> Result<()> {
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
        match &target.node {
            Node::Index(receiver, indices) => {
                self.address(receiver)?;
                for index in indices {
                    self.expr(index)?;
                }
                self.emit(Op::AddressTarget(indices.len(), read));
            }
            Node::Member(receiver, name) => {
                self.address(receiver)?;
                let site = self.call_site(name, true);
                self.emit(Op::AddressMemberTarget(site, read));
            }
            _ => return Err(syntax::unsupported("invalid assignment target")),
        }
        Ok(())
    }
    fn global_binding(&mut self, name: &str) -> Option<usize> {
        if self.locals.contains_key(name) || self.outer.iter().any(|scope| scope.contains_key(name))
        {
            None
        } else {
            self.global_fallback(name)
        }
    }
    fn global_fallback(&mut self, name: &str) -> Option<usize> {
        if self.program.names.contains_key(name)
            || self.program.hosts.iter().any(|host| host == name)
        {
            None
        } else {
            self.global(name)
        }
    }
    fn address(&mut self, receiver: &Expr) -> Result<()> {
        match &receiver.node {
            Node::Var(name) if self.locals.contains_key(name) => {
                let slot = self.locals[name];
                if self.parameters.contains(name) {
                    self.emit(Op::AddressLocal(slot));
                } else {
                    let bound = self.emit(Op::AddressBound(slot, 0));
                    if let Some(global) = self.global_fallback(name) {
                        self.emit(Op::AddressGlobal(global));
                    } else {
                        self.expr(receiver)?;
                        self.emit(Op::AddressValue);
                    }
                    self.patch(bound, self.code.len());
                }
            }
            Node::Var(name) if self.global_fallback(name).is_some() => {
                let global = self.global_fallback(name).unwrap();
                self.emit(Op::AddressGlobal(global));
            }
            Node::Member(root, name) => {
                self.address(root)?;
                let site = self.call_site(name, true);
                self.emit(Op::AddressMember(site));
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
        Ok(())
    }
}

fn call_names<'a>(expr: &'a Expr, names: &mut HashSet<&'a str>) {
    match &expr.node {
        Node::Literal(_) | Node::Integer(_) | Node::BigInteger(..) | Node::Var(_) => (),
        Node::Call(name, args) => {
            names.insert(name);
            for arg in args {
                call_names(&arg.value, names);
            }
        }
        Node::BlockCall(call, block) => {
            if let Node::Var(name) = &call.node {
                names.insert(name);
            }
            call_names(call, names);
            block_call_names(&block.body, names);
        }
        Node::Array(items) | Node::Yield(items) | Node::Template(items, _) => {
            for item in items {
                call_names(item, names);
            }
        }
        Node::Hash(entries) => {
            for (_, item) in entries {
                call_names(item, names);
            }
        }
        Node::Unary(_, value) | Node::Member(value, _) => call_names(value, names),
        Node::Binary(_, a, b) => {
            call_names(a, names);
            call_names(b, names);
        }
        Node::Conditional(a, b, c) => {
            call_names(a, names);
            call_names(b, names);
            call_names(c, names);
        }
        Node::Range(a, b, _) => {
            for item in a.iter().chain(b) {
                call_names(item, names);
            }
        }
        Node::Case(target, clauses, alternate) => {
            for item in target.iter().chain(alternate) {
                call_names(item, names);
            }
            for clause in clauses {
                for (item, _) in &clause.values {
                    call_names(item, names);
                }
                call_names(&clause.result, names);
            }
        }
        Node::Loop(stmt) => block_call_names(std::slice::from_ref(stmt), names),
        Node::Method(receiver, _, args) => {
            call_names(receiver, names);
            for arg in args {
                call_names(&arg.value, names);
            }
        }
        Node::Scope(receiver, _, args) => {
            call_names(receiver, names);
            for arg in args.iter().flatten() {
                call_names(&arg.value, names);
            }
        }
        Node::Index(receiver, args) => {
            call_names(receiver, names);
            for arg in args {
                call_names(arg, names);
            }
        }
    }
}

fn target_call_names<'a>(target: &'a Target, names: &mut HashSet<&'a str>) {
    match target {
        Target::Value(expr) => call_names(expr, names),
        Target::Tuple(parts) => {
            for (part, _) in parts {
                if let Some(part) = part {
                    target_call_names(part, names);
                }
            }
        }
    }
}

fn block_call_names<'a>(body: &'a [Stmt], names: &mut HashSet<&'a str>) {
    for stmt in body {
        match stmt {
            Stmt::Expr(expr) => call_names(expr, names),
            Stmt::Assign(target, _, value) => {
                target_call_names(target, names);
                call_names(value, names);
            }
            Stmt::If(condition, yes, no) => {
                call_names(condition, names);
                block_call_names(yes, names);
                block_call_names(no, names);
            }
            Stmt::While(condition, body) => {
                call_names(condition, names);
                block_call_names(body, names);
            }
            Stmt::For(target, source, body) => {
                target_call_names(target, names);
                call_names(source, names);
                block_call_names(body, names);
            }
            Stmt::Return(value) | Stmt::Break(value) | Stmt::Next(value) => {
                if let Some(value) = value {
                    call_names(value, names);
                }
            }
        }
    }
}

fn target_names<'a>(target: &'a Target, names: &mut Vec<&'a str>) {
    match target {
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => names.push(name),
        Target::Tuple(parts) => {
            for (part, _) in parts {
                if let Some(part) = part {
                    target_names(part, names);
                }
            }
        }
        _ => (),
    }
}

fn statement_names<'a>(body: &'a [Stmt], names: &mut Vec<&'a str>) {
    for stmt in body {
        match stmt {
            Stmt::Assign(target, _, _) => target_names(target, names),
            Stmt::If(_, yes, no) => {
                statement_names(yes, names);
                statement_names(no, names);
            }
            Stmt::While(_, body) => statement_names(body, names),
            Stmt::For(target, _, body) => {
                target_names(target, names);
                statement_names(body, names);
            }
            _ => (),
        }
    }
}
