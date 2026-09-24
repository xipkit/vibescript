//! Type aliases, resolved once per program so annotations can substitute them.

use crate::{
    Error, Result,
    compilation::{Buffer, Type, Work},
    syntax::{TypeAlias, modules::Module},
    types,
};
use std::collections::HashMap;

/// How many type nodes an alias may expand to, so nested aliases cannot
/// multiply a type's size without bound.
const MAX_NODES: usize = 1 << 16;
/// How tall an expanded type may grow, which keeps walks over it shallow.
const MAX_HEIGHT: usize = 64;

/// The aliases a program declares, each resolved to the type it names.
#[derive(Default)]
pub(super) struct Aliases {
    /// Resolved aliases by scope and name. The scope is empty for the top
    /// level and a module or class's qualified name otherwise.
    scopes: HashMap<String, HashMap<String, types::Type>>,
}

struct Declared {
    scope: String,
    alias: TypeAlias,
}

/// Resolution state shared by the recursive walk over alias references.
struct Resolving<'a> {
    declared: &'a [Declared],
    index: HashMap<(&'a str, &'a str), usize>,
    resolved: Vec<Option<types::Type>>,
    /// The aliases being resolved, innermost last.
    path: Vec<usize>,
}

impl Aliases {
    /// Resolves the program's aliases, refusing duplicates and cycles. Each
    /// comes with the offset of its declaring module or class, which
    /// `modules` names, or none at the top level.
    pub fn new(
        declarations: Buffer<(Option<u32>, TypeAlias)>,
        modules: &Buffer<Module>,
        work: &dyn Work,
    ) -> Result<Self> {
        if declarations.is_empty() {
            return Ok(Self::default());
        }
        let mut qualified = HashMap::new();
        let mut stack: Vec<(&Module, String)> = modules
            .iter()
            .map(|module| (module, module.name.to_string()))
            .collect();
        while let Some((module, scope)) = stack.pop() {
            work.charge(1)?;
            for nested in &module.modules {
                stack.push((nested, format!("{scope}::{}", nested.name)));
            }
            qualified.insert(module.offset, scope);
        }
        let mut declared = Vec::with_capacity(declarations.len());
        for (owner, alias) in declarations {
            // A class declared inside another body is never bound, and
            // neither are its aliases.
            let scope = match owner {
                None => String::new(),
                Some(offset) => match qualified.get(&offset) {
                    Some(scope) => scope.clone(),
                    None => continue,
                },
            };
            declared.push(Declared { scope, alias });
        }
        let mut resolving = Resolving {
            declared: &declared,
            index: HashMap::new(),
            resolved: vec![None; declared.len()],
            path: Vec::new(),
        };
        for (position, entry) in declared.iter().enumerate() {
            work.charge(1)?;
            let key = (entry.scope.as_str(), entry.alias.name.as_str());
            if resolving.index.insert(key, position).is_some() {
                return Err(Error::syntax(
                    work,
                    entry.alias.offset as usize,
                    format_args!("duplicate type alias {}", entry.alias.name),
                ));
            }
        }
        for position in 0..declared.len() {
            resolving.resolve(position, work)?;
        }
        let mut scopes: HashMap<String, HashMap<String, types::Type>> = HashMap::new();
        for (entry, resolved) in declared.iter().zip(resolving.resolved) {
            scopes
                .entry(entry.scope.clone())
                .or_default()
                .insert(entry.alias.name.to_string(), resolved.unwrap());
        }
        Ok(Self { scopes })
    }

    /// The type `name` names as an alias seen from `scope`: the scope's own
    /// aliases, then each enclosing module's, then the top level's.
    pub fn get(&self, scope: &str, name: &str) -> Option<&types::Type> {
        if self.scopes.is_empty() {
            return None;
        }
        scopes(scope).find_map(|scope| self.scopes.get(scope)?.get(name))
    }

