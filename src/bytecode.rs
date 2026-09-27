use crate::{
    Result, Value,
    builtin::{Builtin, Global},
    compilation::{Buffer, Name, Table, Task, Tasks},
    syntax::{
        self, Argument, ArgumentKind, Block, CallForm, Expr, Node, ParamKind, Statement, Stmt,
        Target,
    },
};
use std::collections::HashMap;

mod aliases;
mod calls;
mod errors;
mod loops;
mod namespaces;
mod regex;
mod typing;

/// An instruction. Sources are at most [`crate::syntax::MAX_SOURCE`] bytes,
/// so every slot, jump target and table index it holds fits in 32 bits,
/// which keeps an instruction 16 bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    TryBegin(u32),
    TryBody,
    TryEnd,
    EnsureEnd,
    Retry,
    /// Starts a `raise`, naming its class by an index into
    /// [`Program::raises`], and jumps to `.1` when the class is unbound.
    RaiseStart(Option<u32>, u32),
    RaiseValue,
    Raise(u8),
    InitNamespace(u32),
    UnboundClass(u32),
    /// Refuses a nested function declaration when it runs, as Go does.
    Unsupported,
    BindIvar(u32, u32),
    BindField(u32, u32),
    InstanceField(u32),
    InstanceStore(u32),
    InstanceAddress(u32),
    NamespaceSelf(u32),
    NamespaceConstant(u32, u32),
    NamespaceConstantAddress(u32, u32),
    NamespaceVariable(u32, bool),
    NamespaceStore(u32),
    NamespaceAddress(u32, bool),
    AmbientValue(u32, u32),
    AmbientAddress(u32, u32),
    ImplicitAddress(u32, u32),
    FileValue(u32, u32, Receiving),
    FileAddress(u32, u32),
    RootAddress(u32, u32),
    PrepareMember(CallSite, bool),
    StoreDeclaration(u32),
    Regex(u32),
    TypeShadowed(u32, u32),
    Normalize(u32, u32),
    /// Validates the value on top of the stack against a type, naming it by
    /// a constant subject: a typed local, a `yield` argument or a block result.
    Check(u32, u32),
    Declaration(u32),
    Global(u32),
    GlobalReceiver(u32, Receiving),
    StoreGlobal(u32),
    ResolveGlobalCall(u32),
    AddressGlobal(u32),
    Integer(u32, u32),
    Constant(u32),
    /// Pushes a string or symbol literal, which every literal of the program
    /// with the same text shares, by its slot in [`Program::shared`]: each
    /// call imports it once, so the records and strings it builds from
    /// literals share one copy.
    Shared(u32),
    Nil,
    Load(u32),
    LoadOptional(u32, u32, Receiving),
    ReceiverBound(u32, u32),
    Unbound(u32, Receiving),
    NonCallable(u32),
    Bind(u32, u32),
    BindEnd,
    Declare(u32),
    Shadow(u32),
    BlockArg(u32, bool),
    Attach(u32),
    BlockGiven(bool, bool),
    CheckBlock,
    Yield(u32),
    Store(u32),
    Pop,
    Dup,
    Unary(Operator),
    Binary(Operator),
    /// The flag marks an append whose enclosing loop result is unused.
    Shovel(CallSite, bool),
    AddStore(u32),
    Array(u32),
    TextStart,
    TextPart,
    TextEnd(bool),
    Hash(u32),
    RangeStart,
    Range(bool, bool, bool),
    Index(u32),
    /// Indexes by a literal string, borrowing its compiled bytes for plain hashes.
    IndexLiteral(u32),
    AddressLocal(u32),
    AddressBound(u32, u32),
    AddressValue,
    AddressIndex(u32),
    AddressTarget(u32, bool),
    AddressMember(CallSite),
    AddressNamespaceField(CallSite),
    AddressMemberTarget(CallSite, bool),
    AddressStore,
    AddressDrop,
    Mutate(CallSite, u32),
    /// Selects a destructuring target's value by an index into
    /// [`Program::selections`].
    Extract(u32),
    CaseCompare(bool, bool),
    LoopStart {
        iterable: bool,
        expression: bool,
        next: u32,
        end: u32,
    },
    LoopTest,
    IterNext,
    LoopBody,
    LoopEnd,
    LoopGuard(bool),
    Break(bool),
    Next(bool),
    Call(u32, u32),
    /// Calls script function `.0` with the `.1` arguments on top of the stack
    /// and block `.2`, as a call with a block and plain arguments.
    CallBlock(u32, u32, u32),
    AutoCall(u32, Receiving),
    Host(u32, u32),
    HostValue(u32, Receiving),
    Method(CallSite, u32),
    /// Calls instance method `.0` of class `.1` with the `.2` arguments on
    /// top of the stack when the receiver under them is an instance of that
    /// class, as the checker proves it is, and then skips the dynamic call
    /// that follows it for any other receiver.
    MethodOf(u32, u32, u32),
    /// Calls a builtin member the checker bound to the receiver's static
    /// base type ([`crate::members::direct`]), dispatching dynamically when
    /// the receiver's runtime kind is another.
    Direct(CallSite, u32),
    Arguments,
    RootCall(u32, bool),
    ResolveCall(u32, u32),
    CallName(u32, u32),
    CallValue,
    CallMember(CallSite),
    Bypass(u32),
    BypassEnd(u32),
    Argument(ArgumentOp),
    Invoke(Invocation),
    InvokeRoot(Invocation),
    Jump(u32),
    JumpFalse(u32),
    JumpTrue(u32),
    JumpNil(u32),
    AddressJumpNil(u32, bool),
    Return,
    Finish,
}

const _: () = assert!(std::mem::size_of::<Op>() == 16);

/// Narrows an index to the 32 bits instructions and frames keep: a
/// program's slots, jump targets and tables fit, as its source is at most
/// [`crate::syntax::MAX_SOURCE`] bytes, and no stack holds four billion
/// entries within memory.
#[inline]
pub(crate) fn narrow(index: usize) -> u32 {
    debug_assert!(u32::try_from(index).is_ok());
    index as u32
}

/// The slot of [`Op::ResolveCall`] and [`Op::CallName`] for a name without
/// a local slot, or whose local an assignment skips.
pub(crate) const NO_SLOT: u32 = u32::MAX;

macro_rules! operators {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// A unary or binary operator, kept as a code through scalar execution.
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub(crate) enum Operator {
            $($variant),+
        }

        impl Operator {
            /// The operator spelled `name`, if it is one.
            pub fn new(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }

            /// The operator's spelling for overloads and non-scalar operations.
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }

        impl std::fmt::Debug for Operator {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple("Operator").field(&(*self as u8)).finish()
            }
        }
    };
}

operators! {
    Add => "+",
    Subtract => "-",
    Multiply => "*",
    Divide => "/",
    FloorDivide => "//",
    Modulo => "%",
    Power => "**",
    Equal => "==",
    NotEqual => "!=",
    CaseEqual => "===",
    Match => "=~",
    NotMatch => "!~",
    Less => "<",
    LessEqual => "<=",
    Greater => ">",
    GreaterEqual => ">=",
    Compare => "<=>",
    BitAnd => "&",
    BitOr => "|",
    BitXor => "^",
    ShiftLeft => "<<",
    ShiftRight => ">>",
    And => "&&",
    Or => "||",
    Not => "!",
}

/// How a read of a bare name treats executable code the name resolves to. Go
/// runs a statically bound function or builtin named in a value position, but
/// a member receiver keeps a dynamically bound one, such as a required file's
/// own function or a module export, as a value, and `name.call` keeps every
/// callable. The member's name index names it in the resulting error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receiving(u32);

impl Receiving {
    /// An ordinary read.
    pub(crate) const VALUE: Self = Self(0);
    // The low bits hold how the receiver is read, and the rest the member's
    // name index. The receiver of a member other than `call`:
    const MEMBER: u32 = 1;
    // Of `call` without parentheses, arguments or a block:
    const CALL: u32 = 2;
    // Of `call()` or `call { }`, which passes no argument values:
    const CALL_EMPTY: u32 = 3;
    // Of `call` with argument values:
    const CALL_ARGUMENTS: u32 = 4;
    const KIND_BITS: u32 = 3;

    /// Selects the rule for the receiver of `member`, called in `form` with
    /// `arguments` argument values.
    pub(crate) fn of(member: &str, index: usize, form: CallForm, arguments: usize) -> Self {
        let kind = if member != "call" {
            Self::MEMBER
        } else if form == CallForm::Auto {
            Self::CALL
        } else if arguments == 0 {
            Self::CALL_EMPTY
        } else {
            Self::CALL_ARGUMENTS
        };
        debug_assert!(index < 1 << (32 - Self::KIND_BITS));
        Self(narrow(index) << Self::KIND_BITS | kind)
    }

    fn kind(self) -> u32 {
        self.0 & ((1 << Self::KIND_BITS) - 1)
    }

    /// The name index of the member the receiver is read for.
    pub(crate) fn member(self) -> Option<usize> {
        (self != Self::VALUE).then_some((self.0 >> Self::KIND_BITS) as usize)
    }

    /// Reports whether a statically bound callable runs. `parameters` is a
    /// script function's parameter count and `None` for other callables.
    pub(crate) fn runs_static(self, parameters: Option<usize>) -> bool {
        match self.kind() {
            Self::CALL => false,
            Self::CALL_EMPTY => parameters != Some(0),
            _ => true,
        }
    }

    /// Reports whether a dynamically bound callable runs.
    pub(crate) fn runs_dynamic(self) -> bool {
        self == Self::VALUE
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ArgumentOp {
    Positional,
    Splat,
    Keyword(u32),
    KeywordSplat,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Invocation {
    ImplicitMember(u32, u32),
    Builtin(Builtin),
    Function(u32),
    Host(u32),
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
    pub name: u32,
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
    Slice,
    ByteSlice,
    GetByte,
    First,
    Last,
    ToArray,
    ExcludeEnd,
    Empty,
    Reverse,
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
            "slice" => Self::Slice,
            "byteslice" => Self::ByteSlice,
            "getbyte" => Self::GetByte,
            "first" => Self::First,
            "last" => Self::Last,
            "to_a" => Self::ToArray,
            "exclude_end?" => Self::ExcludeEnd,
            "empty?" => Self::Empty,
            "reverse" => Self::Reverse,
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
            "key?" => Self::Key,
            "value?" => Self::HasValue,
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
            "bytesize" => Self::ByteSize,
            "include?" => Self::Include,
            "index" => Self::Index,
            "rindex" => Self::Rindex,
            "split" => Self::Split,
            "join" => Self::Join,
            "push" => Self::Push,
            "prepend" => Self::Prepend,
            "pop" => Self::Pop,
            "shift" => Self::Shift,
            "delete" => Self::Delete,
            "insert" => Self::Insert,
            "clear" => Self::Clear,
            "fill" => Self::Fill,
            "replace" => Self::Replace,
            "dup" => Self::Dup,
            "sum" => Self::Sum,
            "keys" => Self::Keys,
            "values" => Self::Values,
            "to_s" => Self::ToString,
            "to_i" => Self::ToInt,
            "to_f" => Self::ToFloat,
            _ => return None,
        })
    }
}

/// A set of a function's instruction indices.
#[derive(Clone, Debug, Default)]
pub(crate) struct Bits(Vec<u64>);

impl Bits {
    fn insert(&mut self, index: usize) {
        let word = index / 64;
        if self.0.len() <= word {
            self.0.resize(word + 1, 0);
        }
        self.0[word] |= 1 << (index % 64);
    }

    /// Whether the set holds instruction `index`.
    #[inline]
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
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
    /// Where a call from checked code starts, past the prologue's parameter
    /// checks, when every parameter is a required positional one that such
    /// a call binds directly: untyped, or typed with a type whose check the
    /// checker proves (see [`crate::types::Type::unproven`]). Host entry
    /// calls still run the prologue.
    pub proven: Option<usize>,
    pub locals: usize,
    pub code: Vec<Op>,
    /// Instructions whose value the checker proves plain (see
    /// [`crate::typing::Facts`]), so the scan for host methods and exported
    /// functions that would follow them is skipped: indexes, member and
    /// function calls, arguments, block parameters, `for` elements and
    /// returns.
    pub plain_values: Bits,
    /// Iterating member calls whose receiver and arguments are plain, so
    /// the values they give their block need no scan either.
    pub plain_inputs: Bits,
    /// Writes of an instance variable whose type the checker proves, so the
    /// runtime does not check the value against it (see
    /// [`Program::prove_instance_variables`]).
    pub proven_ivars: Bits,
    pub captures: Vec<Option<Capture>>,
    pub block_arity: usize,
    pub local_names: Vec<String>,
    pub return_type: Option<usize>,
    /// The declared result type when the runtime still checks the returned
    /// value against it: an unproven type, or any result of an instance
    /// method. The checker proves the rest.
    pub return_check: Option<usize>,
    /// Whether the function returns `nil` when its body finishes, having
    /// evaluated the last expression for effect: a function the static
    /// language compiles without `-> T` (ADR-007).
    pub returns_nil: bool,
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
    /// The instance variables each class declares, with their types, by
    /// namespace index.
    pub ivars: HashMap<usize, Vec<(String, usize)>>,
    /// Field names by fixed slot, including declared variables and accessors.
    pub field_layouts: HashMap<usize, Vec<String>>,
    pub declarations: Vec<Value>,
    pub enum_definitions: std::sync::Arc<[std::sync::Arc<crate::enums::Definition>]>,
    pub declaration_names: HashMap<String, usize>,
    pub globals: Vec<(Global, Value)>,
    pub functions: Vec<Function>,
    pub constants: Vec<Value>,
    /// Compiled literal code, or a pattern error deferred until evaluation.
    pub regexes: Vec<regex::Literal>,
    /// The constant index of each distinct string and symbol literal, which
    /// [`Op::Shared`] reads by slot.
    pub shared: Vec<usize>,
    /// The slot of each literal in [`Self::shared`] by whether it is a
    /// symbol and its text, while compiling.
    shared_slots: HashMap<(bool, Vec<u8>), usize>,
    pub names: HashMap<String, usize>,
    pub hosts: Vec<String>,
    pub members: Vec<String>,
    /// The class each `raise` names ([`Op::RaiseStart`]): its name index,
    /// and the slot of a local that may shadow it.
    pub raises: Vec<(usize, Option<usize>)>,
    /// How each destructuring target selects its value ([`Op::Extract`]).
    pub selections: Vec<Selection>,
    /// Top-level declarations in source order, for [`crate::Script::declarations`].
    pub outline: Vec<crate::Declaration>,
}

