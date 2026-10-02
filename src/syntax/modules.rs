use super::{Label, Parser, Parsing, Stmt, Token, keyword, source_text};
use crate::{
    Result,
    compilation::{Buffer, Name, Table, Text},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
}

#[derive(Debug)]
pub(crate) struct Module {
    pub offset: u32,
    pub is_class: bool,
    pub instance_methods: Buffer<(super::Definition, Visibility)>,
    pub name: Name,
    pub methods: Buffer<(super::Definition, Visibility)>,
    pub body: Buffer<Stmt>,
    pub modules: Buffer<Module>,
    /// The `public` and `protected` directives in member order, which Go
    /// refuses when a top-level function shares the word.
    pub directives: Buffer<&'static str>,
    /// Go's compile error for the first alias or named visibility directive
    /// whose method was not yet defined.
    pub missing: Option<Text>,
    /// Classes declared in the body, whose directives Go also checks.
    pub inner: Buffer<Module>,
    /// Each alias's index in `instance_methods` and the offset of its
    /// statement; an alias's definition copies its target's, offset included.
    pub aliases: Buffer<(usize, u32)>,
    pub(super) depth: u32,
}

impl Module {
    /// The height of the declaration's syntax tree, which the parser bounds.
    pub(crate) fn height(&self) -> u32 {
        self.depth
    }
}

/// How a visibility word applies, as Go's `parseVisibilityMember` reads it.
enum Directive {
    /// It applies to the declaration that follows on its line.
    Inline,
    /// It sets the visibility of the definitions that follow.
    Section,
    /// It names methods defined earlier.
    Named,
}

enum Member {
    Done,
    Statement,
    Module,
    Method,
    Ivar,
    ClassVar,
}

/// What removed class features name as their replacement.
pub(super) const NAMESPACES: &str = "define module functions with def self.name and call them on the module (Naming.display_name(person))";

