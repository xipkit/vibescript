//! What a statically typed host declares before compiling a case.

use serde_json::Value as Json;
use std::collections::BTreeMap;
use vibescript::{Capability, Engine, HostMethod, Value};

/// Declares the globals and capabilities a call supplies, each typed by its
/// value, as a statically typed host would. A capability built when a call
/// starts is declared by a fresh value of the same kind.
pub fn declare(
    engine: &mut Engine,
    case: &Json,
    globals: &BTreeMap<String, Value>,
    signature: Option<&HostMethod>,
) -> vibescript::Result<()> {
    let flag = |name: &str| case[name].as_bool().unwrap_or(false);
    let mut declared = Vec::new();
    if flag("capability_probe") {
        declared.push(Capability::from_value("host", crate::probe::template()));
    }
    if flag("block_probe") {
        declared.push(Capability::from_value("blocks", crate::blocks::template()));
    }
    for name in case["notifications"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
    {
        declared.push(crate::support::notification(name)?);
    }
    if let Some(method) = signature
        && case["signature_probe"]["registration"]
            .as_str()
            .is_none_or(|registration| registration == "capability")
    {
        declared.push(Capability::from_value(
            "typed",
            Value::object(vec![(b"echo".to_vec(), method.value())]),
        ));
    }
    // A global of a capability's name overrides it, as it does at runtime.
    for (name, value) in globals {
        declared.push(Capability::from_value(name.clone(), value.clone()));
    }
    for capability in &declared {
        engine.declare_capability(capability)?;
    }
    Ok(())
}