/// Compiles a host script without type checking it, for tests of code
/// generation alone.
#[cfg(test)]
pub(crate) fn compile(
    source: &str,
    hosts: Vec<String>,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    compile_mode(source, hosts, false, work)
}

/// Compiles a required file without type checking it, for tests of code
/// generation alone.
#[cfg(test)]
pub(crate) fn compile_file(
    source: &str,
    hosts: Vec<String>,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    compile_mode(source, hosts, true, work)
}

#[cfg(test)]
fn compile_mode(
    source: &str,
    hosts: Vec<String>,
    file: bool,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    let facts = crate::typing::Facts::default();
    compile_parsed(
        source,
        syntax::parse(source, work)?,
        hosts,
        file,
        &facts,
        work,
    )
}

/// Compiles parsed declarations of `source`, which the static type checker
/// has already read, with the `facts` it proved about them.
pub(crate) fn compile_parsed(
    source: &str,
    parsed: syntax::Declarations,
    hosts: Vec<String>,
    file: bool,
    facts: &crate::typing::Facts,
    work: &dyn crate::compilation::Work,
) -> Result<Program> {
    let mut outline = Vec::with_capacity(parsed.outline.len());
    for entry in parsed.outline {
        work.bytes(entry.name.len())?;
        outline.push(crate::Declaration {
            kind: entry.kind,
            name: entry.name.into_string(),
            span: entry.start..entry.end,
        });
    }
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
        ivars: HashMap::new(),
        field_layouts: HashMap::new(),
        declarations,
        enum_definitions,
        declaration_names,
        globals: Vec::new(),
        functions: Vec::new(),
        constants: Vec::new(),
        regexes: Vec::new(),
        shared: Vec::new(),
        shared_slots: HashMap::new(),
        names,
        hosts,
        members: Vec::new(),
        raises: Vec::new(),
        selections: Vec::new(),
        outline,
    };
    let declared = |name: &str| {
        program.declaration_names.contains_key(name)
            || parsed.modules.iter().any(|module| module.name == name)
    };
    let mut typing = typing::Typing::new(parsed.additions, &parsed.modules, declared, work)?;
    for module in parsed.modules {
        program.register_module(module, "", &mut defs, &mut contexts, &mut typing, work)?;
    }
    program.functions = (0..defs.len()).map(|_| Function::default()).collect();
    for (index, def) in defs.into_iter().enumerate() {
        work.bytes(def.name.len())?;
        let binds_parameters = def
            .params
            .iter()
            .any(|p| p.default.is_some() || p.ty.is_some());
        let plain = !binds_parameters && def.params.iter().all(|p| p.kind == ParamKind::Positional);
        let compiling = Compiling::new(Compiler {
            work,
            facts,
            aliases: &typing.aliases,
            namespace: contexts[index].0,
            instance: contexts[index].2,
            program: &mut program,
            block: None,
            typed: Table::new(),
            outer_typed: Buffer::new(),
            locals: Table::new(),
            slots: 0,
            code: Vec::new(),
            plain_values: Bits::default(),
            plain_inputs: Bits::default(),
            locations: Vec::new(),
            offset: def.offset,
            parameters: Table::new(),
            loop_bindings: Buffer::new(),
            outer: Buffer::new(),
            reads: Table::new(),
            assigned: Table::new(),
        });
        let additions = Additions {
            block: if index == 0 {
                None
            } else {
                typing.block(def.offset)
            },
            prologue: typing.prologues.get(&index).map_or(&[], Vec::as_slice),
        };
        compiling.run(Call::Function(&def, binds_parameters, additions))?;
        let params = compiling.params.take();
        let proven = compiling.proven.take();
        let mut c = compiling.compiler.into_inner();
        let finish = c.emit(Op::Finish);
        c.locations[finish] = def.body.last().map_or(def.offset, |stmt| stmt.offset);
        // The top level, a namespace body and an accessor keep their
        // value; a getter returns it explicitly anyway.
        let returns_nil =
            index != 0 && def.return_type.is_none() && def.accessor.is_none() && !contexts[index].1;
        let plain_finish = returns_nil || c.plain_result(&def.body);
        c.mark_plain(finish, plain_finish);
        let return_type = def
            .return_type
            .as_ref()
            .map(|ty| c.annotation(ty))
            .transpose()?;
        // A method may return an instance variable its class never assigned,
        // which reads as nil whatever its declared type, so a method keeps
        // its result check unless the checker proves its class assigns every
        // variable before any method can read it.
        let unassigned = contexts[index].2 && !facts.proven_result(&def);
        let return_check = return_type.filter(|&ty| unassigned || c.program.types[ty].unproven());
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
            proven,
            locals: c.slots,
            local_names: local_names(&c.locals, c.slots, work)?,
            code: c.code,
            plain_values: c.plain_values,
            plain_inputs: c.plain_inputs,
            proven_ivars: Bits::default(),
            captures: Vec::new(),
            block_arity: 0,
            returns_nil,
            return_type,
            return_check,
        };
        program.functions[index] = function;
    }
    program.shared_slots = HashMap::new();
    loops::discarded(&mut program.functions);
    program.prove_instance_variables(work)?;
    work.checkpoint()?;
    Ok(program)
}

impl Program {
    /// Marks the writes of instance variables in each class's methods whose
    /// check the checker proves: an instance variable whose declared or
    /// accessor type is absent, or is a type the checker proves (see
    /// [`crate::types::Type::unproven`]). A method runs only on an instance
    /// of its own class, so its writes reach that class's variables, typed
    /// as the runtime would find them.
    fn prove_instance_variables(&mut self, work: &dyn crate::compilation::Work) -> Result<()> {
        for index in 0..self.functions.len() {
            let function = &self.functions[index];
            let (true, Some(class)) = (function.instance, function.namespace) else {
                continue;
            };
            let mut proven = Bits::default();
            for (ip, op) in function.code.iter().enumerate() {
                work.charge(1)?;
                let (Op::NamespaceStore(name)
                | Op::NamespaceAddress(name, _)
                | Op::BindIvar(name, _)) = *op
                else {
                    continue;
                };
                let raw = &self.members[name as usize];
                let field = raw.strip_prefix('@').unwrap_or(raw);
                let checked = matches!(op, Op::BindIvar(..))
                    || (raw.starts_with('@') && !raw.starts_with("@@"));
                if checked
                    && self
                        .instance_variable_type(class, field)
                        .is_none_or(|ty| !self.types[ty].unproven())
                {
                    proven.insert(ip);
                }
            }
            self.functions[index].proven_ivars = proven;
            let layout = &self.field_layouts[&class];
            for op in &mut self.functions[index].code {
                let name = match *op {
                    Op::BindIvar(name, _) => self.members[name as usize].as_str(),
                    Op::NamespaceVariable(name, _)
                    | Op::NamespaceStore(name)
                    | Op::NamespaceAddress(name, _) => {
                        let raw = &self.members[name as usize];
                        if !raw.starts_with('@') || raw.starts_with("@@") {
                            continue;
                        }
                        &raw[1..]
                    }
                    _ => continue,
                };
                let slot = narrow(
                    layout
                        .iter()
                        .position(|field| field == name)
                        .expect("declared field"),
                );
                *op = match *op {
                    Op::BindIvar(_, local) => Op::BindField(slot, local),
                    Op::NamespaceVariable(..) => Op::InstanceField(slot),
                    Op::NamespaceStore(..) => Op::InstanceStore(slot),
                    Op::NamespaceAddress(..) => Op::InstanceAddress(slot),
                    _ => unreachable!(),
                };
            }
        }
        Ok(())
    }

    /// The type an instance of `class` checks its variable `name` against,
    /// as the runtime's `property_type` finds it: the declared type, or else
    /// the type of a generated setter's value or getter's result.
    fn instance_variable_type(&self, class: usize, name: &str) -> Option<usize> {
        if let Some(&(_, ty)) = self
            .ivars
            .get(&class)
            .and_then(|ivars| ivars.iter().find(|(field, _)| field == name))
        {
            return Some(ty);
        }
        let methods = &self.namespaces[class].instance_methods;
        let mut getter = None;
        let mut setter = None;
        for method in methods {
            if method.name.strip_suffix('=') == Some(name) {
                setter = Some(method.function);
            }
            if method.name == name {
                getter = Some(method.function);
            }
        }
        if let Some(setter) = setter {
            let function = &self.functions[setter];
            return function
                .accessor
                .as_ref()
                .filter(|(field, setter)| field == name && *setter)
                .and_then(|_| function.params.first().and_then(|param| param.ty));
        }
        getter.and_then(|getter| {
            let function = &self.functions[getter];
            function
                .accessor
                .as_ref()
                .filter(|(field, setter)| field == name && !setter)
                .and(function.return_type)
        })
    }
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
    /// What the checker proved about the syntax being compiled.
    facts: &'a crate::typing::Facts,
    aliases: &'a aliases::Aliases,
    instance: bool,
    namespace: Option<usize>,
    program: &'a mut Program,
    /// The declared block of the function being generated, which each
    /// `yield` in it and its blocks is checked against.
    block: Option<BlockContract>,
    /// The declared type and check subject of each typed local.
    typed: Table<(usize, usize)>,
    /// The typed locals of each enclosing scope, parallel to `outer`.
    outer_typed: Buffer<Table<(usize, usize)>>,
    locals: Table<usize>,
    slots: usize,
    code: Vec<Op>,
    /// The generated code's [`Function::plain_values`].
    plain_values: Bits,
    /// The generated code's [`Function::plain_inputs`].
    plain_inputs: Bits,
    locations: Vec<u32>,
    offset: u32,
    parameters: Table<()>,
    loop_bindings: Buffer<Buffer<usize>>,
    outer: Buffer<Table<usize>>,
    reads: Table<()>,
    assigned: Table<()>,
}

/// A declared block's argument and result types, each with its check
/// subject. Without a result type the block's value is discarded.
struct BlockContract {
    params: Vec<(usize, usize)>,
    result: Option<(usize, usize)>,
}

/// The state of the function being generated, set aside while a nested block
/// is generated in its place.
struct Scope {
    typed: Table<(usize, usize)>,
    outer_typed: Buffer<Table<(usize, usize)>>,
    locals: Table<usize>,
    slots: usize,
    code: Vec<Op>,
    /// The generated code's [`Function::plain_values`].
    plain_values: Bits,
    /// The generated code's [`Function::plain_inputs`].
    plain_inputs: Bits,
    locations: Vec<u32>,
    offset: u32,
    parameters: Table<()>,
    loop_bindings: Buffer<Buffer<usize>>,
    outer: Buffer<Table<usize>>,
    reads: Table<()>,
    assigned: Table<()>,
}

