//! Programs of a required file whose top-level locals its functions read,
//! assign and shadow with parameters of the same names, calling one another
//! and called from the file's top level between a narrowing of a local and
//! a use of it, and a script that requires the file and calls into it.
//!
//! A call of the file's functions widens the locals any of them assigns, so
//! a local none of them assigns keeps its narrowing across the calls, and a
//! parameter of a local's name is the function's own: assigning it assigns
//! no local. Each program uses a local only where that holds, but for a
//! few, which use a local a function assigns `nil` as if it were narrowed,
//! and which a sound checker rejects.

use super::{harness::Case, rng::Rng};

/// A local of the file: its name, and whether it is declared optional.
struct Local {
    name: String,
    optional: bool,
    /// Whether some function assigns it.
    written: bool,
}

/// The program of `seed`.
pub fn program(seed: u64) -> Case {
    let mut rng = Rng::new(seed ^ 0x6669_6c65);
    let mut locals: Vec<Local> = (0..2 + rng.below(4))
        .map(|index| Local {
            name: format!("v{index}"),
            optional: rng.chance(60),
            written: false,
        })
        .collect();
    let mut file = String::new();
    for (index, local) in locals.iter().enumerate() {
        if local.optional {
            file.push_str(&format!("{}: int? = {index}\n", local.name));
        } else {
            file.push_str(&format!("{} = {index}\n", local.name));
        }
    }
    // Each function's parameters, by arity, for its callers.
    let mut arities = Vec::new();
    for function in 0..2 + rng.below(4) {
        let mut params: Vec<String> = Vec::new();
        for param in 0..1 + rng.below(3) {
            let shadow = rng.chance(60);
            let name = if shadow {
                locals[rng.below(locals.len())].name.clone()
            } else {
                format!("a{param}")
            };
            if !params.contains(&name) {
                params.push(name);
            }
        }
        let signature: Vec<String> = params.iter().map(|name| format!("{name}: int")).collect();
        file.push_str(&format!(
            "def f{function}({}) -> int\n",
            signature.join(", ")
        ));
        for _ in 0..rng.below(4) {
            match rng.weighted(&[4, 2, 3]) {
                // A parameter, which may take a local's name, assigned.
                0 => {
                    let name = &params[rng.below(params.len())];
                    file.push_str(&format!("  {name} = {name} + 1\n"));
                }
                // A local the function does not shadow, assigned.
                1 => {
                    let index = rng.below(locals.len());
                    let local = &mut locals[index];
                    if params.contains(&local.name) {
                        continue;
                    }
                    local.written = true;
                    if local.optional {
                        file.push_str(&format!("  {} = nil\n", local.name));
                    } else {
                        file.push_str(&format!("  {} += 1\n", local.name));
                    }
                }
                // A call of a function before it.
                _ if function > 0 => {
                    let callee = rng.below(function);
                    let arguments = arguments(&mut rng, arities[callee]);
                    file.push_str(&format!("  f{callee}({arguments})\n"));
                }
                _ => (),
            }
        }
        file.push_str(&format!("  {}\nend\n", params[rng.below(params.len())]));
        arities.push(params.len());
    }
    // The file's top level narrows its locals, calls its functions, and
    // uses the locals as the calls leave them.
    for local in locals.iter().filter(|local| local.optional) {
        file.push_str(&format!("{} = {}\n", local.name, rng.below(9)));
    }
    for _ in 0..1 + rng.below(3) {
        let callee = rng.below(arities.len());
        let arguments = arguments(&mut rng, arities[callee]);
        file.push_str(&format!("f{callee}({arguments})\n"));
    }
    for (index, local) in locals.iter().enumerate() {
        let name = &local.name;
        if !local.optional || !local.written {
            file.push_str(&format!("held{index} = {name} + 1\n"));
        } else if rng.chance(10) {
            // The unsound form: a function may have assigned it `nil`.
            file.push_str(&format!("held{index} = {name} + 1\n"));
        } else {
            file.push_str(&format!(
                "if {name} != nil\n  held{index} = {name} + 1\nend\n"
            ));
        }
    }
    let callee = rng.below(arities.len());
    let arguments = arguments(&mut rng, arities[callee]);
    let main = format!("m = require(\"lib\")\np(m.f{callee}({arguments}))\n");
    Case {
        main,
        modules: vec![("lib.vibe".to_owned(), file)],
        host: Default::default(),
    }
}

/// `count` integer arguments.
fn arguments(rng: &mut Rng, count: usize) -> String {
    (0..count)
        .map(|_| rng.below(9).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
