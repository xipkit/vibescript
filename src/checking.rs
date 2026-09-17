mod addresses;
mod arguments;
mod blocks;
mod builtins;
mod calls;
mod cases;
mod collections;
mod equality;
mod facts;
mod flow;
mod graph;
mod iteration;
mod lexical;
mod mutations;
mod ordering;
mod pending;
mod relation;
mod scalar;
mod slots;
mod widening;

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
