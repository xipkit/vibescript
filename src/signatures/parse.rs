//! A parser for signature files: the declaration subset of Vibescript the
//! builtin table is written in, plus type parameters and bounds.

use super::{
    Alias, Block, Class, Constant, Field, Function, Item, Member, Module, Param, ParamKind, Table,
    Type, TypeParam,
};
use std::{collections::BTreeMap, fmt};

/// A syntax error in a signature file, at a one-based line and column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    /// An identifier, which may end in `?` or `!`.
    Word(String),
    /// A symbol literal without its colon.
    Symbol(String),
    /// A number, string, `nil`, `true` or `false` literal, as written.
    Literal(String),
    Punct(&'static str),
    Comment(String),
    Newline,
    End,
}

struct Lexed {
    token: Token,
    line: usize,
    column: usize,
}

const PUNCTUATION: [&str; 19] = [
    "...", "->", "**", "(", ")", "[", "]", "<", ">", "{", "}", ",", ":", "|", "?", "=", "*", "&",
    ".",
];

fn lex(source: &str) -> Result<Vec<Lexed>> {
    let mut tokens = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let mut rest = line;
        let column = |rest: &str| line.len() - rest.len() + 1;
        loop {
            rest = rest.trim_start_matches([' ', '\t']);
            let (token, remaining) = match rest.chars().next() {
                None => break,
                Some('#') => {
                    let text = rest[1..].strip_prefix(' ').unwrap_or(&rest[1..]);
                    (Token::Comment(text.trim_end().to_owned()), "")
                }
                Some('"') => {
                    let end = string_end(rest).ok_or_else(|| ParseError {
                        line: index + 1,
                        column: column(rest),
                        message: "unterminated string".to_owned(),
                    })?;
                    (Token::Literal(rest[..end].to_owned()), &rest[end..])
                }
                Some(':')
                    if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') =>
                {
                    let end = 1 + word_end(&rest[1..]);
                    (Token::Symbol(rest[1..end].to_owned()), &rest[end..])
                }
                Some(c)
                    if c.is_ascii_digit()
                        || c == '-' && rest[1..].starts_with(|c: char| c.is_ascii_digit()) =>
                {
                    let end = 1 + rest[1..]
                        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
                        .unwrap_or(rest.len() - 1);
                    (Token::Literal(rest[..end].to_owned()), &rest[end..])
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let end = word_end(rest);
                    let word = &rest[..end];
                    let token = if matches!(word, "nil" | "true" | "false") {
                        Token::Literal(word.to_owned())
                    } else {
                        Token::Word(word.to_owned())
                    };
                    (token, &rest[end..])
                }
                Some(_) => {
                    let Some(punct) = PUNCTUATION.iter().find(|p| rest.starts_with(**p)) else {
                        return Err(ParseError {
                            line: index + 1,
                            column: column(rest),
                            message: format!(
                                "unexpected character {:?}",
                                rest.chars().next().unwrap()
                            ),
                        });
                    };
                    (Token::Punct(punct), &rest[punct.len()..])
                }
            };
            tokens.push(Lexed {
                token,
                line: index + 1,
                column: column(rest),
            });
            rest = remaining;
        }
        tokens.push(Lexed {
            token: Token::Newline,
            line: index + 1,
            column: line.len() + 1,
        });
    }
    let line = source.lines().count() + 1;
    tokens.push(Lexed {
        token: Token::End,
        line,
        column: 1,
    });
    Ok(tokens)
}

/// The length of an identifier at the start of `text`, including a directly
/// attached `?` or `!`.
fn word_end(text: &str) -> usize {
    let end = text
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(text.len());
    if text[end..].starts_with(['?', '!']) {
        end + 1
    } else {
        end
    }
}

fn string_end(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, c) in text.char_indices().skip(1) {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(index + 1),
            _ => escaped = false,
        }
    }
    None
}

struct Parser {
    tokens: Vec<Lexed>,
    pos: usize,
    /// Type variables in scope, innermost last.
    vars: Vec<String>,
    /// The call shapes of each function name declared so far, keyed by its
    /// scope: global functions, a module, or every class on one base type.
    shapes: BTreeMap<(String, String), Vec<Shape>>,
}