impl Parser<'_> {
    /// Reports whether `module` declares a module: Go requires a name on the
    /// same line.
    pub(super) fn module_ahead(&self) -> bool {
        matches!(self.token(), Token::Word(w) if w == "module")
            && self.ident(self.pos + 1)
            && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
    }

    /// Go's `peekEndsStatement` for the token after the one at `index`, which
    /// starts on `line`.
    pub(super) fn ends_after(&self, index: usize, line: usize) -> bool {
        let next = self.significant(index + 1);
        match &self.tokens[next].token {
            Token::Eof | Token::P('}') | Token::EndLine => true,
            Token::Word(w)
                if matches!(w.as_str(), "end" | "else" | "elsif" | "ensure" | "rescue") =>
            {
                true
            }
            _ => self.tokens[next].line != line,
        }
    }

    /// Reports whether a visibility word is followed on its line by a
    /// declaration it applies to.
    fn inline_visibility(&self) -> bool {
        let next = self.significant(self.pos + 1);
        self.tokens[next].line == self.tokens[self.pos].line
            && matches!(&self.tokens[next].token, Token::Word(w) if matches!(w.as_str(), "def" | "property" | "getter" | "setter"))
    }

    fn symbol_at(&self, index: usize) -> bool {
        matches!(
            self.tokens[index].token,
            Token::Symbol(_) | Token::QuotedSymbol(_)
        )
    }

    /// Go's `startsVisibilityDirective` for `public` and `protected`, which
    /// are otherwise ordinary names.
    fn visibility_directive(&self) -> Result<bool> {
        let Token::Word(word) = self.token() else {
            return Ok(false);
        };
        let line = self.tokens[self.pos].line;
        let next = self.significant(self.pos + 1);
        if self.inline_visibility() || (self.symbol_at(next) && self.tokens[next].line == line) {
            return Ok(true);
        }
        Ok(!self.locals.contains(self.work, word.as_str())? && self.ends_after(self.pos, line))
    }

    /// Go's `startsMixinDirective` for `include` and `extend`.
    fn mixin_directive(&self) -> Result<bool> {
        let Token::Word(word) = self.token() else {
            return Ok(false);
        };
        let line = self.tokens[self.pos].line;
        let next = self.significant(self.pos + 1);
        let opens = match &self.tokens[next].token {
            Token::P('(') => true,
            Token::Word(w) => *w == "self",
            _ => false,
        };
        if self.tokens[next].line == line && (self.ident(next) || opens) {
            return Ok(true);
        }
        Ok(!self.locals.contains(self.work, word.as_str())? && self.ends_after(self.pos, line))
    }

    /// Parses a visibility member, as Go's `parseVisibilityMember` does.
    fn visibility_member(
        &mut self,
        class: &mut Module,
        level: Visibility,
        word: &'static str,
    ) -> Result<Directive> {
        self.work.charge(1)?;
        if matches!(word, "public" | "protected") {
            class.directives.push(self.work, word)?;
        }
        let line = self.tokens[self.pos].line;
        if self.inline_visibility() {
            self.bump()?;
            self.line_breaks()?;
            return Ok(Directive::Inline);
        }
        let next = self.significant(self.pos + 1);
        if self.symbol_at(next) && self.tokens[next].line == line {
            self.pos = next;
            loop {
                let name = self.symbol_name()?.unwrap();
                self.work
                    .charge(class.instance_methods.len() + class.methods.len())?;
                let instance = class
                    .instance_methods
                    .iter_mut()
                    .rev()
                    .find(|(m, _)| m.name == name);
                let target = instance
                    .or_else(|| class.methods.iter_mut().rev().find(|(m, _)| m.name == name));
                match target {
                    Some((_, current)) => *current = level,
                    None if class.missing.is_none() => {
                        let message = format!(
                            "{word} target method {name} is not defined on class {}",
                            class.name
                        );
                        class.missing = Some(Text::new(self.work, &message)?);
                    }
                    None => (),
                }
                let comma = self.significant(self.pos);
                if self.tokens[comma].token != Token::P(',') {
                    return Ok(Directive::Named);
                }
                let symbol = self.significant(comma + 1);
                self.pos = symbol;
                if !self.symbol_at(symbol) {
                    return self.expected(Label::Text("method name symbol"));
                }
            }
        }
        if !self.ends_after(self.pos, line) {
            self.pos = next;
            return self.err(format_args!(
                "{word} expects a method definition, symbol method names, or no argument"
            ));
        }
        self.bump()?;
        Ok(Directive::Section)
    }

    /// Reads a function name as Go's `parseFunctionStatement` does.
    fn function_name(&mut self, def: u32) -> Result<(Name, bool, bool)> {
        self.work.charge(1)?;
        if matches!(self.token(), Token::Word(w) if w == "self")
            && self.tokens[self.significant(self.pos + 1)].token == Token::P('.')
        {
            self.bump()?;
            self.line_breaks()?;
            self.bump()?;
            self.line_breaks()?;
            if !self.ident(self.pos) {
                return self.expected(Label::Text("identifier"));
            }
            return Ok((self.method_name()?, true, false));
        }
        let operator = match self.token() {
            Token::Op(op) if super::def_operator(op) => Some(*op),
            Token::P('[') if self.tokens[self.significant(self.pos + 1)].token == Token::P(']') => {
                Some("[]")
            }
            _ => None,
        };
        if let Some(op) = operator {
            self.method_spelling(op, self.tokens[self.pos].offset)?;
            if !self.inside_class {
                return Err(crate::Error::syntax(
                    self.work,
                    def as usize,
                    format_args!("operator method {op} must be defined in a class"),
                ));
            }
            if op == "[]" {
                self.bump()?;
                self.line_breaks()?;
            }
            self.bump()?;
            return Ok((Name::new(self.work, op)?, false, true));
        }
        if !self.ident(self.pos) {
            return self.expected(Label::Text("function name"));
        }
        Ok((self.method_name()?, false, false))
    }
}

