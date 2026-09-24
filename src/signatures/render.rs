//! Prints signature tables in their canonical text form.

use super::{
    Block, Class, Constant, Field, Function, Item, Member, Param, ParamKind, Table, Type, TypeParam,
};
use std::fmt::{self, Display, Formatter, Write};

impl Display for Table {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        comments(f, "", &self.header)?;
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 || !self.header.is_empty() {
                f.write_char('\n')?;
            }
            match item {
                Item::Function(function) => {
                    comments(f, "", &function.doc)?;
                    writeln!(f, "{function}")?;
                }
                Item::Constant(constant) => {
                    comments(f, "", &constant.doc)?;
                    writeln!(f, "{}: {}", constant.name, constant.ty)?;
                }
                Item::Alias(alias) => {
                    comments(f, "", &alias.doc)?;
                    writeln!(f, "type {} = {}", alias.name, alias.ty)?;
                }
                Item::Module(module) => {
                    comments(f, "", &module.doc)?;
                    writeln!(f, "module {}", module.name)?;
                    members(f, &module.members)?;
                    writeln!(f, "end")?;
                }
                Item::Class(class) => {
                    comments(f, "", &class.doc)?;
                    write!(f, "class ")?;
                    pattern(f, class, &class.receiver, &mut Vec::new())?;
                    f.write_char('\n')?;
                    members(f, &class.members)?;
                    writeln!(f, "end")?;
                }
            }
        }
        Ok(())
    }
}

fn comments(f: &mut Formatter<'_>, indent: &str, lines: &[String]) -> fmt::Result {
    for line in lines {
        if line.is_empty() {
            writeln!(f, "{indent}#")?;
        } else {
            writeln!(f, "{indent}# {line}")?;
        }
    }
    Ok(())
}

fn members(f: &mut Formatter<'_>, members: &[Member]) -> fmt::Result {
    for member in members {
        let doc = match member {
            Member::Function(function) => &function.doc,
            Member::Getter(value) | Member::Constant(value) => &value.doc,
        };
        comments(f, "  ", doc)?;
        match member {
            Member::Function(function) => writeln!(f, "  {function}")?,
            Member::Getter(Constant { name, ty, .. }) => writeln!(f, "  getter {name}: {ty}")?,
            Member::Constant(Constant { name, ty, .. }) => writeln!(f, "  {name}: {ty}")?,
        }
    }
    Ok(())
}

/// Prints a receiver pattern, attaching each variable's bound where it first
/// appears.
fn pattern(f: &mut Formatter<'_>, class: &Class, ty: &Type, seen: &mut Vec<String>) -> fmt::Result {
    match ty {
        Type::Var(name) => {
            f.write_str(name)?;
            if !seen.contains(name) {
                seen.push(name.clone());
                let bound = class.vars.iter().find(|var| var.name == *name);
                if let Some(TypeParam {
                    bound: Some(bound), ..
                }) = bound
                {
                    write!(f, ": {bound}")?;
                }
            }
            Ok(())
        }
        Type::Name(name, args) if !args.is_empty() => {
            write!(f, "{name}<")?;
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                pattern(f, class, arg, seen)?;
            }
            f.write_char('>')
        }
        Type::Optional(inner) => {
            pattern(f, class, inner, seen)?;
            f.write_char('?')
        }
        ty => write!(f, "{ty}"),
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "def {}", self.name)?;
        if !self.type_params.is_empty() {
            f.write_char('<')?;
            for (index, param) in self.type_params.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                f.write_str(&param.name)?;
                if let Some(bound) = &param.bound {
                    write!(f, ": {bound}")?;
                }
            }
            f.write_char('>')?;
        }
        if !self.params.is_empty() || self.block.is_some() {
            f.write_char('(')?;
            for (index, param) in self.params.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{param}")?;
            }
            if let Some(block) = &self.block {
                if !self.params.is_empty() {
                    f.write_str(", ")?;
                }
                write!(f, "{block}")?;
            }
            f.write_char(')')?;
        }
        if let Some(result) = &self.result {
            write!(f, " -> {result}")?;
        }
        Ok(())
    }
}

impl Display for Param {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let marker = if self.optional && self.default.is_none() {
            "?"
        } else {
            ""
        };
        match self.kind {
            ParamKind::Positional => write!(f, "{}{marker}: {}", self.name, self.ty)?,
            ParamKind::Rest => write!(f, "*{}: {}", self.name, self.ty)?,
            ParamKind::Keyword => write!(f, "{}{marker}: {}:", self.name, self.ty)?,
            ParamKind::KeywordRest => write!(f, "**{}: {}", self.name, self.ty)?,
        }
        if let Some(default) = &self.default {
            write!(f, " = {default}")?;
        }
        Ok(())
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let marker = if self.optional { "?" } else { "" };
        write!(f, "&{}{marker}: ", self.name)?;
        let single = match (self.params.as_slice(), &self.rest) {
            ([param], None) => {
                let text = param.to_string();
                (!text.starts_with('(')).then_some(text)
            }
            _ => None,
        };
        match single {
            Some(text) => f.write_str(&text)?,
            None => {
                f.write_char('(')?;
                for (index, param) in self.params.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{param}")?;
                }
                if let Some(rest) = &self.rest {
                    if !self.params.is_empty() {
                        f.write_str(", ")?;
                    }
                    write!(f, "*{rest}")?;
                }
                f.write_char(')')?;
            }
        }
        if let Some(result) = &self.result {
            write!(f, " -> {result}")?;
        }
        Ok(())
    }
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name, args) => {
                f.write_str(name)?;
                if !args.is_empty() {
                    f.write_char('<')?;
                    for (index, arg) in args.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    f.write_char('>')?;
                }
                Ok(())
            }
            Self::Var(name) => f.write_str(name),
            Self::Optional(inner) => match **inner {
                Self::Union(_) => write!(f, "({inner})?"),
                _ => write!(f, "{inner}?"),
            },
            Self::Union(arms) => {
                for (index, arm) in arms.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" | ")?;
                    }
                    write!(f, "{arm}")?;
                }
                Ok(())
            }
            Self::Shape(fields, open) => {
                if fields.is_empty() {
                    return f.write_str(if *open { "{ ... }" } else { "{}" });
                }
                f.write_str("{ ")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{field}")?;
                }
                if *open {
                    f.write_str(", ...")?;
                }
                f.write_str(" }")
            }
            Self::Symbol(name) => write!(f, ":{name}"),
        }
    }
}

impl Display for Field {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let plain = self
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if plain {
            f.write_str(&self.name)?;
        } else {
            f.write_char('"')?;
            for c in self.name.chars() {
                if matches!(c, '"' | '\\') {
                    f.write_char('\\')?;
                }
                f.write_char(c)?;
            }
            f.write_char('"')?;
        }
        let marker = if self.optional { "?" } else { "" };
        write!(f, "{marker}: {}", self.ty)
    }
}
