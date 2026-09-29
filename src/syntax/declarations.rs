use super::{
    Declarations, Definition, Label, Outline, ParamKind, Parameter, Parser, Parsing, Statement,
    Stmt, Token, modules::Module, record, source_text,
};
use crate::{
    Error, Result,
    compilation::{Buffer, Name, Table, Type, TypeKind},
};

/// A statement Go parses, with the declarations this port keeps apart.
pub(super) enum Declared {
    Statement(Stmt),
    /// A function, and whether `private` declared it.
    Function(Definition, bool),
    Class(Module),
    Enum(Name, Buffer<Name>, Vec<u32>),
    Alias(Name, Name),
    TypeAlias(super::TypeAlias),
}

/// A top-level declaration in source order, for Go's compile checks.
/// A top-level declaration in source order; a function's and an alias's
/// carry the offset of the name they declare.
enum Order {
    Function(Name, usize),
    Module(usize),
    Enum(usize),
    Alias(Name, Name, bool, usize),
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses one statement, as Go's `parseStatement` does, keeping the
    /// declarations it may make.
    pub(super) async fn declaration(&self) -> Result<Declared> {
        let (word, offset) = {
            let p = self.p();
            let word = match p.token() {
                Token::Word(w)
                    if matches!(w.as_str(), "def" | "class" | "enum" | "export" | "private") =>
                {
                    Some(w.as_str())
                }
                Token::Word(w) if *w == "alias" && p.alias_ahead() => Some("alias"),
                Token::Word(w) if *w == "type" && p.type_alias_ahead() => Some("type"),
                Token::Word(_) if p.module_ahead() => Some("module"),
                _ => None,
            };
            (word, p.tokens[p.pos].offset)
        };
        let Some(word) = word else {
            return Ok(Declared::Statement(self.plain().await?));
        };
        self.p().enter()?;
        let top = {
            let p = self.p();
            !p.inside_class && p.nesting == 0
        };
        let declared = match word {
            "def" => {
                let function = self.function(false).await?;
                Declared::Function(function.definition, false)
            }
            "class" => Declared::Class(self.nested_class().await?),
            "module" => {
                if !top {
                    return self.p().err(
                        "module declarations are only supported at the top level and nested in module bodies",
                    );
                }
                Declared::Class(self.nested_module().await?)
            }
            "enum" => {
                if !top {
                    return self.p().err("enum is only supported at the top level");
                }
                self.p().enumeration()?
            }
            "type" => {
                let mut p = self.p();
                if !top {
                    return p.err(
                        "type aliases are only supported at the top level and in module or class bodies",
                    );
                }
                Declared::TypeAlias(p.type_alias()?)
            }
            "alias" => {
                let mut p = self.p();
                if !top {
                    return p.err(
                        "alias declarations are only supported at the top level or in class bodies",
                    );
                }
                let (new, old) = p.alias_names()?;
                Declared::Alias(new, old)
            }
            _ => {
                let private = word == "private";
                {
                    let mut p = self.p();
                    if !top {
                        return p.err(if private {
                            "private is only supported for top-level functions and class methods"
                        } else {
                            "export is only supported for top-level functions"
                        });
                    }
                    p.bump()?;
                    p.line_breaks()?;
                    if !matches!(p.token(), Token::Word(w) if w == "def") {
                        return p.expected(Label::Keyword("def"));
                    }
                }
                let function = self.function(false).await?;
                if function.class_method {
                    return Err(Error::syntax(
                        self.p().work,
                        offset,
                        if private {
                            "private cannot be used with class methods"
                        } else {
                            "export cannot be used with class methods"
                        },
                    ));
                }
                Declared::Function(function.definition, private)
            }
        };
        self.reject_modifier().await?;
        let mut p = self.p();
        if let Declared::Function(definition, _) = &declared {
            p.check_depth(definition.depth(), definition.offset)?;
        }
        p.depth -= 1;
        Ok(declared)
    }