/// A parsed `def`, with the shape Go records for it.
pub(super) struct Function {
    pub definition: super::Definition,
    pub class_method: bool,
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses a function from its `def` through its `end`, as Go's
    /// `parseFunctionStatement` does. `constants` exposes the enclosing class
    /// body's constants to the body.
    pub(super) async fn function(&self, constants: bool) -> Result<Function> {
        let _scope = self.recovery_scope()?;
        let work = self.p().work;
        let (offset, def_line, name, class_method, outer_locals, outer_it, outer_body) = {
            let mut p = self.p();
            work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            let def_line = p.tokens[p.pos].line;
            p.bump()?;
            p.line_breaks()?;
            let (mut name, class_method, operator) = p.function_name(offset)?;
            let name_offset = p.tokens[p.pos - 1].offset;
            // `ok!=` reads as `ok` and `!=`, but after a definition's name it
            // can only be a setter spelled with a suffix, as `ok?=` is.
            let next = &p.tokens[p.pos];
            if !operator && next.token == Token::Op("!=") && next.offset == p.tokens[p.pos - 1].end
            {
                if !super::suffixes::lenient() {
                    return Err(p.name_suffix_error(next.offset));
                }
                // A lenient parse reads the setter as it was spelled.
                p.bump()?;
                name = Name::join(work, &[&name, "!="])?;
            }
            p.line_breaks()?;
            if p.token() == &Token::Op("=") && (!operator || name == "[]") {
                p.bump()?;
                p.line_breaks()?;
                name = Name::join(work, &[&name, "="])?;
                p.method_spelling(&name, name_offset)?;
            }
            let outer_locals = std::mem::take(&mut p.locals);
            if constants {
                for (name, &id) in outer_locals.iter(work)? {
                    if name.chars().next().is_some_and(super::unicode::upper) {
                        p.locals.insert(work, name.clone(), id)?;
                    }
                }
            }
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            // A function's locals are no namespace's constants.
            let outer_body = std::mem::replace(&mut p.namespace_body, false);
            (
                offset,
                def_line,
                name,
                class_method,
                outer_locals,
                outer_it,
                outer_body,
            )
        };
        let (parenthesized, bare) = {
            let p = self.p();
            let line = p.tokens[p.pos].line;
            let parenthesized = p.token() == &Token::P('(') && line == def_line;
            let bare = !parenthesized
                && line == def_line
                && match p.token() {
                    Token::Word(w) => !keyword(w) && !w.starts_with("@@"),
                    Token::Op("*" | "**" | "&") => true,
                    _ => false,
                };
            (parenthesized, bare)
        };
        let mut signature = def_line;
        let (params, block) = if parenthesized {
            let empty = {
                let mut p = self.p();
                p.bump()?;
                p.line_breaks()?;
                p.token() == &Token::P(')')
            };
            let params = if empty {
                (Buffer::new(), None)
            } else {
                self.p().groups += 1;
                let params = self.parameters(true).await?;
                let mut p = self.p();
                p.groups -= 1;
                p.line_breaks()?;
                if p.token() != &Token::P(')') {
                    return p.expected(Label::Char(')'));
                }
                params
            };
            let mut p = self.p();
            signature = p.tokens[p.pos].line;
            p.bump()?;
            params
        } else if bare {
            let params = self.parameters(false).await?;
            signature = self.p().previous()?.line;
            params
        } else {
            (Buffer::new(), None)
        };
        let outer_block = {
            let mut p = self.p();
            let name = block.as_ref().map(|block| block.name.clone());
            if let Some(block) = block {
                p.additions.blocks.push(work, (offset, block))?;
            }
            std::mem::replace(&mut p.block_name, name)
        };
        self.p().note(|record| record.enter_function(&params));
        let return_type = {
            let mut p = self.p();
            let arrow = p.significant(p.pos);
            if p.tokens[arrow].token == Token::Op("->") && p.tokens[arrow].line == signature {
                p.pos = arrow + 1;
                p.line_breaks()?;
                Some(p.type_expr(1, false)?)
            } else {
                None
            }
        };
        let body = self.block(&["rescue", "else", "ensure", "end"]).await?;
        let rescued = matches!(self.p().token(), Token::Word(w) if w != "end");
        let body = if rescued {
            let attempt = self.rescue_tail(body, true, offset).await?;
            let depth = attempt.depth();
            let p = self.p();
            Buffer::from_array(
                work,
                [super::Statement::Expr(p.make_at(
                    super::Node::Try(crate::compilation::Boxed::new(work, attempt)?),
                    depth,
                    offset,
                )?)
                .at(work, offset)?],
            )?
        } else {
            self.p().expect_word("end")?;
            body
        };
        let mut p = self.p();
        p.note(super::record::Record::leave_function);
        p.locals = outer_locals;
        p.namespace_body = outer_body;
        p.declared_it = outer_it;
        p.block_name = outer_block;
        Ok(Function {
            definition: super::Definition {
                private: false,
                offset,
                accessor: None,
                name,
                params,
                body,
                return_type,
            },
            class_method,
        })
    }

