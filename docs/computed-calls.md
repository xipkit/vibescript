# Computed calls

Call targets can be selected by rescue modifiers, read from collections, or returned by a call. Script functions remain confined to call syntax, as required by ADR-006:

```vibescript
def fallback(value)
  value + 1
end

(missing rescue fallback)(41)
```

Target selection finishes before positional arguments, keywords, splats and an attached block are passed to the callee. The rescue above catches lookup failures while selecting `fallback`; errors from its arguments or body propagate to the surrounding handler. Nested selections preserve this boundary. Private implicit methods, class methods, constructors, stored builtin exports and registered host capabilities use the same existing binding rules.

Selected class methods and bound helpers retain their receivers while arguments run. For example, reassigning the variable that supplied a receiver does not redirect an already selected method. Go's shared primitive member implementations still require direct member-call syntax to supply a receiver; wrapping `"abc".size` in a rescue does not bind the string. A safe member access on nil selects nil and subsequently fails if called.

Targets live in accounted argument storage rather than escaping as script-function values. Unwinding discards abandoned targets and arguments. Cancellation and actual work or memory exhaustion remain uncatchable. Match data and error objects retain their selected protection policy through duplicates and wrapped mutator lookup.

Nine native integration tests cover binding, call order, executable-value restrictions, receiver capture, reclamation, exact memory and step limits, cancellation, host keywords, protection and parser depth. The focused reference comparison also exercises shared primitive members and temporal properties. Permanent conformance cases exclude unresolved differences and values the JSON comparison harness cannot encode.

Four differences remain open in [the probe cases](computed-call-gaps.json): Go parses some calls following `begin` differently, and a missing builtin-namespace member can escape a rescue before Go wraps its error. These cases are not counted as passing conformance. Type-error wording and detached temporal-comparison diagnostics also remain pending. Enum equality helpers remain part of the unfinished standard library. The full language port still has the broader requirements listed in [the language plan](language-port.md).
