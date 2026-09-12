use crate::{
    Result, Value,
    syntax::{self, Expr, Node, Stmt},
};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    Constant(usize),
    Nil,
    Load(usize),
    Store(usize),
    ReleaseLocal(usize),
    Pop,
    Dup,
    Dup2,
    Unary(&'static str),
    Binary(&'static str),
    Array(usize),
    Hash(usize),
    Index,
    SetIndex(usize),
    Call(usize, usize),
    Host(usize, usize),
    Method(Method, usize),
    JsonParse,
    JsonStringify,
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
    Return,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Method {
    Length,
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
    Sum,
    Keys,
    Values,
    ToString,
    ToInt,
}
impl Method {
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "length" | "size" => Self::Length,
            "bytesize" => Self::ByteSize,
            "upcase" => Self::Upcase,
            "downcase" => Self::Downcase,
            "include?" => Self::Include,
            "index" => Self::Index,
            "rindex" => Self::Rindex,
            "strip" => Self::Strip,
            "split" => Self::Split,
            "join" => Self::Join,
            "push" => Self::Push,
            "sum" => Self::Sum,
            "keys" => Self::Keys,
            "values" => Self::Values,
            "to_s" => Self::ToString,
            "to_i" => Self::ToInt,
            _ => {
                return Err(syntax::unsupported(&format!(
                    "method {name} is not implemented"
                )));
            }
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
    };
    for def in defs {
        let mut c = Compiler {
            program: &mut program,
            locals: HashMap::new(),
            code: Vec::new(),
            loops: Vec::new(),
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

struct Loop {
    start: usize,
    breaks: Vec<usize>,
}
struct Compiler<'a> {
    program: &'a mut Program,
    locals: HashMap<String, usize>,
    code: Vec<Op>,
    loops: Vec<Loop>,
}
impl Compiler<'_> {
    fn slot(&mut self, name: &str) -> usize {
        let n = self.locals.len();
        *self.locals.entry(name.to_owned()).or_insert(n)
    }
    fn declare(&mut self, body: &[Stmt]) {
        for stmt in body {
            match stmt {
                Stmt::Assign(
                    Expr {
                        node: Node::Var(name),
                        ..
                    },
                    _,
                    _,
                ) => {
                    self.slot(name);
                }
                Stmt::If(_, yes, no) => {
                    self.declare(yes);
                    self.declare(no);
                }
                Stmt::While(_, body) => self.declare(body),
                _ => (),
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
            self.stmt(stmt)?;
        }
        Ok(())
    }
    fn stmt(&mut self, stmt: &Stmt) -> Result<()> {
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
                match &target.node {
                    Node::Var(name) => {
                        let slot = self.slot(name);
                        if binary.is_some() {
                            self.emit(Op::Load(slot));
                        }
                        self.expr(rhs)?;
                        if let Some(op) = binary {
                            self.emit(Op::Binary(op));
                        }
                        self.emit(Op::Store(slot));
                    }
                    Node::Index(root, index) => {
                        let Node::Var(name) = &root.node else {
                            return Err(syntax::unsupported(
                                "nested index assignment is not implemented",
                            ));
                        };
                        let slot = self.slot(name);
                        self.emit(Op::Load(slot));
                        self.expr(index)?;
                        if binary.is_some() {
                            self.emit(Op::Dup2);
                            self.emit(Op::Index);
                        }
                        self.expr(rhs)?;
                        if let Some(op) = binary {
                            self.emit(Op::Binary(op));
                        }
                        self.emit(Op::SetIndex(slot));
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
                let start = self.code.len();
                self.expr(cond)?;
                let done = self.emit(Op::JumpFalse(0));
                self.loops.push(Loop {
                    start,
                    breaks: Vec::new(),
                });
                self.block(body)?;
                self.emit(Op::Pop);
                self.emit(Op::Jump(start));
                let exit = self.code.len();
                self.patch(done, exit);
                let state = self.loops.pop().unwrap();
                for pos in state.breaks {
                    self.patch(pos, exit);
                }
                self.emit(Op::Nil);
            }
            Stmt::Return(value) => {
                if let Some(e) = value {
                    self.expr(e)?;
                } else {
                    self.emit(Op::Nil);
                }
                self.emit(Op::Return);
            }
            Stmt::Break => {
                if self.loops.is_empty() {
                    return Err(syntax::unsupported("break outside loop"));
                }
                let pos = self.emit(Op::Jump(0));
                self.loops.last_mut().unwrap().breaks.push(pos);
            }
            Stmt::Next => {
                let start = self
                    .loops
                    .last()
                    .ok_or_else(|| syntax::unsupported("next outside loop"))?
                    .start;
                self.emit(Op::Jump(start));
            }
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
                    if *op == "<<" {
                        self.release_receiver(a);
                    }
                    self.emit(Op::Binary(op));
                    if *op == "<<" {
                        self.write_receiver(a)?;
                    }
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
            Node::Method(recv, name, args) => {
                if matches!(&recv.node,Node::Var(v) if v=="JSON") {
                    if args.len() != 1 {
                        return Err(syntax::unsupported("JSON methods require one argument"));
                    }
                    self.expr(&args[0])?;
                    self.emit(match name.as_str() {
                        "parse" => Op::JsonParse,
                        "stringify" => Op::JsonStringify,
                        _ => return Err(syntax::unsupported("unknown JSON method")),
                    });
                } else {
                    self.expr(recv)?;
                    for a in args {
                        self.expr(a)?;
                    }
                    if name == "push" {
                        self.release_receiver(recv);
                    }
                    self.emit(Op::Method(Method::parse(name)?, args.len()));
                    if name == "push" {
                        self.write_receiver(recv)?;
                    }
                }
            }
            Node::Index(value, index) => {
                self.expr(value)?;
                self.expr(index)?;
                self.emit(Op::Index);
            }
        }
        Ok(())
    }
    fn release_receiver(&mut self, recv: &Expr) {
        if let Node::Var(name) = &recv.node {
            let slot = self.slot(name);
            self.emit(Op::ReleaseLocal(slot));
        }
    }
    fn write_receiver(&mut self, recv: &Expr) -> Result<()> {
        match &recv.node {
            Node::Var(name) => {
                let slot = self.slot(name);
                self.emit(Op::Store(slot));
            }
            Node::Index(_, _) => {
                return Err(syntax::unsupported(
                    "nested collection mutation is not implemented",
                ));
            }
            _ => (),
        }
        Ok(())
    }
}
