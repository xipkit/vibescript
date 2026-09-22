use super::{Call, Compiler, Compiling, Op, Program, syntax};
use crate::{Result, Value, compilation::Name, namespace, syntax::modules::Module, value::Kind};

struct Frame {
    module: Module,
    name: Name,
    short: Name,
    children: <crate::compilation::Buffer<Module> as IntoIterator>::IntoIter,
    nested: Vec<(String, usize)>,
}

impl Program {
    // Nested modules register before their parents. Module nesting reaches the
    // syntax depth limit, so the walk keeps its own stack.
    pub(super) fn register_module(
        &mut self,
        module: Module,
        qualifier: &str,
        functions: &mut crate::compilation::Buffer<syntax::Definition>,
        contexts: &mut crate::compilation::Buffer<(Option<usize>, bool, bool)>,
        work: &dyn crate::compilation::Work,
    ) -> Result<usize> {
        let mut stack = vec![self.open_module(module, qualifier, work)?];
        loop {
            let top = stack.last_mut().unwrap();
            if let Some(child) = top.children.next() {
                let frame = self.open_module(child, top.name.as_str(), work)?;
                stack.push(frame);
                continue;
            }
            let frame = stack.pop().unwrap();
            let short = frame.short.clone();
            let index = self.close_module(frame, functions, contexts, work)?;
            match stack.last_mut() {
                Some(parent) => parent.nested.push((short.into_string(), index)),
                None => return Ok(index),
            }
        }
    }

    fn open_module(
        &self,
        mut module: Module,
        qualifier: &str,
        work: &dyn crate::compilation::Work,
    ) -> Result<Frame> {
        work.bytes(qualifier.len() + module.name.len())?;
        let short = module.name.clone();
        let name = if qualifier.is_empty() {
            module.name.clone()
        } else {
            Name::join(work, &[qualifier, "::", &module.name])?
        };
        if self.declaration_names.contains_key(name.as_str())
            || self.names.contains_key(name.as_str())
        {
            return Err(syntax::unsupported(
                work,
                "duplicate module or top-level declaration",
            ));
        }
        if module.directives.iter(work)?.any(|(directive, _)| {
            matches!(directive.as_str(), "public" | "protected")
                && self.names.contains_key(directive.as_str())
        }) {
            return Err(syntax::unsupported(
                work,
                "module visibility directive conflicts with a top-level function",
            ));
        }
        let children = std::mem::take(&mut module.modules).into_iter();
        Ok(Frame {
            module,
            name,
            short,
            children,
            nested: Vec::new(),
        })
    }

    fn close_module(
        &mut self,
        frame: Frame,
        functions: &mut crate::compilation::Buffer<syntax::Definition>,
        contexts: &mut crate::compilation::Buffer<(Option<usize>, bool, bool)>,
        work: &dyn crate::compilation::Work,
    ) -> Result<usize> {
        let Frame {
            module,
            name,
            nested,
            ..
        } = frame;
        let index = self.namespaces.len();
        let mut methods = Vec::<namespace::Method>::new();
        for (mut method, visibility) in module.methods {
            work.bytes(name.len() + method.name.len())?;
            work.charge(methods.len())?;
            let short = method.name.clone();
            method.name = Name::join(work, &[&name, ".", &method.name])?;
            let function = functions.len();
            functions.push(work, method)?;
            contexts.push(work, (Some(index), false, false))?;
            if let Some(previous) = methods.iter_mut().find(|m| m.name == short.as_str()) {
                previous.function = function;
                previous.visibility = visibility;
            } else {
                methods.push(namespace::Method {
                    name: short.into_string(),
                    function,
                    visibility,
                });
            }
        }
        let mut instance_methods = Vec::<namespace::Method>::new();
        for (mut method, visibility) in module.instance_methods {
            work.bytes(name.len() + method.name.len())?;
            work.charge(instance_methods.len())?;
            let short = method.name.clone();
            method.name = Name::join(work, &[&name, "#", &method.name])?;
            let function = functions.len();
            functions.push(work, method)?;
            contexts.push(work, (Some(index), false, true))?;
            if let Some(previous) = instance_methods
                .iter_mut()
                .find(|m| m.name == short.as_str())
            {
                previous.function = function;
                previous.visibility = visibility;
            } else {
                instance_methods.push(namespace::Method {
                    name: short.into_string(),
                    function,
                    visibility,
                });
            }
        }
        let body = if module.body.is_empty() {
            None
        } else {
            let function = functions.len();
            functions.push(
                work,
                syntax::Definition {
                    private: true,
                    offset: module.offset,
                    accessor: None,
                    name: Name::join(work, &[&name, "::<body>"])?,
                    params: crate::compilation::Buffer::new(),
                    body: module.body,
                    return_type: None,
                },
            )?;
            contexts.push(work, (Some(index), true, false))?;
            Some(function)
        };
        let constructor = if module.is_class {
            if let Some(method) = instance_methods.iter().find(|m| m.name == "initialize") {
                Some((method.function, true))
            } else {
                let function = functions.len();
                functions.push(
                    work,
                    syntax::Definition {
                        private: true,
                        offset: module.offset,
                        accessor: None,
                        name: Name::join(work, &[&name, "#<initialize>"])?,
                        params: crate::compilation::Buffer::new(),
                        body: crate::compilation::Buffer::new(),
                        return_type: None,
                    },
                )?;
                contexts.push(work, (Some(index), false, true))?;
                Some((function, false))
            }
        } else {
            None
        };
        let definition = namespace::Definition::new(
            index,
            name.as_str().to_owned(),
            methods,
            instance_methods,
            constructor,
            nested,
            body,
        );
        self.namespaces.push(definition.clone());
        self.declaration_names
            .insert(name.into_string(), self.declarations.len());
        self.declarations
            .push(Value(Kind::Namespace(namespace::Namespace::untracked(
                definition,
            ))));
        Ok(index)
    }
}

