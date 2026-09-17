use super::*;
use crate::Value;
use std::ops::Deref;

#[derive(Clone, Copy)]
pub(in crate::checking::flow) struct MemberSite {
    pub call: CallSite,
    pub selected: Option<Fact>,
}

impl From<CallSite> for MemberSite {
    fn from(call: CallSite) -> Self {
        Self {
            call,
            selected: None,
        }
    }
}

impl Deref for MemberSite {
    type Target = CallSite;

    fn deref(&self) -> &Self::Target {
        &self.call
    }
}

pub(in crate::checking::flow) enum Name<'a> {
    Compiled(&'a str),
    Selected(Value),
}

impl Name<'_> {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Compiled(name) => name.as_bytes(),
            Self::Selected(name) => name.as_bytes().unwrap(),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Compiled(name) => name,
            Self::Selected(name) => std::str::from_utf8(name.as_bytes().unwrap()).unwrap(),
        }
    }
}

impl MemberSite {
    pub fn text<'a>(self, program: &'a Program, facts: &Facts) -> Name<'a> {
        match self.selected {
            None => Name::Compiled(&program.members[self.call.name]),
            Some(value) => match facts.node(value) {
                super::super::super::facts::Node::String(value)
                | super::super::super::facts::Node::Symbol(value) => Name::Selected(value.clone()),
                _ => unreachable!("selected member names are literal strings or symbols"),
            },
        }
    }
}