    /// Parses a class or module declaration from its keyword, as Go's
    /// `parseClassStatement`, `parseModuleStatement` and `parseClassLikeBody` do.
    pub(super) async fn class_like(&self, module: bool) -> Result<Module> {
        let _scope = self.recovery_scope()?;
        let work = self.p().work;
        let (mut class, outer_locals, outer_it, outer_class, outer_namespace) = {
            let mut p = self.p();
            work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            p.bump()?;
            let next = p.significant(p.pos);
            if !module && p.tokens[next].token == Token::Op("<<") {
                p.pos = next;
                return p.err("class << self definitions are not supported; use def self.name");
            }
            p.pos = next;
            if !p.ident(p.pos) {
                return p.expected(Label::Text("identifier"));
            }
            let at = p.tokens[p.pos].offset;
            let name = p.name()?;
            if module && !name.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
                p.pos -= 1;
                return p.err("module name must start with an uppercase letter");
            }
            let next = p.significant(p.pos);
            if !module && p.tokens[next].token == Token::Op("<") {
                p.pos = next;
                if !super::suffixes::lenient() {
                    return p.err(format_args!(
                        "class inheritance is not supported; modules are namespaces: {NAMESPACES}"
                    ));
                }
                // A lenient parse reads the parent only for the uses it records.
                p.inherited()?;
            }
            p.enter()?;
            let outer_locals = std::mem::take(&mut p.locals);
            let inner = p.namespace_entered(&name, at)?;
            let outer_namespace = (
                std::mem::replace(&mut p.namespace, inner),
                std::mem::replace(&mut p.namespace_body, true),
            );
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            let outer_class = std::mem::replace(&mut p.inside_class, true);
            p.nesting += 1;
            let class = Module {
                offset,
                is_class: !module,
                instance_methods: Buffer::new(),
                name,
                methods: Buffer::new(),
                body: Buffer::new(),
                modules: Buffer::new(),
                directives: Buffer::new(),
                missing: None,
                inner: Buffer::new(),
                aliases: Buffer::new(),
                depth: 1,
            };
            (class, outer_locals, outer_it, outer_class, outer_namespace)
        };
        let mut section = Visibility::Public;
        let mut pending = None;
        loop {
            {
                let mut p = self.p();
                p.lines()?;
                if matches!(p.token(), Token::Eof)
                    || matches!(p.token(), Token::Word(w) if w == "end")
                {
                    break;
                }
            }
            let checkpoint = self.recovery_checkpoint();
            let result = async {
                let member = {
                    let mut p = self.p();
                    let word = match p.token() {
                        Token::Word(w) => w.as_str(),
                        _ => "",
                    };
                    work.charge(1)?;
                    match word {
                        "class" if module => {
                            return p.err("class declarations are not supported in module bodies");
                        }
                        "def" => Member::Method,
                        "alias" if p.alias_ahead() => {
                            let offset = p.tokens[p.pos].offset;
                            let (new, old) = p.alias_names()?;
                            if module {
                                return Err(crate::Error::syntax(
                                    work,
                                    offset,
                                    format_args!(
                                        "alias in module {} is not supported; a module has no instance methods to rename, so {NAMESPACES}",
                                        source_text(&class.name)
                                    ),
                                ));
                            }
                            p.class_alias(&mut class, new, old, offset as u32)?;
                            Member::Done
                        }
                        "alias_method" => {
                            let offset = p.tokens[p.pos].offset;
                            let (new, old) = p.alias_method()?;
                            if module {
                                return Err(crate::Error::syntax(
                                    work,
                                    offset,
                                    format_args!(
                                        "alias_method in module {} is not supported; a module has no instance methods to rename, so {NAMESPACES}",
                                        source_text(&class.name)
                                    ),
                                ));
                            }
                            p.class_alias(&mut class, new, old, offset as u32)?;
                            Member::Done
                        }
                        "public" | "protected" | "private"
                            if word == "private" || p.visibility_directive()? =>
                        {
                            let (level, word) = match word {
                                "public" => (Visibility::Public, "public"),
                                "protected" => (Visibility::Protected, "protected"),
                                _ => (Visibility::Private, "private"),
                            };
                            match p.visibility_member(&mut class, level, word)? {
                                Directive::Inline => pending = Some(level),
                                Directive::Section => section = level,
                                Directive::Named => (),
                            }
                            Member::Done
                        }
                        "module" if module && p.module_ahead() => Member::Module,
                        "type" if p.type_alias_ahead() => {
                            let alias = p.type_alias()?;
                            p.additions
                                .aliases
                                .push(work, (Some(class.offset), alias))?;
                            Member::Done
                        }
                        // A module has no instances, so `@name:` stays the syntax
                        // error it always was there.
                        _ if !module && p.ivar_ahead() => Member::Ivar,
                        _ if p.class_var_ahead()? => Member::ClassVar,
                        "include" | "extend" if p.mixin_directive()? => {
                            return p.err(format_args!(
                                "{word} is not supported; modules are namespaces: {NAMESPACES}"
                            ));
                        }
                        "property" | "getter" | "setter" => {
                            let kind = if word == "property" {
                                "property"
                            } else if word == "getter" {
                                "getter"
                            } else {
                                "setter"
                            };
                            let offset = p.tokens[p.pos].offset;
                            let visibility = pending.take().unwrap_or(section);
                            p.class_properties(&mut class, kind, visibility)?;
                            if module {
                                return Err(crate::Error::syntax(
                                    work,
                                    offset,
                                    format_args!(
                                        "{kind} in module {} is not supported; a module has no instances, so {NAMESPACES}",
                                        source_text(&class.name)
                                    ),
                                ));
                            }
                            Member::Done
                        }
                        _ => Member::Statement,
                    }
                };
                match member {
                    Member::Done => (),
                    Member::Statement => {
                        let (stmt, inner) = self.class_statement().await?;
                        class.depth = class.depth.max(1 + stmt.depth);
                        class.body.push(work, stmt)?;
                        if let Some(inner) = inner {
                            class.inner.push(work, inner)?;
                        }
                    }
                    Member::Module => {
                        let nested = self.nested_module().await?;
                        class.depth = class.depth.max(1 + nested.depth);
                        class.modules.push(work, nested)?;
                    }
                    Member::Ivar => {
                        let (ivar, default) = self.ivar().await?;
                        let mut p = self.p();
                        let ivars = &mut p.additions.ivars;
                        work.charge(ivars.len())?;
                        let duplicate = ivars
                            .iter()
                            .any(|(owner, prior)| *owner == class.offset && prior.name == ivar.name);
                        if duplicate {
                            return Err(crate::Error::syntax(
                                work,
                                ivar.offset as usize,
                                format_args!(
                                    "duplicate instance variable declaration @{}",
                                    source_text(&ivar.name)
                                ),
                            ));
                        }
                        ivars.push(work, (class.offset, ivar))?;
                        if let Some(default) = default {
                            class.depth = class.depth.max(1 + default.depth);
                            p.additions.defaults.push(work, (class.offset, default))?;
                        }
                    }
                    Member::ClassVar => {
                        let (declared, assignment) = self.class_var().await?;
                        let mut p = self.p();
                        let class_vars = &mut p.additions.class_vars;
                        work.charge(class_vars.len())?;
                        let duplicate = class_vars.iter().any(|(owner, prior)| {
                            *owner == class.offset && prior.name == declared.name
                        });
                        if duplicate {
                            return Err(crate::Error::syntax(
                                work,
                                declared.offset as usize,
                                format_args!(
                                    "duplicate class variable declaration {}",
                                    source_text(&declared.name)
                                ),
                            ));
                        }
                        class_vars.push(work, (class.offset, declared))?;
                        class.depth = class.depth.max(1 + assignment.depth);
                        class.body.push(work, assignment)?;
                    }
                    Member::Method => {
                        let Function {
                            definition,
                            class_method,
                        } = self.function(true).await?;
                        if module && !class_method {
                            return Err(module_function(work, &definition, &class.name));
                        }
                        let mut visibility = pending.take().unwrap_or(section);
                        if definition.name == "initialize" {
                            visibility = Visibility::Private;
                        }
                        class.depth = class.depth.max(1 + definition.depth());
                        let methods = if class_method {
                            &mut class.methods
                        } else {
                            &mut class.instance_methods
                        };
                        methods.push(work, (definition, visibility))?;
                    }
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                self.recover(checkpoint, &["end"], error)?;
            }
        }
        let mut p = self.p();
        p.inside_class = outer_class;
        p.nesting -= 1;
        p.expect_word("end")?;
        p.check_depth(class.depth, class.offset)?;
        p.locals = outer_locals;
        (p.namespace, p.namespace_body) = outer_namespace;
        p.declared_it = outer_it;
        p.depth -= 1;
        Ok(class)
    }
}