impl<'x> Compiling<'_, 'x> {
    async fn nested_assignment_address(&self, receiver: &'x syntax::Expr) -> Result<()> {
        self.tasks.call(Call::AssignmentAddress(receiver)).await
    }

    pub(super) async fn assignment_address(&self, receiver: &'x syntax::Expr) -> Result<()> {
        let bound = {
            let mut c = self.c();
            c.work.charge(1)?;
            match &receiver.node {
                syntax::Node::Var(name)
                    if c.namespace.is_some()
                        && (!c.instance || name.starts_with('@'))
                        && c.namespace_binding(name)? =>
                {
                    // A missing field may need a builtin without another read registering it.
                    c.global(name);
                    let optional = name.starts_with('@');
                    let name = c.call_site(name, false).name;
                    c.emit(Op::NamespaceAddress(name, optional));
                    true
                }
                _ => false,
            }
        };
        if bound {
            return Ok(());
        }
        match &receiver.node {
            syntax::Node::Index(root, indices) => {
                self.nested_assignment_address(root).await?;
                for index in indices {
                    self.expr(index).await?;
                }
                self.c().emit(Op::AddressIndex(indices.len()));
            }
            syntax::Node::Member(root, name) => {
                self.nested_assignment_address(root).await?;
                let mut c = self.c();
                let site = c.call_site(name, true);
                c.emit(Op::AddressMember(site));
            }
            syntax::Node::Scope(root, name, None) => {
                self.nested_assignment_address(root).await?;
                let mut c = self.c();
                let mut site = c.call_site(name, true);
                site.scope = true;
                c.emit(Op::AddressNamespaceField(site));
            }
            _ => self.address_root(receiver, true).await?,
        }
        Ok(())
    }

    pub(super) async fn namespace_assignment(
        &self,
        name: &str,
        binding: &'x syntax::Target,
        target: &'x syntax::Expr,
        op: &str,
        rhs: &'x syntax::Expr,
    ) -> Result<()> {
        self.c().work.charge(1)?;
        if matches!(op, "||=" | "&&=") {
            let namespaced = {
                let mut c = self.c();
                let namespaced = name.starts_with('@') || (c.namespace.is_some() && !c.instance);
                if namespaced {
                    let name = c.call_site(name, false).name;
                    c.emit(Op::NamespaceVariable(name, true));
                }
                namespaced
            };
            if !namespaced {
                self.expr(target).await?;
            }
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
            self.assignment_rhs(binding, &[rhs]).await?;
            let mut c = self.c();
            c.store_namespace_name(name);
            let end = c.code.len();
            c.patch(skip, end);
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
            self.expr(target).await?;
        }
        self.assignment_rhs(binding, &[rhs]).await?;
        let mut c = self.c();
        if let Some(op) = binary {
            c.emit(Op::Binary(op));
        }
        c.store_namespace_name(name);
        Ok(())
    }
}

impl Compiler<'_> {
    pub(super) fn namespace_binding(&self, name: &str) -> Result<bool> {
        if name.starts_with('@') {
            return Ok(true);
        }
        if self.parameters.contains(self.work, name)? || self.outer_binding(name)?.is_some() {
            return Ok(false);
        }
        if self.namespace.is_some()
            && !self.instance
            && name.chars().next().is_some_and(syntax::unicode::upper)
        {
            return Ok(true);
        }
        Ok(self
            .program
            .declaration_names
            .get(name)
            .is_some_and(|&index| matches!(self.program.declarations[index].0, Kind::Namespace(_))))
    }

    pub(super) fn store_namespace_name(&mut self, name: &str) {
        if name.starts_with('@') || (self.namespace.is_some() && !self.instance) {
            let name = self.call_site(name, false).name;
            self.emit(Op::NamespaceStore(name));
        } else {
            self.emit(Op::StoreDeclaration(self.program.declaration_names[name]));
        }
    }
}