    /// Compiles an annotation written in `scope`, substituting its aliases.
    pub fn compile(&self, scope: &str, ty: &Type, work: &dyn Work) -> Result<types::Type> {
        if self.scopes.is_empty() {
            return ty.compile(work);
        }
        let mut nodes = 0;
        let compiled = ty.compile_with(work, &mut |work, name| {
            let Some(resolved) = self.get(scope, name) else {
                return Ok(None);
            };
            nodes += resolved.nodes();
            work.charge(resolved.nodes())?;
            if nodes > MAX_NODES {
                return Err(too_large(work, name));
            }
            Ok(Some(resolved.clone()))
        })?;
        if compiled.height() > 2 * MAX_HEIGHT {
            return Err(crate::compilation::error(
                work,
                None,
                format_args!("type annotation nests too deeply after expanding its aliases"),
            ));
        }
        Ok(compiled)
    }
}

impl Resolving<'_> {
    fn resolve(&mut self, position: usize, work: &dyn Work) -> Result<()> {
        if self.resolved[position].is_some() {
            return Ok(());
        }
        let declared = self.declared;
        let entry = &declared[position];
        if let Some(start) = self.path.iter().position(|&open| open == position) {
            let mut cycle: Vec<&str> = self.path[start..]
                .iter()
                .map(|&open| declared[open].alias.name.as_str())
                .collect();
            cycle.push(&entry.alias.name);
            let first = &declared[self.path[start]].alias;
            return Err(Error::syntax(
                work,
                first.offset as usize,
                if cycle.len() == 2 {
                    format!("type alias {} refers to itself", first.name)
                } else {
                    format!("type alias cycle: {}", cycle.join(" -> "))
                },
            ));
        }
        if self.path.len() >= MAX_HEIGHT {
            return Err(Error::syntax(
                work,
                entry.alias.offset as usize,
                format_args!("type alias {} nests too deeply", entry.alias.name),
            ));
        }
        self.path.push(position);
        let mut nodes = 0;
        let compiled = entry.alias.ty.compile_with(work, &mut |work, name| {
            let Some(target) = self.lookup(&entry.scope, name) else {
                return Ok(None);
            };
            self.resolve(target, work)?;
            let resolved = self.resolved[target].as_ref().unwrap();
            nodes += resolved.nodes();
            work.charge(resolved.nodes())?;
            if nodes > MAX_NODES {
                return Err(too_large(work, &entry.alias.name));
            }
            Ok(Some(resolved.clone()))
        })?;
        self.path.pop();
        if compiled.height() > MAX_HEIGHT {
            return Err(Error::syntax(
                work,
                entry.alias.offset as usize,
                format_args!("type alias {} nests too deeply", entry.alias.name),
            ));
        }
        self.resolved[position] = Some(compiled);
        Ok(())
    }

    fn lookup(&self, scope: &str, name: &str) -> Option<usize> {
        scopes(scope).find_map(|scope| self.index.get(&(scope, name)).copied())
    }
}

/// A scope and each scope enclosing it, ending with the top level.
fn scopes(scope: &str) -> impl Iterator<Item = &str> {
    let mut next = Some(scope);
    std::iter::from_fn(move || {
        let current = next?;
        next = if current.is_empty() {
            None
        } else {
            Some(current.rsplit_once("::").map_or("", |(parent, _)| parent))
        };
        Some(current)
    })
}

fn too_large(work: &dyn Work, name: &str) -> Error {
    crate::compilation::error(
        work,
        None,
        format_args!("type alias {name} expands to more than {MAX_NODES} types"),
    )
}

/// The qualified name of the namespace a function is compiled in, which is
/// the scope its annotations read aliases from.
pub(super) fn scope(
    namespaces: &[std::sync::Arc<crate::namespace::Definition>],
    namespace: Option<usize>,
) -> &str {
    namespace.map_or("", |index| namespaces[index].name.as_str())
}