/// Go's rejection of a module method that is not declared with `self.`.
fn module_function(
    work: &dyn crate::compilation::Work,
    definition: &super::Definition,
    module: &str,
) -> crate::Error {
    let name = definition.name.as_str();
    let base = name.strip_suffix('=').unwrap_or(name);
    let mut chars = base.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c == '_' || super::unicode::letter(c))
        && chars.all(|c| matches!(c, '_' | '?' | '!') || super::unicode::letter_or_digit(c))
        && !keyword(base);
    let message = if identifier {
        format!(
            "def {} in module {} must be {}; a module is a namespace, not a method source",
            source_text(name),
            source_text(module),
            source_text(&format!("def self.{name}"))
        )
    } else {
        format!(
            "operator method {} is not supported in module {}; an operator dispatches on an instance and a module has none, so define {} on a class",
            source_text(name),
            source_text(module),
            source_text(name)
        )
    };
    crate::Error::syntax(work, definition.offset as usize, message)
}

/// The names Go's compile phase registers, used to report its first error.
#[derive(Default)]
pub(super) struct Registry {
    pub functions: Table<()>,
    classes: Table<()>,
    enums: Table<()>,
}

impl Registry {
    /// Registers a class or module and its nested modules, as Go's
    /// `registerClassStmt` does.
    pub(super) fn module(
        &mut self,
        module: &Module,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        // Module nesting reaches the syntax depth limit, so the walk keeps
        // its own stack, reserved from `work` as it grows.
        let mut stack = Buffer::new();
        stack.push(work, (module, module.name.clone(), 0usize))?;
        self.open(module, &module.name, work)?;
        while let Some((module, name, child)) = stack.pop() {
            work.charge(1)?;
            if let Some(nested) = module.modules.get(child) {
                let qualified = Name::join(work, &[&name, "::", &nested.name])?;
                stack.push(work, (module, name, child + 1))?;
                self.open(nested, &qualified, work)?;
                stack.push(work, (nested, qualified, 0))?;
                continue;
            }
            if let Some(missing) = &module.missing {
                return Err(super::unsupported(work, missing));
            }
            self.classes.insert(work, name, ())?;
        }
        Ok(())
    }

