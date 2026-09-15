# Introspection

`respond_to?(name, include_all = false)` checks member availability without invoking a method. The name must be a string or symbol, and `include_all` must be a boolean. Explicit receivers expose public methods by default; an implicit call inside a class can report its private and protected methods. User-defined overrides keep their normal visibility.

Plain hash builtin names take precedence over stored data. Callable hash entries also respond, including entries whose keys contain invalid UTF-8. Namespace exports shadow hash methods, so `JSON[:keys] = 1` makes `JSON.respond_to?(:keys)` false. Instance and class data fields do not become methods. Data can shadow `tap` and `yield_self`; the other universal helpers remain available. Enum and temporal properties follow the reference's member-availability rules.

`is_a?(class)`, `kind_of?(class)` and `instance_of?(class)` all test direct class identity. Vibescript has no inheritance or module ancestry. The argument must be a class or module value; modules never match an instance.

`is_type?(atom)` performs a strict type test without conversion. Supported primitive atoms are `nil`, `bool`, `int`, `float`, `number`, `string`, `symbol`, `array`, `hash`, `object`, `range`, `duration`, `time` and `money`. A trailing `?` permits nil. For example, `1.is_type?(:number)` is true and `"1".is_type?(:int)` is false.

Class and enum names resolve in the active caller's lexical scope. The spelling must match the definition's name exactly. An enum member matches its enum type; an enum definition itself does not. A qualified atom such as `exports.Status` resolves an enum exported by a namespace object. Qualification accepts one dot, and an unknown qualified atom raises even for a nil receiver. An unresolved unqualified name uses exact structural name matching for non-nil values; its nullable form never accepts nil. Class data constants are not lexical type bindings.

Atoms accept at most 256 bytes. Empty names, generics, unions, shapes and `any` are rejected. Invalid-byte errors quote the original bytes, and oversized errors report the length without copying the argument into the message.

All five predicates reject keywords and blocks. Bare reads auto-invoke and fail the argument-count check. Ordinary parenthesized calls retain their receivers: `(value.is_type?)(:int)` is a member call. Helpers selected through `rescue` follow the reference's computed-call rules; an introspection helper selected this way receives nil. Class overrides and callable namespace exports retain their own targets.

Symbolic reductions such as `[value, :int].reduce(:is_type?)` use the same VM member dispatch as ordinary calls. They honor script methods, visibility, typed arguments and returns, callable exports, and the caller's lexical type scope. Arithmetic operation symbols keep their existing builtin arithmetic rules. Namespace writes inside instance, class and module methods preserve the visible binding, including a class constant that shadows a builtin namespace.

Method names are decoded in bounded chunks; raw hash names remain byte strings. Method-name scans, type lookup, dispatched arguments and temporary VM frames are accounted. Predicate results do not retain their inputs. Cancellation and invocation exhaustion remain uncatchable, including when a symbolic method invokes a host callback. The existing restrictions on foreign-script class dispatch and namespace-state transfer still apply. Dynamic `send`/`public_send`, file exports and static checking remain part of the unfinished language port.