// A syntax item still to visit, walked on an explicit stack because syntax
// nests as deeply as the parser allows.
#[derive(Clone, Copy)]
enum Item<'x> {
    Stmts(&'x [Stmt]),
    Stmt(&'x Stmt),
    Expr(&'x Expr),
    Callee(&'x Expr),
    Target(&'x Target),
}

impl<'x> Item<'x> {
    // Push children in reverse so they are visited in source order.
    fn push_statement(
        stmt: &'x Stmt,
        pending: &mut Buffer<Item<'x>>,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        let mut items = Buffer::new();
        match &stmt.node {
            Statement::Raise(value, message) => {
                for value in value.iter().chain(message) {
                    items.push(work, Item::Expr(value))?;
                }
            }
            Statement::Module(_)
            | Statement::UnboundClass(_)
            | Statement::Unsupported
            | Statement::Retry => (),
            Statement::Expr(e) => items.push(work, Item::Expr(e))?,
            Statement::Assign(target, _, value) => {
                items.push(work, Item::Target(target))?;
                items.push(work, Item::Expr(value))?;
            }
            Statement::If(branches, alternate, _) => {
                for (cond, body) in branches {
                    items.push(work, Item::Expr(cond))?;
                    items.push(work, Item::Stmts(body))?;
                }
                items.push(work, Item::Stmts(alternate))?;
            }
            Statement::While(cond, body, _) => {
                items.push(work, Item::Expr(cond))?;
                items.push(work, Item::Stmts(body))?;
            }
            Statement::For(target, iterable, body) => {
                items.push(work, Item::Target(target))?;
                items.push(work, Item::Expr(iterable))?;
                items.push(work, Item::Stmts(body))?;
            }
            Statement::Return(e) | Statement::Break(e) | Statement::Next(e) => {
                if let Some(e) = e {
                    items.push(work, Item::Expr(e))?;
                }
            }
        }
        while let Some(item) = items.pop() {
            pending.push(work, item)?;
        }
        Ok(())
    }

    // Visit an expression's operands, leaving attached block bodies to their
    // own functions unless `blocks` asks for them too.
    fn push_expression(
        expr: &'x Expr,
        blocks: bool,
        pending: &mut Buffer<Item<'x>>,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        let mut items = Buffer::new();
        match &expr.node {
            Node::Try(attempt) => {
                items.push(work, Item::Stmts(&attempt.body))?;
                for rescue in &attempt.rescues {
                    items.push(work, Item::Stmts(&rescue.body))?;
                }
                items.push(work, Item::Stmts(&attempt.alternate))?;
                items.push(work, Item::Stmts(&attempt.ensure))?;
            }
            Node::Shape(_, fallback, _) => {
                if let Some(fallback) = fallback {
                    items.push(work, Item::Expr(fallback))?;
                }
            }
            Node::Regex(..)
            | Node::Literal(_)
            | Node::Integer(_)
            | Node::BigInteger(..)
            | Node::Var(_) => (),
            Node::Array(values) | Node::Yield(values) | Node::Template(values, _) => {
                for value in values {
                    items.push(work, Item::Expr(value))?;
                }
            }
            Node::Call(_, args, _) => {
                for arg in args {
                    items.push(work, Item::Expr(&arg.value))?;
                }
            }
            Node::BlockCall(call, block) => {
                items.push(work, Item::Expr(call))?;
                if blocks {
                    items.push(work, Item::Stmts(&block.body))?;
                }
            }
            Node::Hash(entries) => {
                for (_, value) in entries {
                    items.push(work, Item::Expr(value))?;
                }
            }
            Node::Unary(_, value) | Node::Member(value, _) | Node::SafeMember(value, _) => {
                items.push(work, Item::Expr(value))?
            }
            Node::Binary(_, a, b) => {
                items.push(work, Item::Expr(a))?;
                items.push(work, Item::Expr(b))?;
            }
            Node::Range(a, b, _) => {
                for value in a.iter().chain(b) {
                    items.push(work, Item::Expr(value))?;
                }
            }
            Node::Conditional(branches, alternate) => {
                for (cond, result) in branches {
                    items.push(work, Item::Expr(cond))?;
                    items.push(work, Item::Expr(result))?;
                }
                items.push(work, Item::Expr(alternate))?;
            }
            Node::Case(target, clauses, alternate) => {
                if let Some(e) = target {
                    items.push(work, Item::Expr(e))?;
                }
                for clause in clauses {
                    for (e, _) in &clause.values {
                        items.push(work, Item::Expr(e))?;
                    }
                    items.push(work, Item::Expr(&clause.result))?;
                }
                if let Some(e) = alternate {
                    items.push(work, Item::Expr(e))?;
                }
            }
            Node::Compound(stmt) => items.push(work, Item::Stmt(stmt))?,
            Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
                items.push(work, Item::Expr(recv))?;
                for arg in args {
                    items.push(work, Item::Expr(&arg.value))?;
                }
            }
            Node::ComputedCall(recv, args) => {
                items.push(work, Item::Callee(recv))?;
                for arg in args {
                    items.push(work, Item::Expr(&arg.value))?;
                }
            }
            Node::Scope(recv, _, args) => {
                items.push(work, Item::Expr(recv))?;
                for arg in args.iter().flatten() {
                    items.push(work, Item::Expr(&arg.value))?;
                }
            }
            Node::Index(recv, args) => {
                items.push(work, Item::Expr(recv))?;
                for arg in args {
                    items.push(work, Item::Expr(arg))?;
                }
            }
        }
        while let Some(item) = items.pop() {
            pending.push(work, item)?;
        }
        Ok(())
    }

    fn push_target(
        target: &'x Target,
        pending: &mut Buffer<Item<'x>>,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        match target {
            Target::Typed(target, _) => pending.push(work, Item::Target(target))?,
            Target::Value(e) => pending.push(work, Item::Expr(e))?,
            Target::Tuple(parts) => {
                for (part, _) in parts.iter().rev() {
                    if let Some(part) = part {
                        pending.push(work, Item::Target(part))?;
                    }
                }
            }
        }
        Ok(())
    }
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
    fn local(&self, name: &str) -> Result<bool> {
        self.locals.contains(self.work, name)
    }
    fn capture_name(&mut self, name: &Name) -> Result<()> {
        if self.outer_binding(name)?.is_some() {
            self.slot(name)?;
        }
        Ok(())
    }
    fn declare(&mut self, body: &[Stmt]) -> Result<()> {
        self.work.charge(1)?;
        let mut pending = Buffer::from_array(self.work, [Item::Stmts(body)])?;
        self.declare_items(&mut pending)
    }
    fn declare_target(&mut self, target: &Target) -> Result<()> {
        let mut pending = Buffer::from_array(self.work, [Item::Target(target)])?;
        self.declare_items(&mut pending)
    }
    fn declare_expr(&mut self, e: &Expr) -> Result<()> {
        let mut pending = Buffer::from_array(self.work, [Item::Expr(e)])?;
        self.declare_items(&mut pending)
    }
    // Allocate slots for assigned and captured names in source order.
    fn declare_items<'x>(&mut self, pending: &mut Buffer<Item<'x>>) -> Result<()> {
        while let Some(item) = pending.pop() {
            self.work.charge(1)?;
            match item {
                Item::Stmts(body) => {
                    for stmt in body.iter().rev() {
                        pending.push(self.work, Item::Stmt(stmt))?;
                    }
                }
                Item::Stmt(stmt) => Item::push_statement(stmt, pending, self.work)?,
                Item::Target(Target::Value(Expr {
                    node: Node::Var(name),
                    ..
                })) => {
                    if !self.namespace_binding(name)?
                        && (!self.outer.is_empty() || self.global_binding(name)?.is_none())
                    {
                        self.slot(name)?;
                    }
                    self.assigned.insert(self.work, name.clone(), ())?;
                }
                Item::Target(target) => Item::push_target(target, pending, self.work)?,
                Item::Expr(e) => {
                    match &e.node {
                        Node::Var(name) => {
                            self.reads.insert(self.work, name.clone(), ())?;
                            self.capture_name(name)?;
                        }
                        Node::Call(name, _, _) => {
                            if name != "it" {
                                self.reads.insert(self.work, name.clone(), ())?;
                            }
                            self.capture_name(name)?;
                        }
                        _ => (),
                    }
                    Item::push_expression(e, false, pending, self.work)?;
                }
                // As with a named call, `it` in callee position, including either branch
                // of a rescue modifier callee, names a function rather than the implicit
                // parameter.
                Item::Callee(callee) => match &callee.node {
                    Node::Var(name) if name == "it" => self.capture_name(name)?,
                    Node::Try(attempt) if attempt.modifier => {
                        pending.push(self.work, Item::Stmts(&attempt.ensure))?;
                        pending.push(self.work, Item::Stmts(&attempt.alternate))?;
                        let rescues = attempt.rescues.iter().flat_map(|rescue| rescue.body.iter());
                        for stmt in attempt.body.iter().chain(rescues).rev() {
                            let item = match &stmt.node {
                                Statement::Expr(expr) => Item::Callee(expr),
                                _ => Item::Stmt(stmt),
                            };
                            pending.push(self.work, item)?;
                        }
                    }
                    _ => pending.push(self.work, Item::Expr(callee))?,
                },
            }
        }
        Ok(())
    }
    fn emit(&mut self, op: Op) -> usize {
        let pos = self.code.len();
        self.code.push(op);
        self.locations.push(self.offset);
        pos
    }
    /// Emits unary operator `op`.
    fn unary(&mut self, op: &str) -> Result<usize> {
        let operator = Operator::new(op)
            .ok_or_else(|| syntax::unsupported(self.work, "unsupported operator"))?;
        Ok(self.emit(Op::Unary(operator)))
    }
    /// Emits binary operator `op`.
    fn binary(&mut self, op: &str) -> Result<usize> {
        let operator = Operator::new(op)
            .ok_or_else(|| syntax::unsupported(self.work, "unsupported operator"))?;
        Ok(self.emit(Op::Binary(operator)))
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
            | Op::FileValue(_, n, _)
            | Op::FileAddress(_, n)
            | Op::RootAddress(_, n)
            | Op::AmbientValue(_, n)
            | Op::AmbientAddress(_, n)
            | Op::ImplicitAddress(_, n)
            | Op::TypeShadowed(_, n)
            | Op::AddressBound(_, n)
            | Op::ReceiverBound(_, n) => *n = narrow(target),
            _ => unreachable!(),
        }
    }
    fn constant(&mut self, v: Value) {
        let n = self.program.constants.len();
        self.program.constants.push(v);
        self.emit(Op::Constant(narrow(n)));
    }
    /// Pushes a literal, sharing a string or symbol with every literal of
    /// the program that has the same text.
    fn literal(&mut self, value: Value) {
        let (symbol, bytes) = match &value.0 {
            crate::value::Kind::Bytes(bytes) => (false, bytes),
            crate::value::Kind::Symbol(bytes) => (true, bytes),
            _ => return self.constant(value),
        };
        let text = (symbol, bytes.data.to_vec());
        let slot = match self.program.shared_slots.get(&text) {
            Some(&slot) => slot,
            None => {
                let slot = self.program.shared.len();
                self.program.shared.push(self.program.constants.len());
                self.program.constants.push(value);
                self.program.shared_slots.insert(text, slot);
                slot
            }
        };
        self.emit(Op::Shared(narrow(slot)));
    }
    fn integer_literal(&mut self, text: Value, radix: u32) {
        let n = self.program.constants.len();
        self.program.constants.push(text);
        self.emit(Op::Integer(narrow(n), radix));
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
    fn declare_bindings(&mut self, body: &[Stmt]) -> Result<()> {
        for slot in self.statement_bindings(body)? {
            self.emit(Op::Declare(narrow(slot)));
        }
        Ok(())
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
                self.emit(Op::InitNamespace(narrow(module.index)));
            }
        }
        Ok(())
    }
    fn variable_expression(&mut self, name: &str, receiving: Receiving) -> Result<()> {
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
            self.emit(Op::FileValue(name, 0, receiving))
        });
        if let Some(&slot) = self.locals.get(self.work, name)? {
            if !self.parameters.contains(self.work, name)? {
                let name = self.call_site(name, false).name;
                self.emit(Op::LoadOptional(narrow(slot), name, receiving));
            } else {
                self.emit(Op::Load(narrow(slot)));
            }
        } else if let Some(&index) = self.program.declaration_names.get(name) {
            self.emit(Op::Declaration(narrow(index)));
        } else if let Some(&fun) = self.program.names.get(name) {
            self.emit(Op::AutoCall(narrow(fun), receiving));
        } else if let Some(host) = self.host_position(name)? {
            self.emit(Op::HostValue(narrow(host), receiving));
        } else if let Some(global) = global {
            self.emit(Op::Global(narrow(global)));
        } else if let Some(alias) = self.aliases.get(
            aliases::scope(&self.program.namespaces, self.namespace),
            name,
        ) {
            let shape = crate::shapes::compile(alias.clone());
            self.constant(shape);
        } else {
            let site = self.call_site(name, false);
            self.emit(Op::Unbound(site.name, receiving));
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
    fn leaf(&mut self, e: &Expr, receiving: Receiving) -> Result<()> {
        self.work.charge(1)?;
        match &e.node {
            Node::Regex(pattern, flags) => {
                self.work.bytes(pattern.len())?;
                let index = self.program.regexes.len();
                let compiled =
                    regex::Literal::new(pattern.compiler_constant(), *flags, self.work.unmetered());
                self.work.checkpoint()?;
                self.program.regexes.push(compiled);
                self.emit(Op::Regex(narrow(index)));
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
            Node::Literal(v) => self.literal(v.compiler_constant()),
            Node::Var(name) if name.starts_with('@') => {
                let name = self.call_site(name, false).name;
                self.emit(Op::NamespaceVariable(name, true));
            }
            Node::Var(name) if name == "self" && self.namespace.is_some() => {
                self.emit(Op::NamespaceSelf(narrow(self.namespace.unwrap())));
            }
            Node::Var(name) if name == "block_given?" => {
                self.emit(Op::BlockGiven(false, false));
            }
            Node::Var(name) => return self.variable_expression(name, receiving),
            _ => unreachable!(),
        }
        Ok(())
    }
    fn call_site(&mut self, name: &str, auto: bool) -> CallSite {
        let index = self.program.members.len();
        self.program.members.push(name.to_owned());
        CallSite {
            name: narrow(index),
            method: Method::parse(name),
            auto,
            scope: false,
        }
    }
    fn annotation(&mut self, ty: &crate::compilation::Type) -> Result<usize> {
        let index = self.program.types.len();
        let scope = aliases::scope(&self.program.namespaces, self.namespace);
        let compiled = self.aliases.compile(scope, ty, self.work)?;
        self.program.types.push(compiled);
        Ok(index)
    }
    /// Adds the subject a value check names in its failure, such as
    /// `local variable count`.
    fn subject(&mut self, parts: &[&str]) -> Result<usize> {
        let mut text = Buffer::new();
        for part in parts {
            text.extend_from_slice(self.work, part.as_bytes())?;
        }
        let index = self.program.constants.len();
        self.program.constants.push(Value::bytes(&*text));
        Ok(index)
    }
    /// The declared type and check subject of a typed local visible here.
    fn local_type(&self, name: &str) -> Result<Option<(usize, usize)>> {
        if self.typed.is_empty() && self.outer_typed.is_empty() {
            return Ok(None);
        }
        if let Some(&typed) = self.typed.get(self.work, name)? {
            return Ok(Some(typed));
        }
        if self.parameters.contains(self.work, name)? {
            return Ok(None);
        }
        let Some(capture) = self.outer_binding(name)? else {
            return Ok(None);
        };
        Ok(match self.outer_typed.get(capture.depth) {
            Some(typed) => typed.get(self.work, name)?.copied(),
            None => None,
        })
    }
    /// The instance method `name` and its class, when the checker proved
    /// that `call` always calls it on an instance of a class this program
    /// declares.
    fn method_of(&self, call: &Expr, name: &str) -> Option<(usize, usize)> {
        let class = self.facts.class(call)?;
        let index = self
            .program
            .namespaces
            .iter()
            .position(|definition| definition.name == class)?;
        let method = self.program.namespaces[index]
            .instance_methods
            .iter()
            .find(|method| method.name == name)?;
        Some((method.function, index))
    }
    /// Records that the value instruction `ip` leaves is plain.
    fn mark_plain(&mut self, ip: usize, plain: bool) {
        if plain {
            self.plain_values.insert(ip);
        }
    }
    /// Whether the value `body`'s statements leave is plain, as far as the
    /// checker proved it for the last one.
    fn plain_result(&self, body: &[Stmt]) -> bool {
        match body.last().map(|stmt| &stmt.node) {
            None => true,
            Some(Statement::Expr(value) | Statement::Assign(_, "=", value)) => {
                self.facts.plain(value)
            }
            Some(_) => false,
        }
    }
    /// Checks the value on top of the stack against `ty`, naming it by
    /// `subject`, unless the checker proves the check (see
    /// [`crate::types::Type::unproven`]).
    fn check(&mut self, ty: usize, subject: usize) {
        if self.program.types[ty].unproven() {
            self.emit(Op::Check(narrow(ty), narrow(subject)));
        }
    }
    /// Checks a value stored into `name` when it is a typed local.
    fn check_local(&mut self, name: &str) -> Result<()> {
        if let Some((ty, subject)) = self.local_type(name)? {
            self.check(ty, subject);
        }
        Ok(())
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
    // No declaration, function, host or builtin claims the name, so a read that
    // finds no binding falls back to a member of the running instance or class.
    fn implicit_name(&mut self, name: &str) -> Result<bool> {
        Ok(self.namespace.is_some()
            && !name.starts_with('@')
            && !matches!(name, "self" | "block_given?")
            && !self.program.declaration_names.contains_key(name)
            && !self.program.names.contains_key(name)
            && self.host_position(name)?.is_none()
            && self.global(name).is_none())
    }
    // Set the enclosing function aside and generate a block in its place,
    // closing over every scope the block can see.
    fn begin_block(&mut self) -> Result<Scope> {
        self.work.charge(1)?;
        let mut outer = Buffer::new();
        for scope in std::iter::once(&self.locals).chain(&self.outer) {
            outer.push(self.work, scope.copy(self.work)?)?;
        }
        let mut outer_typed = Buffer::new();
        if !self.typed.is_empty() || self.outer_typed.iter().any(|typed| !typed.is_empty()) {
            for scope in std::iter::once(&self.typed).chain(&self.outer_typed) {
                outer_typed.push(self.work, scope.copy(self.work)?)?;
            }
        }
        Ok(self.swap_scope(Scope {
            typed: Table::new(),
            outer_typed,
            locals: Table::new(),
            slots: 0,
            code: Vec::new(),
            plain_values: Bits::default(),
            plain_inputs: Bits::default(),
            locations: Vec::new(),
            offset: self.offset,
            parameters: Table::new(),
            loop_bindings: Buffer::new(),
            outer,
            reads: Table::new(),
            assigned: Table::new(),
        }))
    }
    fn swap_scope(&mut self, mut scope: Scope) -> Scope {
        std::mem::swap(&mut self.typed, &mut scope.typed);
        std::mem::swap(&mut self.outer_typed, &mut scope.outer_typed);
        std::mem::swap(&mut self.locals, &mut scope.locals);
        std::mem::swap(&mut self.slots, &mut scope.slots);
        std::mem::swap(&mut self.code, &mut scope.code);
        std::mem::swap(&mut self.plain_values, &mut scope.plain_values);
        std::mem::swap(&mut self.plain_inputs, &mut scope.plain_inputs);
        std::mem::swap(&mut self.locations, &mut scope.locations);
        std::mem::swap(&mut self.offset, &mut scope.offset);
        std::mem::swap(&mut self.parameters, &mut scope.parameters);
        std::mem::swap(&mut self.loop_bindings, &mut scope.loop_bindings);
        std::mem::swap(&mut self.outer, &mut scope.outer);
        std::mem::swap(&mut self.reads, &mut scope.reads);
        std::mem::swap(&mut self.assigned, &mut scope.assigned);
        scope
    }
    // Finish the block generated since `begin_block` and restore its parent.
    fn finish_block(&mut self, parent: Scope, block_arity: usize) -> Result<usize> {
        let mut captures = vec![None; self.slots];
        for (name, &slot) in self.locals.iter(self.work)? {
            self.work.bytes(name.len())?;
            if name.starts_with('\0') {
                continue;
            }
            captures[slot] = self.outer_binding(name)?;
        }
        let local_names = local_names(&self.locals, self.slots, self.work)?;
        let child = self.swap_scope(parent);
        let function = Function {
            offset: self.offset,
            locations: child.locations,
            trace_name: "<block>".into(),
            instance: self.instance,
            namespace: self.namespace,
            name: "<block>".into(),
            locals: child.slots,
            local_names,
            code: child.code,
            plain_values: child.plain_values,
            plain_inputs: child.plain_inputs,
            captures,
            block_arity,
            ..Function::default()
        };
        let index = self.program.functions.len();
        self.program.functions.push(function);
        Ok(index)
    }
}

/// What a function's typed declarations add to it: the block it declares and
/// the defaults a constructor assigns before its parameters bind.
#[derive(Clone, Copy)]
struct Additions<'x> {
    block: Option<&'x syntax::BlockParam>,
    prologue: &'x [Stmt],
}

/// Recursive code generation steps that run as tasks instead of native calls.
#[derive(Clone, Copy)]
enum Call<'x> {
    Function(&'x syntax::Definition, bool, Additions<'x>),
    Expr(&'x Expr),
    Block(&'x [Stmt]),
    Assign(&'x Target),
    Address(&'x Expr),
    AssignmentAddress(&'x Expr),
}

/// Generates one function over shared compiler state, so syntax nesting grows
/// a heap task stack instead of the native one.
struct Compiling<'a, 'x> {
    compiler: std::cell::RefCell<Compiler<'a>>,
    tasks: Tasks<Call<'x>, ()>,
    params: std::cell::RefCell<Vec<Parameter>>,
    /// The function's [`Function::proven`] start, once its prologue is generated.
    proven: std::cell::Cell<Option<usize>>,
}

impl<'a, 'x> Compiling<'a, 'x> {
    fn new(compiler: Compiler<'a>) -> Self {
        Self {
            compiler: std::cell::RefCell::new(compiler),
            tasks: Tasks::new(),
            params: std::cell::RefCell::new(Vec::new()),
            proven: std::cell::Cell::new(None),
        }
    }

    fn c(&self) -> std::cell::RefMut<'_, Compiler<'a>> {
        self.compiler.borrow_mut()
    }

    fn run(&self, call: Call<'x>) -> Result<()> {
        self.tasks.run(call, |call| self.start(call))
    }

    fn start(&self, call: Call<'x>) -> Task<'_, ()> {
        match call {
            Call::Function(def, binds_parameters, additions) => {
                Box::pin(self.function(def, binds_parameters, additions))
            }
            Call::Expr(e) => Box::pin(self.expr_task(e)),
            Call::Block(body) => Box::pin(self.block_task(body)),
            Call::Assign(target) => Box::pin(self.assign_value(target)),
            Call::Address(receiver) => Box::pin(self.address(receiver)),
            Call::AssignmentAddress(receiver) => Box::pin(self.assignment_address(receiver)),
        }
    }

    async fn expr(&self, e: &'x Expr) -> Result<()> {
        // Most expressions are leaves, which need no nested task.
        if leaf(e) {
            let mut c = self.c();
            c.work.charge(1)?;
            let previous = std::mem::replace(&mut c.offset, e.offset);
            let result = c.leaf(e, Receiving::VALUE);
            c.offset = previous;
            return result;
        }
        self.tasks.call(Call::Expr(e)).await
    }

    /// Compiles `e` as a member receiver, where a bare name treats a callable it
    /// resolves to by `receiving`.
    async fn receiver_expr(&self, e: &'x Expr, receiving: Receiving) -> Result<()> {
        if !matches!(e.node, Node::Var(_)) {
            return self.expr(e).await;
        }
        let mut c = self.c();
        c.work.charge(1)?;
        let previous = std::mem::replace(&mut c.offset, e.offset);
        let result = c.leaf(e, receiving);
        c.offset = previous;
        result
    }

    async fn block(&self, body: &'x [Stmt]) -> Result<()> {
        self.tasks.call(Call::Block(body)).await
    }

    // Destructuring and receiver chains nest, so their inner steps are tasks.
    async fn nested_assign(&self, target: &'x Target) -> Result<()> {
        self.tasks.call(Call::Assign(target)).await
    }

    async fn nested_address(&self, receiver: &'x Expr) -> Result<()> {
        self.tasks.call(Call::Address(receiver)).await
    }

    async fn function(
        &self,
        def: &'x syntax::Definition,
        binds_parameters: bool,
        additions: Additions<'x>,
    ) -> Result<()> {
        let work = self.c().work;
        if !additions.prologue.is_empty() {
            self.c().declare(additions.prologue)?;
            for stmt in additions.prologue {
                self.stmt(stmt, false).await?;
                self.c().emit(Op::Pop);
            }
        }
        if let Some(block) = additions.block {
            let mut c = self.c();
            let mut params = Vec::with_capacity(block.params.len());
            for (index, ty) in block.params.iter().enumerate() {
                let ty = c.annotation(ty)?;
                let subject = c.subject(&["yield argument ", &(index + 1).to_string()])?;
                params.push((ty, subject));
            }
            let result = match &block.result {
                Some(ty) => Some((c.annotation(ty)?, c.subject(&["block result"])?)),
                None => None,
            };
            c.block = Some(BlockContract { params, result });
        }
        let mut params = Vec::new();
        for (i, param) in def.params.iter().enumerate() {
            let (ty, bind) = {
                let mut c = self.c();
                work.bytes(param.name.len())?;
                let ty = param.ty.as_ref().map(|ty| c.annotation(ty)).transpose()?;
                let bind = binds_parameters.then(|| c.emit(Op::Bind(narrow(i), 0)));
                (ty, bind)
            };
            if let Some(value) = &param.default {
                self.c().declare_expr(value)?;
                self.expr(value).await?;
                // The checker proves most defaults' types.
                if let Some(ty) = ty.filter(|&ty| self.c().program.types[ty].unproven()) {
                    let mut c = self.c();
                    let label = c.program.constants.len();
                    c.program
                        .constants
                        .push(Value::bytes(param.name.as_bytes()));
                    c.emit(Op::Normalize(narrow(ty), narrow(label)));
                }
            }
            let mut c = self.c();
            let slot = c.slot(&param.name)?;
            if param.default.is_some() {
                c.emit(Op::Store(narrow(slot)));
                c.emit(Op::Pop);
            }
            if let Some(bind) = bind {
                let end = c.code.len();
                c.patch(bind, end);
            }
            if c.instance {
                if let Some(name) = &param.ivar {
                    let name = c.call_site(name, false).name;
                    c.emit(Op::BindIvar(name, narrow(slot)));
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
        {
            let mut c = self.c();
            if binds_parameters {
                c.emit(Op::BindEnd);
            }
            let direct = additions.prologue.is_empty()
                && def.params.iter().zip(&params).all(|(param, compiled)| {
                    param.kind == ParamKind::Positional
                        && param.default.is_none()
                        && !(c.instance && param.ivar.is_some())
                        && compiled.ty.is_none_or(|ty| !c.program.types[ty].unproven())
                });
            self.proven.set(direct.then_some(c.code.len()));
            c.declare(&def.body)?;
        }
        *self.params.borrow_mut() = params;
        self.block(&def.body).await
    }

    async fn block_task(&self, body: &'x [Stmt]) -> Result<()> {
        {
            let mut c = self.c();
            c.work.charge(1)?;
            if body.is_empty() {
                c.emit(Op::Nil);
            }
        }
        for (i, stmt) in body.iter().enumerate() {
            if i > 0 {
                self.c().emit(Op::Pop);
            }
            self.stmt(stmt, false).await?;
        }
        Ok(())
    }

    async fn stmt(&self, stmt: &'x Stmt, expression: bool) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            std::mem::replace(&mut c.offset, stmt.offset)
        };
        let result = self.statement_at(stmt, expression).await;
        self.c().offset = previous;
        result
    }

    async fn statement_at(&self, stmt: &'x Stmt, expression: bool) -> Result<()> {
        {
            let mut c = self.c();
            let work = c.work;
            work.charge(1)?;
            if let Statement::Assign(target, _, _) = &stmt.node {
                let mut names = Buffer::new();
                target_names(target, &mut names, work)?;
                for name in names {
                    if c.program.declaration_names.contains_key(name.as_str()) {
                        continue;
                    }
                    if let Some(&slot) = c.locals.get(work, name)? {
                        c.emit(Op::Declare(narrow(slot)));
                    }
                }
            }
        }
        self.statement(stmt, expression).await?;
        if !expression
            && matches!(
                stmt.node,
                Statement::If(..) | Statement::While(..) | Statement::For(..)
            )
        {
            let mut c = self.c();
            for slot in c.statement_bindings(std::slice::from_ref(stmt))? {
                c.emit(Op::Declare(narrow(slot)));
            }
        }
        Ok(())
    }

    async fn assignment_rhs(&self, target: &'x Target, values: &[&'x Expr]) -> Result<()> {
        let slots = {
            let mut c = self.c();
            let work = c.work;
            work.charge(1)?;
            let mut names = Buffer::new();
            target_names(target, &mut names, work)?;
            let mut calls = Table::new();
            for value in values {
                call_names(value, &mut calls, work)?;
            }
            let mut seen = Table::new();
            let mut slots = Buffer::new();
            for name in names {
                if let Some(&slot) = c.locals.get(work, name)? {
                    if calls.contains(work, name)? && seen.insert(work, name.clone(), ())?.is_none()
                    {
                        slots.push(work, slot)?;
                    }
                }
            }
            drop(calls);
            drop(seen);
            for &slot in &slots {
                c.emit(Op::Bypass(narrow(slot)));
            }
            slots.len()
        };
        for value in values {
            self.expr(value).await?;
        }
        if slots != 0 {
            self.c().emit(Op::BypassEnd(narrow(slots)));
        }
        Ok(())
    }

    async fn loop_body(&self, body: &'x [Stmt]) -> Result<()> {
        {
            let mut c = self.c();
            c.work.charge(1)?;
            let bindings = c.statement_bindings(body)?;
            let work = c.work;
            c.loop_bindings.push(work, bindings)?;
        }
        self.block(body).await?;
        self.c().loop_bindings.pop();
        Ok(())
    }

    async fn statement(&self, stmt: &'x Stmt, expression: bool) -> Result<()> {
        self.c().work.charge(1)?;
        match &stmt.node {
            Statement::Raise(value, message) => {
                Box::pin(self.raise(value.as_deref(), message.as_deref())).await?
            }
            Statement::Retry => {
                self.c().emit(Op::Retry);
            }
            Statement::UnboundClass(name) => {
                let mut c = self.c();
                let name = c.call_site(name, false).name;
                c.emit(Op::UnboundClass(name));
            }
            Statement::Unsupported => {
                self.c().emit(Op::Unsupported);
            }
            Statement::Module(name) => {
                let mut c = self.c();
                c.initialize_namespaces(name)?;
                c.emit(Op::Nil);
            }
            Statement::Expr(e) => self.expr(e).await?,
            Statement::Assign(target, op, rhs) => self.assignment(target, op, rhs).await?,
            Statement::If(branches, alternate, modifier) => {
                // Like Go, a local assigned earlier in the source exists, as nil,
                // wherever control skips its assignment: a modifier's body precedes
                // its condition, and a skipped branch precedes the later ones.
                if modifier.is_some() {
                    let mut c = self.c();
                    for (_, body) in branches {
                        c.declare_bindings(body)?;
                    }
                    c.declare_bindings(alternate)?;
                }
                let mut done = Buffer::new();
                for (cond, body) in branches {
                    self.expr(cond).await?;
                    let branch = self.c().emit(Op::JumpFalse(0));
                    self.block(body).await?;
                    let mut c = self.c();
                    let work = c.work;
                    done.push(work, c.emit(Op::Jump(0)))?;
                    let end = c.code.len();
                    c.patch(branch, end);
                    c.declare_bindings(body)?;
                }
                self.block(alternate).await?;
                let mut c = self.c();
                let end = c.code.len();
                for done in done {
                    c.patch(done, end);
                }
            }
            Statement::While(cond, body, modifier) => {
                let (mark, next) = {
                    let mut c = self.c();
                    if modifier.is_some() {
                        c.declare_bindings(body)?;
                    }
                    let mark = c.emit(Op::LoopStart {
                        iterable: false,
                        expression,
                        next: 0,
                        end: 0,
                    });
                    (mark, c.code.len())
                };
                self.expr(cond).await?;
                self.c().emit(Op::LoopTest);
                self.loop_body(body).await?;
                let mut c = self.c();
                c.emit(Op::LoopBody);
                let end = c.emit(Op::LoopEnd);
                c.code[mark] = Op::LoopStart {
                    iterable: false,
                    expression,
                    next: narrow(next),
                    end: narrow(end),
                };
            }
            Statement::For(target, iterable, body) => {
                self.expr(iterable).await?;
                let mark = {
                    let mut c = self.c();
                    let work = c.work;
                    let mut names = Buffer::new();
                    target_names(target, &mut names, work)?;
                    for name in names {
                        if let Some(&slot) = c.locals.get(work, name)? {
                            c.emit(Op::Declare(narrow(slot)));
                        }
                    }
                    c.emit(Op::LoopStart {
                        iterable: true,
                        expression,
                        next: 0,
                        end: 0,
                    })
                };
                let next = {
                    let mut c = self.c();
                    let next = c.emit(Op::IterNext);
                    let plain = c.facts.plain(iterable);
                    c.mark_plain(next, plain);
                    next
                };
                self.assign_value(target).await?;
                self.c().emit(Op::Pop);
                self.loop_body(body).await?;
                let mut c = self.c();
                c.emit(Op::LoopBody);
                let end = c.emit(Op::LoopEnd);
                c.code[mark] = Op::LoopStart {
                    iterable: true,
                    expression,
                    next: narrow(next),
                    end: narrow(end),
                };
            }
            Statement::Return(value) => {
                if let Some(e) = value {
                    self.expr(e).await?;
                } else {
                    self.c().emit(Op::Nil);
                }
                let mut c = self.c();
                let ip = c.emit(Op::Return);
                let plain = value.as_ref().is_none_or(|e| c.facts.plain(e));
                c.mark_plain(ip, plain);
            }
            Statement::Break(value) => {
                if let Some(value) = value {
                    {
                        let mut c = self.c();
                        if c.loop_bindings.is_empty() && c.outer.is_empty() {
                            c.emit(Op::LoopGuard(true));
                        }
                    }
                    self.expr(value).await?;
                }
                self.c().emit(Op::Break(value.is_some()));
            }
            Statement::Next(value) => {
                if let Some(value) = value {
                    {
                        let mut c = self.c();
                        if c.loop_bindings.is_empty() && c.outer.is_empty() {
                            c.emit(Op::LoopGuard(false));
                        }
                    }
                    self.expr(value).await?;
                }
                let mut c = self.c();
                let c = &mut *c;
                if let Some(bindings) = c.loop_bindings.last() {
                    for &slot in bindings {
                        c.work.charge(1)?;
                        c.code.push(Op::Declare(narrow(slot)));
                        c.locations.push(c.offset);
                    }
                }
                c.emit(Op::Next(value.is_some()));
            }
        }
        Ok(())
    }

    async fn assignment(&self, target: &'x Target, op: &'static str, rhs: &'x Expr) -> Result<()> {
        let binding_target = target;
        let binary = match op {
            "+=" => Some("+"),
            "-=" => Some("-"),
            "*=" => Some("*"),
            "/=" => Some("/"),
            "//=" => Some("//"),
            "%=" => Some("%"),
            "**=" => Some("**"),
            _ => None,
        };
        if let Some((name, ty)) = syntax::typed::declared_local(target) {
            return self.typed_local(target, name, ty, rhs).await;
        }
        let Target::Value(target) = target else {
            self.assignment_rhs(target, &[rhs]).await?;
            return self.assign_value(target).await;
        };
        match &target.node {
            Node::Var(name) => {
                if self.c().namespace_binding(name)? {
                    return self
                        .namespace_assignment(name, binding_target, target, op, rhs)
                        .await;
                }
                let global = self.c().global_binding(name)?;
                if let Some(global) = global {
                    if matches!(op, "||=" | "&&=") {
                        let skip = {
                            let mut c = self.c();
                            c.emit(Op::Global(narrow(global)));
                            c.emit(Op::Dup);
                            let skip = c.emit(if op == "||=" {
                                Op::JumpTrue(0)
                            } else {
                                Op::JumpFalse(0)
                            });
                            c.emit(Op::Pop);
                            skip
                        };
                        self.assignment_rhs(binding_target, &[rhs]).await?;
                        let mut c = self.c();
                        c.emit(Op::StoreGlobal(narrow(global)));
                        let end = c.code.len();
                        c.patch(skip, end);
                        return Ok(());
                    }
                    if binary.is_some() {
                        self.c().emit(Op::Global(narrow(global)));
                    }
                    self.assignment_rhs(binding_target, &[rhs]).await?;
                    let mut c = self.c();
                    if let Some(op) = binary {
                        c.binary(op)?;
                    }
                    c.emit(Op::StoreGlobal(narrow(global)));
                    return Ok(());
                }
                let slot = self.c().slot(name)?;
                if matches!(op, "||=" | "&&=") {
                    self.expr(target).await?;
                    let skip = {
                        let mut c = self.c();
                        c.emit(Op::Dup);
                        let skip = c.emit(if op == "||=" {
                            Op::JumpTrue(0)
                        } else {
                            Op::JumpFalse(0)
                        });
                        c.emit(Op::Pop);
                        skip
                    };
                    self.assignment_rhs(binding_target, &[rhs]).await?;
                    let mut c = self.c();
                    c.check_local(name)?;
                    c.emit(Op::Store(narrow(slot)));
                    let end = c.code.len();
                    c.patch(skip, end);
                    return Ok(());
                }
                let typed = self.c().local_type(name)?.is_some();
                // A typed local checks each value before storing it, so it
                // cannot take the fused add-and-store.
                let fused = !self.c().program.file && !typed;
                if binary.is_none() && fused {
                    if let Node::Binary("+", left, right) = &rhs.node {
                        self.assignment_rhs(binding_target, &[left, right]).await?;
                        let mut c = self.c();
                        let instruction = c.emit(Op::AddStore(narrow(slot)));
                        c.locations[instruction] = rhs.offset;
                        return Ok(());
                    }
                }
                if binary.is_some() {
                    self.expr(target).await?;
                }
                self.assignment_rhs(binding_target, &[rhs]).await?;
                let mut c = self.c();
                if binary == Some("+") && fused {
                    c.emit(Op::AddStore(narrow(slot)));
                    return Ok(());
                }
                if let Some(op) = binary {
                    c.binary(op)?;
                }
                if typed {
                    c.check_local(name)?;
                }
                c.emit(Op::Store(narrow(slot)));
            }
            // Go reports a failed write, and a compound operator's failure, at the target.
            Node::Index(..) | Node::Member(..) => {
                if binary.is_none() && !matches!(op, "||=" | "&&=") {
                    self.assignment_rhs(binding_target, &[rhs]).await?;
                    self.address_target(target, false).await?;
                    let mut c = self.c();
                    let store = c.emit(Op::AddressStore);
                    c.locations[store] = target.offset;
                } else {
                    self.address_target(target, true).await?;
                    if matches!(op, "||=" | "&&=") {
                        let skip = {
                            let mut c = self.c();
                            c.emit(Op::Dup);
                            let skip = c.emit(if op == "||=" {
                                Op::JumpTrue(0)
                            } else {
                                Op::JumpFalse(0)
                            });
                            c.emit(Op::Pop);
                            skip
                        };
                        self.assignment_rhs(binding_target, &[rhs]).await?;
                        let mut c = self.c();
                        let store = c.emit(Op::AddressStore);
                        c.locations[store] = target.offset;
                        let end = c.emit(Op::Jump(0));
                        let drop = c.code.len();
                        c.patch(skip, drop);
                        c.emit(Op::AddressDrop);
                        let done = c.code.len();
                        c.patch(end, done);
                    } else {
                        self.assignment_rhs(binding_target, &[rhs]).await?;
                        let mut c = self.c();
                        let operator = c.binary(binary.unwrap())?;
                        let store = c.emit(Op::AddressStore);
                        c.locations[operator] = target.offset;
                        c.locations[store] = target.offset;
                    }
                }
            }
            _ => {
                let work = self.c().work;
                return Err(syntax::unsupported(work, "invalid assignment target"));
            }
        }
        Ok(())
    }

    /// Declares a typed local, `name: T = value`: the value and every later
    /// assignment to the local are checked against `T`.
    async fn typed_local(
        &self,
        target: &'x Target,
        name: &'x crate::compilation::Name,
        ty: &'x crate::compilation::Type,
        rhs: &'x Expr,
    ) -> Result<()> {
        self.assignment_rhs(target, &[rhs]).await?;
        let check = {
            let mut c = self.c();
            let ty = c.annotation(ty)?;
            let kind = if c.namespace_binding(name)? {
                "constant "
            } else {
                "local variable "
            };
            let subject = c.subject(&[kind, name])?;
            c.check(ty, subject);
            (ty, subject)
        };
        let Target::Typed(inner, _) = target else {
            unreachable!()
        };
        self.assign_value(inner).await?;
        let mut c = self.c();
        let work = c.work;
        // A typed constant keeps its type in the body that declares it.
        let constant =
            c.namespace_binding(name)? && name.chars().next().is_some_and(syntax::unicode::upper);
        if (constant || !c.namespace_binding(name)?) && c.global_binding(name)?.is_none() {
            c.typed.insert(work, name.clone(), check)?;
        }
        Ok(())
    }

    async fn assign_value(&self, target: &'x Target) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            let offset = target.offset().unwrap_or(c.offset);
            std::mem::replace(&mut c.offset, offset)
        };
        let result = self.assign_value_at(target).await;
        self.c().offset = previous;
        result
    }

    async fn assign_value_at(&self, target: &'x Target) -> Result<()> {
        self.c().work.charge(1)?;
        match target {
            Target::Typed(target, ty) => {
                {
                    let mut c = self.c();
                    let work = c.work;
                    let ty = c.annotation(ty)?;
                    let mut text = Buffer::new();
                    target_label(target, &mut text, work)?;
                    if text.is_empty() {
                        text.extend_from_slice(work, b"destructured value")?;
                    }
                    if c.program.types[ty].unproven() {
                        let label = c.program.constants.len();
                        c.program.constants.push(Value::bytes(&*text));
                        c.emit(Op::Normalize(narrow(ty), narrow(label)));
                    }
                    drop(text);
                }
                self.nested_assign(target).await?;
            }
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                let mut c = self.c();
                if c.namespace_binding(name)? {
                    c.check_local(name)?;
                    c.store_namespace_name(name);
                } else if let Some(global) = c.global_binding(name)? {
                    c.emit(Op::StoreGlobal(narrow(global)));
                } else {
                    let slot = c.slot(name)?;
                    c.check_local(name)?;
                    c.emit(Op::Store(narrow(slot)));
                }
            }
            Target::Value(
                target @ Expr {
                    node: Node::Index(..) | Node::Member(..),
                    ..
                },
            ) => {
                self.address_target(target, false).await?;
                self.c().emit(Op::AddressStore);
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
                    {
                        let mut c = self.c();
                        let index = narrow(c.program.selections.len());
                        c.program.selections.push(select);
                        c.emit(Op::Extract(index));
                    }
                    self.nested_assign(part).await?;
                    self.c().emit(Op::Pop);
                }
            }
            _ => {
                let work = self.c().work;
                return Err(syntax::unsupported(work, "invalid assignment target"));
            }
        }
        Ok(())
    }

    async fn expr_task(&self, e: &'x Expr) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            std::mem::replace(&mut c.offset, e.offset)
        };
        let result = self.expression(e).await;
        self.c().offset = previous;
        result
    }

    async fn expression(&self, e: &'x Expr) -> Result<()> {
        if leaf(e) {
            return self.c().leaf(e, Receiving::VALUE);
        }
        self.c().work.charge(1)?;
        match &e.node {
            Node::Try(attempt) => Box::pin(self.attempt(attempt, None)).await?,
            Node::Shape(ty, fallback, names) => {
                return Box::pin(self.shape_expression(ty, fallback.as_deref(), names)).await;
            }
            Node::Unary("-", value) if matches!(value.node, Node::Integer(n) if n == i64::MAX as u64 + 1) =>
            {
                self.c().constant(Value::int(i64::MIN));
            }
            Node::Array(values) => {
                for v in values {
                    self.expr(v).await?;
                }
                self.c().emit(Op::Array(narrow(values.len())));
            }
            Node::Template(parts, symbol) => {
                self.c().emit(Op::TextStart);
                for part in parts {
                    self.expr(part).await?;
                    let mut c = self.c();
                    let instruction = c.emit(Op::TextPart);
                    c.locations[instruction] = part.offset;
                }
                self.c().emit(Op::TextEnd(*symbol));
            }
            Node::Hash(values) => {
                for (k, v) in values {
                    self.c().literal(k.compiler_constant());
                    self.expr(v).await?;
                }
                self.c().emit(Op::Hash(narrow(values.len())));
            }
            Node::Unary(op, v) => {
                self.expr(v).await?;
                self.c().unary(op)?;
            }
            Node::Range(start, end, exclusive) => {
                if let Some(start) = start {
                    self.expr(start).await?;
                    if end.is_some() {
                        // The start converts before the end runs, as in Go.
                        self.c().emit(Op::RangeStart);
                    }
                }
                if let Some(end) = end {
                    self.expr(end).await?;
                }
                self.c()
                    .emit(Op::Range(start.is_some(), end.is_some(), *exclusive));
            }
            Node::Conditional(branches, alternate) => {
                let mut done = Buffer::new();
                for (cond, result) in branches {
                    self.expr(cond).await?;
                    let branch = self.c().emit(Op::JumpFalse(0));
                    self.expr(result).await?;
                    let mut c = self.c();
                    let work = c.work;
                    done.push(work, c.emit(Op::Jump(0)))?;
                    let end = c.code.len();
                    c.patch(branch, end);
                }
                self.expr(alternate).await?;
                let mut c = self.c();
                let end = c.code.len();
                for done in done {
                    c.patch(done, end);
                }
            }
            Node::Compound(stmt) => Box::pin(self.stmt(stmt, true)).await?,
            Node::Case(target, clauses, alternate) => {
                return Box::pin(self.case_expression(
                    target.as_deref(),
                    clauses,
                    alternate.as_deref(),
                ))
                .await;
            }
            Node::Binary("<<", a, b) => {
                Box::pin(self.address(a)).await?;
                self.expr(b).await?;
                let mut c = self.c();
                let site = c.call_site("push", false);
                c.emit(Op::Shovel(site, false));
            }
            Node::Binary(op, a, b) => {
                self.expr(a).await?;
                if matches!(*op, "&&" | "||") {
                    let jump = {
                        let mut c = self.c();
                        c.emit(Op::Dup);
                        let jump = c.emit(if *op == "&&" {
                            Op::JumpFalse(0)
                        } else {
                            Op::JumpTrue(0)
                        });
                        c.emit(Op::Pop);
                        jump
                    };
                    self.expr(b).await?;
                    let mut c = self.c();
                    let end = c.code.len();
                    c.patch(jump, end);
                } else {
                    self.expr(b).await?;
                    self.c().binary(op)?;
                }
            }
            Node::Call(name, args, _) if name == "block_given?" => {
                self.c().emit(Op::BlockGiven(!args.is_empty(), false));
            }
            Node::Yield(args) => {
                self.c().emit(Op::CheckBlock);
                for (index, arg) in args.iter().enumerate() {
                    self.expr(arg).await?;
                    let mut c = self.c();
                    let check = c.block.as_ref().and_then(|block| block.params.get(index));
                    if let Some(&(ty, subject)) = check {
                        c.check(ty, subject);
                    }
                }
                let mut c = self.c();
                c.emit(Op::Yield(narrow(args.len())));
                match c.block.as_ref().map(|block| block.result) {
                    Some(Some((ty, subject))) => {
                        c.check(ty, subject);
                    }
                    Some(None) => {
                        c.emit(Op::Pop);
                        c.emit(Op::Nil);
                    }
                    None => (),
                }
            }
            Node::BlockCall(call, block) => Box::pin(self.block_call(e, call, block)).await?,
            Node::ComputedCall(call, args) => {
                Box::pin(self.computed_call(call, args, None)).await?
            }
            Node::Call(name, args, _) => self.named_call(e, name, args).await?,
            Node::Member(recv, name) | Node::SafeMember(recv, name) => {
                let direct = self.c().facts.base(e);
                (self.member_call(
                    recv,
                    name,
                    &[],
                    CallForm::Auto,
                    None,
                    matches!(e.node, Node::SafeMember(..)),
                    direct,
                    e,
                ))
                .await?;
            }
            Node::Scope(recv, name, args) => {
                Box::pin(self.scoped_call(recv, name, args.as_deref(), None)).await?
            }
            Node::Method(recv, name, args, form) | Node::SafeMethod(recv, name, args, form) => {
                let direct = self.c().facts.base(e);
                (self.member_call(
                    recv,
                    name,
                    args,
                    *form,
                    None,
                    matches!(e.node, Node::SafeMethod(..)),
                    direct,
                    e,
                ))
                .await?;
            }
            Node::Index(value, index) => {
                self.expr(value).await?;
                if let [
                    Expr {
                        node: Node::Literal(key),
                        ..
                    },
                ] = index.as_slice()
                    && key.as_bytes().is_some()
                {
                    let mut c = self.c();
                    c.literal(key.clone());
                    let ip = c.code.len() - 1;
                    let Op::Shared(slot) = c.code[ip] else {
                        unreachable!()
                    };
                    c.code[ip] = Op::IndexLiteral(slot);
                    let plain = c.facts.plain(e);
                    c.mark_plain(ip, plain);
                    return Ok(());
                }
                for index in index {
                    self.expr(index).await?;
                }
                let mut c = self.c();
                let ip = c.emit(Op::Index(narrow(index.len())));
                let plain = c.facts.plain(e);
                c.mark_plain(ip, plain);
            }
            Node::Regex(..)
            | Node::Integer(_)
            | Node::BigInteger(..)
            | Node::Literal(_)
            | Node::Var(_) => unreachable!(),
        }
        Ok(())
    }

    async fn shape_expression(
        &self,
        ty: &'x crate::compilation::Type,
        fallback: Option<&'x Expr>,
        names: &'x [crate::compilation::Name],
    ) -> Result<()> {
        let guard = {
            let mut c = self.c();
            c.work.names(names)?;
            let guard = fallback.map(|_| {
                let index = c.program.type_guards.len();
                c.program
                    .type_guards
                    .push(names.iter().map(|name| name.as_str().to_owned()).collect());
                c.emit(Op::TypeShadowed(narrow(index), 0))
            });
            let scope = aliases::scope(&c.program.namespaces, c.namespace);
            let shape = crate::shapes::compile(c.aliases.compile(scope, ty, c.work)?);
            c.constant(shape);
            guard
        };
        if let Some(fallback) = fallback {
            let done = {
                let mut c = self.c();
                let done = c.emit(Op::Jump(0));
                let end = c.code.len();
                c.patch(guard.unwrap(), end);
                done
            };
            self.expr(fallback).await?;
            let mut c = self.c();
            let end = c.code.len();
            c.patch(done, end);
        }
        Ok(())
    }

    async fn case_expression(
        &self,
        target: Option<&'x Expr>,
        clauses: &'x [syntax::When],
        alternate: Option<&'x Expr>,
    ) -> Result<()> {
        let work = self.c().work;
        if let Some(target) = target {
            self.expr(target).await?;
        }
        let mut completed = Buffer::new();
        for clause in clauses {
            let mut matches = Buffer::new();
            for (value, splat) in &clause.values {
                if target.is_some() {
                    self.c().emit(Op::Dup);
                }
                self.expr(value).await?;
                let mut c = self.c();
                c.emit(Op::CaseCompare(target.is_some(), *splat));
                matches.push(work, c.emit(Op::JumpTrue(0)))?;
            }
            let next = {
                let mut c = self.c();
                let next = c.emit(Op::Jump(0));
                for matched in matches {
                    let end = c.code.len();
                    c.patch(matched, end);
                }
                if target.is_some() {
                    c.emit(Op::Pop);
                }
                next
            };
            self.expr(&clause.result).await?;
            let mut c = self.c();
            completed.push(work, c.emit(Op::Jump(0)))?;
            let end = c.code.len();
            c.patch(next, end);
        }
        if target.is_some() {
            self.c().emit(Op::Pop);
        }
        if let Some(alternate) = alternate {
            self.expr(alternate).await?;
        } else {
            self.c().emit(Op::Nil);
        }
        let mut c = self.c();
        for completed in completed {
            let end = c.code.len();
            c.patch(completed, end);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Calls member `name` of `receiver`, as the expression `whole`. A call
    /// whose receiver the checker proved to have the one base type `direct`
    /// becomes a direct builtin call when that base serves the member.
    #[allow(clippy::too_many_arguments)]
    async fn member_call(
        &self,
        receiver: &'x Expr,
        name: &str,
        args: &'x [Argument],
        form: CallForm,
        block: Option<usize>,
        safe: bool,
        direct: Option<crate::members::direct::Base>,
        whole: &'x Expr,
    ) -> Result<()> {
        self.c().work.charge(1)?;
        let mutating = mutating_member(name);
        let receiving = self.receiving(receiver, name, form, args.len());
        if mutating {
            self.address_receiver(receiver, receiving).await?;
        } else {
            self.member_receiver(receiver, receiving).await?;
        }
        let (skip, site, direct) = {
            let mut c = self.c();
            let skip = safe.then(|| {
                c.emit(if mutating {
                    Op::AddressJumpNil(0, true)
                } else {
                    Op::JumpNil(0)
                })
            });
            let site = c.call_site(name, form == CallForm::Auto);
            let direct = direct.filter(|&base| {
                !mutating
                    && name != "call"
                    && block.is_none()
                    && !expanded(args)
                    && site.method.is_some_and(|method| {
                        crate::members::direct::serves(base, method, args.len())
                    })
            });
            // Preparing a member reads fields only of a hash receiver.
            let prepared = direct.is_none_or(|base| base == crate::members::direct::Base::Hash);
            if name != "call" && (mutating || form != CallForm::Auto) && prepared {
                c.emit(Op::PrepareMember(site, mutating));
            }
            (skip, site, direct)
        };
        let (plain, plain_args) = {
            let c = self.c();
            let plain_args = args.iter().all(|arg| c.facts.plain(&arg.value));
            (c.facts.plain(whole), plain_args)
        };
        if direct.is_some() {
            for arg in args {
                self.expr(&arg.value).await?;
            }
            let mut c = self.c();
            let ip = c.emit(Op::Direct(site, narrow(args.len())));
            c.mark_plain(ip, plain);
            if plain_args {
                c.plain_inputs.insert(ip);
            }
            if let Some(skip) = skip {
                let end = c.code.len();
                c.patch(skip, end);
            }
            return Ok(());
        }
        if name == "call" && form != CallForm::Auto {
            {
                let mut c = self.c();
                c.emit(Op::Arguments);
                c.emit(Op::CallMember(site));
            }
            self.argument_values(args).await?;
            let mut c = self.c();
            if let Some(block) = block {
                c.emit(Op::Attach(narrow(block)));
            }
            c.emit(Op::Invoke(Invocation::Resolved));
            if let Some(skip) = skip {
                let end = c.code.len();
                c.patch(skip, end);
            }
            return Ok(());
        }
        if expanded(args)
            || block.is_some()
            || crate::iteration::method(name)
            || name == "is_type?"
            || name == "as"
        {
            self.call_arguments(args).await?;
            let mut c = self.c();
            if let Some(block) = block {
                c.emit(Op::Attach(narrow(block)));
            }
            let ip = c.emit(Op::Invoke(Invocation::Member(site, mutating)));
            c.mark_plain(ip, plain);
            if plain_args && c.facts.plain(receiver) {
                c.plain_inputs.insert(ip);
            }
        } else {
            for arg in args {
                self.expr(&arg.value).await?;
            }
            let mut c = self.c();
            if !mutating {
                if let Some((function, class)) = c.method_of(whole, name) {
                    c.emit(Op::MethodOf(
                        narrow(function),
                        narrow(class),
                        narrow(args.len()),
                    ));
                }
            }
            let ip = c.emit(if mutating {
                Op::Mutate(site, narrow(args.len()))
            } else {
                Op::Method(site, narrow(args.len()))
            });
            c.mark_plain(ip, plain);
        }
        if let Some(skip) = skip {
            let mut c = self.c();
            let end = c.code.len();
            c.patch(skip, end);
        }
        Ok(())
    }

    /// Selects how a bare name receiving `member`, called in `form` with
    /// `arguments` argument values, treats a callable it resolves to.
    pub(super) fn receiving(
        &self,
        receiver: &Expr,
        member: &str,
        form: CallForm,
        arguments: usize,
    ) -> Receiving {
        match &receiver.node {
            Node::Var(name)
                if !name.starts_with('@') && !matches!(name.as_str(), "self" | "block_given?") =>
            {
                let index = self.c().call_site(member, false).name;
                Receiving::of(member, index as usize, form, arguments)
            }
            _ => Receiving::VALUE,
        }
    }

    pub(super) async fn member_receiver(
        &self,
        receiver: &'x Expr,
        receiving: Receiving,
    ) -> Result<()> {
        self.c().work.charge(1)?;
        if let Node::Var(name) = &receiver.node {
            let slot = {
                let c = self.c();
                c.locals.get(c.work, name.as_str())?.copied()
            };
            if let Some(slot) = slot {
                let bound = self.c().emit(Op::ReceiverBound(narrow(slot), 0));
                self.receiver_expr(receiver, receiving).await?;
                let mut c = self.c();
                let end = c.code.len();
                c.patch(bound, end);
                return Ok(());
            }
            let mut c = self.c();
            if let Some(global) = c.global_fallback(name)? {
                c.emit(Op::GlobalReceiver(narrow(global), receiving));
                return Ok(());
            }
        }
        self.receiver_expr(receiver, receiving).await
    }

    async fn scoped_call(
        &self,
        receiver: &'x Expr,
        name: &str,
        args: Option<&'x [Argument]>,
        block: Option<usize>,
    ) -> Result<()> {
        self.c().work.charge(1)?;
        self.expr(receiver).await?;
        let site = {
            let mut c = self.c();
            let mut site = c.call_site(name, args.is_none() && block.is_none());
            site.scope = true;
            site
        };
        let args = args.unwrap_or(&[]);
        if expanded(args) || block.is_some() {
            self.call_arguments(args).await?;
            let mut c = self.c();
            if let Some(block) = block {
                c.emit(Op::Attach(narrow(block)));
            }
            c.emit(Op::Invoke(Invocation::Member(site, false)));
        } else {
            for arg in args {
                self.expr(&arg.value).await?;
            }
            self.c().emit(Op::Method(site, narrow(args.len())));
        }
        Ok(())
    }

    /// Calls `call` with `block`, as the expression `whole`.
    async fn block_call(&self, whole: &'x Expr, call: &'x Expr, block: &'x Block) -> Result<()> {
        self.c().work.charge(1)?;
        let function = self.compile_block(block).await?;
        let (name, args) = match &call.node {
            Node::Var(name) => (name.as_str(), &[][..]),
            Node::Call(name, args, _) => (name.as_str(), args.as_slice()),
            Node::Member(receiver, name) | Node::SafeMember(receiver, name) => {
                return self
                    .member_call(
                        receiver,
                        name,
                        &[],
                        CallForm::Bare,
                        Some(function),
                        matches!(call.node, Node::SafeMember(..)),
                        None,
                        whole,
                    )
                    .await;
            }
            Node::Method(receiver, name, args, form)
            | Node::SafeMethod(receiver, name, args, form) => {
                return self
                    .member_call(
                        receiver,
                        name,
                        args,
                        *form,
                        Some(function),
                        matches!(call.node, Node::SafeMethod(..)),
                        None,
                        whole,
                    )
                    .await;
            }
            Node::Scope(receiver, name, args) => {
                return self
                    .scoped_call(receiver, name, args.as_deref(), Some(function))
                    .await;
            }
            Node::ComputedCall(call, args) => {
                return self.computed_call(call, args, Some(function)).await;
            }
            _ => {
                return self.computed_call(call, &[], Some(function)).await;
            }
        };
        if name == "block_given?" {
            self.c().emit(Op::BlockGiven(!args.is_empty(), true));
            return Ok(());
        }
        let resolved = {
            let mut c = self.c();
            if c.program.file || c.namespace.is_some() {
                c.global(name);
                let slot = c.locals.get(c.work, name)?.copied().map_or(NO_SLOT, narrow);
                let name = c.call_site(name, false).name;
                c.emit(Op::ResolveCall(slot, name));
                true
            } else {
                false
            }
        };
        if resolved {
            self.argument_values(args).await?;
            let mut c = self.c();
            c.emit(Op::Attach(narrow(function)));
            let ip = c.emit(Op::Invoke(Invocation::Resolved));
            let plain = c.facts.plain(whole);
            c.mark_plain(ip, plain);
            return Ok(());
        }
        let target = {
            let mut c = self.c();
            if let Some(&slot) = c.locals.get(c.work, name)? {
                let name = c.call_site(name, false).name;
                c.emit(Op::ResolveCall(narrow(slot), name));
                Some(Invocation::Resolved)
            } else if let Some(global) = c.global_binding(name)? {
                c.emit(Op::ResolveGlobalCall(narrow(global)));
                Some(Invocation::Resolved)
            } else {
                let target = if c.program.declaration_names.contains_key(name) {
                    Some(Invocation::NonCallable)
                } else if let Some(&function) = c.program.names.get(name) {
                    Some(Invocation::Function(narrow(function)))
                } else if let Some(host) = c.host_position(name)? {
                    Some(Invocation::Host(narrow(host)))
                } else {
                    let site = c.call_site(name, false);
                    c.emit(Op::ResolveCall(NO_SLOT, site.name));
                    None
                };
                // A script function called with plain arguments takes them
                // from the stack, without an argument list.
                let listed = expanded(args) || !matches!(target, Some(Invocation::Function(_)));
                if target.is_some() {
                    let name = c.call_site(name, false).name;
                    c.emit(Op::RootCall(name, listed));
                }
                target
            }
        };
        if let (Some(Invocation::Function(callee)), false) = (target, expanded(args)) {
            for arg in args {
                self.expr(&arg.value).await?;
            }
            self.c()
                .emit(Op::CallBlock(callee, narrow(args.len()), narrow(function)));
            return Ok(());
        }
        self.argument_values(args).await?;
        let mut c = self.c();
        c.emit(Op::Attach(narrow(function)));
        let ip = match target {
            Some(Invocation::Resolved) | None => c.emit(Op::Invoke(Invocation::Resolved)),
            Some(target) => c.emit(Op::InvokeRoot(target)),
        };
        let plain = c.facts.plain(whole);
        c.mark_plain(ip, plain);
        Ok(())
    }

    async fn compile_block(&self, block: &'x Block) -> Result<usize> {
        let parent = self.c().begin_block()?;
        let result = self.block_function(block).await;
        let mut c = self.c();
        match result {
            Ok(block_arity) => c.finish_block(parent, block_arity),
            Err(error) => {
                c.swap_scope(parent);
                Err(error)
            }
        }
    }

    // Generate a block's body into the scope `begin_block` set up.
    async fn block_function(&self, block: &'x Block) -> Result<usize> {
        let mut block_arity = block.params.len();
        {
            let mut c = self.c();
            let work = c.work;
            c.declare(&block.body)?;
            for target in &block.params {
                c.declare_target(target)?;
                let mut names = Buffer::new();
                target_names(target, &mut names, work)?;
                for name in names {
                    let slot = c.slot(name)?;
                    c.parameters.insert(work, name.clone(), ())?;
                    c.emit(Op::Shadow(narrow(slot)));
                }
            }
        }
        let plain_params = self.c().facts.plain_block(block);
        for (index, target) in block.params.iter().enumerate() {
            {
                let mut c = self.c();
                let ip = c.emit(Op::BlockArg(narrow(index), block.params.len() > 1));
                c.mark_plain(ip, plain_params);
            }
            self.assign_value(target).await?;
            self.c().emit(Op::Pop);
        }
        if block.implicit {
            let mut c = self.c();
            let work = c.work;
            let candidates = ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| (name, index))
                .chain(block.infer_it.then_some(("it", 0)));
            for (name, index) in candidates {
                if c.reads.contains(work, name)? && !c.assigned.contains(work, name)? {
                    block_arity = block_arity.max(index + 1);
                    let name = Name::new(work, name)?;
                    let slot = c.slot(&name)?;
                    c.parameters.insert(work, name, ())?;
                    c.emit(Op::Shadow(narrow(slot)));
                    let ip = c.emit(Op::BlockArg(narrow(index), false));
                    c.mark_plain(ip, plain_params);
                    c.emit(Op::Store(narrow(slot)));
                    c.emit(Op::Pop);
                }
            }
        }
        self.block(&block.body).await?;
        let mut c = self.c();
        let finish = c.emit(Op::Finish);
        let plain = c.plain_result(&block.body);
        c.mark_plain(finish, plain);
        let parent_offset = c.offset;
        c.locations[finish] = block.body.last().map_or(parent_offset, |stmt| stmt.offset);
        debug_assert_eq!(c.code.len(), c.locations.len());
        Ok(block_arity)
    }

    async fn call_arguments(&self, args: &'x [Argument]) -> Result<()> {
        {
            let mut c = self.c();
            c.work.charge(1)?;
            c.emit(Op::Arguments);
        }
        self.argument_values(args).await
    }

    async fn argument_values(&self, args: &'x [Argument]) -> Result<()> {
        self.c().work.charge(1)?;
        for arg in args {
            self.expr(&arg.value).await?;
            let mut c = self.c();
            let kind = match &arg.kind {
                ArgumentKind::Positional => ArgumentOp::Positional,
                ArgumentKind::Splat => ArgumentOp::Splat,
                ArgumentKind::Keyword(name) => ArgumentOp::Keyword(c.call_site(name, false).name),
                ArgumentKind::KeywordSplat => ArgumentOp::KeywordSplat,
            };
            let ip = c.emit(Op::Argument(kind));
            let plain = c.facts.plain(&arg.value);
            c.mark_plain(ip, plain);
        }
        Ok(())
    }

    async fn address_target(&self, target: &'x Expr, read: bool) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            std::mem::replace(&mut c.offset, target.offset)
        };
        let result = self.address_target_at(target, read).await;
        self.c().offset = previous;
        result
    }

    async fn address_target_at(&self, target: &'x Expr, read: bool) -> Result<()> {
        self.c().work.charge(1)?;
        match &target.node {
            Node::Index(receiver, indices) => {
                self.assignment_address(receiver).await?;
                for index in indices {
                    self.expr(index).await?;
                }
                self.c()
                    .emit(Op::AddressTarget(narrow(indices.len()), read));
            }
            Node::Member(receiver, name) => {
                self.assignment_address(receiver).await?;
                let mut c = self.c();
                let site = c.call_site(name, true);
                c.emit(Op::AddressMemberTarget(site, read));
            }
            _ => {
                let work = self.c().work;
                return Err(syntax::unsupported(work, "invalid assignment target"));
            }
        }
        Ok(())
    }

    async fn address(&self, receiver: &'x Expr) -> Result<()> {
        self.address_root(receiver, false).await
    }

    /// Addresses the receiver of a mutating member, where a bare name treats a
    /// callable it resolves to by `receiving`.
    async fn address_receiver(&self, receiver: &'x Expr, receiving: Receiving) -> Result<()> {
        self.address_with(receiver, false, receiving).await
    }

    /// Addresses `receiver`. An `assignment` root in an instance method writes an
    /// existing class constant in place, unless a bound local of the same name takes
    /// precedence. In any class context, an assignment through an unbound name that
    /// reads a field of the running instance or class writes that field, as in Go,
    /// while a mutating call through it still receives a copy.
    pub(super) async fn address_root(&self, receiver: &'x Expr, assignment: bool) -> Result<()> {
        self.address_with(receiver, assignment, Receiving::VALUE)
            .await
    }

    async fn address_with(
        &self,
        receiver: &'x Expr,
        assignment: bool,
        receiving: Receiving,
    ) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            std::mem::replace(&mut c.offset, receiver.offset)
        };
        let result = self.address_at(receiver, assignment, receiving).await;
        self.c().offset = previous;
        result
    }

    async fn address_at(
        &self,
        receiver: &'x Expr,
        assignment: bool,
        receiving: Receiving,
    ) -> Result<()> {
        let (constant, early, root, file) = {
            let mut c = self.c();
            let work = c.work;
            work.charge(1)?;
            let constant = match &receiver.node {
                Node::Var(name)
                    if assignment
                        && c.instance
                        && c.namespace.is_some()
                        && name.chars().next().is_some_and(syntax::unicode::upper)
                        && !c.parameters.contains(work, name.as_str())? =>
                {
                    Some(c.call_site(name, false).name)
                }
                _ => None,
            };
            let local = match &receiver.node {
                Node::Var(name) => c.local(name)?,
                _ => false,
            };
            let early = constant
                .filter(|_| !local)
                .map(|name| c.emit(Op::NamespaceConstantAddress(name, 0)));
            let root = if let Node::Var(name) = &receiver.node {
                if !name.starts_with('@') && !c.locals.contains(work, name.as_str())? {
                    let name = c.call_site(name, false).name;
                    Some(c.emit(Op::RootAddress(name, 0)))
                } else {
                    None
                }
            } else {
                None
            };
            let file = if c.program.file {
                if let Node::Var(name) = &receiver.node {
                    if !name.starts_with('@') && !c.parameters.contains(work, name.as_str())? {
                        let name = c.call_site(name, false).name;
                        Some(c.emit(Op::FileAddress(name, 0)))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            (constant, early, root, file)
        };
        match &receiver.node {
            Node::Var(name) if name.starts_with('@') => {
                let mut c = self.c();
                let name = c.call_site(name, false).name;
                c.emit(Op::NamespaceAddress(name, true));
            }
            Node::Var(name) if self.c().local(name)? => {
                let (bound, constant, ambient, global, implicit) = {
                    let mut c = self.c();
                    let work = c.work;
                    let slot = *c.locals.get(work, name)?.unwrap();
                    if c.parameters.contains(work, name.as_str())? {
                        c.emit(Op::AddressLocal(narrow(slot)));
                        (None, None, None, true, None)
                    } else {
                        let bound = c.emit(Op::AddressBound(narrow(slot), 0));
                        let constant =
                            constant.map(|name| c.emit(Op::NamespaceConstantAddress(name, 0)));
                        let ambient = c.namespace.map(|_| {
                            let name = c.call_site(name, false).name;
                            c.emit(Op::AmbientAddress(name, 0))
                        });
                        let global = if let Some(global) = c.global_fallback(name)? {
                            c.emit(Op::AddressGlobal(narrow(global)));
                            true
                        } else {
                            false
                        };
                        let implicit = if assignment && !global && c.implicit_name(name)? {
                            let name = c.call_site(name, false).name;
                            Some(c.emit(Op::ImplicitAddress(name, 0)))
                        } else {
                            None
                        };
                        (Some(bound), constant, ambient, global, implicit)
                    }
                };
                if let Some(bound) = bound {
                    if !global {
                        self.receiver_expr(receiver, receiving).await?;
                        self.c().emit(Op::AddressValue);
                    }
                    let mut c = self.c();
                    let end = c.code.len();
                    c.patch(bound, end);
                    for jump in constant.into_iter().chain(ambient).chain(implicit) {
                        c.patch(jump, end);
                    }
                }
            }
            Node::Var(name) if self.c().namespace.is_some() => {
                let (ambient, global, implicit) = {
                    let mut c = self.c();
                    let index = c.call_site(name, false).name;
                    let ambient = c.emit(Op::AmbientAddress(index, 0));
                    let global = c.global_fallback(name)?.is_some();
                    if global {
                        c.emit(Op::NamespaceAddress(index, false));
                    }
                    let implicit = if assignment && !global && c.implicit_name(name)? {
                        Some(c.emit(Op::ImplicitAddress(index, 0)))
                    } else {
                        None
                    };
                    (ambient, global, implicit)
                };
                if !global {
                    self.receiver_expr(receiver, receiving).await?;
                    self.c().emit(Op::AddressValue);
                }
                let mut c = self.c();
                let end = c.code.len();
                for jump in std::iter::once(ambient).chain(implicit) {
                    c.patch(jump, end);
                }
            }
            Node::Var(name) if self.c().global_fallback(name)?.is_some() => {
                let mut c = self.c();
                let global = c.global_fallback(name)?.unwrap();
                c.emit(Op::AddressGlobal(narrow(global)));
            }
            Node::Member(root, name) | Node::SafeMember(root, name) => {
                self.nested_address(root).await?;
                let mut c = self.c();
                let skip = matches!(receiver.node, Node::SafeMember(..))
                    .then(|| c.emit(Op::AddressJumpNil(0, false)));
                let site = c.call_site(name, true);
                c.emit(Op::AddressMember(site));
                if let Some(skip) = skip {
                    let end = c.code.len();
                    c.patch(skip, end);
                }
            }
            Node::Index(root, indices) => {
                self.nested_address(root).await?;
                for index in indices {
                    self.expr(index).await?;
                }
                self.c().emit(Op::AddressIndex(narrow(indices.len())));
            }
            _ => {
                self.receiver_expr(receiver, receiving).await?;
                self.c().emit(Op::AddressValue);
            }
        }
        let mut c = self.c();
        let end = c.code.len();
        for jump in [early, root, file].into_iter().flatten() {
            c.patch(jump, end);
        }
        Ok(())
    }
}

// Expressions that compile without generating any nested expression.
fn leaf(e: &Expr) -> bool {
    matches!(
        e.node,
        Node::Regex(..) | Node::Integer(_) | Node::BigInteger(..) | Node::Literal(_) | Node::Var(_)
    )
}

// Collect every name a call could resolve, including inside attached blocks.
fn call_names(
    expr: &Expr,
    names: &mut Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    let mut pending = Buffer::from_array(work, [Item::Expr(expr)])?;
    call_items(&mut pending, names, work)
}

fn call_items<'x>(
    pending: &mut Buffer<Item<'x>>,
    names: &mut Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    while let Some(item) = pending.pop() {
        work.charge(1)?;
        match item {
            Item::Stmts(body) => {
                for stmt in body.iter().rev() {
                    pending.push(work, Item::Stmt(stmt))?;
                }
            }
            Item::Stmt(stmt) => Item::push_statement(stmt, pending, work)?,
            Item::Target(target) => Item::push_target(target, pending, work)?,
            Item::Expr(expr) | Item::Callee(expr) => {
                match &expr.node {
                    Node::Call(name, _, _) => {
                        names.insert(work, name.clone(), ())?;
                    }
                    Node::BlockCall(call, _) => {
                        if let Node::Var(name) = &call.node {
                            names.insert(work, name.clone(), ())?;
                        }
                    }
                    _ => (),
                }
                Item::push_expression(expr, true, pending, work)?;
            }
        }
    }
    Ok(())
}

// Label a typed destructuring target for type errors, as `(a, *rest: T)`.
fn target_label(
    target: &Target,
    text: &mut Buffer<u8>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    enum Piece<'x> {
        Target(&'x Target),
        Type(&'x crate::compilation::Type),
        Text(&'static [u8]),
    }
    let mut pending = Buffer::from_array(work, [Piece::Target(target)])?;
    while let Some(piece) = pending.pop() {
        work.charge(1)?;
        match piece {
            Piece::Text(bytes) => text.extend_from_slice(work, bytes)?,
            // Type annotations nest only to their own shallow limit.
            Piece::Type(ty) => crate::shapes::format(ty, &mut (work, &mut *text))?,
            Piece::Target(Target::Value(Expr {
                node: Node::Var(name),
                ..
            })) => text.extend_from_slice(work, name.as_bytes())?,
            Piece::Target(Target::Typed(target, ty)) => {
                pending.push(work, Piece::Type(ty))?;
                pending.push(work, Piece::Text(b": "))?;
                pending.push(work, Piece::Target(target))?;
            }
            Piece::Target(Target::Tuple(parts)) => {
                pending.push(work, Piece::Text(b")"))?;
                for (index, (target, rest)) in parts.iter().enumerate().rev() {
                    if let Some(target) = target {
                        pending.push(work, Piece::Target(target))?;
                    }
                    if *rest {
                        pending.push(work, Piece::Text(b"*"))?;
                    }
                    if index > 0 {
                        pending.push(work, Piece::Text(b", "))?;
                    }
                }
                pending.push(work, Piece::Text(b"("))?;
            }
            Piece::Target(_) => (),
        }
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
    let mut pending = Buffer::from_array(work, [target])?;
    while let Some(target) = pending.pop() {
        work.charge(1)?;
        match target {
            Target::Typed(target, _) => pending.push(work, target)?,
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => names.push(work, name)?,
            Target::Tuple(parts) => {
                for (part, _) in parts.iter().rev() {
                    if let Some(part) = part {
                        pending.push(work, part)?;
                    }
                }
            }
            _ => (),
        }
    }
    Ok(())
}

// Names assigned anywhere in compound statements, in source order. A rescue
// binding is scoped to its clause, so names matching it there are dropped.
fn statement_names<'a>(
    body: &'a [Stmt],
    names: &mut Buffer<&'a Name>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    enum Step<'a> {
        Body(&'a [Stmt]),
        Target(&'a Target),
        Clause,
        EndClause(&'a Name),
    }
    let mut pending = Buffer::from_array(work, [Step::Body(body)])?;
    let mut clauses = Buffer::new();
    while let Some(step) = pending.pop() {
        work.charge(1)?;
        match step {
            Step::Target(target) => target_names(target, names, work)?,
            Step::Clause => clauses.push(work, names.len())?,
            Step::EndClause(binding) => {
                let start = clauses.pop().unwrap();
                let mut kept = start;
                for index in start..names.len() {
                    work.charge(1)?;
                    if names[index] != binding {
                        names[kept] = names[index];
                        kept += 1;
                    }
                }
                names.truncate(kept);
            }
            Step::Body(body) => {
                for stmt in body.iter().rev() {
                    work.charge(1)?;
                    match &stmt.node {
                        Statement::Expr(Expr {
                            node: Node::Try(attempt),
                            ..
                        }) => {
                            pending.push(work, Step::Body(&attempt.ensure))?;
                            pending.push(work, Step::Body(&attempt.alternate))?;
                            for rescue in attempt.rescues.iter().rev() {
                                if let Some(binding) = &rescue.binding {
                                    pending.push(work, Step::EndClause(binding))?;
                                    pending.push(work, Step::Body(&rescue.body))?;
                                    pending.push(work, Step::Clause)?;
                                } else {
                                    pending.push(work, Step::Body(&rescue.body))?;
                                }
                            }
                            pending.push(work, Step::Body(&attempt.body))?;
                        }
                        Statement::Assign(target, _, _) => {
                            pending.push(work, Step::Target(target))?;
                        }
                        Statement::If(branches, alternate, _) => {
                            pending.push(work, Step::Body(alternate))?;
                            for (_, body) in branches.iter().rev() {
                                pending.push(work, Step::Body(body))?;
                            }
                        }
                        Statement::While(_, body, _) => pending.push(work, Step::Body(body))?,
                        Statement::For(target, _, body) => {
                            pending.push(work, Step::Body(body))?;
                            pending.push(work, Step::Target(target))?;
                        }
                        _ => (),
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn mutating_member(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "prepend"
            | "pop"
            | "shift"
            | "delete"
            | "delete_if"
            | "keep_if"
            | "insert"
            | "clear"
            | "fill"
            | "replace"
    )
}
