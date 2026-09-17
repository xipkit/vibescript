mod addresses;
mod arguments;
mod builtins;
mod calls;
mod cases;
mod collections;
mod facts;
mod flow;
mod graph;
mod iteration;
mod mutations;
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