    fn open(&self, module: &Module, name: &str, work: &dyn crate::compilation::Work) -> Result<()> {
        let kind = if module.is_class { "class" } else { "module" };
        if self.classes.contains(work, name)? {
            return Err(super::unsupported(
                work,
                &format!("duplicate {kind} {name}"),
            ));
        }
        if self.functions.contains(work, name)? || self.enums.contains(work, name)? {
            return Err(super::unsupported(
                work,
                &format!("duplicate top-level name {name}"),
            ));
        }
        Ok(())
    }

    /// Registers a function or alias name.
    pub(super) fn function(
        &mut self,
        name: &Name,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        work.charge(1)?;
        if name == "__main__" {
            return Err(super::unsupported(
                work,
                "duplicate or reserved function name",
            ));
        }
        if self.functions.contains(work, name)? {
            return Err(super::unsupported(
                work,
                &format!("duplicate function {name}"),
            ));
        }
        if self.classes.contains(work, name)? || self.enums.contains(work, name)? {
            return Err(super::unsupported(
                work,
                &format!("duplicate top-level name {name}"),
            ));
        }
        self.functions.insert(work, name.clone(), ())?;
        Ok(())
    }

    /// Registers an enum and checks its name and members, as Go's
    /// `compileEnumDef` does.
    pub(super) fn enumeration(
        &mut self,
        name: &Name,
        members: &Buffer<Name>,
        work: &dyn crate::compilation::Work,
    ) -> Result<()> {
        work.charge(1)?;
        if self.enums.contains(work, name)? {
            return Err(super::unsupported(work, &format!("duplicate enum {name}")));
        }
        if self.functions.contains(work, name)? || self.classes.contains(work, name)? {
            return Err(super::unsupported(
                work,
                &format!("duplicate top-level name {name}"),
            ));
        }
        if name.ends_with('?') {
            return Err(super::unsupported(
                work,
                &format!("enum name {name} must not end with '?'"),
            ));
        }
        if crate::types::builtin_name(name).is_some() {
            return Err(super::unsupported(
                work,
                &format!("enum name {name} conflicts with built-in type"),
            ));
        }
        let mut symbols: Table<usize> = Table::new();
        for (index, member) in members.iter().enumerate() {
            work.charge(1)?;
            let symbol = Name::new(work, &crate::enums::symbol(member))?;
            if let Some(&prior) = symbols.get(work, &symbol)? {
                let prior = &members[prior];
                return Err(super::unsupported(
                    work,
                    &format!(
                        "enum {name} member {member} conflicts with {prior} after symbol normalization"
                    ),
                ));
            }
            symbols.insert(work, symbol, index)?;
        }
        self.enums.insert(work, name.clone(), ())?;
        Ok(())
    }
}