    /// Go accepts a statement modifier only after an expression, assignment or
    /// leaf control statement, and reports one elsewhere after its condition.
    pub(super) async fn reject_modifier(&self) -> Result<()> {
        let (offset, modifier) = {
            let mut p = self.p();
            let modifier = match p.token() {
                Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until") => {
                    w.as_str()
                }
                _ => return Ok(()),
            };
            let offset = p.tokens[p.pos].offset;
            p.bump()?;
            p.line_breaks()?;
            (offset, modifier)
        };
        self.line_expr(0).await?;
        Err(Error::syntax(
            self.p().work,
            offset,
            format_args!(
                "modifier {modifier} is only supported after expression or assignment statements, or leaf control-flow statements"
            ),
        ))
    }

    /// Parses a statement where Go runs a nested function or class only to
    /// report it.
    pub(super) async fn statement(&self) -> Result<Stmt> {
        Ok(match self.declaration().await? {
            Declared::Statement(stmt) => stmt,
            Declared::Function(definition, _) => Stmt {
                depth: definition.depth(),
                offset: definition.offset,
                node: Statement::Unsupported,
            },
            Declared::Class(class) => Statement::UnboundClass(class.name).at(class.offset),
            Declared::Enum(..) | Declared::Alias(..) | Declared::TypeAlias(..) => unreachable!(),
        })
    }

    /// Parses a class body statement, keeping a nested class for Go's
    /// directive checks.
    pub(super) async fn class_statement(&self) -> Result<(Stmt, Option<Module>)> {
        Ok(match self.declaration().await? {
            Declared::Class(class) => {
                let stmt = Statement::UnboundClass(class.name.clone()).at(class.offset);
                (stmt, Some(class))
            }
            Declared::Statement(stmt) => (stmt, None),
            Declared::Function(definition, _) => (
                Stmt {
                    depth: definition.depth(),
                    offset: definition.offset,
                    node: Statement::Unsupported,
                },
                None,
            ),
            Declared::Enum(..) | Declared::Alias(..) | Declared::TypeAlias(..) => unreachable!(),
        })
    }

    pub(super) async fn program(&self) -> Result<Declarations> {
        let work = self.p().work;
        let mut defs = Buffer::new();
        // Indexes top-level functions by name for aliases.
        let mut def_names: Table<usize> = Table::new();
        let mut enums = Buffer::new();
        let mut modules = Buffer::new();
        let mut top = Buffer::new();
        let mut outline = Buffer::new();
        let mut order = Vec::new();
        loop {
            let (offset, first) = {
                let mut p = self.p();
                p.lines()?;
                if matches!(p.token(), Token::Eof) {
                    break;
                }
                (p.tokens[p.pos].offset as u32, p.pos)
            };
            let checkpoint = self.recovery_checkpoint();
            let declared = match self.declaration().await {
                Ok(declared) => declared,
                Err(error) => {
                    self.recover(checkpoint, &[], error)?;
                    continue;
                }
            };
            let mut p = self.p();
            let end = p.declaration_end(first);
            match declared {
                Declared::Function(mut definition, private) => {
                    definition.private = private;
                    outline.push(
                        work,
                        Outline {
                            kind: crate::DeclarationKind::Function,
                            name: definition.name.clone(),
                            start: offset as usize,
                            end,
                        },
                    )?;
                    let at = p.tokens[p.significant(first + 1)].offset;
                    order.push(Order::Function(definition.name.clone(), at));
                    let index = defs.len();
                    def_names.insert(work, definition.name.clone(), index)?;
                    defs.push(work, definition)?;
                    p.note(|record| record.top.push((offset, record::Top::Function(index))));
                }
                Declared::Alias(name, target) => {
                    // Like Go, an alias copies a function declared before it.
                    let found = match def_names.get(work, &target)? {
                        Some(&original) => {
                            let mut definition = super::work::definition(work, &defs[original])?;
                            definition.name = name.clone();
                            let index = defs.len();
                            def_names.insert(work, name.clone(), index)?;
                            defs.push(work, definition)?;
                            p.note(|record| {
                                let alias = record::Top::Alias(index, target.to_string());
                                record.top.push((offset, alias));
                            });
                            true
                        }
                        None => false,
                    };
                    outline.push(
                        work,
                        Outline {
                            kind: crate::DeclarationKind::Function,
                            name: name.clone(),
                            start: offset as usize,
                            end,
                        },
                    )?;
                    let at = p.tokens[p.significant(first + 1)].offset;
                    order.push(Order::Alias(name, target, found, at));
                }
                Declared::Class(module) => {
                    let kind = if module.is_class {
                        crate::DeclarationKind::Class
                    } else {
                        crate::DeclarationKind::Module
                    };
                    outline.push(
                        work,
                        Outline {
                            kind,
                            name: module.name.clone(),
                            start: offset as usize,
                            end,
                        },
                    )?;
                    top.push(work, Statement::Module(module.name.clone()).at(offset))?;
                    let index = modules.len();
                    order.push(Order::Module(index));
                    modules.push(work, module)?;
                    p.note(|record| record.top.push((offset, record::Top::Module(index))));
                }
                Declared::Enum(name, members, offsets) => {
                    outline.push(
                        work,
                        Outline {
                            kind: crate::DeclarationKind::Enum,
                            name: name.clone(),
                            start: offset as usize,
                            end,
                        },
                    )?;
                    let index = enums.len();
                    order.push(Order::Enum(index));
                    enums.push(work, (name, members))?;
                    p.note(|record| {
                        record.top.push((offset, record::Top::Enum(index)));
                        record.enums.push(offsets);
                    });
                }
                Declared::Statement(stmt) => {
                    let index = top.len();
                    top.push(work, stmt)?;
                    p.note(|record| record.top.push((offset, record::Top::Statement(index))));
                }
                Declared::TypeAlias(alias) => p.additions.aliases.push(work, (None, alias))?,
            }
        }
        if M::RECOVER
            && let Some(error) = self.recovered_error()
        {
            return Err(error);
        }
        compile_checks(&order, &modules, &enums, work)?;
        defs.insert(
            work,
            0,
            Definition {
                offset: 0,
                private: true,
                accessor: None,
                name: Name::new(work, "__main__")?,
                params: Buffer::new(),
                body: top,
                return_type: None,
            },
        )?;
        let interpolations = std::mem::take(&mut self.p().interpolations);
        let additions = std::mem::take(&mut self.p().additions);
        Ok(Declarations {
            functions: defs,
            enums,
            modules,
            additions,
            outline,
            interpolations,
        })
    }

    /// Parses a parameter list, as Go's `parseParamsWithOptions` does, and
    /// the typed block parameter that may end it.
    ///
    /// Parameters after a bare `*` or a `*rest` parameter are keyword
    /// parameters (ADR-007): `def send(to: string, *, cc: string? = nil)`.
    /// After a bare `*` every parameter is written `name: T = default`; after
    /// `*rest` the removed keyword forms, such as `name: default`, still parse.
    pub(super) async fn parameters(
        &self,
        parenthesized: bool,
    ) -> Result<(Buffer<Parameter>, Option<super::BlockParam>)> {
        let work = self.p().work;
        work.charge(1)?;
        let mut params: Buffer<Parameter> = Buffer::new();
        let (mut rest, mut keywords, mut keyword_rest) = (false, false, false);
        let mut marker = false;
        loop {
            if self.p().keyword_marker_ahead() {
                let mut p = self.p();
                let message = if marker {
                    Some("duplicate `*` before keyword parameters")
                } else if rest {
                    Some(
                        "parameters after a rest parameter are keyword parameters already; remove the bare `*`",
                    )
                } else if keywords || keyword_rest {
                    Some("a bare `*` must precede keyword and keyword rest parameters")
                } else {
                    None
                };
                if let Some(message) = message {
                    return p.err(message);
                }
                p.bump()?;
                p.pos = p.significant(p.pos) + 1;
                p.line_breaks()?;
                let named = p.ident(p.pos)
                    || matches!(p.token(), Token::Word(w) if w.starts_with('@') && !w.starts_with("@@"));
                if !named {
                    return p.err("a bare `*` must be followed by keyword parameters");
                }
                marker = true;
                continue;
            }
            if self.p().block_param_ahead() {
                let mut p = self.p();
                let block = p.block_param()?;
                work.charge(params.len())?;
                if params.iter().any(|param| param.name == block.name) {
                    return Err(Error::syntax(
                        work,
                        block.offset as usize + 1,
                        format_args!("duplicate parameter {}", source_text(&block.name)),
                    ));
                }
                let comma = p.significant(p.pos);
                if p.tokens[comma].token == Token::P(',') {
                    p.pos = comma;
                    return p.err("the block parameter must be the last parameter");
                }
                return Ok((params, Some(block)));
            }
            let (mut param, offset) = self.parameter(parenthesized, marker).await?;
            if (marker || rest) && param.kind == ParamKind::Positional {
                param.kind = ParamKind::Keyword;
            }
            let mut p = self.p();
            let order = match param.kind {
                ParamKind::Positional if rest || keywords || keyword_rest => {
                    Some(ordinary_order(&param, &params))
                }
                ParamKind::Rest if rest => Some("duplicate rest parameter".into()),
                ParamKind::Rest if keywords || keyword_rest => {
                    Some("rest parameter must precede keyword and keyword rest parameters".into())
                }
                ParamKind::Keyword if keyword_rest => {
                    Some("keyword parameters must precede keyword rest parameters".into())
                }
                ParamKind::KeywordRest if keyword_rest => {
                    Some("duplicate keyword rest parameter".into())
                }
                _ => None,
            };
            if let Some(message) = order {
                return Err(Error::syntax(work, offset, message));
            }
            match param.kind {
                ParamKind::Rest => rest = true,
                ParamKind::Keyword => keywords = true,
                ParamKind::KeywordRest => keyword_rest = true,
                ParamKind::Positional => (),
            }
            let id = p.local_id(&param.name, offset)?;
            p.locals.insert(work, param.name.clone(), id)?;
            p.declared_it |= param.name == "it";
            params.push(work, param)?;
            let comma = p.significant(p.pos);
            if p.tokens[comma].token != Token::P(',') {
                break;
            }
            p.pos = comma + 1;
            p.line_breaks()?;
        }
        Ok((params, None))
    }

    /// Parses one parameter, as Go's `parseParam` does, with its name's offset.
    /// A `strict` parameter follows a bare `*` and is written only as
    /// `name`, `name: T`, `name = default` or `name: T = default`.
    async fn parameter(&self, parenthesized: bool, strict: bool) -> Result<(Parameter, usize)> {
        let work = self.p().work;
        let (mut kind, name, instance, offset) = {
            let mut p = self.p();
            work.charge(1)?;
            let kind = match p.token() {
                Token::Op("*") => {
                    p.bump()?;
                    let next = p.significant(p.pos);
                    if p.tokens[next].token == Token::Op("*") {
                        p.pos = next + 1;
                        ParamKind::KeywordRest
                    } else {
                        ParamKind::Rest
                    }
                }
                Token::Op("**") => {
                    p.bump()?;
                    ParamKind::KeywordRest
                }
                Token::Op("&") => {
                    return p.err(
                        "block capture parameters are not supported; a block is not a value. Run the caller's block with `yield`, and ask `block_given?` when it is optional",
                    );
                }
                _ => ParamKind::Positional,
            };
            p.line_breaks()?;
            let instance =
                matches!(p.token(), Token::Word(w) if w.starts_with('@') && !w.starts_with("@@"));
            if !p.ident(p.pos) && !(instance && kind == ParamKind::Positional) {
                return p.expected(Label::Text(match kind {
                    ParamKind::Rest => "rest parameter name",
                    ParamKind::KeywordRest => "keyword rest parameter name",
                    _ => "parameter name",
                }));
            }
            let offset = p.tokens[p.pos].offset;
            let Token::Word(word) = p.bump()? else {
                unreachable!()
            };
            p.binding_name(&word, offset)?;
            let name = Name::new(work, word.strip_prefix('@').unwrap_or(&word))?;
            (kind, name, instance, offset)
        };
        let mut ty = None;
        let colon = {
            let p = self.p();
            let colon = p.significant(p.pos);
            (p.tokens[colon].token == Token::P(':')).then_some(colon)
        };
        if let Some(colon) = colon {
            // The canonical surface declares keywords only after `*`.
            let plain =
                kind == ParamKind::Positional && !instance && !strict && !super::canonical();
            let keyword = {
                let mut p = self.p();
                p.pos = colon + 1;
                if plain && p.ends_required_keyword(colon, parenthesized) {
                    return Ok((
                        Parameter {
                            ivar: None,
                            name,
                            kind: ParamKind::Keyword,
                            default: None,
                            ty: None,
                        },
                        offset,
                    ));
                }
                plain && p.keyword_default(parenthesized)?
            };
            if keyword {
                self.p().line_breaks()?;
                let default = if parenthesized {
                    self.expr(0).await?
                } else {
                    self.line_expr(0).await?
                };
                return Ok((
                    Parameter {
                        ivar: None,
                        name,
                        kind: ParamKind::Keyword,
                        default: Some(default),
                        ty: None,
                    },
                    offset,
                ));
            }
            let mut p = self.p();
            p.line_breaks()?;
            let start = p.tokens[p.pos].offset;
            let annotation = p.type_expr(1, false)?;
            if matches!(kind, ParamKind::Rest | ParamKind::KeywordRest)
                && !annotation.captures(kind == ParamKind::KeywordRest)
            {
                let name = source_text(&name);
                return Err(Error::syntax(
                    work,
                    start,
                    if kind == ParamKind::Rest {
                        format!(
                            "rest parameter {name} captures an array; annotate it as array<...> or any"
                        )
                    } else {
                        format!(
                            "keyword rest parameter {name} captures a hash; annotate it as hash<...>, object, a shape type, or any"
                        )
                    },
                ));
            }
            ty = Some(annotation);
            let trailing = p.significant(p.pos);
            if plain && p.tokens[trailing].token == Token::P(':') {
                p.pos = trailing + 1;
                if !p.ends_required_keyword(trailing, parenthesized) {
                    p.pos = trailing;
                    return p.err("typed required keyword parameter must end after trailing ':'");
                }
                kind = ParamKind::Keyword;
            }
        }
        let equals = {
            let mut p = self.p();
            let equals = p.significant(p.pos);
            if p.tokens[equals].token == Token::Op("=") {
                if matches!(kind, ParamKind::Rest | ParamKind::KeywordRest) {
                    p.pos = equals;
                    return p.err("capture parameters cannot have default values");
                }
                p.pos = equals + 1;
                p.line_breaks()?;
                true
            } else {
                false
            }
        };
        let default = if !equals {
            None
        } else if parenthesized {
            Some(self.expr(0).await?)
        } else {
            Some(self.line_expr(0).await?)
        };
        Ok((
            Parameter {
                ivar: instance.then(|| name.clone()),
                name,
                kind,
                default,
                ty,
            },
            offset,
        ))
    }
}

