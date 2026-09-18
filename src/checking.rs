mod addresses;
mod admission;
#[cfg(test)]
mod admission_tests;
mod arguments;
mod blocks;
mod builtins;
mod calls;
mod cases;
mod collections;
mod entry;
#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod enum_tests;
mod environment;
#[cfg(test)]
mod environment_tests;
mod equality;
mod facts;
mod flow;
mod globals;
mod graph;
#[cfg(test)]
mod input_tests;
mod inputs;
mod iteration;
mod lexical;
#[cfg(test)]
mod live_type_tests;
mod mutations;
#[cfg(test)]
mod namespace_tests;
mod normalization;
#[cfg(test)]
mod normalization_tests;
mod objects;
#[cfg(test)]
mod optional_capture_tests;
mod ordering;
mod pending;
mod public;
mod relation;
mod report;
mod scalar;
mod slots;
mod sources;
mod type_bindings;
mod widening;

pub use public::{CheckDiagnostic, CheckReport, CheckedOutcome};

#[cfg(test)]
mod call_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod collection_tests;

#[cfg(test)]
mod mutation_tests;

#[cfg(test)]
mod widening_tests;

#[cfg(test)]
mod address_tests;

#[cfg(test)]
mod recursion_tests;

#[cfg(test)]
mod iteration_tests;

#[cfg(test)]
mod case_tests;

#[cfg(test)]
mod exception_tests;

#[cfg(test)]
mod builtin_tests;

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod primitive_tests;

#[cfg(test)]
mod introspection_tests;

#[cfg(test)]
mod value_tests;

#[cfg(test)]
mod protected_tests;

#[cfg(test)]
mod block_tests;

#[cfg(test)]
mod yield_tests;

#[cfg(test)]
mod lexical_tests;

#[cfg(test)]
mod forwarding_tests;

#[cfg(test)]
mod collection_block_tests;

#[cfg(test)]
mod reduction_tests;

#[cfg(test)]
mod grouping_tests;

#[cfg(test)]
mod schedule_tests;

#[cfg(test)]
mod selection_tests;

#[cfg(test)]
mod order_tests;

#[cfg(test)]
mod pending_tests;

#[cfg(test)]
mod mutable_block_tests;

#[cfg(test)]
mod hash_block_tests;

#[cfg(test)]
mod text_block_tests;

#[cfg(test)]
mod substitution_tests;

#[cfg(test)]
mod dispatch_tests;

#[cfg(test)]
mod type_binding_tests;

#[cfg(test)]
mod global_tests;

#[cfg(test)]
mod global_type_tests;

#[cfg(test)]
mod object_tests;

#[cfg(test)]
mod root_tests;

#[cfg(test)]
mod attached_tests;

#[cfg(test)]
mod host_type_tests;

#[cfg(test)]
mod host_block_tests;