/// Go's check that no class or module uses `public` or `protected` as a
/// directive while a top-level function has that name.
pub(super) fn directive_collisions(
    module: &Module,
    functions: &Table<()>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    // What is left of each level's nested namespaces, rather than every one
    // at once, in a list reserved from `work` as it grows: a namespace's
    // nested namespaces above its inner ones, so they are visited first.
    // Each visited is a step.
    let mut levels = Buffer::new();
    levels.push(work, std::slice::from_ref(module).iter())?;
    while let Some(level) = levels.last_mut() {
        let Some(module) = level.next() else {
            levels.pop();
            continue;
        };
        work.charge(1)?;
        let kind = if module.is_class { "class" } else { "module" };
        for level in &module.directives {
            work.charge(1)?;
            if functions.contains(work, level)? {
                return Err(super::unsupported(
                    work,
                    &format!(
                        "{level} in {kind} {} is a visibility directive, but this script also defines a function named {level}; rename the function, or call it with parentheses ({level}(:name)), which stays a call",
                        module.name
                    ),
                ));
            }
        }
        levels.push(work, module.inner.iter())?;
        levels.push(work, module.modules.iter())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{CallContext, CallOptions, compilation::Meter};
    use std::cell::RefCell;

    #[test]
    fn the_directive_walk_reserves_its_levels() {
        // Namespaces nested as deep as the parser allows, whose walk keeps
        // a level for each.
        let depth = 1_000;
        let opened: String = (0..depth).map(|i| format!("module M{i}\n")).collect();
        let source = format!("{opened}X = 1\n{}", "end\n".repeat(depth));
        let parsed = crate::syntax::parse(&source, &()).unwrap();
        let mut context = CallContext::new(CallOptions::default());
        super::directive_collisions(
            &parsed.modules[0],
            &super::Table::new(),
            &Meter(RefCell::new(&mut context)),
        )
        .unwrap();
        let level = std::mem::size_of::<std::slice::Iter<'static, super::Module>>();
        let peak = context.stats().peak_memory_bytes;
        assert!(
            peak >= depth * level,
            "{peak} bytes reserved for {depth} levels of {level}"
        );
    }
}
