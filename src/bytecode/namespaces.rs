use super::{Compiler, Op, Program, syntax};
use crate::{Result, Value, namespace, syntax::modules::Module, value::Kind};

impl Program {
    pub(super) fn register_module(
        &mut self,
        module: Module,
        qualifier: &str,
        functions: &mut Vec<syntax::Definition>,
        contexts: &mut Vec<(Option<usize>, bool, bool)>,
    ) -> Result<usize> {
        let name = if qualifier.is_empty() {
            module.name
        } else {
            format!("{qualifier}::{}", module.name)
        };
        if self.declaration_names.contains_key(&name) || self.names.contains_key(&name) {
            return Err(syntax::unsupported(
                "duplicate module or top-level declaration",
            ));
        }
        if module.directives.iter().any(|directive| {
            matches!(directive.as_str(), "public" | "protected")
                && self.names.contains_key(directive)
        }) {
            return Err(syntax::unsupported(
                "module visibility directive conflicts with a top-level function",
            ));
        }
        let mut nested = Vec::new();
        for child in module.modules {
            let short = child.name.clone();
            let index = self.register_module(child, &name, functions, contexts)?;
            nested.push((short, index));
        }
        let index = self.namespaces.len();
        let mut methods = Vec::<namespace::Method>::new();
        for (mut method, visibility) in module.methods {
            let short = method.name.clone();
            method.name = format!("{name}.{}", method.name);
            let function = functions.len();
            functions.push(method);
            contexts.push((Some(index), false, false));
            if let Some(previous) = methods.iter_mut().find(|m| m.name == short) {
                previous.function = function;
                previous.visibility = visibility;
            } else {
                methods.push(namespace::Method {
                    name: short,
                    function,
                    visibility,
                });
            }
        }
        let mut instance_methods = Vec::<namespace::Method>::new();
        for (mut method, visibility) in module.instance_methods {
            let short = method.name.clone();
            method.name = format!("{name}#{}", method.name);
            let function = functions.len();
            functions.push(method);
            contexts.push((Some(index), false, true));
            if let Some(previous) = instance_methods.iter_mut().find(|m| m.name == short) {
                previous.function = function;
                previous.visibility = visibility;
            } else {
                instance_methods.push(namespace::Method {
                    name: short,
                    function,
                    visibility,
                });
            }
        }
        let body = if module.body.is_empty() {
            None
        } else {
            let function = functions.len();
            functions.push(syntax::Definition {
                private: true,
                offset: module.offset,
                accessor: None,
                name: format!("{name}::<body>"),
                params: Vec::new(),
                body: module.body,
                return_type: None,
            });
            contexts.push((Some(index), true, false));
            Some(function)
        };
        let constructor = if module.is_class {
            if let Some(method) = instance_methods.iter().find(|m| m.name == "initialize") {
                Some((method.function, true))
            } else {
                let function = functions.len();
                functions.push(syntax::Definition {
                    private: true,
                    offset: module.offset,
                    accessor: None,
                    name: format!("{name}#<initialize>"),
                    params: Vec::new(),
                    body: Vec::new(),
                    return_type: None,
                });
                contexts.push((Some(index), false, true));
                Some((function, false))
            }
        } else {
            None
        };
        let definition = namespace::Definition::new(
            index,
            name.clone(),
            methods,
            instance_methods,
            constructor,
            nested,
            body,
        );
        self.namespaces.push(definition.clone());
        self.declaration_names.insert(name, self.declarations.len());
        self.declarations
            .push(Value(Kind::Namespace(namespace::Namespace::untracked(
                definition,
            ))));
        Ok(index)
    }
}

impl Compiler<'_> {
    pub(super) fn assignment_address(&mut self, receiver: &syntax::Expr) -> Result<()> {
        match &receiver.node {
            syntax::Node::Var(name)
                if self.namespace.is_some()
                    && (!self.instance || name.starts_with('@'))
                    && self.namespace_binding(name) =>
            {
                let optional = name.starts_with('@');
                let name = self.call_site(name, false).name;
                self.emit(Op::NamespaceAddress(name, optional));
            }
            syntax::Node::Index(root, indices) => {
                self.assignment_address(root)?;
                for index in indices {
                    self.expr(index)?;
                }
                self.emit(Op::AddressIndex(indices.len()));
            }
            syntax::Node::Member(root, name) => {
                self.assignment_address(root)?;
                let site = self.call_site(name, true);
                self.emit(Op::AddressMember(site));
            }
            syntax::Node::Scope(root, name, None) => {
                self.assignment_address(root)?;
                let mut site = self.call_site(name, true);
                site.scope = true;
                self.emit(Op::AddressNamespaceField(site));
            }
            _ => self.address(receiver)?,
        }
        Ok(())
    }

    pub(super) fn namespace_binding(&self, name: &str) -> bool {
        if name.starts_with('@') {
            return true;
        }
        if self.parameters.contains(name) || self.outer.iter().any(|scope| scope.contains_key(name))
        {
            return false;
        }
        if self.namespace.is_some()
            && !self.instance
            && name.chars().next().is_some_and(syntax::unicode::upper)
        {
            return true;
        }
        self.program
            .declaration_names
            .get(name)
            .is_some_and(|&index| matches!(self.program.declarations[index].0, Kind::Namespace(_)))
    }

    pub(super) fn store_namespace_name(&mut self, name: &str) {
        if name.starts_with('@') || (self.namespace.is_some() && !self.instance) {
            let name = self.call_site(name, false).name;
            self.emit(Op::NamespaceStore(name));
        } else {
            self.emit(Op::StoreDeclaration(self.program.declaration_names[name]));
        }
    }

    pub(super) fn namespace_assignment(
        &mut self,
        name: &str,
        binding: &syntax::Target,
        target: &syntax::Expr,
        op: &str,
        rhs: &syntax::Expr,
    ) -> Result<()> {
        if matches!(op, "||=" | "&&=") {
            if name.starts_with('@') || (self.namespace.is_some() && !self.instance) {
                let name = self.call_site(name, false).name;
                self.emit(Op::NamespaceVariable(name, true));
            } else {
                self.expr(target)?;
            }
            self.emit(Op::Dup);
            let skip = self.emit(if op == "||=" {
                Op::JumpTrue(0)
            } else {
                Op::JumpFalse(0)
            });
            self.emit(Op::Pop);
            self.assignment_rhs(binding, &[rhs])?;
            self.store_namespace_name(name);
            self.patch(skip, self.code.len());
            return Ok(());
        }
        let binary = match op {
            "+=" => Some("+"),
            "-=" => Some("-"),
            "*=" => Some("*"),
            "/=" => Some("/"),
            "%=" => Some("%"),
            "**=" => Some("**"),
            _ => None,
        };
        if binary.is_some() {
            self.expr(target)?;
        }
        self.assignment_rhs(binding, &[rhs])?;
        if let Some(op) = binary {
            self.emit(Op::Binary(op));
        }
        self.store_namespace_name(name);
        Ok(())
    }
}