/// Go's message for an ordinary parameter after a rest or keyword parameter,
/// which names the likely intent when a bare type annotation spells an
/// earlier parameter.
fn ordinary_order(param: &Parameter, earlier: &Buffer<Parameter>) -> String {
    if let Some(Type {
        name,
        kind: TypeKind::Named,
        nullable: false,
    }) = &param.ty
        && earlier.iter().any(|p| p.name == *name)
    {
        let (param, name) = (source_text(&param.name), source_text(name));
        return format!(
            "{param}: {name} reads as a type annotation, not a default; write {param}: ({name}) to default {param} to parameter {name}"
        );
    }
    "ordinary parameters must precede rest, keyword, and keyword rest parameters".into()
}

impl Parser<'_> {
    /// Whether a bare `*` followed by a comma begins the keyword parameters.
    fn keyword_marker_ahead(&self) -> bool {
        self.token() == &Token::Op("*")
            && self.tokens[self.significant(self.pos + 1)].token == Token::P(',')
    }

    /// Go's `peekEndsRequiredKeywordParam` for the token after the colon at `colon`.
    fn ends_required_keyword(&self, colon: usize, parenthesized: bool) -> bool {
        let next = self.significant(colon + 1);
        match self.tokens[next].token {
            Token::P(',' | ')') => true,
            Token::Op("->") | Token::EndLine | Token::Eof => !parenthesized,
            _ => !parenthesized && self.tokens[next].line != self.tokens[colon].line,
        }
    }

    /// Parses an enum from its keyword, as Go's `parseEnumStatement` does.
    fn enumeration(&mut self) -> Result<Declared> {
        let work = self.work;
        work.charge(1)?;
        let offset = self.tokens[self.pos].offset;
        self.bump()?;
        self.line_breaks()?;
        if !self.ident(self.pos) {
            return self.expected(Label::Text("identifier"));
        }
        let name = self.name()?;
        // Enums are declared at the top level only.
        let namespace = self.namespace_entered(&name)?;
        let mut members = Buffer::new();
        let mut member_offsets = Vec::new();
        let mut seen = Table::new();
        loop {
            self.lines()?;
            if matches!(self.token(), Token::Eof)
                || matches!(self.token(), Token::Word(w) if w == "end")
            {
                break;
            }
            if !self.ident(self.pos) && !matches!(self.token(), Token::Word(w) if w == "enum") {
                return self.expected(Label::Text("enum member name"));
            }
            let member_offset = self.tokens[self.pos].offset;
            if self.record.is_some() {
                member_offsets.push(member_offset as u32);
            }
            let Token::Word(member) = self.bump()? else {
                unreachable!()
            };
            self.binding_name(&member, member_offset)?;
            self.member_binding(&member, member_offset, namespace)?;
            let member = Name::new(work, &member)?;
            if seen.insert(work, member.clone(), ())?.is_some() {
                return Err(Error::syntax(
                    work,
                    member_offset,
                    format_args!("duplicate enum member {}", source_text(&member)),
                ));
            }
            members.push(work, member)?;
        }
        if members.is_empty() {
            return Err(Error::syntax(
                work,
                offset,
                format_args!(
                    "enum {} must define at least one member",
                    source_text(&name)
                ),
            ));
        }
        self.expect_word("end")?;
        Ok(Declared::Enum(name, members, member_offsets))
    }
}

