use crate::{
    Result, Value,
    syntax::{self, Expr, Node, Stmt, Target},
};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    Constant(usize),
    Nil,
    Load(usize),
    Store(usize),
    Pop,
    Dup,
    Unary(&'static str),
    Binary(&'static str),
    AddStore(usize),
    Array(usize),
    Hash(usize),
    Range(bool, bool, bool),
    Index(usize),
    AddressLocal(usize),
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
    Next,
    Call(usize, usize),
    Host(usize, usize),
    Method(CallSite, usize),
    JsonParse,
    JsonStringify,
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
    Return,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSite {
    pub name: usize,
    pub method: Option<Method>,
    pub auto: bool,
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
}
impl Method {
    fn parse(name: &str) -> Option<Self> {
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
            "index" => Self::Index,
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
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Function {
    pub name: String,
    pub arity: usize,
    pub locals: usize,
    pub code: Vec<Op>,
}
#[derive(Debug)]
pub(crate) struct Program {
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
        functions: Vec::new(),
        constants: Vec::new(),
        names,
        hosts,
        members: Vec::new(),
    };
    for def in defs {
        let mut c = Compiler {
            program: &mut program,
            locals: HashMap::new(),
            code: Vec::new(),
        };
        for name in &def.params {
            c.slot(name);
        }
        c.declare(&def.body);
        c.block(&def.body)?;
        c.code.push(Op::Return);
        let function = Function {
            name: def.name,
            arity: def.params.len(),
            locals: c.locals.len(),
            code: c.code,
        };
        program.functions.push(function);
    }
    Ok(program)
}

struct Compiler<'a> {
    program: &'a mut Program,
    locals: HashMap<String, usize>,
    code: Vec<Op>,
}
impl Compiler<'_> {
    fn slot(&mut self, name: &str) -> usize {
        let n = self.locals.len();
        *self.locals.entry(name.to_owned()).or_insert(n)
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
                self.slot(name);
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
            Node::Literal(_) | Node::Var(_) => (),
            Node::Array(values) | Node::Call(_, values) => {
                for value in values {
                    self.declare_expr(value);
                }
            }
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
            Node::Method(recv, _, args) | Node::Index(recv, args) => {
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
            Op::Jump(n) | Op::JumpFalse(n) | Op::JumpTrue(n) => *n = target,
            _ => unreachable!(),
        }
    }
    fn constant(&mut self, v: Value) {
        let n = self.program.constants.len();
        self.program.constants.push(v);
        self.emit(Op::Constant(n));
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
        match stmt {
            Stmt::Expr(e) => self.expr(e)?,
            Stmt::Assign(target, op, rhs) => {
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
                    self.expr(rhs)?;
                    self.assign_value(target)?;
                    return Ok(());
                };
                match &target.node {
                    Node::Var(name) => {
                        let slot = self.slot(name);
                        if matches!(*op, "||=" | "&&=") {
                            self.emit(Op::Load(slot));
                            self.emit(Op::Dup);
                            let skip = self.emit(if *op == "||=" {
                                Op::JumpTrue(0)
                            } else {
                                Op::JumpFalse(0)
                            });
                            self.emit(Op::Pop);
                            self.expr(rhs)?;
                            self.emit(Op::Store(slot));
                            self.patch(skip, self.code.len());
                            return Ok(());
                        }
                        if binary.is_none() {
                            if let Node::Binary("+", left, right) = &rhs.node {
                                self.expr(left)?;
                                self.expr(right)?;
                                self.emit(Op::AddStore(slot));
                                return Ok(());
                            }
                        }
                        if binary.is_some() {
                            self.emit(Op::Load(slot));
                        }
                        self.expr(rhs)?;
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
                            self.expr(rhs)?;
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
                                self.expr(rhs)?;
                                self.emit(Op::AddressStore);
                                let end = self.emit(Op::Jump(0));
                                self.patch(skip, self.code.len());
                                self.emit(Op::AddressDrop);
                                self.patch(end, self.code.len());
                            } else {
                                self.expr(rhs)?;
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
                self.block(body)?;
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
                let mark = self.emit(Op::LoopStart {
                    iterable: true,
                    expression,
                    next: 0,
                    end: 0,
                });
                let next = self.emit(Op::IterNext);
                self.assign_value(target)?;
                self.emit(Op::Pop);
                self.block(body)?;
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
                self.emit(Op::Next);
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
                let slot = self.slot(name);
                self.emit(Op::Store(slot));
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
            Node::Literal(v) => self.constant(v.clone()),
            Node::Var(name) => {
                if let Some(&slot) = self.locals.get(name) {
                    self.emit(Op::Load(slot));
                } else if let Some(&fun) = self.program.names.get(name) {
                    self.emit(Op::Call(fun, 0));
                } else {
                    return Err(syntax::unsupported(&format!("unknown variable {name}")));
                }
            }
            Node::Array(values) => {
                for v in values {
                    self.expr(v)?;
                }
                self.emit(Op::Array(values.len()));
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
            Node::Call(name, args) => {
                for arg in args {
                    self.expr(arg)?;
                }
                if let Some(&fun) = self.program.names.get(name) {
                    self.emit(Op::Call(fun, args.len()));
                } else if let Some(host) = self.program.hosts.iter().position(|h| h == name) {
                    self.emit(Op::Host(host, args.len()));
                } else {
                    return Err(syntax::unsupported(&format!("unknown function {name}")));
                }
            }
            Node::Member(recv, name) => self.member_call(recv, name, &[], true)?,
            Node::Method(recv, name, args) => self.member_call(recv, name, args, false)?,
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
        }
    }
    fn member_call(
        &mut self,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        auto: bool,
    ) -> Result<()> {
        if matches!(&receiver.node, Node::Var(v) if v == "JSON") {
            if args.len() != 1 {
                return Err(syntax::unsupported("JSON methods require one argument"));
            }
            self.expr(&args[0])?;
            self.emit(match name {
                "parse" => Op::JsonParse,
                "stringify" => Op::JsonStringify,
                _ => return Err(syntax::unsupported("unknown JSON method")),
            });
        } else {
            let mutating = matches!(
                name,
                "push"
                    | "append"
                    | "prepend"
                    | "unshift"
                    | "pop"
                    | "shift"
                    | "delete"
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
            for arg in args {
                self.expr(arg)?;
            }
            let site = self.call_site(name, auto);
            self.emit(if mutating {
                Op::Mutate(site, args.len())
            } else {
                Op::Method(site, args.len())
            });
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
    fn address(&mut self, receiver: &Expr) -> Result<()> {
        match &receiver.node {
            Node::Var(name) if self.locals.contains_key(name) => {
                self.emit(Op::AddressLocal(self.locals[name]));
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