pub(super) fn table(source: &str) -> Result<Table> {
    let mut parser = Parser {
        tokens: lex(source)?,
        pos: 0,
        vars: Vec::new(),
        shapes: BTreeMap::new(),
    };
    let header = parser.header();
    let mut items = Vec::new();
    loop {
        let doc = parser.doc()?;
        match parser.peek().clone() {
            Token::End => {
                if !doc.is_empty() {
                    return parser.fail("a comment must precede a declaration");
                }
                break;
            }
            Token::Word(word) if word == "def" => {
                let start = parser.pos;
                let function = parser.function(doc)?;
                parser.overload("", &function, start)?;
                items.push(Item::Function(function));
            }
            Token::Word(word) if word == "module" => items.push(Item::Module(parser.module(doc)?)),
            Token::Word(word) if word == "class" => items.push(Item::Class(parser.class(doc)?)),
            Token::Word(word) if word == "type" => items.push(Item::Alias(parser.alias(doc)?)),
            Token::Word(_) => items.push(Item::Constant(parser.constant(doc)?)),
            _ => return parser.fail("expected a declaration"),
        }
        parser.line_end()?;
    }
    Ok(Table { header, items })
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.pos].token.clone();
        if token != Token::End {
            self.pos += 1;
        }
        token
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T> {
        let token = &self.tokens[self.pos];
        Err(ParseError {
            line: token.line,
            column: token.column,
            message: message.into(),
        })
    }

    fn at(&self, punct: &str) -> bool {
        matches!(self.peek(), Token::Punct(p) if *p == punct)
    }

    fn eat(&mut self, punct: &str) -> bool {
        let found = self.at(punct);
        if found {
            self.pos += 1;
        }
        found
    }

    fn expect(&mut self, punct: &str) -> Result<()> {
        if self.eat(punct) {
            Ok(())
        } else {
            self.fail(format!("expected `{punct}`"))
        }
    }

    fn keyword(&mut self, word: &str) -> Result<()> {
        match self.next() {
            Token::Word(found) if found == word => Ok(()),
            _ => {
                self.pos -= 1;
                self.fail(format!("expected `{word}`"))
            }
        }
    }

    fn word(&mut self, what: &str) -> Result<String> {
        match self.peek() {
            Token::Word(word) => {
                let word = word.clone();
                self.pos += 1;
                Ok(word)
            }
            _ => self.fail(format!("expected {what}")),
        }
    }

    /// Requires the end of a declaration's line; comments go on their own line.
    fn line_end(&mut self) -> Result<()> {
        if matches!(self.peek(), Token::Comment(_)) {
            return self.fail("comments go on their own line");
        }
        match self.next() {
            Token::Newline | Token::End => Ok(()),
            _ => {
                self.pos -= 1;
                self.fail("expected the end of the line")
            }
        }
    }

    fn blank_lines(&mut self) -> usize {
        let mut count = 0;
        while *self.peek() == Token::Newline {
            self.pos += 1;
            count += 1;
        }
        count
    }

    /// The leading comment block, ended by a blank line.
    fn header(&mut self) -> Vec<String> {
        self.blank_lines();
        let start = self.pos;
        let lines = self.comments();
        if lines.is_empty() || self.blank_lines() == 0 && *self.peek() != Token::End {
            self.pos = start;
            return Vec::new();
        }
        lines
    }

    fn comments(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Token::Comment(text) = self.peek() {
            lines.push(text.clone());
            self.pos += 1;
            if *self.peek() == Token::Newline {
                self.pos += 1;
            }
        }
        lines
    }

    /// Comment lines directly above a declaration, after any blank lines.
    fn doc(&mut self) -> Result<Vec<String>> {
        self.blank_lines();
        let start = self.pos;
        let lines = self.comments();
        if !lines.is_empty() && *self.peek() == Token::Newline {
            self.pos = start;
            return self.fail("a comment must directly precede a declaration");
        }
        Ok(lines)
    }

    fn module(&mut self, doc: Vec<String>) -> Result<Module> {
        self.keyword("module")?;
        let name = self.word("a module name")?;
        self.line_end()?;
        let members = self.members(&format!("module {name}"), false)?;
        Ok(Module { doc, name, members })
    }

    fn class(&mut self, doc: Vec<String>) -> Result<Class> {
        self.keyword("class")?;
        let mut vars = Vec::new();
        let receiver = self.pattern(&mut vars)?;
        self.vars = vars
            .iter()
            .map(|var: &TypeParam| var.name.clone())
            .collect();
        self.line_end()?;
        let scope = match &receiver {
            Type::Name(name, _) | Type::Var(name) => format!("class {name}"),
            _ => String::new(),
        };
        let members = self.members(&scope, true)?;
        self.vars.clear();
        Ok(Class {
            doc,
            receiver,
            vars,
            members,
        })
    }

    /// A class's receiver pattern: a type whose capitalized names introduce
    /// type variables, each optionally bounded with `: B`.
    fn pattern(&mut self, vars: &mut Vec<TypeParam>) -> Result<Type> {
        if self.eat("[") {
            let mut elements = Vec::new();
            loop {
                elements.push(self.pattern(vars)?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect("]")?;
            return Ok(Type::Tuple(elements));
        }
        let word = match self.peek().clone() {
            Token::Word(word) => word,
            Token::Literal(literal) if literal == "nil" => literal,
            _ => return self.fail("expected a receiver type"),
        };
        self.pos += 1;
        let (name, nullable) = match word.strip_suffix('?') {
            Some(name) => (name.to_owned(), true),
            None => (word, false),
        };
        let ty = if variable(&name) {
            if vars.iter().any(|var| var.name == name) {
                return self.fail(format!("type variable {name} is already declared"));
            }
            let bound = if !nullable && self.eat(":") {
                Some(self.ty()?)
            } else {
                None
            };
            vars.push(TypeParam {
                name: name.clone(),
                bound,
            });
            Type::Var(name)
        } else {
            let mut args = Vec::new();
            if !nullable && self.eat("<") {
                loop {
                    args.push(self.pattern(vars)?);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(">")?;
            }
            Type::Name(name, args)
        };
        Ok(if nullable { optional(ty) } else { ty })
    }

    fn members(&mut self, scope: &str, class: bool) -> Result<Vec<Member>> {
        let mut members: Vec<Member> = Vec::new();
        loop {
            let doc = self.doc()?;
            let start = self.pos;
            let member = match self.peek().clone() {
                Token::Word(word) if word == "end" && doc.is_empty() => {
                    self.pos += 1;
                    return Ok(members);
                }
                Token::Word(word) if word == "def" => {
                    let function = self.function(doc)?;
                    self.overload(scope, &function, start)?;
                    Member::Function(function)
                }
                Token::Word(word) if word == "getter" && class => {
                    self.pos += 1;
                    Member::Getter(self.constant(doc)?)
                }
                Token::Word(_) if !class => Member::Constant(self.constant(doc)?),
                _ => return self.fail("expected a member declaration or `end`"),
            };
            let overload = |other: &Member| {
                matches!(other, Member::Function(_)) && matches!(member, Member::Function(_))
            };
            if members
                .iter()
                .any(|other| other.name() == member.name() && !overload(other))
            {
                self.pos = start;
                return self.fail(format!("duplicate member {}", member.name()));
            }
            members.push(member);
            self.line_end()?;
        }
    }

    /// Records a function's call shape, refusing an overload that could
    /// accept the same call as an earlier one.
    fn overload(&mut self, scope: &str, function: &Function, start: usize) -> Result<()> {
        let shape = Shape::of(function);
        let key = (scope.to_owned(), function.name.clone());
        let shapes = self.shapes.entry(key).or_default();
        if shapes.iter().any(|other| other.overlaps(&shape)) {
            self.pos = start;
            return self.fail(format!(
                "overloads of {} could accept the same call",
                function.name
            ));
        }
        shapes.push(shape);
        Ok(())
    }

    fn constant(&mut self, doc: Vec<String>) -> Result<Constant> {
        let name = self.word("a name")?;
        self.expect(":")?;
        let ty = self.ty()?;
        Ok(Constant { doc, name, ty })
    }

    fn alias(&mut self, doc: Vec<String>) -> Result<Alias> {
        self.keyword("type")?;
        let name = self.word("a type name")?;
        self.expect("=")?;
        let ty = self.ty()?;
        Ok(Alias { doc, name, ty })
    }

    fn function(&mut self, doc: Vec<String>) -> Result<Function> {
        self.keyword("def")?;
        let name = self.word("a function name")?;
        let scope = self.vars.len();
        let mut type_params = Vec::new();
        if self.eat("<") {
            loop {
                let name = self.word("a type variable")?;
                if !variable(&name) {
                    return self.fail("a type variable is a single capital letter");
                }
                if self.vars.contains(&name) {
                    return self.fail(format!("type variable {name} is already declared"));
                }
                let bound = if self.eat(":") {
                    Some(self.ty()?)
                } else {
                    None
                };
                self.vars.push(name.clone());
                type_params.push(TypeParam { name, bound });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(">")?;
        }
        let mut function = Function {
            doc,
            name,
            type_params,
            ..Function::default()
        };
        if self.eat("(") && !self.eat(")") {
            loop {
                if self.at("&") {
                    function.block = Some(self.block()?);
                    self.expect(")")?;
                    break;
                }
                let param = self.param()?;
                self.check_order(&function.params, &param)?;
                function.params.push(param);
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        if self.eat("->") {
            function.result = Some(self.ty()?);
        }
        self.vars.truncate(scope);
        Ok(function)
    }

    fn check_order(&self, params: &[Param], param: &Param) -> Result<()> {
        if params.iter().any(|other| other.name == param.name) {
            return self.fail(format!("duplicate parameter {}", param.name));
        }
        let rank = |param: &Param| match param.kind {
            ParamKind::Positional => 0,
            ParamKind::Rest => 1,
            ParamKind::Keyword => 2,
            ParamKind::KeywordRest => 3,
        };
        match params.last() {
            Some(last)
                if rank(last) > rank(param)
                    || rank(last) == rank(param) && rank(param) % 2 == 1 =>
            {
                self.fail(format!("parameter {} is out of order", param.name))
            }
            Some(last)
                if param.kind == ParamKind::Positional && last.optional && !param.optional =>
            {
                self.fail(format!(
                    "required parameter {} follows an optional one",
                    param.name
                ))
            }
            _ => Ok(()),
        }
    }

    fn param(&mut self) -> Result<Param> {
        let kind = if self.eat("**") {
            ParamKind::KeywordRest
        } else if self.eat("*") {
            ParamKind::Rest
        } else {
            ParamKind::Positional
        };
        let mut name = self.word("a parameter name")?;
        let mut optional = false;
        if let Some(stripped) = name.strip_suffix('?') {
            name = stripped.to_owned();
            optional = true;
        } else if self.eat("?") {
            optional = true;
        }
        if optional && kind != ParamKind::Positional {
            return self.fail("only positional and keyword parameters can be optional");
        }
        self.expect(":")?;
        let ty = self.ty()?;
        let kind = if kind == ParamKind::Positional && self.eat(":") {
            ParamKind::Keyword
        } else {
            kind
        };
        let default = if self.eat("=") {
            if matches!(kind, ParamKind::Rest | ParamKind::KeywordRest) {
                return self.fail("rest parameters have no default");
            }
            if optional {
                return self.fail("a parameter with a default is written `name: T = value`");
            }
            match self.next() {
                Token::Literal(literal) => Some(literal),
                Token::Symbol(symbol) => Some(format!(":{symbol}")),
                _ => {
                    self.pos -= 1;
                    return self.fail("expected a literal default");
                }
            }
        } else {
            None
        };
        Ok(Param {
            name,
            kind,
            ty,
            optional: optional || default.is_some(),
            default,
        })
    }

    fn block(&mut self) -> Result<Block> {
        self.expect("&")?;
        let mut name = self.word("a block name")?;
        let optional = match name.strip_suffix('?') {
            Some(stripped) => {
                name = stripped.to_owned();
                true
            }
            None => self.eat("?"),
        };
        self.expect(":")?;
        let mut block = Block {
            name,
            optional,
            params: Vec::new(),
            rest: None,
            result: None,
        };
        if self.eat("(") {
            if !self.eat(")") {
                loop {
                    if self.eat("*") {
                        block.rest = Some(self.ty()?);
                        self.expect(")")?;
                        break;
                    }
                    block.params.push(self.ty()?);
                    if self.eat(")") {
                        break;
                    }
                    self.expect(",")?;
                }
            }
        } else {
            block.params.push(self.ty()?);
        }
        if self.eat("->") {
            block.result = Some(self.ty()?);
        }
        Ok(block)
    }

    fn ty(&mut self) -> Result<Type> {
        let mut arms = vec![self.postfix()?];
        while self.eat("|") {
            arms.push(self.postfix()?);
        }
        Ok(union(arms))
    }

    fn postfix(&mut self) -> Result<Type> {
        let mut ty = self.primary()?;
        while self.eat("?") {
            ty = optional(ty);
        }
        Ok(ty)
    }

    fn primary(&mut self) -> Result<Type> {
        match self.next() {
            Token::Punct("(") => {
                let ty = self.ty()?;
                self.expect(")")?;
                Ok(ty)
            }
            Token::Punct("{") => self.shape(),
            Token::Punct("[") => {
                let mut elements = Vec::new();
                loop {
                    elements.push(self.ty()?);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect("]")?;
                Ok(Type::Tuple(elements))
            }
            Token::Symbol(symbol) => Ok(Type::Symbol(symbol)),
            Token::Literal(literal) if literal == "nil" => Ok(Type::name("nil")),
            Token::Word(word) if self.vars.contains(&word) => Ok(Type::Var(word)),
            Token::Word(word) => {
                let mut name = word;
                let mut ty_optional = false;
                if let Some(stripped) = name.strip_suffix('?') {
                    name = stripped.to_owned();
                    ty_optional = true;
                }
                if self.vars.contains(&name) {
                    return Ok(optional(Type::Var(name)));
                }
                let mut args = Vec::new();
                if !ty_optional && self.eat("<") {
                    loop {
                        args.push(self.ty()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(">")?;
                }
                let ty = Type::Name(name, args);
                Ok(if ty_optional { optional(ty) } else { ty })
            }
            _ => {
                self.pos -= 1;
                self.fail("expected a type")
            }
        }
    }

    fn shape(&mut self) -> Result<Type> {
        let mut fields: Vec<Field> = Vec::new();
        let mut open = false;
        if self.eat("}") {
            return Ok(Type::Shape(fields, false));
        }
        loop {
            if self.eat("...") {
                open = true;
                self.expect("}")?;
                break;
            }
            let mut name = match self.next() {
                Token::Word(word) => word,
                Token::Literal(literal) if literal.starts_with('"') => {
                    unescape(&literal[1..literal.len() - 1])
                }
                _ => {
                    self.pos -= 1;
                    return self.fail("expected a field name");
                }
            };
            let mut optional = false;
            if let Some(stripped) = name.strip_suffix('?') {
                name = stripped.to_owned();
                optional = true;
            } else if self.eat("?") {
                optional = true;
            }
            self.expect(":")?;
            let ty = self.ty()?;
            if fields.iter().any(|field| field.name == name) {
                return self.fail(format!("duplicate field {name}"));
            }
            fields.push(Field { name, ty, optional });
            if self.eat("}") {
                break;
            }
            self.expect(",")?;
        }
        Ok(Type::Shape(fields, open))
    }
}

/// Whether a name introduces or refers to a type variable: an uppercase ASCII
/// letter followed by nothing else.
fn variable(name: &str) -> bool {
    name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase()
}

fn optional(ty: Type) -> Type {
    match ty {
        Type::Optional(_) => ty,
        ty => Type::Optional(Box::new(ty)),
    }
}

fn union(arms: Vec<Type>) -> Type {
    let mut flat = Vec::new();
    for arm in arms {
        match arm {
            Type::Union(inner) => flat.extend(inner),
            arm => flat.push(arm),
        }
    }
    if flat.len() == 1 {
        flat.pop().unwrap()
    } else {
        Type::Union(flat)
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The calls a signature accepts, by the properties overload selection
/// reads: the positional argument count, the keyword names, and whether a
/// block is given and how many parameters it declares.
struct Shape {
    positional: (usize, Option<usize>),
    required: Vec<String>,
    /// The keywords a call may pass, or `None` for any.
    keywords: Option<Vec<String>>,
    /// The block's parameter counts, or `None` without a block.
    block: Option<(usize, Option<usize>)>,
    block_required: bool,
}

impl Shape {
    fn of(function: &Function) -> Self {
        let mut shape = Self {
            positional: (0, Some(0)),
            required: Vec::new(),
            keywords: Some(Vec::new()),
            block: function.block.as_ref().map(|block| {
                let count = block.params.len();
                (count, block.rest.is_none().then_some(count))
            }),
            block_required: function.block.as_ref().is_some_and(|block| !block.optional),
        };
        for param in &function.params {
            match param.kind {
                ParamKind::Positional => {
                    shape.positional.1 = shape.positional.1.map(|max| max + 1);
                    if !param.optional {
                        shape.positional.0 += 1;
                    }
                }
                ParamKind::Rest => shape.positional.1 = None,
                ParamKind::Keyword => {
                    if !param.optional {
                        shape.required.push(param.name.clone());
                    }
                    if let Some(keywords) = &mut shape.keywords {
                        keywords.push(param.name.clone());
                    }
                }
                ParamKind::KeywordRest => shape.keywords = None,
            }
        }
        shape
    }

    /// Whether some call is accepted by both shapes.
    fn overlaps(&self, other: &Self) -> bool {
        let below = |count: usize, max: Option<usize>| max.is_none_or(|max| count <= max);
        let positional = below(self.positional.0, other.positional.1)
            && below(other.positional.0, self.positional.1);
        let allowed = |shape: &Self, name: &String| {
            shape
                .keywords
                .as_ref()
                .is_none_or(|keywords| keywords.contains(name))
        };
        let keywords = self
            .required
            .iter()
            .chain(&other.required)
            .all(|name| allowed(self, name) && allowed(other, name));
        let without = !self.block_required && !other.block_required;
        let with = match (self.block, other.block) {
            (Some(left), Some(right)) => below(left.0, right.1) && below(right.0, left.1),
            _ => false,
        };
        positional && keywords && (without || with)
    }
}