/// Locates Go's error for a function or alias whose name is taken, at the
/// name, with its code: V0209 for a name the program defines twice, or
/// V0210 for a reserved one.
fn duplicate_name(
    error: crate::Error,
    name: &Name,
    at: usize,
    work: &dyn crate::compilation::Work,
) -> crate::Error {
    if error.kind != crate::ErrorKind::Syntax {
        return error;
    }
    let span = crate::diagnostic::Span::new(at, at + name.len());
    let code = if error.message.contains("reserved") {
        crate::diagnostic::Code::RESERVED_NAME
    } else {
        crate::diagnostic::Code::DUPLICATE_NAME
    };
    let diagnostic = crate::diagnostic::Diagnostic::error(code, span, &error.message);
    crate::Error::syntax(work, at, &error.message).with_diagnostic(diagnostic)
}

/// Reports Go's first compile error for the top-level declarations.
fn compile_checks(
    order: &[Order],
    modules: &Buffer<Module>,
    enums: &Buffer<(Name, Buffer<Name>)>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    let mut registry = super::modules::Registry::default();
    if !modules.is_empty() {
        let mut functions = Table::new();
        for item in order {
            work.charge(1)?;
            match item {
                Order::Function(name, _) | Order::Alias(name, ..) => {
                    functions.insert(work, name.clone(), ())?;
                }
                _ => (),
            }
        }
        for module in modules {
            super::modules::directive_collisions(module, &functions, work)?;
        }
    }
    for item in order {
        work.charge(1)?;
        match item {
            Order::Function(name, at) => {
                registry
                    .function(name, work)
                    .map_err(|error| duplicate_name(error, name, *at, work))?;
            }
            Order::Module(index) => registry.module(&modules[*index], work)?,
            Order::Enum(index) => {
                let (name, members) = &enums[*index];
                registry.enumeration(name, members, work)?;
            }
            Order::Alias(name, target, found, at) => {
                registry
                    .function(name, work)
                    .map_err(|error| duplicate_name(error, name, *at, work))?;
                if !found {
                    return Err(super::unsupported(
                        work,
                        &format!("alias target function {target} is not defined"),
                    ));
                }
            }
        }
    }
    Ok(())
}
