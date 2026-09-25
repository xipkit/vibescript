//! The static type checker of ADR-007: each rule and diagnostic code, with
//! programs it accepts and programs it rejects.

mod common;

#[path = "static_types/support.rs"]
mod support;

#[path = "static_types/any.rs"]
mod any;
#[path = "static_types/calls.rs"]
mod calls;
#[path = "static_types/classes.rs"]
mod classes;
#[path = "static_types/collections.rs"]
mod collections;
#[path = "static_types/conditions.rs"]
mod conditions;
#[path = "static_types/engine.rs"]
mod engine;
#[path = "static_types/hosts.rs"]
mod hosts;
#[path = "static_types/locals.rs"]
mod locals;
#[path = "static_types/modules.rs"]
mod modules;
#[path = "static_types/narrowing.rs"]
mod narrowing;
#[path = "static_types/operators.rs"]
mod operators;
#[path = "static_types/scaling.rs"]
mod scaling;
#[path = "static_types/signatures.rs"]
mod signatures;
