# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

- Ongoing work toward the next release.

<!-- Unreleased entries are tracked as individual files in changelog.d/ so
     pull requests never conflict on this file. They are compiled into a
     versioned section by scripts/build_changelog.sh at release time. -->
## v0.60.0 - 2026-08-22

Vibescript 0.60 narrows the language around predictable sandboxing. Tasks,
sleep, escaping callables, mixed-key hashes, shared collection mutation, and
module behavior injection are removed in favor of synchronous host capabilities,
nonescaping blocks, string-keyed hashes, value semantics, and explicit namespace
calls. The release also closes a broad set of parser,
checker, runtime, and memory-quota denial-of-service paths while improving
diagnostics, collection helpers, conversions, and editor support.

Go module users already on `v1.0.0-rc9` must select this release explicitly
because `v0.60.0` has lower semantic-version precedence:
`go get github.com/mgomes/vibescript@v0.60.0`.

- **Fixed: `==` compares an int and a float numerically.** `1 == 1.0` was false,
  so an average computed as `sum / 3.0` never matched the integer it printed as,
  with no diagnostic. `eql?` keeps its kind gate — that is the distinction the
  documentation draws — while hashes now accept only string and symbol keys.
- **Fixed: an unknown method now reports the offending line and can be rescued.**
  Calling a method that does not exist on a string, array, hash, money, or
  temporal value reported the start of the script instead of the call, and could
  not be caught by `rescue` at all. Sandbox limits stay deliberately outside
  `rescue`, as before.
- **Added: `vibes check` reports an unknown method on a statically-known
  receiver.** `s.uppercase` on a `s: string` parameter reported "No issues
  found" and failed only at runtime. Choosing a wrong method name is the most
  frequent authoring mistake there is, and it was previously unassisted at check
  time. Receivers that could dispatch elsewhere -- dynamic, named types, and the
  kinds whose member list is hand-maintained -- stay silent.
- **Added: `Hash#map`.** Hashes had `map_with_index` but no `map`, so every hash
  aggregation had to route through `to_a` and positional pair indexing. It yields
  the `[key, value]` pair, so `h.map { |k, v| ... }` and `h.map { |pair| ... }`
  both behave as in Ruby.
- **Fixed: `assert(cond, "msg")` was a parse error.** `assert` was parsed as a
  bespoke statement form, so the parenthesised call read `(cond, "msg")` as a
  grouped expression and failed on the comma, despite the documented two-argument
  signature. It now parses as an ordinary call. The paren-less forms, including
  `assert (a > b), "msg"` where the space makes the parentheses a grouped first
  argument, are unchanged.
- **Improved: diagnostics for deliberate Ruby divergences now name the
  alternative.** `attr_accessor` points at `property`/`getter`/`setter` and notes
  the name is bare rather than a symbol; `property :x` shows the bare form;
  `class B < A` says inheritance is unsupported and names the module-function
  replacement; and an
  undefined variable that *is* bound at the top level explains that functions do
  not capture top-level bindings. An agent reads the error far more often than
  it reads the docs, so the error is where the idiom has to be taught.
- **Added: `Array#flat_map` (alias `collect_concat`).** `map { }.flatten(1)`
  covered it, so this is the missing shorthand for a common shape -- notable
  mainly because `filter_map` was already present, which made the absence look
  arbitrary. It flattens exactly one level, as in Ruby.
- **Added: `to_s` and `inspect` on enum values.** Interpolation rendered an enum
  value correctly but the explicit conversion did not exist, so `.to_s` reported
  an unknown member -- with no suggestion, since neither `name` nor `symbol` is
  close enough for the did-you-mean to fire. Both return exactly what
  interpolation produces.
- **Added: `String#*` repeats a string.** There was no way to build a separator,
  indent, table rule, or progress bar at a computed width without a loop -- a
  literal `"------"` was the only alternative. `"-" * 20` now works as in Ruby,
  truncating a float count toward zero and reporting a negative one. The
  projected size is charged against the memory quota before anything is
  allocated.
- **Fixed: `vibes check` reported one copy of a body diagnostic per call site.**
  A helper called eight times produced eight identical lines for a single
  mistake -- same file, line, column, and text -- which buried the other genuine
  findings and made "check failed with N issue(s)" count call sites rather than
  problems. Diagnostics identical in every field are now reported once.
- **Fixed: a minus before a numeric literal binds to the literal.** `-5.abs`
  returned `-5` because the sign bound looser than the member call and landed on
  the method's result, and `-5.to_s` failed outright. The sign now folds into the
  literal, matching Ruby, while `-2 ** 2` keeps its `-4` value and `-x.abs` on a
  variable still means `-(x.abs)`.
- **Fixed: `String#to_i` rejected integers beyond int64.** A value the language
  can represent, print, and compute with could not be parsed back from its own
  string form, so `(2 ** 100).to_s.to_i` failed. Integers are arbitrary
  precision, and the surfaces documented as staying within 64 bits are indexes,
  counts, sizes, precisions, and the temporal types -- a value conversion is none
  of those. `to_int` had the same gap on strings while already promoting floats,
  and is fixed with it. Malformed strings are still rejected; only the range
  limit is lifted.
- **Added: arrays compare and sort.** `[1, 2] <=> [1, 3]` returned nil and
  `[[2, 1], [1, 9]].sort` failed, so an array of arrays could not be ordered at
  all. Arrays now compare lexicographically as in Ruby: the first differing
  element decides, and a shorter array with an equal prefix sorts first. This
  reaches further than arrays themselves, because a hash entry is a pair, so
  `hash.to_a.sort` and `sort_by { |r| [r[:a], r[:b]] }` now work. As in Ruby, the
  relational operators still reject arrays -- `Array` defines `<=>` but does not
  include `Comparable`.
- **Added: `vibes check` reports a space before a parenthesised argument.**
  `f (x).length` binds as `(f(x)).length` here but as `f((x).length)` in Ruby.
  Both readings produce a value and neither reported anything, so the difference
  was invisible -- most damagingly in `puts (x).inspect`, where the one line
  written to see a value accurately renders it inaccurately. The binding is
  unchanged; only the ambiguity is now reported.
- **Fixed: concatenating a non-renderable value into a string is now an error.**
  `"Hello, " + name` silently produced `"Hello, "` when `name` was nil, so a
  missing value disappeared into the message instead of being reported, and
  containers, host objects, and class instances rendered as placeholders such
  as `"[1]a"` or `"user: <User instance>"`. Concatenating
  scalars (`"total: " + count`) is unchanged, and the checker now reports the
  same mismatch statically when both operand types are known.
- **Fixed: string interpolation and `puts` ignored a class's `to_s`.** They
  rendered the `<ClassName instance>` placeholder even when the class defined
  `to_s`, so a value printed one way when interpolated and another when `.to_s`
  was called explicitly. Both now dispatch to a user-defined `to_s`, falling back
  to the placeholder when the class defines none, when its `to_s` cannot be
  called with zero arguments, or when it returns a non-string. As in Ruby, a
  value nested inside a container still renders through the container.
- **Improved: completion after `.` narrows to the receiver's type.** It offered
  the union of all 306 builtin members whatever the receiver was, so a string
  receiver was offered money and temporal methods and completion could not rule
  out a wrong method name. It now narrows when the receiver's kind is known from
  the source -- a literal, or a parameter with a declared type -- and falls back
  to the full union for anything else.
- **Added: named captures are accessible.** A pattern's named groups compiled and
  matched but were never surfaced, so `m[:name]` read nil and `named_captures`
  did not exist -- every non-trivial extraction had to be read back positionally.
  `m[:name]`, `m["name"]`, and `m.named_captures` now work. An index naming an
  entry the match result already has (`captures`, `pre_match`, ...) still reads
  that entry.
- **Added: the missing `to_s` and `inspect` members.** Every value kind already
  rendered correctly through interpolation and inside a container's `inspect`,
  but the direct methods were absent on most kinds, and which kind had which
  corresponded to nothing -- `duration`, `money`, and `time` had `to_s` but not
  `inspect`, `array` had `inspect` but not `to_s`, `range` had neither. Array and
  range now render with `to_s`, and duration, money, and time with `inspect`. The
  aggregate renderings charge the memory quota exactly as `inspect` does.
- **Fixed: a keyword default referencing an earlier parameter reported parameter
  ordering.** `def f(a:, b: a)` -- the simplest spelling of the documented "a
  later default may reference an earlier parameter" feature -- parses the bare
  identifier as a type annotation, which makes the parameter ordinary and trips
  the ordering rule. The error now explains the type-annotation reading and shows
  the parenthesized form (`b: (a)`) that expresses the default.
- **Fixed: scaling a duration by a fraction no longer collapses it to zero.**
  `1.hour * 0.5` returned `0s` because a float operand was truncated to an
  integer before scaling, so every factor below one produced a zero duration and
  `1.hour / 0.5` reported a division by zero. Float factors and divisors now
  scale properly and round to the nearest second; integer operands keep their
  exact path.
- **Fixed: crossing `Time#format` and `Time#strftime` returned the format string
  as data.** `t.strftime("2006-01-02")` answered `"2006-01-02"` -- a string that
  looks exactly like a formatted date, because the Go reference layout is one, so
  a report could emit the same twenty-year-old date on every row and pass a
  visual check, a type check, and an ISO-8601 regex. Both directions now report
  which format language they expected and which method takes the one supplied.
- **Changed: zoneless times default to UTC instead of the host zone.** `Time.now`
  and a `Time.parse` of a timestamp carrying no zone bound to the host process's
  `TZ`, so the same script and the same data produced a different calendar day
  depending on where it ran -- and `Time.now` disagreed with the `now` builtin,
  which is documented as UTC and always was. Both are UTC now. An explicit `in:`
  zone, or a `Z`/offset in the input, is honored as before.
- **Fixed: the embedding starter templates compile again.** All three scaffolds
  under `templates/embedding/` referenced value constructors on the `vibes`
  package rather than `vibes/value`, so none of them built. CI now compiles every
  template, which is what let them drift.
- **Fixed: `Hash#map` ignored the block's arity and hid its retained output.**
  A block using numbered parameters (`{ _2 }`) received the whole `[key, value]`
  pair as `_1` and nil as `_2`; the yield shape now follows the block's
  positional arity as every other hash iterator does. The accumulating result is
  also reserved as releasable scratch, so a large temporary allocated inside a
  later block call is measured against a baseline that includes what the loop
  has already retained -- previously the two could pass separately though they
  coexisted.
- **Fixed: `retry` escaped an implicit `to_s`.** A class's `to_s` invoked by
  interpolation inside a rescue handler could run `retry` and restart the
  caller's rescue, while an explicit `obj.to_s` in the same position reported
  `retry cannot cross call boundary`. The implicit call is a call boundary too
  and now behaves like one.
- **Fixed: a rescued error rendered as `<object>`.** `rescue => e` followed by
  `puts "failed: #{e}"` is the most common error-reporting idiom there is, and it
  printed `failed: <object>` -- a silent loss, since `e.message` and `e.to_s`
  both held the text. Interpolation, `puts`, and `print` now render the message,
  as in Ruby. `inspect` keeps the full detail.
- **Fixed: `format("%s", obj)` ignored a class's `to_s`.** `%s` is defined as the
  `to_s` form, but `format` was the last direct string conversion that did not
  consult it -- interpolation and `puts` were connected in #1055 and this was
  left out, so one conversion still disagreed with the other two.
- **Fixed: array comparison was unmetered and exponential on shared graphs.**
  Comparing two arrays that share subtrees did work exponential in their depth
  -- 2.1 seconds at depth 24 -- while charging no steps, so a script could
  monopolize the runtime from inside `<=>` whatever its limits. Completed pairs
  are now memoized and every compared element charges a step. A sandbox limit
  reached during a sort keeps its classification rather than being relabelled as
  an incomparability.
- **Fixed: `<=>` did not order symbols or nil though `sort` did.** `:a <=> :b`
  answered nil while `[:c, :a].sort` worked, so the operator misreported what the
  language can do. Coverage now follows Ruby per kind: symbols order under `<=>`
  and the relational operators (Symbol includes Comparable), nil orders under
  `<=>` alone, and booleans order under neither.
- **Fixed: an incomparable comparison could not be rescued and pointed at line
  1.** `1 < "a"` raised an error that `rescue` could not catch and that reported
  the start of the script rather than the comparison, because the relational
  operators returned without positioning the error. Sandbox limits stay
  deliberately uncatchable; an ordinary type error is now both catchable and
  located.
- **Fixed: an unbounded decimal conversion could occupy a worker.**
  `big.Int.SetString` is superlinear, so the linear step charge passed well
  before a multi-million-digit conversion finished. String conversions are now
  capped at the same 100,000 digits the parser already applies to integer
  literals, and the source string is charged alongside the parsed bignum since
  both are live at once.
- **Fixed: detecting a crossed `Time` format rendered it first.** Classifying
  `t.format("%1000000000N")` ran the strftime renderer, which honors a
  directive's requested width, allocating about a gigabyte purely to decide --
  and with no memory limit set that exhausts the process. Detection now scans
  for a recognized directive instead of rendering.
- **Fixed: a duplicate named capture reported nil.** When a pattern reuses a
  group name, only one of those groups participates in a given match, but a
  later non-participating duplicate overwrote an earlier match -- so
  `/(?<x>a)|(?<x>b)/` against `"ab"` reported nil for `x` rather than `"a"`. The
  last participating group is kept, as in Ruby.
- **Fixed: a zone abbreviation was silently treated as UTC.** Applying the
  zoneless UTC default to an input naming a zone made Go fabricate the
  abbreviation at offset zero, so `Mon, 27 Jul 2026 14:30:45 EDT` shifted by
  four hours. Such inputs resolve against the host's zone database again; a
  timestamp naming no zone still defaults to UTC on every host.
- **Fixed: enum conversions rendered before checking the quota.** `to_s`,
  `string`, and `inspect` on an enum value allocated the whole `Enum::Member`
  text before the memory guard ran, which matters when an identifier is larger
  than the quota. The length is now projected from the two identifiers first.
  A typo near `string` also suggests it.
- **Added: `name`, `to_s`, and `inspect` on an enum type.** The type object
  supported no member access at all -- it interpolated as `<Enum Status>` but
  `Status.to_s` reported "unsupported member access on enum" -- leaving it the
  one value kind you could not ask anything about.
- **Fixed: `vibes check` rejected `"-" * 12`.** `String#*` works at runtime, but
  the checker's operator matrix had no string case, so it reported unsupported
  multiplication operands on valid programs.
- **Fixed: any object with a `to_s` field had it rendered.** The shortcut that
  renders a rescued error's message keyed on the field name alone, so ordinary
  host data carrying a string field of that name had its payload rendered in
  place of `<object>`. Only the two bags that deliberately publish a string form
  -- a rescued error and match data -- are rendered.
- **Fixed: `vibes check` rejected symbol comparisons.** `:a < :b` runs, but the
  checker's comparison matrix accepted only numeric, string, money, duration,
  and time pairs, so check reported an unsupported comparison and `CheckedCall`
  refused to run the script.
- **Fixed: `rescue ArgumentError` catches an incomparable comparison.** The
  language reference documents `<`, `<=`, `>`, and `>=` as raising Ruby's
  `ArgumentError` on operands that cannot be ordered, but `1 < "a"` raised an
  untyped error that only a bare `rescue` could catch, so a handler written from
  the documentation silently missed it. `<=>` is unaffected and still answers
  `nil`.
- **Fixed: an internal key leaked into `String#match` results.** The result
  carried a NUL-prefixed sentinel holding the positional values, visible in
  `keys`, `values`, `to_a`, `size`, `each`, and `inspect` -- a key the author
  never created and which read back as nil through its own name. The positional
  view is now rebuilt from the public entries, with the whole match under `to_s`
  as in Ruby, so no separate copy exists to leak.
- **Added: `break` works inside blocks.** The most common early-exit idiom in a
  Ruby-flavored language was rejected, and the restriction was asymmetric --
  `next` and `return` both crossed a block boundary and `break` did not. As in
  Ruby, `break` now terminates the call the block was passed to and that call
  evaluates to the break value: `[1, 2, 3, 4].each { |n| break n if n > 3 }` is
  `4`. A break crossing a call that received no block still reports.
- **Fixed: `vibes check` rejected a hash's stored entries.** `({answer: 42}).answer`
  returns 42 at runtime but was reported as an unknown member: a hash serves
  stored entries for any name its member table does not own, so that table is
  not the authoritative set the check assumed. Regex literals are now detected
  as receivers, which the check claimed to cover but could not reach.
- **Fixed: an absorbed `break` bypassed return-type validation and capability
  binding.** A break becoming a call's result skipped the function's declared
  return type -- so a string could leave an `-> int` function -- and skipped the
  post-call capability scan, leaving a builtin published during that call
  reachable without its contract. The break now continues down the normal
  return path.
- **Fixed: `eql?` was not kind-strict for nested values.** `[1].eql?([1.0])` was
  true where Ruby says false: `eql?` checked only the outermost kind and then
  delegated to `==`, so elements took the numeric comparison. Strictness now
  holds at every level. `==` is unchanged and stays numeric nested.
- **Faster: building a one-element array no longer walks the whole reachable
  graph.** Every array literal opened an incremental build accumulator whose
  baseline is an unmemoized reference walk, so allocating a single slot cost
  O(reachable). A script nesting a structure in a loop paid that on every
  iteration. A one-element literal now charges its result once through the
  memoized check instead, which measures 1.7x faster on a 2000-deep build under
  a quota.
- **Added: a measurement harness for deep-nesting construction under a memory
  quota.** Building a chain of nested arrays is quadratic in depth when a quota
  is in force and linear without one; the benchmark and scaling test state that
  as an executable baseline so an incremental-estimation fix has a pass/fail
  target rather than a table in an issue.
- **Performance: building collections in a hand-written loop is no longer
  quadratic under a memory quota.** Growing an array with `<<` or filling a
  hash with new keys invalidated the memory estimator's base-walk memo every
  iteration — the append's epoch bump, the hash literal's construction-time
  reference walk and its three epoch bumps, and the capacity-doubling
  reservation each forced whole-graph re-walks, so a loop that builds n
  records did O(n²) estimator work (the quota is on by default at 16 MiB).
  Literals now build epoch-silently and price their entries through the
  memoized walk, growth reservations resume the memo, and eligible appends
  and added hash entries commit their marginal bytes into the memo instead of
  discarding it. Building 2,000 `{id:, name:}` records under a 64 MiB quota
  drops from 2.30s to 6.6ms and scales linearly. Byte totals are unchanged:
  the smallest admitting quota is pinned identical to the uncached reference
  walk, and `VIBES_ESTIMATOR_VERIFY` re-derives every incremental commit from
  scratch. Loops whose bodies call mutating or undeclared builtins such as
  `push` still discard the memo and stay super-linear — that contributor is
  tracked separately.
- **Performance: a loop under a memory quota no longer re-walks the heap every
  iteration.** A statement list evaluates in the scope it is handed rather than a
  fresh one, so a loop body re-pushes the enclosing scope each iteration. That
  duplicate stack slot was treated as a topology change and invalidated the
  memory estimator's memo twice per iteration, making a loop that only reads
  quadratic in its own iteration count. Reading a 4,000-element array in a
  `while` loop under a 64 MiB quota drops from 63ms to 5ms, and the same loop is
  now linear rather than quadratic.
- **Fixed: array scans charge the step quota per element.** `include?`, `index`,
  `rindex`, `min`, `max`, `minmax`, and the blockless `uniq` scanned the whole
  receiver for a flat handful of steps, so a script could scan an arbitrarily
  large host-supplied array on a constant budget. They now charge one step per
  element, as `sum` and `reverse` already did. An early match still exits early
  and costs only the elements it examined. `uniq` in both forms additionally
  charges for the equality probes a composite costs, since deduplicating
  composites compares each one against every distinct composite already seen.
- **Fixed: string methods charge the step quota for the bytes they scan.** A
  string method's work grows with its receiver but it dispatches as a single
  call, so charging a flat handful of steps let a script process a
  host-supplied string of any size on a constant budget: repeatedly upcasing an
  800 KB string burned five minutes inside the default 1M-step profile before
  the quota fired. String methods now charge one step per 64 bytes of receiver,
  the same rate big-integer operands are charged at. Methods whose cost does not
  grow with the receiver (`bytesize`, `empty?`, `getbyte`, `ord`, `chr`, `to_s`,
  `clear`, `byteslice`, `to_sym`, `intern`) stay exempt, and any receiver
  shorter than 64 bytes rounds down to no charge, so ordinary short strings are
  unaffected.
  Rendering a value charges for the bytes it prints -- `inspect`,
  interpolation, `to_s`, `puts`, and `join` alike -- so converting a large
  string to a symbol stays free while printing that symbol's name does not.
  `format` charges for its pattern as well as its arguments: a 512 KB pattern
  with no arguments at all previously ran for over a minute inside the default
  profile without the quota firing.
  The charge covers string arguments copied into a result as well as the
  receiver, so a short receiver with a large argument (`"".concat(s)`) costs
  the same as the reverse.
  Operations whose output size comes from a number rather than their inputs --
  `ljust`, `rjust`, `center`, and `String#*` -- charge for the bytes they write,
  since a few-byte receiver can produce megabytes.
  Serializing and templating charge for the strings they reach through a
  structure, so `JSON.stringify({v: big})` and `"{{v}}".template({v: big})`
  cost what they copy rather than what their receiver holds. `template` also
  checks its scratch against the memory quota as it builds it rather than once
  it has finished, so a template it is going to reject no longer renders every
  placeholder first.
  Operators, index syntax, and equality predicates charge too: `+`, the
  comparisons, `s[0]`, and `eql?` never pass through method dispatch, so each
  copied or scanned a whole string for a flat cost. Comparing two scalars
  charges for the bytes a comparison can read -- nothing when equality can
  answer from a length mismatch, the shorter operand when it cannot.
  A regex operand is sized from its source rather than by rendering it, so
  measuring what a regex will print no longer costs a full escape pass on top of
  the one that prints it.
- **Comparisons through arrays and hashes now charge for the string bytes they
  read.** The #1131 charge stopped at the operator boundary, so `[s] == [s]`,
  `{k: s} == {k: s}`, `[s] <=> [s]`, `include?`, `index`, `count`, `sort`,
  `max`, `uniq`, set operations, `eql?`, and `case/when` all scanned
  arbitrarily long payloads for a flat handful of steps — a loop over
  host-supplied strings with a long common prefix ran unbounded on a constant
  budget. The charge now lands at the recursive scalar comparison with the
  operator-level rules (equality bills only equal-length pairs, ordering
  bills the common prefix), shared and cyclic structures are billed once per
  distinct pair, and string-like hash and set keys are charged at every
  key-canonicalization site, closing the flat-cost `[s].uniq` and hash-key
  hashing gap noted alongside. `count(value)` and `hash.value?` also gain the
  per-element step their sibling scans already charged. Payloads under 64
  bytes round to free, so ordinary scripts see no new cost.
- **Changed: quota exhaustion is no longer rescuable.** `rescue LimitError`
  (and `RuntimeError`, `Error`, and unions) used to catch a tripped step or
  memory quota, so a loop that rescued the error could absorb the signal that
  its budget was spent and keep burning a fresh operation's worth of work per
  iteration — the memory quota even recovered once the oversized value was
  dropped. Genuine exhaustion (step quota, memory quota, `string.scan`'s
  output cap) now latches the execution: no rescue clause matches it, `ensure`
  bodies and `retry` cannot run work past it, and the host always receives the
  termination. Recursion-limit errors, stdlib input guards, and script-raised
  `raise LimitError` remain rescuable — they describe one rejected operation,
  not a spent budget. The host-visible error is unchanged:
  `*vibes.RuntimeError` with `Type == "LimitError"` and the same messages.
- **Fixed: a source of malformed percent-array candidates no longer makes
  parsing superlinear.** Deciding whether a `%` the lexer read as modulo really
  opens a `%w`/`%i`/`%W`/`%I` literal means re-scanning from that point, and the
  scan can only report failure by reaching the end of the input, after which the
  caller advances a single byte to the next candidate. A source that just
  repeats ` %w[` therefore paid for a near-full re-scan per byte, and because
  each of those scans re-entered the interpolation finder for every `#{` it
  crossed, a source of many small interpolations was cubic rather than quadratic.
  Only the source-size limit stood in the way, so a script at the default 1 MiB
  cap held the parser for over eight minutes before any runtime quota could
  apply; it now finishes in 149ms. The fruitless scans are charged against one
  parse-wide allowance of four times the source length, shared by every lexer
  and sub-parser involved, while a scan that does find its literal stays free --
  so percent literals of any length and number keep parsing as before. Ordinary
  modulo on a local (`a %w+ b`) is settled by the existing local-variable
  suppression before any scanning happens, so it costs nothing, and a source
  that does outrun the allowance is reported rather than parsed with `%` quietly
  demoted to modulo.
- **Fixed: a line of many parenless calls no longer crashes the host.** A
  parenless call parses its argument as a full expression, and that argument may
  be another parenless call, so `a a a a ...` on one line nests one call per
  identifier. Half a megabyte of them fits well inside the default source-size
  limit and drove the parser deep enough to overflow the goroutine stack, which
  is fatal for the whole process rather than for the script, and long before
  that the parse was already taking seconds. Parenless calls now nest at most 64
  deep, the same bound type annotations have carried, and a line past it reports
  "parenless call nesting too deep" against the argument that would have opened
  the next level. Real code stacks two or three deep (`puts format value`), so
  nothing written on purpose comes near the cap.
- **Fixed: a class returned to the host no longer copies its property types
  once per parameter.** An unannotated ivar parameter (`def m(@x)`) carries the
  property contract its class declares once, and cloning a class for the host
  copied that whole type expression again for every parameter that named it. A
  38KB script with a 1000-field property type and 500 such methods therefore
  retained 80MB, allocated inside the host clone after the run had ended, where
  no quota could observe it. A clone now copies each type expression once for
  the whole operation and gives that copy to every parameter and class reaching
  it, so that script retains 0.5MB. The copy is still the clone's own: editing
  a contract on a class from `Classes()` cannot reach the compiled script that
  later calls run.
- **Fixed: what a required file builds during initialization is inside the
  calling execution's memory quota.** A required module's environment is only a
  Go local until require publishes its exports, so the classes and constants it
  was building hung off no root the memory estimator walked, and every check
  running inside its initialization measured a graph that did not contain them.
  That environment is now reachable by the estimator while the module
  initializes.
- **Fixed: string character-set scans are now charged for the entries they
  compare.** `count`, `delete`, `tr` and `squeeze` charged one step per receiver
  character but compared that character against every entry of every character
  set, so a large non-matching set bought the product of both arguments for the
  price of the receiver alone: over a fixed 100 KB receiver, growing the set
  from 1 entry to 8192 took `count` from 1.8ms to 750ms and `tr` from 1.6ms to
  5.9s while both charged exactly 100,000 steps either way. Every lookup now
  bills the entries it compares, in the output pass as well as the sizing pass,
  carrying the sub-step remainder so ordinary short sets still cost nothing.
- **Fixed: three static-check paths no longer cost the square of the source.**
  Every one of them runs inside `CheckWarnings` and `CheckedCall`, before any
  script step or memory quota can meter it. Binding a destructured block
  parameter re-derived the same decomposition and rest element for every
  element, so a 4,000-target `(x1, ..., xN, *rest)` inspected 16.0M elements
  against 4,002 now. Modeling a rest parameter rebuilt every aggregate at every
  argument, so `sink(0, 0, ...)` against `def sink(*xs)` allocated 274MB at
  2,000 arguments against 5.5MB now. Return summaries were keyed by a context
  naming every namespace member recorded as possibly reassigned, rebuilt on each
  of the many lookups a statement makes and kept once per call: 800 member
  writes with a call after each allocated 475MB against 68MB now, and a summary
  whose walk does read those members is still separated by all of them.
- **Fixed: `Hash#fetch_values` accumulated results are now visible to the memory
  quota while its block runs.** The method builds its result in a Go local the
  estimator had no root for, so every check performed inside an attached block
  measured a graph missing everything the loop had already retained: a block
  returning an individually permitted value could accumulate past
  `MemoryQuotaBytes` one accepted result at a time. The output is now registered
  as a base-walk root, so each check re-derives what
  the output holds at the moment it runs and deduplicates it against the receiver
  and the arguments. The root covers the results produced so far rather than the
  slice sized from the argument count, so a wide lookup does not pay for its
  whole output on its first miss. Costs are unchanged while the callback leaves
  the base-walk memo intact; a callback that mutates state discards it, and the
  re-walks that forces are described below. Whatever a lookup is charged is settled
  as it returns, so a callback that mutates state and then raises pays what one
  that returns pays, and a rescued failure leaves nothing behind for a later lookup
  to be billed for.
  `VIBES_ESTIMATOR_VERIFY` re-derives every commit from scratch.
- **Changed: a lookup whose callback destructures with a named rest costs more
  steps.** Such a callback is the only one that makes `Hash#fetch_values` weigh
  a binding against the reachable graph, and that weighing
  builds a charge whose construction walk previously reached no counter. It is now
  billed, so the cost scales with the graph the callback is weighed against: four
  such lookups over a tenfold graph cost about 2.8 times the steps. A script doing
  this in bulk under a tight `StepQuota` may need a larger one. Callbacks without
  a named rest are unaffected.
- **Known: re-walking a lookup's retained results is not charged to the step
  quota, and a callback that mutates makes those re-walks quadratic.** When a
  callback mutates anything, the estimator's memo is discarded and the lookup's
  retained results are walked again on the next memory check, so the walking grows
  with the square of the number of results. Registering the output is what
  introduces that shape: the same script is linear without it.

  That walking is deliberately not charged to the step quota. It is triggered by a
  memo whose key is process-wide, so an unrelated script running at the same time
  invalidates it just as a script's own mutation does, and billing the walk let one
  script's mutations push an unrelated script over its `StepQuota` -- a worse
  failure than leaving the walk uncharged.

  It is bounded, but by the quotas rather than by a small constant: the results
  walked are bounded by `MemoryQuotaBytes` and the number of walks by `StepQuota`,
  so the work cannot exceed their product. A mutating `fetch_values` block over a
  wide lookup can still do more estimator work than its step count suggests.
  Charging it accurately needs per-execution mutation tracking, which is left to
  its own change.
- **Known: one nested lookup shape is charged more memory than it uses.** A
  `Hash#fetch_values` block that destructures with a named rest
  (`|(head, *tail)|`), runs inside another iterator's block, and returns a value
  held by the enclosing block's scope is charged for that value twice from its
  second callback onward. A script doing this needs roughly one extra copy of the
  returned value's size in `MemoryQuotaBytes`. Nothing is let through unchecked
  -- the lookup is over-charged, not under-charged -- and the quota still bounds
  what it can allocate. Flat lookups, callbacks without a named rest, and nested
  lookups returning a value held outside the enclosing block are unaffected and
  cost what they did before. This is a known cost of bounding a path that was
  previously unbounded, and the accounting fix is deliberately left to its own
  change.
- **Fixed: a memory quota now counts a frame that a block rebound while it was
  dormant.** The estimator charges a call frame holding only scalars once and
  skips it on later checks, on the understanding that nothing can change it while
  it is dormant. That cached total was dropped only inside the one walk that
  reads `nonBaseParentDepth`, and both a builtin driving a script block and a
  block-iteration region route around that walk, so a block closed over its
  dormant caller could rebind the caller's `Int` to a 400KB string and leave the
  frame charged at its scalar-only 245 bytes. Either shape ran to completion
  under a 404,951-byte quota while retaining two live 400KB strings; both now
  need the 804,361 bytes they hold. The committed total is retracted when a scope
  that could rebind it is pushed, so the invalidation no longer depends on which
  walk the next check takes.
- **Fixed: refining a witnessed hash shape no longer costs the square of the
  writes.** A field write refines the receiver's fact by copying it, so the copy
  every other holder depends on grew with the shape: 2,000 `h[:kN] = 1` writes
  against a growing literal allocated 520MB inside `CheckWarnings`, where no
  script step or memory quota can meter it, and 800 writes of one key against a
  literal whose other field is an 800-field shape allocated 97MB, since counting
  fields rather than the nodes each copy walks missed the second entirely. A
  budget spent by the copies bounds both, and it travels with the fact, so a
  fresh literal elsewhere starts with all of it. The same pairs allocate 11.8MB
  and 5.4MB now. Past the budget the fact gives up claiming to name every key
  and never a key it already names. The asymmetry is what matters: the checker
  rules a branch out from the type of a field the fact names, so a fact that
  stopped naming one would stop ruling that branch out, while the claim to name
  them all decides nothing by itself and costs nothing to give up.
- **Changed: a hash built from a very wide literal may now report mistakes in
  branches the checker used to prove unreachable.** The budget above stops
  refining once a script overwrites a few hundred fields of a literal that names
  a few hundred, with values of a type the literal did not give them; a hundred
  such overwrites is still well inside it, and smaller scripts never reach it at
  all. Past that point the checker gives the hash's shape up rather than keep a
  partial one, so a condition reading one of its fields stops being decided and
  the arm that condition used to rule out is checked like any other. What turns
  up there is real: a mistyped call or an undefined name that would have failed
  had the arm ever run. It is never a complaint about correct code, since an arm
  with nothing wrong in it stays silent, and code that runs either way is
  unaffected, because a field the checker has stopped tracking reads as unknown
  rather than as something else. Keeping the shape instead would have meant
  copying it once per overwrite, which is the cost this fix exists to remove.
- **Fixed: comparing hashes no longer costs quadratic host CPU under a memory
  quota.** Validating an equality walk's sort scratch ran a reachable-graph
  estimate before every allocation, and every check a builtin drives bypasses the
  base-walk memo, so each compared hash paid a full uncached whole-graph walk to
  place 24 bytes of key slice. `array.include?` and its siblings charge one step
  per candidate, so probing an array of n small hashes ran n uncached graph walks
  for n charged steps: quadratic host work under a linear step budget, on a
  process shared with other tenants. Probing 800 one-entry hashes falls from
  4,154,117 estimator node visits to 302,917, and from 498ms to 36ms at 1600
  candidates. Scratch is now counted for as long as a walk holds it, so the
  periodic quota check and every call-root and admission estimator account for it,
  and it is repriced against a granule derived from the configured quota rather
  than a constant sized for the default profile.

  **Known residual, relevant if you configure a small `MemoryQuotaBytes`.** A
  single comparison may reach up to one granule of transient sort scratch — a
  256th of the configured quota, so 64 KiB under the default 16 MiB profile and
  proportionally smaller on smaller quotas — before that footprint is validated
  against the quota. The scratch is released when the comparison ends and does not
  accumulate across comparisons, so the peak exposure is one such window rather
  than a sum, and it cannot grow without bound. Closing the window entirely would
  mean validating at every comparison, which measures at 15.7x the estimator work
  of the equivalent unvalidated scan and reinstates the quadratic cost above.
  Hosts sizing a quota tightly against physical memory should leave that window as
  headroom.
- **Fixed: a small result of a large string no longer holds the whole string.**
  A Go substring shares its source's backing allocation, so a member returning a
  window onto its receiver kept the entire receiver alive while the memory quota
  priced the window by its own length. Keeping 200 one-character results of a
  megabyte each held 192.2 MiB under an 8 MiB quota. `strip`, `lstrip`, `rstrip`
  and their `!` forms, `chomp(sep)` and `chomp("")` and theirs, `delete_prefix`,
  `delete_suffix` and theirs, the parts of `split`, the lines of `lines` and
  `each_line`, and the matches of `scan` now copy what they return, so a string's
  footprint equals its length. `byteslice`, `slice`, bracket reads and
  `partition` were already copied. A result that spans its whole receiver is
  still handed back untouched and costs nothing. Argumentless `chomp`, `chop` and
  `chop!` deliberately still return a window: they remove at most a couple of
  bytes, so reaching a meaningful gap costs one call per byte — which the step
  quota already prices — while copying would make two constant-cost methods scale
  with the receiver.
- **Changed: these members now allocate their result, which a tight memory quota
  can notice.** The bytes were always charged; they are now actually used, and
  they are reserved before they are allocated, so a call that cannot fit its
  result is rejected before making it rather than after. Trimming a 64 KiB string
  that is almost entirely padding takes 6.6µs where it took 1.7µs, splitting a
  60 KB document into 10,000 fields 6.6% longer, and `lines` over 2,000 lines
  12.7%; a receiver with nothing to trim is unchanged. Walking a string with
  `each_line`, or `scan` with a block, costs about 10-12% more when a memory
  quota is configured and is unchanged without one.
- **Fixed: a deeply nested value no longer makes an in-place hash or array
  update report a mismatch against correct code.** An update keeps what the
  checker knows about the container only when the operands it evaluated left the
  variable holding the same value, and that comparison was made from a
  canonicalized key that stops at eight levels of nesting. A variable rebound
  while the update's own operands were being evaluated — to a value differing
  from the old one only below that depth — therefore looked untouched, so the
  update was applied to the value the variable used to hold and put it back.
  Reading the rebound field afterwards answered from the old value, and a call
  taking the new one was reported. The comparison now proves the two values are
  the same instead of grouping them, which is also cheaper: 1,600 `store` calls
  on a growing hash allocate 4.4MB against 11.2MB.
- **Fixed: comparing two type facts that share their nested values no longer
  takes exponential time.** A fact built by repeatedly nesting a value under
  more than one key — `x = { a: x, b: x }` — has as many paths through it as it
  has combinations of keys, but only as many distinct values as lines that built
  it. The exact comparison the checker uses to decide whether two facts are the
  same walked those paths rather than those values, so comparing two
  independently built copies of a 20-line fact of that shape took two million
  comparisons and every further line doubled it, hanging `CheckWarnings` on a
  script well inside the source-size limit. It now remembers the pairs it has
  already proved equal, which reaches each pair once: the same comparison takes
  68. Scripts that do not nest a value under more than one key are unaffected and
  allocate nothing extra.
- **Fixed: an array drained with `shift` no longer holds the slots it gave up.**
  Removing from the front narrows the receiver onto a window further into the
  same allocation, and Go keeps an allocation live as a whole for any pointer
  into it, so the vacated slots stayed held by an array that could no longer
  reach them. The memory estimator prices a header at its capacity plus its
  elements, both of which a forward reslice shrinks, so it stopped charging for
  them too: a 4096 element array drained to one element was charged 377 bytes
  while holding 131,072, and sixty-four rounds of building and draining one held
  7.98 MiB against a charge that rose by 10,563 bytes, without bound. A fully
  drained array now moves onto fresh empty storage, and a partially drained one
  is compacted once the prefix it has vacated grows larger than what it still
  shows. `pop` was unaffected in its charge, since it does not move the start of
  the header, but it held the same storage and now releases it too.
- **Changed: a `shift` that compacts allocates and can be refused.** The copy is
  charged and reserved before it is made, so a shift that cannot fit one is
  rejected and leaves the array holding every element it did. Draining costs
  9.00 steps per element where it cost 8.00, and stays linear: a copy of m
  elements happens only after more than m have been removed, so a drain of any
  size copies fewer elements than it removes.
- **Fixed: an array's wrapper is charged the memory it actually occupies.** The
  struct the runtime boxes every array's elements in was exactly a slice header,
  so the estimator's slice-base charge priced it by accident. The shrink above
  adds a field to it, which would have left 8 bytes per array unmetered -- an
  under-count introduced by a fix for an under-count -- so the charge is now
  derived from the struct and the next field is priced by the commit that adds
  it. Every projection that reserves a new array was moved with it, so a build
  and the walk that supersedes it still agree to the byte -- including the
  destructured-rest and range/string materialization preflights, which promise
  the allocation will fit before making it and could otherwise allocate an array
  wrapper with 8 bytes fewer than it needs. Adding the two constants together to
  price an array is now a build failure rather than a convention, since the
  helper alone does not stop the next projection from restating the old formula.
- **Fixed: the estimated slice header size is derived rather than stated, so
  memory accounting is correct on 32-bit targets.** It was hard-coded to the
  64-bit value of 24 bytes, which over-charged every slice, string table and
  array by 12 bytes on the 386 builds in the release matrix. Deriving the array
  wrapper's charge from its struct is what exposed it: the two are subtracted
  from each other, and on 386 the stated 24 exceeded the whole 16-byte wrapper.
  Nothing changes on 64-bit, where the derived value is the same 24.
- **Fixed: builtins that publish inner arrays reserve the wrapper each one
  allocates.** `zip`, `product`, `combination` and `permutation` priced every row
  as a bare slice, so a wide call allocated one array wrapper per row beyond what
  the quota had admitted; `group_by` did the same once per group, and
  `String#scan` once per match with captures. `partition` missed the two its
  result holds. The projection helpers now name which of the three referents they
  price -- an array that owns its Value, an inner array whose Value a surrounding
  backing already counts, or a slice nothing ever boxes -- since the arithmetic
  is identical and only the referent differs.
- **Fixed: a block body calling a builtin is no longer quadratic in its
  receiver under a memory quota.** `rows.map { |r| r.to_s }` re-walked the whole
  receiver once per element, because builtin dispatch invalidated the memory
  estimator's memo and a memory check inside a builtin bypassed it.
- **Added: `vibes.DeclareNonMutating`.** A host builtin can declare that it
  writes to nothing reachable from its receiver, arguments, keyword arguments,
  block, or an execution's roots, and the runtime stops paying for the
  assumption that it might. This is a safety promise rather than a performance
  hint: an untrue one lets an execution allocate past its `MemoryQuotaBytes`.
  Undeclared builtins are unaffected.
- **Added: `vibes.DeclareNonRetaining`.** A host builtin can promise that it
  retains no reference to anything it receives or returns and that its output
  shares no storage the host already holds. The host boundary consults the
  promise: inputs skip retention marking, returns skip their detach copy, and
  values exchanged through `Execution.CallBlock` avoid boundary copies. This is
  a safety promise, not a performance hint; an untrue declaration creates a live
  aliasing channel. Undeclared builtins keep the conservative boundary behavior.
- **Removed: the `Tasks` namespace and script-visible `sleep`.** Per
  [ADR-006](docs/adr/006-slim-language-for-predictable-sandboxing.md), hosts own
  concurrency and delay. `Tasks.run`, `Tasks.map`, `tasks.spawn`, `tasks.wait`
  and `task.value` are gone, as is `sleep`. Scripts that fanned out with
  `Tasks.map(items, with: :work)` move that loop to the host, which runs
  independent `Script.Call` invocations concurrently — separate execution state
  per call, so the host's own goroutine pool, tracing, cancellation and
  rate limits apply — or exposes a bounded batch operation as a capability with
  its own aggregate limits. Scripts that used `sleep` to model a delayed
  workflow step move it to the host's timer or durable job system, which owns
  the step's lifecycle outside an interpreter call.
- **Removed: `Config.DefaultTaskConcurrency`, `Config.MaxTaskConcurrency` and
  `Config.MaxSleepDuration`.** Setting them is now a compile error rather than
  a silent no-op. `QuotaProfile.MaxSleepDuration` is gone with them, so a
  profile is a bundle of three quotas rather than four, and `ConfigSummary`
  reports `steps`, `memory` and `recursion` only. Step, memory and recursion
  remain the sandbox budgets and are unchanged.
- **Changed: the memory quota no longer spans a chain of nested calls.** The
  chain existed to stop nested task levels each receiving the host's whole
  allowance; with task nesting removed there is no such chain, so
  `MemoryQuotaBytes` is again one bound on one execution's reachable graph. A
  host that reached a callee on another engine through a context carrying a
  chain node no longer passes a ceiling to it; every other path is unchanged.
- **Removed: escaping callables; blocks are enforced-nonescaping.** Proc and
  lambda constructors, stabby-lambda literals, first-class function and
  bound-method values, callable `.call`, block capture and forwarding with
  `&`, symbol-to-proc, and hash default procs are gone; each spelling is a
  compile error naming the replacement. A block stays syntax attached to a
  call, runs synchronously under `yield`, and is retired when the receiving
  call returns, so a late invocation -- however the block was retained -- is
  a hard runtime error rather than a documented host contract. Migrate a
  callable value to a named function called where it is needed, or a block
  written at the call that runs it: `words.map(&:upcase)` becomes
  `words.map { |word| word.upcase }`. (#1206)
- **Changed: modules are namespaces, not behavior injection.** `include` and
  `extend` are removed, along with instance-style module methods, module
  accessors and aliases, the constants an include copied into the including
  class, and the `is_a?`/type relationships inclusion created. A module holds
  constants, nested modules, and `def self.` functions, and `Outer::Inner`
  scoped resolution is unchanged. Classes remain, still without inheritance.
  Mixins introduced a hidden method and constant source, a collision order, a
  transitive membership graph, visibility copying, and per-execution accounting
  for adopted state; explicit namespace calls provide the same reuse with the
  dependency visible at the call site
  ([ADR-006](docs/adr/006-slim-language-for-predictable-sandboxing.md)).

  Migration: move each mixed-in method to `def self.name(receiver, ...)` on the
  module and call it there, so `person.display_name` becomes
  `Naming.display_name(person)`. Read a module's constants through the module
  (`Limits::MAX`) rather than by the bare name an include used to supply. Replace
  an `is_a?(SomeModule)` test or a `(value: SomeModule)` annotation with the
  concrete class, a union of classes, or a duck-typed check. `include` and
  `extend` in a class or module body, and a plain `def`, `property`, `getter`,
  `setter`, or `alias` in a module body, are compile errors naming the
  replacement.
- **Changed: hashes use one string keyspace.** Strings and symbols are the only
  accepted hash keys, a symbol normalizes to its string, and `hash["name"]`,
  `hash[:name]` and the label `name:` all address one entry, so `keys` returns
  strings and a JSON round trip no longer changes lookup behavior. Every other
  key type is rejected. Arbitrary keys made the memory boundary a graph problem
  -- recursive canonicalization, cycle detection and per-occurrence accounting
  for composite keys -- while the separate string and symbol keyspaces made
  ordinary JSON-shaped data behave differently depending on which spelling built
  it ([ADR-006](docs/adr/006-slim-language-for-predictable-sandboxing.md)).
  Insertion order, `nil` on a missing `[]`, `fetch` fallbacks, and any
  Vibescript value as a hash *value* are unchanged.

  Migration: convert a computed key explicitly, usually with `to_s`
  (`counts[id.to_s]`); the rejection names the conversion. `group_by` and
  `tally` produce keys the same way, so `words.group_by { |w| w.size }` becomes
  `words.group_by { |w| w.size.to_s }`. Code that relied on `:name` and `"name"`
  being different entries now has one entry holding the later write, and a
  literal such as `{ name: 1, "name": 2 }` is a single entry. A block that
  compares a yielded key against a symbol (`if key == :id`) must compare against
  the string (`if key == "id"`); symbols are unchanged as values, only keys
  normalize. `hash<symbol, V>` keeps working and describes the same hash as
  `hash<string, V>`.

- **Removed: per-hash default values.** `Hash.new` takes no argument and no
  block, and `hash.default` / `hash.default_proc` are gone. A default stored on
  a hash made every missing-key read a potential script callback -- with its own
  effects, accounting and type-checking surface -- for a fallback the call site
  can state directly.

  Migration: replace `Hash.new(0)` with `{}` and read misses through
  `hash.fetch(key, 0)`, which supplies the fallback per lookup without inserting.
  Replace a default proc that filled the hash with an explicit store where the
  miss is handled (`cache[key] = build(key) if cache[key].nil?`). `dig` and
  `values_at` now contribute `nil` for a missing key rather than consulting a
  stored default.

- **Fixed: a hash store and a hash transform charged inconsistent memory.** The
  charged-commit path for `hash[key] = value` added an entry slot when the write
  grew the hash past its reserved capacity, when that slot is only ever consumed
  rather than added, so a store loop drifted above the reference walk. The final
  admission before a hash transform allocated its output also missed the
  insertion-order backing that the matching reservation charges, so it admitted
  outputs the reservation then refused. Both now agree with the reference walk.
- **Changed: arrays and hashes are values.** Binding, passing, or returning a
  collection produces another logical value, so updating one binding or path can
  no longer be seen through a sibling ([ADR-006](docs/adr/006-slim-language-for-predictable-sandboxing.md)
  item 2). A mutating operation updates the local, instance variable, or nested
  path its receiver names; a receiver naming no such path is a temporary whose
  update is returned and reaches nothing else. `==` remains content equality and
  `equal?` now answers the same question, since collections carry no identity.
  The runtime uses copy-on-write internally: binding stays free and a copy is
  paid only where a write meets a value something else can still see.
- **Removed: the collection bang variants that duplicated a non-bang form.**
  `map!`, `sort!`, `reverse!`, `uniq!`, `compact!`, `select!`, and `reject!` on
  arrays, and `merge!` with its alias `update` on hashes. Reassign the non-bang
  result (`a = a.sort`, `h = h.merge(other)`), or use `keep_if` / `delete_if`
  where `select!` / `reject!` updated in place.
- **Changed: the host boundary hands out independent values.** Every collection
  crossing between host Go code and script state is now independent of the
  other side: `Script.Call` results clone whenever their graph is shared,
  host-builtin arguments isolate from every script slot, host-builtin returns
  and values exchanged through `Execution.CallBlock` detach from any backing
  the host retains, and wrappers a factory installs into its capability object
  never transfer out live. Two embedder-visible channels are gone with this: a
  builtin can no longer publish behavior or results by mutating an argument
  (write into the receiver, the sanctioned factory channel, or return the
  value), and host writes through retained handles no longer reach
  script-observable state. Builtins that declare `DeclareNonRetaining` keep
  copy-free crossings; first-party capability adapters declare non-mutation,
  so their single internal copy is the only one. Adapters should not
  defensively pre-clone returns anymore -- the boundary detaches them. (#1210)
- **Improved docs: the lightweight 1.0 language boundary is now explicit.**
  The README, overview, reference, module guides, and runnable examples now
  distinguish direct calls, synchronous blocks, value collections, namespace
  modules, and host-owned scheduling from general-purpose Ruby features.
- **Fixed docs: leftover pre-ADR-006 teaching is gone.** Collection `equal?`
  is content equality, including the exported `value.Value.Identical`
  contract, hashes are one string keyspace rather than symbol-keyed, the
  `function` type is not listed as live, and the 1.0 migration guide now
  covers Tasks/`sleep` and the string keyspace. `Hash.new(0)` is a runtime
  error, not a compile error. Hover on `Proc` teaches the callable removal.
- **Fixed: the array-comparison tests no longer fail under the race detector.**
  Four of them enforced wall-clock ceilings to catch an exponential regression,
  and the race detector's slowdown blew those ceilings on work that had not
  changed. An exponential regression does not finish at all, which go test's
  own package timeout already reports.

## v1.0.0-rc9 - 2026-07-27

Ninth release candidate: finishes the memory-quota work rc8 started. `Hash#transform_keys`
was the last block driver still building its result inside the iteration loop, which
re-measured the whole receiver on every check and made remapping grow quadratically with
the entry count. Both its branches now build after the loop, so every hash driver is
linear whether the receiver came from a script or from the host. The benchmark suite gained
the block-driver shapes that made these regressions invisible in the first place.

- **Performance: remapping the keys of a script-built hash no longer degrades
  under a memory quota.** `transform_keys` inserted into its result between block
  calls, which re-measured the whole receiver on every check and made the walk
  grow quadratically with the entry count. It now builds the result after the
  loop. Remapping a 600-entry hash this way is roughly 24x faster.
- **Testing: block-driver benchmarks now cover the shapes that went quadratic.**
  The suite gained coverage for pure versus accumulating block bodies,
  host-built versus script-built hash receivers, and walking versus
  result-building drivers, each wired into the CI smoke gate. Every one of these
  shapes regressed at some point without a benchmark noticing.
- **Performance: remapping the keys of a host-supplied hash no longer degrades
  under a memory quota.** `transform_keys` inserted into its result between block
  calls when the receiver came from the host rather than from a script, which
  re-measured the whole receiver on every check and made the walk grow
  quadratically with the entry count. Remapping a 1000-entry hash this way is now
  roughly 40x faster.

## v1.0.0-rc8 - 2026-07-26

Eighth release candidate: the gradual checker becomes a real typed surface.
Builtin and scalar member contracts move into one runtime-owned registry,
constructors and predicates carry nominal facts through control flow,
unannotated functions and methods expose inferred return summaries, and typed
arrays, hashes, and properties reject incompatible writes. Shapes gain optional
(`age?`) and open (`...`) fields, `is_type?` joins the predicate set, and
embedders gain `Script.CheckedCall`, published host-callable signatures, and
the public `vibes.CheckWarning` type. Hash and scalar workloads no longer
degrade under a memory quota.

- **Performance: accumulating into a local while iterating no longer degrades
  under a memory quota.** Summing or counting into a variable from inside a
  block (`total = total + x`) re-measured the whole receiver collection on every
  iteration, so the loop cost grew quadratically with the collection size.
  Iterating 600 rows this way is now roughly 48x faster.
- **Performance: iterating a host-supplied hash no longer degrades under a
  memory quota.** `each`, `each_key`, `each_value`, `select`, `reject`, and
  `transform_values` re-measured the whole receiver on every check when the hash
  came from the host rather than from a script, so the walk cost grew
  quadratically with the entry count. Iterating a 600-entry hash this way is now
  roughly 50x faster.
- **Performance: transforming and filtering a script-built hash no longer
  degrades under a memory quota.** `transform_values`, `select`, and `reject`
  inserted into their result between block calls, which re-measured the whole
  receiver on every check and made the walk grow quadratically with the entry
  count. They now build the result after the loop. Transforming a 600-entry hash
  this way is roughly 24x faster, and it holds slightly less memory while
  iterating.
- **Fixed: word boolean spellings remain ordinary identifiers.** `and`, `or`,
  and `not` can again be used as variable and function names; boolean logic
  continues to use `&&`, `||`, and `!`.
- **Improved: the checker narrows nullable locals across control flow.**
  Truthiness tests, explicit nil comparisons, and `nil?` predicates with
  proven universal dispatch now refine a local's known type on both branches —
  including `unless`, `elsif`, negation,
  short-circuits, and guard clauses that exit early — so nil misuse inside a
  guarded branch is reported and provably dead branches stop warning.
- **Improved: known unions must fully satisfy typed boundaries.** Calls,
  defaults, and returns now reject finite inferred unions when any arm can
  violate the required type, including nested nullable, array, hash, and shape
  facts. `any`, unknown JSON and host values, and dynamic dispatch continue to
  defer to runtime validation.
- **Improved: static checks compare resolved named types.** Different resolved
  enums and classes are now incompatible with each other and with unrelated
  primitives and containers at typed boundaries, while symbols keep coercing
  into enums, classes keep satisfying included modules, and unresolved or
  host-supplied names stay conservative.
- **Improved: constructor results carry nominal class facts.** A statically
  resolved `User.new` now infers as a `User` instance, so the fact flows
  through locals, branches, arguments, and returns, instance methods resolve
  their shapes and result types from the fact, and shadowed class names or
  dynamic constructor dispatch stay unknown.
- **Added: typed signatures in builtin metadata.** Builtin definitions can now
  declare positional, keyword, and result types; the checker validates known
  arguments and infers known results from the resolved builtin, while
  argument-dependent contracts and host overrides stay unknown.
- **Improved: the checker knows core builtin signatures.** Conversions, ID,
  money, Math, Duration, Time, and JSON/Regex helpers now declare fixed
  argument and result types, so provably wrong arguments and misused results
  (`takes_string(to_int("1"))`) are reported by `vibes check` instead of
  failing at runtime. Argument-dependent results stay unknown and host
  overrides still disable the default contracts.
- **Improved: builtin member contracts live in one runtime registry.** The
  checker's static member specs and editor member completion now resolve
  from a single runtime-owned contract table (receiver kind, name, aliases,
  call shape, parameter and result types, effect metadata), and a
  registry-completeness test requires every public member to be registered
  or explicitly exempted. Dispatch for unknown receivers and user-defined
  overrides is unchanged.
- **Improved: the checker knows scalar member contracts.** Conversions such as
  `to_i`, `to_f`, `to_s`, and `to_sym` and universal predicates such as `nil?`,
  `eql?`, and `respond_to?` now declare fixed results resolved from the
  receiver's inferred type, safe navigation adds `nil` to known results, and
  class instances or dynamic receivers stay unknown.
- **Improved: unannotated functions expose inferred return summaries.** Calls
  to a plain script function whose body provably yields known types now carry
  that union — explicit returns, implicit finals, and nil fallthrough
  included — so `takes_string(build_count())` is checked without an
  annotation. Recursive, dynamic, or partially unknown bodies stay unknown,
  and explicit annotations remain authoritative.
- **Improved: unannotated methods expose inferred return summaries.** Calls
  to statically resolved instance and class methods now carry the same
  branch, fallthrough, and cycle-aware summaries as plain functions. Opaque
  or unresolved receivers and universal-member overrides remain unknown,
  while explicit return annotations stay authoritative.
- **Improved: class predicates narrow nominal unions.** `is_a?`, `kind_of?`,
  and `instance_of?` guards against statically resolved classes and modules
  now refine known union locals in both branches (guard clauses included), so
  guarded nominal code satisfies stricter known-union boundaries. Overridden
  predicates, module-typed arms, and dynamic receivers stay unchanged.
- **Added: the `is_type?` predicate.** `value.is_type?(:int)` tests any value
  against a type atom — a primitive (`:int`, `:string`, `:number`, …), a bare
  container (`:array`, `:hash`, `:range`, `:function`), a class or enum name,
  or a nullable form (`'int?'`) — without coercion. The checker gives it a
  typed contract and narrows known union locals through both branches of a
  literal-atom test.
- **Improved: the checker reports incompatible element writes to typed
  arrays.** A local known to be `array<T>` now checks shovel appends, indexed
  assignment, and the in-place mutators (`push`, `append`, `prepend`,
  `unshift`, `insert`, `fill`) against `T`, so a provably incompatible element
  is reported at the write instead of silently corrupting a checked boundary.
  Exact `fill` value, selector, padding, and block outcomes participate in the
  same check. Compatible writes preserve the known element type — including
  through aliases, loops, and blocks — while unknown values, unknown
  receivers, unresolved outcomes, and `array<any>` stay gradual.
- **Improved: the checker reports incompatible writes to typed hashes and
  shapes.** A local known to be `hash<K, V>` now checks `h[k] = v`, `store`,
  and the in-place `merge!`/`update` against the key and value bounds, and a
  local with a declared shape type checks field writes against their declared
  types — including a statically known extra field on an exact shape. Hash
  and shape literals stay freely writable (their known fields update in
  place), and unknown keys, values, and receivers stay gradual.
- **Fixed: direct instance-variable writes honor typed property contracts.**
  `@name = value` inside any method now normalizes and validates against the
  type declared by a generated `property`/`getter`/`setter` accessor when the
  write executes, instead of failing only when a later typed getter or
  boundary observed the value. Compound, logical, destructuring, and `@ivar`
  constructor-parameter writes validate the same way; untyped accessors and
  undeclared instance variables stay fully dynamic.
- **Improved: the checker validates direct writes to typed properties.**
  Instance-method analysis now seeds facts for typed accessor-backed instance
  variables, so a direct write such as `@name = 1` against
  `property name: string` is reported when the value's known type provably
  contradicts the contract, and reads observe the declared type. Unknown
  values still pass and rely on the runtime guard; untyped accessors and
  undeclared instance variables stay dynamic.
- **Changed: capability return validation can no longer be bypassed by host
  adapters (#976).** The public `CapabilityMethodContract` no longer has the
  `ReturnValidatedByBuiltin` field, which let any adapter assert an internal
  runtime proof and skip its declared `ValidateReturn`. The runtime now always
  validates capability method returns; first-party adapters that already
  validate and isolate their results record an internal, unforgeable per-call
  proof instead, so they still avoid validating the same value twice.
  Embedders that set the field should delete it — return contracts are now
  enforced unconditionally.
- **Added: optional shape fields.** A `?` on a shape field name marks the
  field optional: `{ name: string, age?: int }` accepts payloads with or
  without `age`, and a present `age` still validates as `int`. Optionality is
  distinct from nullability (`age?: int?` may be absent or `nil`), fields stay
  required by default, and optional fields work in nested shapes and
  `JSON.parse_as`. The checker infers `T | nil` for optional field reads and
  no longer flags payloads that merely omit optional fields. A bare shape
  field label ending in `?` now spells optionality; a field whose name
  literally ends in `?` takes a string key (`{ "valid?": bool }`).
- **Added: open shape contracts.** A trailing `...` marks a shape open:
  `{ name: string, ... }` requires and validates the declared fields (optional
  fields compose) while letting undeclared extra fields pass unchecked, and
  `{ ... }` alone accepts any hash. Shapes stay exact by default, open and
  exact shapes nest freely, `JSON.parse_as` carries extras through untouched,
  and the checker treats reads of undeclared fields on an open shape as
  unknown instead of `nil`.
- **Added: `JSON.parse_as` validates non-object JSON roots.** Array,
  primitive, nullable, and union contracts now work at the root —
  `JSON.parse_as(raw, array<int>)`, `JSON.parse_as(raw, int?)`,
  `JSON.parse_as(raw, int | string)` — using the same normalization and
  diagnostics as shape roots, with the declared contract carried as the
  checker's result fact. The type spelling is recognized in parenthesized
  call arguments; spellings that also read as values (a local named `int`)
  keep their value reading under the shape-literal shadowing rules.
- **Fixed: inline snippets check like files.** `vibes run -check -e` now runs
  the same whole-script pass as `vibes check`, so a typed contradiction inside
  a function or method the snippet never calls is reported identically for
  equivalent inline and file sources. Top-level execution order and require
  semantics are unchanged.
- **Added: `vibes.CheckWarning` is the public checker diagnostic type.** The
  `CheckWarnings*` family already returned these values, but embedders could
  not name the type outside inference. The stable alias carries the function,
  source position, message, and originating module path.
- **Added: `Script.CheckedCall` static gate.** One opt-in API checks the exact
  call — function, argument values, and options — and executes only when the
  checker reports no diagnostics, returning static warnings separately from
  runtime failures. The ordinary Call API stays gradual.
- **Added: host callables can publish static signatures.**
  `Engine.RegisterBuiltinWithSignature` and `vibes.NewTypedBuiltin` accept an
  opt-in `Signature` (positional parameter types, optional parameters, result
  type, block policy) written in the annotation grammar. The checker validates
  known arguments and infers the declared result for engine builtins,
  call-option globals, and capability methods, and the same contract is
  enforced at runtime. Callables without a signature stay fully dynamic.
- **Improved: the checker keeps container facts across pure member calls.**
  Member contracts now classify receiver effects (pure, mutates-receiver,
  or unknown), and calls the registry proves pure — reads like `a.at(0)`
  and universal predicates — no longer discard the receiver's or its
  aliases' inferred facts. Mutators, unregistered members, blocks, impure
  arguments, reads that may return a nested mutable element, dynamic
  dispatch, and user overrides stay conservative.

## v1.0.0-rc7 - 2026-07-16

Seventh release candidate: builtin discovery now follows the runtime registry
across the REPL and language server, and the urfave CLI migration is completed
with consistent argument binding, command I/O, and cancellation.

- **Fixed: CLI I/O and cancellation are consistent after the urfave migration
  (#956).** `run`, `check`, `analyze`, `fmt`, and `test` use each command's
  configured streams and report write failures instead of silently succeeding.
  Long-running commands inherit Ctrl-C cancellation, and `lsp` and `repl` help
  no longer advertise positional arguments they reject. Existing flag and
  positional-argument behavior is preserved.
- **Fixed: REPL discovery covers the full builtin surface (#957).**
  `:functions` now lists every runtime callable instead of a 19-name manual
  subset. Tab completion covers each registered global, namespace member and
  constant, and parser keyword, so `Duration`, `JSON.parse_as`, `Math.PI`, and
  `unless` complete correctly; the nonexistent `fn` keyword no longer does.
- **Added: LSP signature help for qualified builtins (#957).** Calls such as
  `JSON.parse_as(...)` now show their documented signatures. Completion, hover
  classification, signatures, checker contracts, and namespace documentation
  share the runtime registry and `docs/builtins.md`; coverage checks reject
  both undocumented runtime additions and stale documentation.

## v1.0.0-rc6 - 2026-07-15

Sixth release candidate: the hash-rocket syntax that #867 accidentally
reinstated is removed again, the CLI moves to urfave/cli, and the language
server now serves real documentation — hover and completions cover builtins,
namespaces, keywords, the stdlib member surface, and user-defined symbols with
scope-aware resolution.

- **Removed: hash rocket (`=>`) syntax, again.** The Ruby stdlib compatibility
  batch (#867) unintentionally reinstated the hash-rocket literal syntax that
  #762 had removed, and #599 extended rockets into shape type annotations.
  Rockets in hash literals (`{ expr => value }`) and in shape annotations
  (`def f(p: { "user-id" => string })`) are parse errors again, reported as a
  single targeted hash-pair diagnostic. The supported spellings are unchanged:
  colon keys (`name:`, `"name":`) in hash literals, index assignment
  (`h[key] = value`) for runtime-computed keys, and colon shape-field
  separators, including string, symbol, and quoted-symbol field names. The
  arbitrary-key runtime behavior from #867 (integer, array, range, and other
  hashable keys with Ruby-style string/symbol identity) is retained; only the
  literal syntax is gone. Rescue bindings (`rescue RuntimeError => err`) are
  unaffected.
- **Changed: the `vibes` CLI now uses urfave/cli v3.** Every command has
  generated `--help` output; successful help now exits zero on stdout. Flag
  parsing still stops at the first positional argument so script arguments
  keep their existing behavior. Unknown commands now name the rejected token,
  and `analyze`, `lsp`, `repl`, and `help` reject undocumented extra positional
  arguments instead of ignoring them.
- **Added: LSP hover and completion documentation.** Hovering a builtin, a
  namespace member (`JSON.parse_as`, `Math.sqrt`), or a keyword now shows its
  documentation from `docs/builtins.md`, and completion items carry the same
  docs plus signature details. The reference gained entries for the output,
  proc/lambda, `Regexp`, `Duration`, `Time`, and `Tasks` builtins, with drift
  gates so new builtins cannot ship undocumented.
- **Added: LSP hover for value member methods.** Hovering a method after a
  `.` receiver (`items.map`, `name.upcase`, `h.fetch`) now shows its entry
  from the stdlib reference and the per-type guides; names shared by several
  types (`size`) render one section per receiver type. Unambiguous members
  carry the same docs in after-dot completion, and a drift gate keeps the
  parsed table honest against the runtime's member dispatch.
- **Added: LSP hover for user-defined symbols.** Hovering a function, class,
  module, enum, or enum member declared in the current document shows its
  reconstructed signature (parameter types, defaults, return type) plus the
  `#` comment block above the declaration.

## v1.0.0-rc5 - 2026-07-12

Fifth release candidate: development-time module reloading for embedding hosts.
`Config.DevMode` keeps a long-running engine in sync with edits to its required
modules while preserving the existing production cache behavior by default.

- **Added: `Config.DevMode` for development-time module reloading (ADR-005).**
  When enabled, every `require` revalidates its cached module against the
  source file's mtime+size and recompiles it on change, and require misses are
  re-resolved from disk so newly created modules load without a restart. The
  zero value keeps production behavior: modules compile once and are served
  from cache until `ClearModuleCache`.

## v1.0.0-rc4 - 2026-07-10

Fourth release candidate: static checking for typed boundaries (ADR-004). The
check path now infers local types and errors wherever known types contradict a
typed boundary, while unknowns stay permitted — the runtime checks remain the
final guard. A new top-level `vibes check` command reports every statically
checkable contract issue across a whole script for CI and deployment gates, and
`JSON.parse_as(raw, shape)` parses and validates JSON in one step, with shape
literals now legal in expression position. Whole-script checks follow the
entrypoint's execution order, and diagnostics carry their source file.

- **Added: static local type inference on the check path (ADR-004).** Locals
  now take the types of the expressions assigned to them, and `vibes run
  -check` errors wherever known types contradict a typed boundary: wrong
  argument types through local bindings, annotated returns, conflicting
  sequential reassignment, and operators whose operand kinds the runtime
  provably rejects. Sibling branches merge into unions, loop and block bodies
  degrade to unknown, and unknown values (JSON payloads, host globals, dynamic
  dispatch) are never rejected — the runtime checks remain the final guard.
- **Added: `vibes check <script>`.** A top-level command that reports every
  statically checkable contract issue across the whole script — all functions,
  class methods, and top-level code — without executing anything, printing
  `path:line:column: message (function)` per issue and exiting non-zero for CI
  and deployment gates.
- **Added: `JSON.parse_as(raw, shape)`.** Parses JSON and validates the result
  against a shape in one step, raising the standard typed-boundary error on
  mismatch; the checker treats its result as that shape, so downstream field
  access is inferred and checked. Shape literals are now legal in expression
  position: a braced group whose field values all name built-in types is a
  first-class shape value, while hashes with value expressions, unknown
  identifiers, or type names shadowed by any runtime binding (locals, host
  globals, implicit self members, engine builtins) keep their existing hash
  semantics. The checker also validates `parse_as` calls statically — a
  provably non-string input or non-shape schema reports before runtime.
- **Changed: whole-script checks follow the entrypoint's execution order.**
  Top-level requires seed their exports for later function checks, and
  functions the top-level code calls are checked under the runtime state at
  each call site — a call before a `require` neither resolves nor validates
  the module's contracts, exactly as `vibes run` executes it.
- **Changed: `vibes check` diagnostics carry their source file.** Warnings
  that originate in a required module print the module's own path, so CI
  annotations and editors jump to the right file. Per-call checks
  (`vibes run -check`, `CheckWarningsForCall`) also bind concrete host
  argument and default values into unannotated parameters, and inferred facts
  track key representations (symbol- versus string-keyed stores), container
  aliasing, and in-place mutation so reported contradictions match what the
  runtime rejects.

## v1.0.0-rc3 - 2026-07-09

Third release candidate: a performance and ergonomics pass on the sandbox. Named
quota profiles (`low`/`medium`/`high`/`xhigh`) bundle the step, memory, and
recursion quotas into one budget you select by name, and the `vibes` CLI now
defaults to `xhigh` — unlimited steps and memory — so it runs your scripts like a
normal interpreter. The memory-quota estimator is substantially faster, so
enforcing a cap costs far less. One breaking change: the zero-value embedding
default was raised to the `low` profile, so hosts that relied on the previous
tighter default must now set the quotas explicitly (see the entry below).

- **Performance: script-function calls reuse their argument backing.** Each call
  evaluated its positional arguments into a freshly allocated slice. Calls to
  script functions now borrow the backing from a per-execution free list and
  return it once the call unwinds — safe because argument binding copies every
  value into the callee's environment and never retains the slice. Combined with
  the non-local-return change, this cuts recursive fib(20) from 65,704 to 21,947
  allocations per call. Calls to builtins and capabilities, which may retain the
  slice, are unaffected.
- **Performance: function returns no longer heap-allocate.** The non-local
  return check ran on every function return through `errors.As`, whose
  address-of-local argument escaped to the heap — one allocation per return even
  on the common path where no block return was in flight. It now fast-paths a
  nil error and an unwrapped signal without allocating, cutting roughly a third
  of the allocations on deep-recursion workloads.
- **Added: a deep-recursion call-path benchmark and CI smoke gate.**
  `BenchmarkExecutionRecursiveFib` exercises naive recursive `fib`, which grows
  the environment stack with call depth — a shape none of the existing
  loop-based benchmarks covered — so call-setup allocation regressions are now
  caught in CI. A paired `BenchmarkExecutionRecursiveFibQuota` measures the same
  recursion under a memory quota for profiling the estimator path.
- **Changed: `vibes run`, `vibes test`, and the REPL now default to the
  `xhigh` quota profile** — unlimited steps and memory with a high finite
  recursion cap — so CPU-heavy scripts (deep recursion, long loops) run out of
  the box instead of tripping the embedding sandbox's small default quotas. Pass
  `-profile low|medium|high|xhigh` to simulate a constrained sandbox budget, or
  `-step-quota` / `-memory-quota` / `-recursion-limit` (with `-1` for unlimited)
  to override an individual quota on top of the selected profile.
- **Added: an explicit `Unlimited` quota spelling and named quota profiles.**
  `Config` now distinguishes an unset quota (zero, which selects the built-in
  default) from an explicitly unbounded one (`Unlimited`), so a host can lift a
  step, memory, or recursion ceiling instead of only tightening it. The named
  profiles `ProfileLow`, `ProfileMedium`, `ProfileHigh`, and `ProfileXHigh`
  bundle the three quotas into a single coherent budget; `xhigh` runs a script
  like a normal interpreter (unlimited steps and memory) while keeping a finite
  recursion cap so runaway recursion still fails with a clean error.
- **Performance: running under a memory quota is far cheaper.** Enforcing the
  memory quota re-walked the whole reachable object graph on every check. The
  estimator now skips immutable dormant frames, charges block iteration
  incrementally by memoizing the reachable-graph prefix beneath a block scope
  (O(block) per check rather than O(collection)), inlines the per-value guard on
  the hot path, and stops reallocating large hash entries on every check.
  Quota-bounded workloads — deep recursion and large-collection loops — now run
  much closer to their unmetered speed, and the incremental block-scope path is
  cross-checked against an exact reference oracle in CI.
- **Changed: the zero-value `Config` quota default is now the `low` profile**
  (1,000,000 steps / 16 MiB / 256 recursion) instead of the previous
  50,000 steps / 64 KiB / 64. An embedder that sets no quotas now gets the
  lowest named profile as its budget, so `low` is the reproducible name for the
  default embedding sandbox. Hosts that relied on the previous tighter default
  should set the quota fields (or lower explicit values) explicitly.

## v1.0.0-rc2 - 2026-07-07

Second release candidate: arbitrary-precision integers land, and the two
gaps the rc1 artifact smoke test surfaced are fixed. Integer overflow
errors are replaced by promotion — see the migration guide
([docs/migrating-to-1.0.md](docs/migrating-to-1.0.md)) for that and the
parenless-bracket rule's accessor edge.

- **Added: array literals as parenless command arguments.** A bracket
  detached from a non-local callee now opens an array-literal argument,
  matching Ruby's spacing rule: `puts [3, 1, 2].sort` is
  `puts([3, 1, 2].sort)`, and further arguments may follow
  (`concat [1], [2]`). A flush bracket keeps the indexing reading —
  `puts[1]` still tries to index the callee — and a known local indexes in
  every spacing, so `a [0]` and `a [0] = 1` are unchanged when `a` is a
  local.
  Breaking edge: bare accessor names in method bodies are non-local callees,
  so `items [0]` with `getter items` now passes an array argument (an arity
  error at runtime, exactly as in Ruby) instead of indexing the accessor
  value — index through a receiver (`self.items[0]`) or a flush bracket.
  `self [0]` is pinned to keep indexing: `self` is never a command callee.
- **Fixed: `first(n)` works on endless ranges.** `(1..).first(3)` returns
  `[1, 2, 3]` instead of rejecting with `cannot iterate an endless range`: the
  leading window starts at the known endpoint, so it is bounded work despite
  the open end, and it charges the sandbox step and memory quotas like any
  materializer. Genuinely unbounded operations (`each`, `to_a`, `last` on the
  open side) still reject up front, and beginless ranges still reject `first`
  entirely, matching Ruby. A window reaching the integer ceiling stops at the
  largest representable integer rather than wrapping.
- **Changed: integers are arbitrary precision with Ruby-style transparent
  promotion (#919).** Arithmetic (`+ - * / % **`, unary minus), `abs`,
  `succ`/`pred`, `div`/`divmod`/`modulo`/`remainder`, rounding,
  `sum`/`reduce`, and `Float`→`int` conversions promote past the signed
  64-bit range instead of raising `... result out of int64 range`; integer
  literals of any length now parse (previously `invalid integer literal`);
  and `JSON.parse` reads oversized integer tokens as exact integers
  (previously degraded to floats) while `JSON.stringify` emits full
  decimals. Division and modulo keep floor semantics for big operands, big
  vs float comparisons are exact, big values work as hash keys, and typed
  `int` contracts accept them. Hosts get `value.NewBigInt`, `Value.BigInt`,
  and `Value.IsBigInt`; integer literals parse up to a 100,000-digit parser
  guard (larger values remain constructible through charged arithmetic);
  `Value.Int` still returns `0` for out-of-range
  integers (never a truncation), and `ValueToInt64` rejects them. Scripts
  or hosts relying on the old overflow errors as range guards must validate
  magnitudes explicitly (see the migration guide). Deliberate 64-bit
  boundaries error loudly: range endpoints, `times`/`upto`/`downto`/`step`
  iteration bounds, `Money`/`Duration`/`Time` arithmetic, string
  `hex`/`oct`, and index/count/size/precision argument positions. The
  sandbox charges big payloads by size, scales step costs with operand
  words, preflights `**` and oversized products in O(1)
  (`2 ** 10_000_000_000` rejects instantly), and preflights digit counts
  before any base conversion renders.

## v1.0.0-rc1 - 2026-07-06

Vibescript 1.0 aligns the language, the class system, and the collection
standard library with Ruby semantics. Several behaviors changed incompatibly
since v0.50.0; every breaking change is collected with before/after examples
and fixes in [docs/migrating-to-1.0.md](docs/migrating-to-1.0.md).

### Language and syntax

- **Added: Ruby-style control-flow values and rescue forms.** `begin`, `unless`,
  `while`, `until`, and `for` can be used as value expressions; rescue modifiers,
  multiple rescue clauses, rescue `retry`, comma-separated destructuring RHS
  values, leaf statement modifiers, contextual `then`, and `next value` in
  iterator blocks now follow Ruby-compatible behavior.
- **Added: multiple ordered `rescue` clauses.** A `begin` block (and a
  function-level rescue tail) may now carry several `rescue` clauses, each with
  its own error type and `=> binding`. The first clause whose type matches the
  raised error handles it, so handlers order from specific to general exactly
  as in Ruby. Previously only a single clause parsed, forcing handlers to
  collapse error types into one union and losing ordered fallback behavior.
- **Added: Ruby-style rescue modifier expressions.** Expression-position
  `rescue` now works as a fallback form, so code such as
  `x = risky_call rescue fallback` evaluates the fallback when the guarded
  expression raises a recoverable runtime error. The modifier follows the same
  catchability rules as structured `begin`/`rescue`, so sandbox and limit errors
  remain unrescuable.
- **Added: Ruby syntax follow-up batch.** Comma-separated `return` values,
  double-quoted hex/unicode escapes, typed `raise Type, "message"`, multiline
  bounded range endpoints, order-independent enum union normalization, scoped
  class constants (`Config::LIMIT`), `alias`/`alias_method`, qualified module
  enum type annotations (`mod.Status`), and parenless operand/block forms.
- **Added: quoted symbol literals.** Symbols can now be written with double or
  single quotes (`:"foo-bar"`, `:'foo bar'`, `:""`) so they can hold
  punctuation, spaces, or be empty, matching Ruby. Quoted symbols use the same
  escapes as the matching string quote and are accepted anywhere a symbol
  literal is, including type-shape field names. A quoted-string hash key such as
  `"foo-bar":` is the same symbol as `:"foo-bar"`. Interpolation inside symbol
  literals is not supported, so `:"a#{b}"` is a parse error.
- **Added: uppercase `%W` and `%I` percent arrays.** These are the
  interpolating companions to `%w` and `%i`: each entry is processed with
  double-quoted string semantics, so `#{...}` is expanded and the usual escape
  sequences (`\t`, `\n`, and so on) apply, while entries still split on
  whitespace that is neither escaped nor inside an interpolation. `%W` builds an
  array of strings and `%I` builds an array of symbols, matching Ruby. The
  lowercase `%w`/`%i` forms keep their literal behavior unchanged.
- **Added: Ruby-style exponent (scientific) notation numeric literals.** Float
  literals may now carry an `e`/`E` exponent marker with an optional sign and
  one or more exponent digits (`1e3`, `1.5e-2`, `1E6`, `1e1_0`). Any literal
  with an exponent is a float even without a decimal point, so `1e3` is
  `1000.0`, matching Ruby. Underscores remain visual separators between exponent
  digits, and an exponent that overflows the 64-bit float range saturates to
  `Infinity` as in Ruby. A numeric literal that directly abuts an identifier
  (`1e3foo`, `123abc`, `1.5x`, `1e`, `1e_3`) now reports a clear parse error
  instead of silently splitting into a number followed by an identifier, and
  committed-but-malformed exponents (`1e+`, `1e3_`, `1e3__4`) are rejected the
  same way. Keyword suffixes stay valid so Ruby modifier forms like `5if cond`
  and `1e3if cond` continue to parse.
- **Added: Ruby-style numeric base prefix literals.** Integer literals now
  accept `0x`/`0X` hexadecimal, `0b`/`0B` binary, `0o`/`0O` octal, and `0d`/`0D`
  explicit decimal prefixes, with underscores permitted between digits in any
  base (`0xDEAD_BEEF`). A prefix must be followed by at least one valid digit,
  and a prefixed literal may not carry a fractional part or trailing letters;
  such malformed literals now fail at parse time with an `invalid numeric
  literal` diagnostic instead of leaving a stray identifier that produced a
  confusing undefined-variable error. A bare leading zero (`010`) remains
  decimal rather than legacy octal.
- **Added: Ruby-style hash value omission shorthand.** A label key followed
  immediately by `,`, `}`, or end-of-input now reads the local variable of the
  same name, so `{ name: }` is shorthand for `{ name: name }`, matching the
  call-site keyword shorthand (`greet name:`). Omission applies only to label
  keys; quoted keys such as `{ "name": }` are still rejected, and an undefined
  local reports the usual undefined-variable error.
- **Added: Ruby-style optional keyword-only parameters.** `def f(a: 0)` now
  declares an optional keyword-only parameter whose default applies when the
  label is omitted, distinct from the required keyword form `a:` and the typed
  positional form `a: int`. A later default may reference an earlier parameter
  (`def g(a:, b: a + 1)`), keyword-only parameters still reject positional
  arguments, and `a: nil` reads as the keyword default `nil` while a nil-leading
  union annotation (`a: nil | int`) stays a typed positional parameter. Defaults
  evaluate under the sandbox step and memory quotas. The token after the colon
  disambiguates the forms: a bare type name stays a typed positional parameter,
  so wrap a bare-identifier default in parentheses (`a: (other)`) to force the
  keyword form. Expression defaults are supported, including a comparison against
  an earlier parameter (`def f(limit:, ok: limit < 10)`) and a hash literal
  (`def f(opts: { retry: 3 })`, `def f(opts: {})`, one with `nil` values like
  `def f(opts: { previous: nil })`, or one with a nested empty hash like
  `def f(opts: { headers: {} })`); the `name: { field: Type }` shape-type
  spelling stays a typed positional parameter, and a built-in generic container
  type (`def f(array, values: array<int>)`) is never shadowed by a value local
  of the same name. A brace group whose field values all parse as types but whose
  shape is structurally invalid, whether a duplicate field
  (`def run(payload: { id: string, id: int })`) or a missing field separator
  (`def run payload: { id: string name: int }`), surfaces its shape diagnostic
  instead of being silently reinterpreted as a hash default. A keyword default is
  never evaluated when the call shape can never bind: a missing required keyword
  or a leftover positional argument is reported before any default runs, so a
  default's side effects, errors, or quota cost cannot mask the real arity or
  keyword mismatch.
- **Added: Ruby-style anonymous rest targets in destructuring assignment.** A
  bare `*` may now appear as a rest target, discarding the values it captures
  without binding a name, as in `first, * = values`, `*, last = values`, and
  `first, *, last = values`. This matches Ruby's `*` discard target and joins
  the existing named `*rest` support.
- **Added: Ruby-style `for` iteration over hashes.** A `for` loop may now
  iterate a hash directly, mirroring Ruby's loop over `each`. Each iteration
  binds a two-element `[key, value]` pair (keys exposed as symbols), visited in
  insertion order (see the hash-ordering change below), and participates in the
  sandbox step and memory quotas like array and range iteration.
- **Added: Ruby-style negative indexes and bracket slicing for arrays and
  strings.** Bracket access now mirrors `Array#[]` and `String#[]`: a single
  index counts a negative value back from the end and returns `nil` when out of
  range rather than raising (`[10, 20, 30][-1]` is `30`, `[1][5]` is `nil`),
  `value[start, length]` returns a subarray or substring, and `value[range]`
  slices by an integer range. Array assignment accepts a negative index
  (`array[-1] = value` updates the last element) but still raises when the index
  falls outside the array. String indexing remains rune-aware.
- **Added: Ruby-style safe navigation operator (`receiver&.member`).** A safe
  navigation read or method call short-circuits to `nil` when the receiver is
  `nil`, and otherwise dispatches exactly like the corresponding `.` access. A
  short-circuited call evaluates neither its arguments nor its block, matching
  Ruby. The operator guards only its immediate access, so `user&.profile.name`
  still dispatches the trailing `.name` on whatever `user&.profile` returns. Safe
  navigation cannot appear anywhere in an assignment target, so `user&.name`,
  `user&.profile.name`, and `user&.items[0]` are all parse errors on the left of
  an assignment rather than silently assigning through `nil`.
- **Added: Ruby-style regex literals and match operators.** `/pattern/flags`
  literals now parse and produce first-class regex values with `source`,
  `flags`, `match`, and `match?` members. The `=~` operator returns the
  character index of the first match or `nil` and `!~` returns whether the
  pattern misses, with string and regex operands accepted in either order. Regex
  values work as `case`/`when` and `===` matchers, are accepted by the string
  pattern helpers (`match`, `match?`, `scan`, `sub`, `gsub`), and
  `Regexp.new`/`Regexp.union` now return them. Supported flags are `i`
  (case-insensitive) and `m` (`.` matches newlines); patterns keep Go's RE2
  syntax and the existing regex guard limits. A slash after a value still
  divides (`10 / 2`), so existing arithmetic is unchanged.
- **Added: parenless command-argument regex literals.** A regex literal can
  now be a parenless call argument, matching Ruby: `match /ID-[0-9]+/` parses
  as `match(/ID-[0-9]+/)`, with flags, escapes, and further arguments working
  exactly like the parenthesized form (`scan /a+/, text`). The parser feeds
  its local-variable table into the decision, so a slash after a known local
  keeps dividing in every spacing — `total /2`, `total /(n + 1)`, and
  `total /-n` stay arithmetic. The implicit `it` block parameter and the
  enclosing class's or module's constants count as locals here, so `it /2`
  in a parameterless block and `LIMIT /2` inside a method of the class that
  assigns `LIMIT` divide too. The splat and block-pass sigils share that
  view, so `it *2` in a parameterless block now multiplies (it previously
  parsed as a splat call that always failed at runtime).
- **Added: Ruby-style beginless and endless ranges.** `start..`, `start...`,
  `..finish`, and `...finish` are supported for slicing (`arr[1..]`, `s[..2]`,
  `values_at`, `fill`, `byteslice`), case/`===` membership (`when 3..`),
  `cover?`/`include?`, one-sided `clamp`, hash keys, and rendering. Every
  iterating helper (`each`, `map`, `to_a`, `size`, `step`, `for`, `min`/`max`,
  `first(n)`/`last(n)`, `rand`) rejects open ranges up front instead of running
  into the sandbox quotas.
- **Added: Ruby-style case equality operator `===`.** Vibescript now parses and
  evaluates `===`, treating its left operand as a matcher and its right operand
  as the value being tested, mirroring `case`/`when` matching. Range matchers
  check membership (`(1..3) === 2` is `true`, `(1...3) === 3` is `false`) and
  every other matcher falls back to `==` (`1 === 1` is `true`, `2 === (1..3)` is
  `false`). Because the scalar path reuses `==`, integers and floats stay
  distinct kinds, so `1 === 1.0` is `false`, unlike Ruby.
- **Added: Ruby-style unary plus.** Prefix `+` is now a valid expression: it
  returns integers, floats, and strings unchanged and raises a clear runtime
  error on any other operand. Strings are immutable values, so `+"x"` yields the
  same string, matching Ruby's observable behavior. A `+` (or `-`) written flush
  against its operand at the start of a line begins a new statement, matching
  Ruby. A sign separated from its operand by surrounding whitespace continues the
  previous line as a binary operator, reusing Vibescript's existing
  indented-continuation rule; this spaced form intentionally differs from Ruby,
  which would start a new statement instead.
- **Removed: hash rocket (`=>`) literal syntax.** Hash literals only accept
  colon-style keys: shorthand labels (`name:`) and quoted string keys
  (`"name":`). Ruby's `=>` syntax is no longer part of the hash grammar, so write
  `{ name: "Ada" }` instead of `{ :name => "Ada" }`. To key a hash on a value
  computed at runtime, assign into the hash with index access after building it.
- **Changed: a newline ends a range at statement level.** `x = 1..` is now an
  endless range and the next line parses as a separate statement, matching
  Ruby; bounded endpoints may still continue onto the next line inside parens,
  brackets, and call arguments.
- **Changed: a same-line `do` block after a parenless call argument now binds
  to the outer call, matching Ruby.** `puts arr.map do |x| ... end` passes the
  block to `puts` (which ignores it), so `map` raises "requires a block";
  parenthesize the receiver call (`puts(arr.map do |x| ... end)`) or use a
  brace block, which keeps binding to the nearest call.
- **Changed: `f /2` where `f` is a zero-arg function or member call now opens
  a regex literal.** Following Ruby's spacing rule (space before the slash,
  none after, non-local callee), the slash starts a command-argument regex
  instead of dividing the call's result. Without a second slash on the line
  this fails loudly with "unterminated regex literal"; with one (for example
  `f /2 + g/i`) the line parses as `f(/2 + g/i)`, silently changing a former
  division chain, so audit division written in this spacing. To keep the
  division reading, space the slash on both sides (`f / 2`), remove the space
  (`f/2`), or call explicitly (`f() / 2`). Locals — including the implicit
  `it` parameter and the enclosing class's constants — are unaffected, and
  `f /= 2` remains a compound assignment. Accessor names (`getter total`)
  are method calls, not locals, so `total /2` inside a method reads as a
  regex argument, exactly as Ruby reads an attr_reader name there.
- **Fixed: double-quoted strings work inside interpolation expressions.** An
  interpolation now extends to its matching `}` even when the embedded expression
  contains its own double-quoted strings or nested interpolations, so common
  fallback and helper shapes such as `"#{name || "guest"}"` and
  `"#{["a", "b"].join(", ")}"` parse instead of reporting an unterminated string
  interpolation, matching Ruby. The lexer no longer guesses where the outer
  string ends by scanning the rest of the input, so an unterminated string now
  reports a clear lexer error and a stray character reports `unexpected character`
  instead of a generic `invalid token`.
- **Fixed: parenthesized function calls bind keyword labels to a positional
  options hash like the parenless form.** When a plain function has no matching
  keyword parameter and exposes a positional options parameter,
  `configure(retries: 3)` now collapses its keyword labels into the options hash
  just as `configure retries: 3` already did, and a typed options parameter is
  validated against the synthesized hash so `configure(retries: "slow")` is
  rejected with the shape mismatch instead of `missing argument`. The same
  binding now applies when invoking a function value through its `call` alias
  and when calling a function value held in a member such as a module function.
  Constructor and method calls keep strict parenthesized keyword binding,
  including an instance method named `call`. A positional argument that follows a
  keyword label inside parentheses, such as `collect(first: 1, "tail")`, is now
  rejected with a parse error matching Ruby (which treats it as a syntax error)
  and the parenless form, rather than silently appending the synthesized options
  hash after the trailing positional.
- **Fixed: anonymous rest targets now parse in block parameter
  destructuring.** A bare `*` discard rest is accepted inside destructured block
  parameters, as in `values.map do |(head, *)| ... end` and
  `do |(head, *, tail)| ... end`, matching assignment destructuring instead of
  being rejected as an invalid block parameter target.
- **Fixed: rest destructuring no longer panics on short right-hand sides.**
  Assignments such as `first, *rest = []` or `first, *, last = [1]` now bind the
  rest target to an empty array (and missing fixed targets to `nil`) instead of
  crashing on an out-of-range slice.
- **Fixed: trailing targets after a rest now bind left-to-right on short
  inputs.** When the right-hand side is shorter than the fixed targets, the
  targets after the rest fill from left to right and pad with `nil` on the
  right, matching Ruby. For example, `a, *, y, z = [1, 2]` now yields `a = 1`,
  `y = 2`, `z = nil` instead of reversing the trailing values.
- **Fixed: destructuring now snapshots the right-hand side before assigning.**
  When a target writes back into the source array, later targets read the
  original values rather than the mutated ones, matching Ruby's whole-RHS
  evaluation. For example, `values = [1, 2, 3]; values[1], *rest = values` now
  binds `rest` to `[2, 3]` (the original snapshot) instead of `[1, 3]`.
- **Fixed: a splat-assignment that begins a line after a continuable
  expression now parses as its own statement.** A line that opens with `*` and
  forms a destructuring left-hand side, such as `*, last = values` or
  `*rest, last = values`, is no longer misread as a multiplication continuation
  of the previous line. This holds even when the `=` lands on the next line via
  the newline-before-`=` continuation (for example `*rest` followed by an
  indented `= values`), and when the target list itself is split across lines
  after a trailing comma (for example `*rest,` followed by an indented
  `last = values`). Genuine multiline multiplication (a line ending or
  beginning with a spaced `*`) still continues as before. The lookahead also
  accepts reserved-word member names, so targets such as `*rest, record.end =
  values` start a new statement instead of failing to parse.
- **Fixed: reserved-word hash labels are accepted consistently.** Hash labels
  and keyword arguments now accept every keyword token that can precede a colon,
  including `begin:`, `rescue:`, `ensure:`, `raise:`, and `export:`. Previously
  these labels failed to parse while other reserved words such as `class:` and
  `return:` were already allowed, so keyword-shaped payload keys worked or failed
  depending on which word they happened to use. This matches Ruby's uniform
  treatment of keyword-shaped labels.
- **Fixed: reject misplaced nullable generic type syntax.** The parser no longer
  accepts a `?` on a generic container name before its type arguments, so
  `array?<int>` and `hash?<string, int>` now raise a parse error pointing to the
  documented spelling (`array<int> | nil`) instead of silently parsing as a
  nullable `array<int>` / `hash<string, int>`. Untyped nullable containers such
  as `array?` and `hash?` (without type arguments) are still accepted.

### Classes, modules, and visibility

- **Added: Ruby visibility directives in class bodies.** `public`, `private`,
  and `protected` work as section directives, inline modifiers (including on
  `property`/`getter`/`setter` declarations), and retroactive symbol
  directives (`private :hidden, :other`). Protected methods allow explicit
  receivers when the caller's `self` is an instance of the same class,
  enforced across member, operator, index, and setter dispatch.
- **Added: Ruby-style module namespace declarations.** `module Name ... end`
  declares an in-source namespace: `def self.` module functions dispatch as
  `Billing.code`, constants resolve as `Billing::LIMIT`, modules nest
  (`Outer::Inner`), and module state is isolated per script invocation.
  Modules cannot be instantiated, and misplaced declarations get targeted
  parse errors.
- **Added: `include`/`extend` mixins in class and module bodies.** `include`
  mixes a module's instance methods (visibility, operator/index methods, and
  accessors included) into a class and surfaces its constants as class
  constants; `extend` adds them as class methods. Collisions follow Ruby's
  ancestor order: own definitions win, later includes beat earlier ones,
  `include A, B` prefers `A`, and re-including a module already in the
  ancestry is a no-op. `is_a?`/`kind_of?` and class type contracts recognize
  included modules, including transitively included ones; `extend self`,
  including a class, and referencing an undeclared module fail with targeted
  diagnostics.
- **Added: Ruby-style operator and index method definitions.** Classes can now
  define `+`, `-`, `*`, `/`, `%`, `**`, `<<`, `&`, `==`, `!=`, `<`, `<=`, `>`,
  `>=`, `<=>`, `[]`, and `[]=` as instance methods, and operator, indexing, and
  index-assignment syntax dispatches to them on the receiver. A user `==`
  defines both `==` and (absent an explicit method) `!=`; compound index
  assignment (`c[k] += 1`) composes `[]` and `[]=`. Operator methods are
  instance-only: a top-level operator definition is a compile error.
- **Added: type annotations on generated class accessors.** `property`,
  `getter`, and `setter` declarations now accept a type annotation, and the
  generated methods enforce the same runtime boundary checks as handwritten
  getters and setters. `property name: string` generates a `name -> string`
  getter and a `name=(value: string)` setter, `getter`/`setter` generate the
  matching half, and the type binds per name so a comma-separated declaration
  (`property x: int, y: string`) can mix types while bare accessors stay
  untyped.
- **Changed: a bare `private` section now covers every following definition
  until another section directive, matching Ruby.** Previously it applied
  only to the next method definition.
- **Changed: `private :name` in a class body now retroactively makes the
  named method private.** Previously the symbol argument was accepted but
  inert, so code that relied on it kept the method public; the same code now
  dispatches the method as private.
- **Changed: `module`, `public`, `protected`, `include`, and `extend` are
  contextual keywords in declaration and directive positions.** Previously
  they were plain identifiers everywhere, so a parenless call to a user
  function of the same name could occupy those positions (`protected :b` in
  a class body called `def protected(...)`; `module Config` called
  `def module(...)`). Such scripts no longer run under the old meaning: a
  bare visibility or mixin directive that collides with a same-named script
  function is now a compile error naming the collision, and reinterpreted
  `module` shapes fail with targeted parse or resolution errors.
  Parenthesized calls (`public(:b)`) keep working, assignments
  (`public = 1`) still bind locals, and a bare word naming a local in scope
  still reads the local.

### Procs, lambdas, and blocks

- **Added: Ruby-style callable literals, block forwarding, and call splats.**
  `Proc.new { }`, `proc { }`, `lambda { }`, and the stabby lambda
  `->(args) { ... }` (brace or `do ... end` body) build first-class callables
  invoked with `.call`. Procs keep block semantics — padded arguments,
  single-array auto-splat, and non-local `return` that unwinds the defining
  method (raising `unexpected return`, a rescuable `LocalJumpError`, once
  that frame is gone) — while lambdas
  enforce strict arity (`lambda expects 2 arguments, got 1`) and treat
  `return`, `break`, and `next` as local to the lambda; `fn.lambda?` reports
  which semantics a callable carries. Ampersand block arguments forward
  callables as a call's block: `m(&blk)` passes a captured block through
  (preserving non-local return across the hop), `&fn` forwards a function or
  bound method with its own arity checking, `&:name` is symbol-to-proc with
  public-only dispatch (private methods raise) and operator symbols routed
  through the arithmetic helpers, and `&nil` means no block. Call splats
  expand prepared argument lists in place — `f(*args)`, `f(**opts)`, and
  combined forms with regular arguments and blocks — before binding, so
  arity, keyword, and type errors match the literal spelling and the expanded
  arguments are charged against the step and memory quotas exactly like
  literal arguments; `tasks.spawn(:name, *args, **opts)` forwarding now works.
- **Added: Ruby-style `block_given?`.** Functions and methods can now ask
  `block_given?` whether the current call was supplied a block, returning `true`
  when one was given and `false` otherwise, so optional block APIs branch with
  `if block_given?` instead of letting `yield` raise. It is reserved (it cannot
  be shadowed by a local), reports the enclosing method's block when used inside
  a block, and the parenthesized `block_given?()` form takes no arguments.
- **Changed: `return` inside a block returns from the enclosing method.**
  Matching Ruby's non-local return, an explicit `return` in a normal block now
  returns from the method whose body created the block — ending iteration
  immediately — instead of acting as a block-local return that let the method
  continue. The unwind runs `ensure` blocks, cannot be intercepted by `rescue`,
  and validates typed returns as usual; a block invoked after its method has
  already returned raises `LocalJumpError`. Blocks that relied on `return` for
  an early block-local value should make the value the block's last expression.
- **Changed: parenless `callee *expr` / `callee **expr` is now a splat call,
  and the `-> Type` annotation must sit on the signature line.** Following
  Ruby's spacing rule, `f *n` and `f **n` — a space before the star, none
  after — where the callee is a zero-arg function or member call now parse as
  a call with a splat argument (raising `splat argument must be an array` /
  `keyword splat argument must be a hash` when the operand is not one)
  instead of multiplying or exponentiating the call's value. To keep the
  arithmetic reading, call explicitly (`f() * n`, `b.size() * n`),
  parenthesize the callee (`(f) * n`), or space the operator on both sides
  (`f * n`, `b.size * n`) — all of which evaluate exactly as before. Local
  variables are unaffected: `x *n` is still multiplication. Relatedly, the
  `-> Type` return annotation on `def` must now sit on the signature line: a
  `->` opening the following line parses as a stabby lambda literal instead
  of the previous line's return annotation.

### Collection semantics

- **Changed: Ruby-named collection mutators now mutate their receiver in
  place.** Arrays and hashes are mutable objects with Ruby's reference
  semantics: two variables bound to the same collection observe each other's
  mutations, `equal?` reports object identity (independently constructed empty
  arrays are now distinct objects), and `dup`/`clone` still detach a deep copy.
  Array `push`/`append`, `prepend`/`unshift`, `<<`, `insert`, `fill`, `clear`,
  and `delete_if`/`keep_if` mutate and return the receiver; `pop`, `shift`, and
  `delete` mutate and return the removed value(s) instead of the former
  `{ array:, popped: }`-style result hashes; `map!`, `sort!`, and `reverse!`
  transform in place and always return the receiver, while `select!`,
  `reject!`, `uniq!`, and `compact!` return `nil` when nothing changed. Hash
  `update`/`merge!` fold their arguments (and optional conflict block) into the
  receiver, `store` becomes true index assignment returning the stored value,
  `delete` returns the removed value, `clear` empties in place keeping the
  hash's default and identity, `delete_if`/`keep_if` prune in place, and
  `replace` adopts the argument's entries and default — all preserving
  Ruby-style insertion order. The non-mutating helpers (`map`, `select`,
  `merge`, `sort`, `+`, ...) are unchanged, string bang helpers keep their
  documented value-or-`nil` contract (strings remain immutable values), and
  host isolation is unchanged: arguments and globals are still deep-cloned per
  call, so in-script mutation never leaks into host originals. Iteration
  helpers walk the elements captured at entry, so structural mutation inside a
  block never extends or shortens the in-flight loop, and in-place growth is
  charged against the memory quota before it is allocated.
- **Changed: hashes preserve Ruby-style insertion order.** `keys`, `values`,
  `each`/`each_key`/`each_value`/`each_with_index`, `to_a`, `flatten`,
  `for ... in`, and hash transforms (`merge`, `select`, `reject`,
  `transform_keys`/`transform_values`, `except`, `compact`, and friends) now
  visit entries in the order they were inserted, matching Ruby's hash contract,
  instead of sorted key order. An overwritten key keeps its original position,
  hash literals with duplicate keys keep the first occurrence's position with
  the last value, `array.group_by` and `array.tally` list keys in
  first-encounter order, `JSON.parse` preserves document order, and
  `JSON.stringify` emits members in insertion order. Hash `inspect`, `to_s`, and
  string interpolation now render entries in the same stable insertion order
  (previously unspecified Go map order). Hashes that reach the runtime as bare
  Go maps (host-provided values and keyword-argument splats) carry no insertion
  record and keep the previous sorted-key iteration.
- **Changed: `Array#fetch` and `Hash#fetch` now follow Ruby's strict
  missing-value contract.** A missing index or key with no fallback raises
  (`array.fetch index N outside of array bounds: ...` and `hash.fetch key not
  found: KEY`) instead of returning `nil`. Both forms now evaluate a Ruby-style
  block default, calling it with the requested index or key when the value is
  absent (`[1, 2, 3].fetch(9) { |i| i + 10 }` returns `19`). When both a default
  argument and a block are supplied, the block supersedes the default and is
  evaluated on a miss, matching Ruby (`[].fetch(0, 7) { 9 }` returns `9`).
  `Array#fetch` also accepts negative indices, counting from the end like `at`.
  For nil-on-miss array lookups use `[]`, `at`, `slice`, or `dig` (array `[]`
  counts negative indices from the end and returns `nil` out of range, like
  Ruby's `Array#[]`); for hashes use `[]` or `dig`. Use `fetch` when a miss
  should raise instead.
- **Added: Ruby-style `eql?` and `equal?` equality predicates.** Every value now
  answers `eql?` (hash-key equality, so `1.eql?(1.0)` is `false`) and `equal?`
  (object identity, so `1.equal?(1)` is `true` while two independently built
  arrays with equal contents are not `equal?`). The predicates report `false`
  rather than raising when the operands' kinds differ, and a class may override
  them with its own methods of the same name. A stored hash/object data field,
  instance ivar, or class var keyed `eql?`/`equal?` is treated as data and never
  shadows the predicate, so `box.equal? = 1` does not stop `box.equal?(box)` from
  answering identity and a data object's `eql?` field stays readable through index
  access. A required module's `eql?`/`equal?` export is a callable member and still
  overrides the predicate, just like a class method. Every empty hash and object
  carries its own backing storage, so two independently built empties (including
  `{}` from `JSON.parse("{}")`) are distinct objects under `equal?`; with the
  collection-mutator alignment above, independently constructed empty arrays are
  distinct objects too.

### Objects and dynamic dispatch

- **Added: Ruby-style dynamic dispatch, comparable, collection, and string
  helper coverage.** Values now answer `send` and `public_send`, with
  `public_send` preserving public visibility while `send` can reach private
  methods; instance `initialize` methods are private constructor hooks by
  default. Comparable scalar families now support `between?`. Arrays and hashes
  gain Ruby-shaped filtering, clearing, bang-transform, sampling, shuffling,
  rotation, product, combination, permutation, and adjacent chunking helpers
  (the Ruby-named mutators among them mutate in place under the 1.0 collection
  model, while the copy-returning helpers keep their non-mutating contract).
  Strings now support Ruby character-set helpers `count`, `delete`, `tr`, and
  `squeeze` including ranges and leading-complement sets.
- **Added: Ruby-style `call` on function values.** A function value now exposes
  a `call` member so `fn.call(...)` mirrors direct `fn(...)` invocation,
  forwarding positional arguments, keyword arguments, and an optional block.
  Arity and type errors stay anchored at the call site, and `call` is the only
  member offered (with a "did you mean" hint for typos).
- **Added: Ruby-style scalar conversion methods and the `nil?` predicate on
  core values.** Every value now answers `nil?` (true only for `nil`),
  including script class instances, classes, function values, and enum values;
  it resolves through the universal `Object#nil?` fallback, so a user-defined
  `nil?` keeps precedence. The scalar kinds whose display form is
  bounded by their own footprint (`nil`, booleans, integers, floats, strings,
  symbols, money, durations, and times) also answer `to_s` and the documented
  `.string` conversion idiom from `docs/typing.md`. Arrays, hashes, and ranges
  deliberately do not gain `to_s`/`string` because their rendering can be
  arbitrarily large; they continue to expose `inspect`, which projects the
  rendered length against the memory quota before allocating. Integers and
  floats convert between numeric kinds with `to_i`/`to_f` (`Float#to_i`
  truncates toward zero like Ruby and raises on a non-finite or out-of-range
  value). Strings parse numeric text with `to_i`/`to_f`; unlike Ruby's lenient
  `String#to_i`/`String#to_f`, these are strict like the global
  `to_int`/`to_float` and raise on an empty, non-numeric, or non-finite string
  so a malformed value never silently becomes zero at a typed boundary.
- **Added: Ruby-style object introspection predicates.** Every value now
  responds to `respond_to?`, `is_a?`, `kind_of?`, and `instance_of?`.
  `respond_to?(name)` reports whether the receiver has a callable member of that
  name (a symbol or string), excluding data such as hash keys, namespace
  constants, and instance variables, and honoring privacy (with the optional
  `include_all` second argument). `is_a?`/`kind_of?`/`instance_of?` test whether
  the receiver is an instance of a given script class; without inheritance they
  test direct class identity. A script class may override any of these with its
  own method definition.
- **Added: Ruby-style `Object#tap` and `Object#yield_self`.** Every core value
  kind now responds to these block-yielding helpers. `tap` yields the receiver
  and returns the receiver (so block results are discarded), while `yield_self`
  yields the receiver and returns the block's result. Both require a block, take
  no other arguments, and resolve only when the receiver does not already define
  a member of the same name, so a hash key, instance variable, or user-defined
  method named `tap`/`yield_self` keeps precedence.
- **Added: Ruby-style `Object#itself`.** `itself` is now available on every
  value kind, including scalars, collections, ranges, temporal values, `nil`,
  and script instances, and returns the receiver unchanged so it preserves
  value ownership and host-boundary isolation. It is handy as an identity step
  in pipelines and block callbacks; it takes no arguments and rejects any
  positional or keyword argument.
- **Added: Ruby-style `inspect` debug representations.** Every core value kind
  (`nil`, booleans, integers, floats, strings, symbols, arrays, and hashes) now
  responds to `inspect`, returning a parseable debug string. Unlike the
  interpolation/`to_s` rendering, `inspect` keeps quotes and escaping for strings
  (`"a\nb".inspect` is `"\"a\\nb\""`), renders symbols with their leading colon
  (`:ok.inspect` is `":ok"`), and recurses into arrays and hashes
  (`[1, "x", nil].inspect` is `"[1, \"x\", nil]"`). Hashes render in Vibescript's
  colon-label key form rather than Ruby's unsupported hash-rocket syntax, so the
  output round-trips as a Vibescript literal. `inspect` takes no arguments, and
  the rendered size is charged against the memory quota before the string is
  built so inspecting a huge composite fails with a quota error instead of
  allocating an oversized result.

### Strings

- **Added: Ruby-style `String#chop` and `String#chop!`.** `chop` removes the
  last character, treating a trailing `"\r\n"` as a single record separator and
  otherwise removing one full Unicode character rather than one byte; an empty
  string is returned unchanged. `chop!` returns the chopped string and returns
  `nil` when there is nothing to remove (the empty-string case), matching the
  existing copy-on-transform bang helper convention.
- **Added: Ruby-style string padding helpers.** `String#center`, `String#ljust`,
  and `String#rjust` pad a string to a requested width, defaulting to a single
  space and accepting a custom pad string that is repeated and truncated to fill
  the span. Width is measured in characters (Unicode code points) like
  Vibescript's other string methods, a `Float` width is truncated toward zero as
  Ruby does, a width at or below the receiver's length returns it unchanged, and
  an empty pad string is rejected. Oversized widths are checked against the
  memory quota before any buffer is allocated, so they fail fast instead of
  materializing a huge string.
- **Added: Ruby-style `String#casecmp` and `String#casecmp?`.** `casecmp`
  case-insensitively compares two strings (folding only ASCII letters and
  comparing other bytes ordinally) and returns `-1`, `0`, `1`, or `nil` for a
  non-string argument, matching Ruby. `casecmp?` returns a boolean using Unicode
  simple case folding (consistent with `upcase`/`downcase`) or `nil` for a
  non-string argument; full-fold expansions such as `ß` matching `SS` are not
  applied. When either operand contains invalid UTF-8, `casecmp?` folds
  byte-wise over ASCII letters so distinct byte sequences stay distinct,
  preserving byte identity like Ruby's binary-string path.
- **Added: Ruby-style `String#partition` and `String#rpartition`.** Both split a
  string into a three-element `[head, separator, tail]` triple around the first
  (`partition`) or last (`rpartition`) occurrence of the separator. A missing
  separator keeps the whole string on the head (`partition`) or tail
  (`rpartition`) with empty surrounding segments, and an empty separator matches
  at the start or end respectively, matching Ruby. The separator must be a
  string.
- **Added: Ruby-style `String#chars` and `String#lines`.** `chars` returns an
  array of the string's Unicode characters using the existing rune-aware
  semantics, and `lines` splits on `"\n"` while retaining the trailing newline
  on each line, leaving carriage returns attached so `"\r\n"` endings round-trip.
- **Added: Ruby-style `String#each_char` and `String#each_line`.** `each_char`
  yields each Unicode character (whole code points, matching `chars`, `length`,
  and `slice`), and `each_line` yields each line with Ruby-compatible newline
  retention (matching `lines`: only `\n` ends a line, a trailing newline does not
  produce a final empty line, and `\r\n` keeps the `\r` attached). Both ignore
  the block's return value and return the receiver string. Vibescript has no
  `Enumerator`, so calling either without a block reports a deliberate
  `requires a block` error instead of falling through as an unknown method.
- **Added: Ruby-style `String#bytes` and `String#each_byte`.** `bytes` returns an
  array of the string's bytes as integers in `0..255`, and `each_byte` streams
  each byte to a block and returns the receiver. Both are byte-level, so a
  multibyte character expands to one entry per UTF-8 byte, and raw bytes are
  returned verbatim without normalizing invalid UTF-8. As with `each_char`,
  `each_byte` requires a block because Vibescript has no `Enumerator`.
- **Added: Ruby-style `String` byte and code-point helpers.** `getbyte(index)`
  returns the byte at a byte offset (or `nil` when out of range), `byteslice`
  extracts a substring by byte offset (single index, `start`/`length`, or range
  forms, returning raw bytes verbatim like Ruby), `codepoints` returns the
  string's Unicode code points as an integer array, and `each_codepoint` yields
  each code point to a block. Negative offsets count back from the end, the
  byte-array helpers honor the sandbox memory quota, and the streaming
  `each_codepoint` participates in block cancellation and quotas. This
  complements the existing `bytes` and `each_byte`.
- **Added: Ruby-style `String#prepend` and `String#insert`.** `prepend` returns
  a copy of the receiver with one or more string arguments prepended in order,
  mirroring `concat`. `insert` returns a copy with a string inserted at a
  character index: a non-negative index inserts before the character at that
  position (a value equal to the length appends), while a negative index inserts
  after the character it selects (`-1` appends). The index counts characters
  rather than bytes, so it behaves the same way for multibyte text, a `Float`
  index is truncated toward zero as Ruby does, and an out-of-range index raises
  an error.
- **Added: Ruby-style string/symbol conversion helpers.** `String#to_sym` and
  its alias `String#intern` return the symbol named by the receiver, accepting
  any contents verbatim (including whitespace, punctuation, and the empty
  string). `Symbol#id2name` and `Symbol#to_s` return the symbol's name as a
  string, and `Symbol#to_sym` returns the receiver. The pair round-trips between
  the two representations, and symbol/string equality stays kind-sensitive so
  `:name == "name"` is `false`.
- **Added: Ruby-style `String#match?`.** `match?(pattern, offset = 0)` is the
  allocation-light boolean counterpart to `match`, returning `true` when the
  pattern has a match at or after the given character offset and `false`
  otherwise without materializing match arrays. It shares the same regex engine
  and size guards as `match`, so anchors such as `\A`, `^`, and `\b` keep the
  full-string context even when an offset is supplied. The offset is a character
  (codepoint) position; an offset past the end of the string yields `false`
  rather than an error, and negative offsets are rejected to match `index` and
  `rindex`.
- **Added: Ruby-style `String#hex` and `String#oct`.** `hex` reads a string as a
  hexadecimal integer and `oct` reads it using a base inferred from a
  `0x`/`0b`/`0o`/`0d` prefix (defaulting to octal). Both skip leading whitespace
  and an optional sign, accept underscore digit separators, stop at the first
  invalid digit, and return `0` for unparseable input, matching Ruby. Because
  Vibescript has only 64-bit integers rather than Ruby's `Bignum`, a value
  outside the `int64` range raises an `integer out of range` error. Overflow is
  now detected exactly before each digit is accumulated, so magnitudes that wrap
  past `uint64` (for example 17-or-more hexadecimal digits) raise the error
  instead of silently returning wrapped garbage.
- **Added: Ruby-style `String#split` limit argument.** `split(separator, limit)`
  now accepts the optional second `limit` argument. A positive limit returns at
  most that many fields with the remainder left unsplit in the final field (a
  limit of `1` returns the whole string), and a negative limit preserves every
  field including trailing empties. The limit applies to every separator mode,
  including the whitespace default and the empty separator that splits a string
  into its characters. Splitting on the empty separator walks UTF-8 character
  boundaries, so invalid bytes in a binary string are preserved as single-byte
  fields rather than rewritten as the U+FFFD replacement character. A
  non-integer limit is rejected.
- **Added: Ruby-style block forms for `String#sub`, `String#gsub`, `String#scan`,
  and `String#match`.** `sub`/`gsub` (and their `!` variants) now accept a block
  instead of a replacement argument: the block receives each matched substring
  and its result (coerced to a string) replaces the match, honoring the same
  `regex` keyword as the value-replacement forms (defaulting to literal
  matching). `scan` with a block yields each match using its array result shape
  and returns the receiver string. `match` with a block yields the match data and
  returns the block's result, returning `nil` without invoking the block when
  there is no match. Supplying both a replacement argument and a block is
  rejected. Literal (non-`regex`) block replacements bypass the regex-only
  pattern- and input-size guards, matching the literal value-replacement forms,
  while the regex form keeps them; every block form still enforces the shared
  output-size and step guards. The `sub!`/`gsub!` variants return the receiver
  whenever the pattern matched — even when the replacement reproduces the
  original text — and `nil` only when the pattern never matched, matching Ruby.
- **Improved: Ruby-style `String#start_with?` and `String#end_with?`.** Both
  predicates now accept one or more string candidates and return true when any
  matches. Candidates are checked left to right and matching short-circuits like
  Ruby, so a non-string candidate is only rejected if reached before a match.
- **Improved: Ruby-style `String#slice` selectors.** `slice` now accepts the
  same selector shapes as Ruby beyond the previous non-negative integer start
  with optional length. A negative integer index counts back from the end; a
  range returns the matching substring with Ruby-compatible negative bounds; and
  a substring argument returns that substring when it is contained, otherwise
  `nil`. The `slice(start, length)` form now also supports a negative start.
  Selectors that fall outside the string return `nil`, and indexing stays
  rune-aware.
- **Changed: `String#upcase`, `downcase`, `capitalize`, and `swapcase` now use
  full Unicode case mapping.** Characters that expand or use special mappings
  follow Ruby, so `"Straße".upcase` is `"STRASSE"`, `"İ".downcase` is `"i̇"`,
  `"ﬁ".upcase` is `"FI"`, and `"ǆ".capitalize` titlecases the digraph to
  `"ǅ"`. The Greek final-sigma rule is not applied, matching Ruby's default
  (`"ΟΔΟΣ".downcase` is `"οδοσ"`). Strings that are not valid UTF-8 fall back to
  ASCII-only mapping, mirroring Ruby's binary-string path.
- **Added: case-mapping options for the string case methods.** `upcase`,
  `downcase`, `capitalize`, and `swapcase` accept `:ascii` to restrict mapping
  to ASCII letters, and `downcase` additionally accepts `:fold` for Unicode case
  folding (so `"Straße".downcase(:fold)` is `"strasse"`). Supplying `:fold` to a
  method other than `downcase`, an unknown option symbol, a non-symbol argument,
  or more than one option raises a clear error. The bang variants accept the
  same options and continue to return `nil` when the value is unchanged.
  `swapcase` toggles every cased character, including cased non-letters such as
  circled letters (`"Ⓐ".swapcase` is `"ⓐ"`) and Roman numerals (`"Ⅰ".swapcase`
  is `"ⅰ"`). Swapcase of a titlecase digraph (such as `ǅ`) is lowercased rather
  than split into its component letters, a deliberate divergence from Ruby for
  those rare codepoints.
- **Changed: `String#strip`, `String#lstrip`, and `String#rstrip` now match
  Ruby's whitespace set.** They remove only the ASCII whitespace bytes
  `\0 \t \n \v \f \r " "`, with NUL (`\0`) trimmed from both edges just like
  Ruby. Unicode spaces such as NBSP (`U+00A0`), the Ogham space mark (`U+1680`),
  em space (`U+2003`), and the byte order mark (`U+FEFF`) are now preserved
  instead of stripped. The bang variants still return `nil` when nothing is
  removed.
- **Changed: default `String#split` now uses Ruby's ASCII whitespace set.**
  The no-separator form previously delegated to Go's `strings.Fields`, which
  treats wider Unicode whitespace such as the non-breaking space (`U+00A0`) and
  the em space (`U+2003`) as separators. It now splits only on the six ASCII
  whitespace bytes Ruby recognizes (space, tab, newline, vertical tab, form
  feed, and carriage return), keeping other Unicode whitespace inside the field
  so `"a b".split` returns `["a b"]` instead of `["a", "b"]`. Leading
  and trailing whitespace is still discarded and runs collapse, matching Ruby's
  default `String#split`.
- **Changed: `String#split` now trims trailing empty fields by default.** With
  the default limit of `0`, `"a,b,".split(",")` returns `["a", "b"]` instead of
  `["a", "b", ""]`, matching Ruby. Use a negative limit to keep trailing empty
  fields.
- **Changed: a single space separator triggers whitespace splitting.** A
  separator of exactly `" "` is Ruby's AWK whitespace mode, so it collapses
  whitespace runs and discards leading whitespace instead of splitting literally.
  `" a  b ".split(" ", 2)` returns `["a", "b "]` rather than `["", "a  b "]`.
- **Changed: `String#split(nil)` now matches Ruby.** An explicit `nil`
  separator behaves like the no-argument form, splitting on runs of ASCII
  whitespace instead of raising a type error. Any other non-string separator
  still raises an error.
- **Changed: `String#scan` returns Ruby-compatible capture results.** When the
  pattern has no capture groups `scan` still returns the full match strings, but
  with one or more groups it now returns a nested array per match holding each
  captured substring (`nil` for an optional group that did not participate),
  matching Ruby instead of always returning the full matches. `scan` charges its
  growing result against the step and memory quotas and bounds the regex engine's
  submatch-index allocation against a fixed 256 MiB host cap, so a pattern with
  many capture groups over a large subject errors instead of exhausting host
  memory. The host cap is derived from the subject length and the pattern's
  minimum match length, so ordinary sparse scans — a pattern that matches little
  or nothing over a modest string — run regardless of the configured memory quota
  instead of being rejected on a pessimistic worst case.
- **Changed: `String#sub`/`gsub` regex replacements use Ruby backreferences.**
  With `regex: true`, `sub`, `sub!`, `gsub`, and `gsub!` now expand
  replacement strings using Ruby's substitution syntax instead of Go's. `\1`–`\9`
  insert capture groups, `\&` (or `\0`) the whole match, `` \` `` and `\'` the
  pre/post-match, `\+` the last participating group, `\k<name>` a named group,
  and `\\` a literal backslash; `$1` and `$&` are now literal text. This makes
  Ruby replacement strings copied into Vibescript produce the same output, so
  `"abc123".sub("([a-z]+)([0-9]+)", "\\2-\\1", regex: true)` yields `"123-abc"`.
  As in Ruby, once a pattern defines any named capture group the numbered refs
  `\1`–`\9` expand to the empty string (use `\k<name>` instead), so
  `"John Smith".sub("(?<first>\\w+) (?<last>\\w+)", "\\2, \\1", regex: true)`
  yields `", "`; the whole-match, pre/post-match, and `\k<name>` refs keep
  working in that mode. An unterminated `\k<name` or a `\k<name>` that names an
  undefined group raises an error, matching Ruby. (`Regex.replace`/
  `Regex.replace_all` keep their existing `$1` syntax.)
- **Changed: `String#match` now accepts Ruby's optional offset.**
  `match(pattern, offset = 0)` searches for the first match starting at or after
  the given character (codepoint) position, so callers can scan from a known
  point without slicing the receiver first. A non-negative offset searches
  forward from that position; a negative offset counts back from the end (with an
  offset before the start returning `nil`); a positive offset greater than the
  receiver length is clamped to the length and the search runs from the end, so a
  zero-width-capable pattern matches the empty string there while a pattern that
  needs a character returns `nil`. The offset accepts an integer or a float
  (truncated toward zero, as in Ruby); any other type is rejected. Anchors such
  as `^`, `\b`, and `\B` keep the full-string context across the offset while
  `\A` only matches at the absolute start, and an invalid regex is still
  reported regardless of the offset.
- **Fixed: `String#chr` returns an empty string for an empty receiver like Ruby.**
  `"".chr` now returns `""` instead of `nil`, so `String#chr` always returns a
  string. Non-empty receivers are unchanged, so `"abc".chr` still returns `"a"`.
- **Fixed: `String#chomp` and `String#chomp!` treat a `nil` separator as "do not
  chomp".** Passing `nil` now returns the string unchanged (`chomp`) or `nil`
  because no change occurs (`chomp!`), matching Ruby, instead of raising
  `separator must be string`. The default separator, empty-string separator, and
  explicit string separator behaviors are unchanged.
- **Fixed: `String#index` and `String#rindex` accept negative offsets like
  Ruby.** A negative offset now counts back from the end of the string, so the
  search starts at `size + offset`, and the call returns `nil` when that
  effective offset falls before the start of the string. Previously both methods
  rejected negative offsets with an error. For example, `"hello".index("l", -3)`
  returns `2` and `"hello".rindex("l", -2)` returns `3`.

### Arrays

- **Added: Ruby-style `Array#transpose`.** `transpose` swaps the rows and
  columns of a matrix made of equal-length array rows, so
  `[[1, 2], [3, 4]].transpose` returns `[[1, 3], [2, 4]]`. An empty array
  transposes to `[]`, and rows of zero length collapse to no columns. It
  rejects extra arguments, raises when any element is not an array, and
  raises when the rows differ in length, reporting the offending index.
- **Added: Ruby-style `Array#each_slice`, `each_cons`, `reverse_each`, and
  `cycle`.** `each_slice(n)` yields non-overlapping slices (including a shorter
  trailing slice) and `each_cons(n)` yields sliding windows; both require a
  positive integer size and yield freshly copied arrays that do not alias the
  receiver. `reverse_each` yields values in reverse index order and returns the
  receiver. `cycle(n)` repeats the array `n` times (a non-positive count is a
  no-op like Ruby), while omitting the count or passing `nil` cycles forever; the
  cycle charges a step per yield so the step quota and context cancellation bound
  even an empty block body. The slice/window/cycle forms return `nil` to match
  Ruby.
- **Added: Ruby-style `Array#reject`, `take_while`, `drop_while`, `grep`, and
  `grep_v`.** `reject` is the inverse of `select`; `take_while` and `drop_while`
  split on the first block miss with early-stop semantics; `grep` and `grep_v`
  filter using the language's case-equality direction (`pattern === element`,
  the same matcher as `case`/`when`), so a `Range` matches by membership and
  other values by equality, with an optional block transforming each kept
  element.
- **Added: Ruby-style `values_at`, `zip`, `take`, and `drop` collection helpers.**
  `Hash#values_at(*keys)` returns values in requested key order with `nil` for
  missing keys. `Array#zip(*arrays)` combines arrays element-wise into rows keyed
  to the receiver's length, padding short arrays with `nil` and rejecting
  non-array arguments. `Array#take(n)` and `Array#drop(n)` return prefix and
  suffix slices without mutating the receiver, truncating fractional counts like
  Ruby's `to_int` conversion and rejecting negative counts.
- **Added: Ruby-style `Array#min`, `#max`, `#minmax`, `#min_by`, and `#max_by`.**
  The extrema helpers reuse the comparison semantics of `sort`/`sort_by`, return
  `nil` (or `[nil, nil]` for `minmax`) on empty arrays, resolve ties to the first
  matching element, participate in step/cancellation accounting for the block
  forms, and raise clear errors on incomparable mixed values.
- **Added: Ruby-style index-aware iteration helpers.** Arrays gain
  `each_with_index { |item, index| }` (returns the receiver) and
  `map_with_index { |item, index| }` (returns a new array of block results),
  both passing each element's 0-based index to the block. Hashes gain matching
  `each_with_index { |pair, index| }` and `map_with_index { |pair, index| }`
  helpers that yield each `[key, value]` pair plus its index, mirroring Ruby's
  `Hash#each_with_index`. All four take no arguments, require a block, and run
  under the sandbox step and memory quotas.
- **Added: Ruby-style array `<<` (shovel) and `&` (intersection) operators.**
  `array << value` appends a single value to the receiver in place and returns
  it (`[1, 2] << 3` is `[1, 2, 3]`; see the collection-mutator alignment
  above), reusing the same backing-buffer fast path as `push` and `+`.
  `array & other` returns the elements common to
  both arrays with duplicates removed and the left array's order preserved
  (`[1, 1, 2, 3] & [1, 3, 4]` is `[1, 3]`). Following Ruby, `+` binds tighter
  than `<<`, which binds tighter than `&`. Mirroring Ruby's spacing rule, only an
  `&` detached from the callee yet flush against its operand (`call &block`) is
  a block pass; every other shape is the intersection
  operator, including the spaced `items & others`, the flush `items&others`, and
  a trailing `&` line continuation. Both operators require array operands and the
  reduce shorthand accepts `"<<"` and `"&"`.
- **Added: Ruby-style collection deletion and insertion helpers.** Arrays gain
  `delete`, `shift`, `unshift`, and `insert`, and hashes gain `delete`. Under
  the 1.0 mutator model these operate on the receiver in place:
  `Array#delete(value)` returns the removed element when found (`nil`
  otherwise, or a block result on a miss), `Array#shift` / `shift(n)` returns
  the removed value(s), and `Hash#delete(key)` returns the removed value (with
  the same block form for misses). `Array#unshift` is a Ruby-style alias for
  `prepend`, and `Array#insert(index, *values)` inserts the values before
  `index`, following Ruby's negative-index and past-the-end padding rules.
- **Added: Ruby-style `Array#append` and `Array#prepend`.** `append` is an alias
  for `push`, adding the given values to the end in order, and `prepend` inserts
  the values at the front in order, so `[3].prepend(1, 2)` is `[1, 2, 3]`. Both
  mutate the receiver in place and return it under the 1.0 mutator model, and
  both reject keyword arguments.
- **Added: Ruby-style `Array#dig` and mixed hash/array `dig` paths.** `dig`
  now descends one level per path component across both collection kinds: an
  integer index into an array or a symbol/string key into a hash, so a single
  `dig` can walk JSON-shaped data, e.g. `[[1, 2], [3, 4]].dig(1, 0)` returns
  `3` and `{ a: [10, 20] }.dig(:a, 1)` returns `20`. Missing keys and
  out-of-range indexes yield `nil` rather than raising, while indexing an array
  with a non-integer component raises, matching how arrays reject non-integer
  indexes elsewhere.
- **Added: Ruby-style `Array#one?`.** `one?` is true only when exactly one
  element is truthy, or with a block, when exactly one block result is truthy.
  It stops scanning once a second match is found and respects the existing step
  quota and cancellation checks during block iteration.
- **Added: Ruby-style `Array#at` and `Array#slice`.** `at(index)` returns the
  single element at `index`, counting a negative index back from the end and
  returning `nil` out of range, so `[10, 20, 30].at(-1)` is `30`. `slice(index)`
  mirrors `at`, while `slice(start, length)` and `slice(range)` return a fresh
  subarray with Ruby-compatible handling of negative starts and bounds, a start
  exactly at the length (yielding `[]`), oversized lengths (clamped to the
  remaining elements), and negative lengths or out-of-range starts (returning
  `nil`). The range form aligns with the range slicing already available for
  strings. Indexes and lengths accept `Float` values truncated toward zero like
  Ruby's `to_int`, and the subarray forms never alias the receiver.
- **Added: Ruby-style `Array#values_at`.** `values_at(*selectors)` reads several
  elements at once, returning a new array in the order the selectors were
  requested. An integer selector reads one element: negative indexes count back
  from the end, out-of-bounds indexes yield `nil`, and duplicate indexes repeat
  their values. A range selector reads a window and flattens its elements into
  the result in place, so `values_at(0..1)` is `[a[0], a[1]]` and integer and
  range selectors can be interleaved (`values_at(0..1, -1)`); a range whose end
  extends past the array pads the missing positions with `nil`, while a range
  whose negative start counts back before the beginning raises. Float indexes and
  float range bounds truncate toward zero like Ruby's `to_int` conversion;
  non-numeric selectors and keyword arguments raise.
- **Added: Ruby-style pattern arguments for `Array#any?`, `Array#all?`, and
  `Array#none?`.** These predicates now accept an optional `pattern` argument
  alongside their existing no-argument and block forms: `any?(pattern)` is true
  when any element matches, `all?(pattern)` when every element matches, and
  `none?(pattern)` when no element matches. As in Ruby, the argument is tested
  with case equality (`===`), so range patterns such as `any?(1..3)` test
  membership rather than object identity. As with `count(value)`, a `pattern`
  argument takes precedence over an attached block, which is then ignored.
- **Added: Ruby-style symbol shorthand for `Array#reduce`.** `reduce(operation)`
  and `reduce(initial, operation)` fold by sending `operation` to the
  accumulator with each element, matching Ruby's
  `["a", "b"].reduce(:concat)`. `operation` is a symbol naming a method on the
  accumulator (`["a", "b"].reduce(:concat)`) or a string naming a method or a
  binary operator (`[1, 2, 3].reduce("+")`, also `"-"`, `"*"`, `"/"`, `"%"`,
  `"**"`). A block still takes precedence, so a lone argument alongside a block
  is treated as the initial value. An empty array now folds to `nil` (or to the
  supplied `initial`) instead of raising, matching Ruby's `[].reduce { ... }`.
  Method dispatch is public-only, mirroring Ruby's
  `accumulator.public_send(operation, item)`: a private method cannot be reached
  through the shorthand even when the accumulator is the current `self`.
  Operator-symbol literals such as `:+` are not yet accepted here because the
  lexer cannot tokenize them; that shorthand is tracked in #801.
- **Added: Ruby-style `Array#filter_map`.** `filter_map` fuses `map` with a
  truthiness filter, calling the block once per element and collecting each
  truthy result while dropping falsy ones. Like Ruby, only `nil` and `false`
  are falsy, so `0`, `""`, and empty collections are retained. It requires a
  block, takes no arguments, and materializes its result under the sandbox step
  and memory quotas so large inputs fail safely and long iterations honor
  cancellation.
- **Added: Ruby-style `Array#union` and `Array#difference`.** `union(*others)`
  concatenates the receiver with every argument array and removes duplicates,
  keeping the first occurrence of each value (with no arguments it deduplicates
  the receiver). `difference(*others)` returns the receiver's elements that do
  not appear in any argument array while preserving the receiver's own
  duplicates. Both compare values by content (so nested arrays and hashes match
  like `uniq`), return a new array without mutating the receiver, and raise when
  handed a non-array argument.
- **Added: Ruby-style `Array#fill`.** `fill` replaces all or part of an array
  with a value (`fill(value)`, `fill(value, start, length)`, `fill(value,
  range)`) or with values computed from each index by a block (`fill { |i|
  ... }`, optionally narrowed by a `start`/`length` or range). It follows Ruby's
  indexing rules: a negative `start` counts back from the end, a `length` or
  range that runs past the end grows the result and pads any gap with `nil`, a
  `nil` `start` is read as `0` and a `nil` `length` as omitted (filling to the
  end), and the value and block forms are mutually exclusive. Under the 1.0
  mutator model `fill` mutates the receiver in place and returns it, and it
  builds the result under the sandbox step and memory quotas so a growth larger
  than the limits fails safely instead of exhausting memory.
- **Changed: `Array#sum` now honors a Ruby-style initial value and block.**
  `sum(initial)` seeds the accumulator instead of starting from `0`
  (`[1, 2, 3].sum(10)` is `16`, `["a", "b"].sum("")` is `"ab"`), and a block
  transforms each element before it is added (`[1, 2, 3].sum { |n| n * 2 }` is
  `12`, with `sum(initial) { ... }` combining both). Previously the argument and
  block were silently ignored. Each addition must operate on compatible operands,
  mirroring Ruby's `+`, so summing a string with a non-string (such as the
  default `0` accumulator against string elements) raises instead of silently
  coercing the operands.
- **Fixed: `Array#index`, `Array#find_index`, and `Array#rindex` accept both a
  value and a block like Ruby.** `[1, 2, 3].index { |x| x > 1 }` and
  `[1, 2, 3, 2].rindex { |x| x == 2 }` now return the matching index instead of
  raising, and `find_index(value)` now accepts a value argument. Each method
  takes either a value (with Vibescript's optional non-negative offset) or a
  block, never both; passing both now raises. The nil-on-miss behavior is
  unchanged.
- **Fixed: `Array#join` joins nested arrays recursively like Ruby.** `join` now
  flattens nested arrays into the output using the active separator instead of
  rendering their inspect form, so `[1, [2, 3], 4].join("-")` is `"1-2-3-4"` and
  `[1, [2, [3, 4]], 5].join("-")` is `"1-2-3-4-5"`. Scalar elements are unchanged:
  `nil` still contributes an empty segment (`[1, nil, "x"].join(",")` is `"1,,x"`)
  and an empty array still joins to `""`.
- **Fixed: `Array#flatten` accepts `nil` and negative depths like Ruby.**
  `[1, [2, [3]]].flatten(nil)` and `[1, [2, [3]]].flatten(-1)` now flatten fully
  instead of raising, matching the no-argument form. A depth of `0` still returns
  a shallow copy, positive integers flatten that many levels, a `Float` depth is
  truncated to an integer, and a nonnumeric depth raises.
- **Fixed: `Array#count(value)` ignores an attached block like Ruby.**
  `[1, 2, 1].count(1) { |x| x > 1 }` now returns `2` instead of raising. A value
  argument takes precedence: matching elements are counted and the block is
  never invoked. The block-only form `count { ... }` and the no-argument form
  `count` are unchanged.
- **Fixed: `Array#first` and `Array#last` reject extra arguments like Ruby.**
  `[1, 2, 3].first(1, 2)` and `[1, 2, 3].last(1, 2)` now raise instead of
  silently ignoring the extra argument and returning `[1]` or `[3]`, and
  keyword arguments such as `first(n: 2)` or `last(1, n: 2)` raise instead of
  being silently ignored. The optional count is still the only accepted
  argument, so the no-argument forms and the single-count forms (including
  `first(0)` / `last(0)`) are unchanged.
- **Fixed: zero-argument `Array#push` returns the array like Ruby.** A bare
  `array.push` now reads as a zero-argument call that returns the array instead
  of leaking the unbound method value, and `array.push()` no longer raises
  "expects at least one argument". This matches Ruby, where the call has no
  parentheses distinction, so `[1, 2].push` and `[1, 2].push()` both return
  `[1, 2]` while `[1, 2].push(3)` still returns `[1, 2, 3]`. A keyword-only
  call such as `[1, 2].push(foo: 1)` now raises rather than silently dropping
  the keyword map, since `Array#push` does not accept keyword arguments.

### Hashes

- **Added: Ruby-style `Hash#fetch_values`.** `Hash#fetch_values(*keys)` returns
  the values for several keys at once, in the requested order. Unlike
  `values_at`, it raises a `key not found` error for any missing key; pass a
  block to compute a replacement value for each missing key instead of raising.
- **Added: Ruby-style hash member, value, and store helpers.** `Hash#member?`
  joins `key?`/`has_key?`/`include?` as a key-membership alias, and
  `Hash#value?` and `Hash#has_value?` report value membership using the same
  `==` equality as the rest of the language. `Hash#store(key, value)` assigns
  the key; under the 1.0 mutator model it is true index assignment on the
  receiver, returning the stored value.
- **Added: Ruby-style conflict blocks for `Hash#merge`.** `Hash#merge` now
  honors an optional block to resolve key conflicts: for keys present in both
  hashes the block is yielded `(key, old_value, new_value)` and its result is
  stored, while keys present on only one side are copied without invoking the
  block. Without a block the incoming hash still wins on conflicts. The conflict
  key is yielded as a symbol, matching the other hash helpers, and the block was
  previously accepted but silently ignored.
- **Added: Ruby-style `Hash#update`, `Hash#merge!`, `Hash#replace`,
  `Hash#flatten`, and multi-hash `Hash#merge`.** `Hash#merge` now accepts any
  number of hashes (applied left to right, so later hashes win on conflicts)
  and returns a copy of the receiver when called with no arguments; the
  optional conflict block folds through each argument in turn. `Hash#update`
  and `Hash#merge!` apply the same multi-hash merge to the receiver in place,
  and `Hash#replace` adopts another hash's entries — both mutate under the 1.0
  collection model. `Hash#flatten(depth = 1)` returns the entries as a flat
  array, defaulting to `[key, value, ...]`, with `0` keeping the `[key, value]`
  pairs nested and a negative depth flattening completely, in insertion order.
- **Added: Ruby-style `Hash#to_a` and `Array#to_h` conversion helpers.**
  `Hash#to_a` returns the `[key, value]` pairs in insertion order (the inverse
  of `Array#to_h`, equivalent to `flatten(0)`). `Array#to_h` builds a hash from
  an array of two-element `[key, value]` pairs, converting keys through the
  same symbol/string hash-key rules used elsewhere and keeping the last pair on
  duplicate keys. A block form `to_h { |element| [key, value] }` maps each
  element to its pair. Malformed input raises: a non-array element, a pair that
  is not exactly two elements, or a key that is not a symbol or string.
- **Added: Ruby-style `Hash.new` defaults and `Hash#default` / `Hash#default_proc` readers.**
  `Hash.new(default)` builds a hash that returns `default` for a missing `[]`
  lookup without inserting it, and `Hash.new { |hash, key| ... }` installs a
  default proc invoked on a missing-key lookup (which inserts only if its body
  assigns one). The value and block forms are mutually exclusive, and bare
  `Hash.new` matches a `{}` literal with a `nil` default. `default` returns the
  configured default value (never running the proc, matching Ruby) and
  `default_proc` returns the configured proc. `[]` access, `dig`, and
  `values_at` all consult the default for a missing key (each is a `[]` lookup in
  Ruby), so `Hash.new(0).dig(:missing)` is `0` and a default proc fires per miss;
  `fetch` keeps ignoring the default, matching Ruby. The default travels with
  the hash through index assignment and is copied onto the result of `merge`,
  while the in-place `update`/`merge!` keep the receiver's default; every other
  transform returns a plain hash with no default. A default proc that escapes
  one `Script.Call` and is passed back into another (as an argument, global, or
  task-inherited hash) is re-rooted onto the current call, so a missing-key
  lookup resolves globals, capabilities, and functions against the current
  invocation rather than the stale environment it was created in, while still
  keeping any local variables the proc legitimately closed over (such as a
  parameter of the function that built the hash). A capability copied into a
  local that the proc captured (for example `cap = jobs`) is revoked on
  re-entry rather than preserved, so a missing-key lookup cannot invoke a
  capability the re-entering call never granted; a free reference to the live
  capability global still resolves through the re-rooted ambient root. Because
  a missing-key lookup returns the default, the default is part of a typed
  hash's value type: validating a hash against `hash<key, value>` requires the
  default value to match `value` (the validated default travels with the
  normalized hash) and rejects a hash carrying a default proc, whose result
  cannot be type-checked.
- **Fixed: `Hash#except` ignores unsupported key types like Ruby misses.**
  `Hash#except` no longer raises when given an argument whose type cannot be a
  hash key (anything other than a symbol or string). Because Vibescript hash keys
  are only symbols or strings, such an argument can never match an entry, so it is
  now treated as a Ruby-style miss and ignored. Mixed argument lists still exclude
  the supported keys, so `{ a: 1 }.except(1)` returns `{ a: 1 }` while
  `{ a: 1 }.except(1, :a)` returns `{}`.
- **Fixed: `Hash#slice` omits unsupported candidate keys like Ruby misses.**
  `Hash#slice` no longer raises when given a candidate key whose type cannot be a
  hash key (anything other than a symbol or string). Such a candidate can never
  match an entry, so it is treated as a Ruby-style miss and dropped from the
  result instead of failing. Mixed argument lists still keep the supported keys,
  so `{ a: 1, b: 2 }.slice(:a, 1)` returns `{ a: 1 }` while
  `{ a: 1 }.slice(1)` and `{ a: 1 }.slice` both return `{}`.
- **Fixed: Hash membership predicates align with Ruby.** `Hash#key?`,
  `Hash#has_key?`, and `Hash#include?` now return `false` for candidate keys of
  unsupported types instead of raising, matching Ruby's predicate semantics.
- **Fixed: `Hash#each` yields the key/value pair to single-parameter blocks like
  Ruby.** A block declaring one positional parameter now receives each entry as a
  two-element `[key, value]` array instead of only the key, so
  `{ a: 1 }.each { |pair| pair }` yields `[:a, 1]`. Blocks with two parameters
  still receive the key and value separately, extra parameters still receive
  `nil`, and a single destructuring parameter such as `|(key, value)|` unpacks the
  pair.
- **Fixed: parenless `Hash#merge` returns a copy of the receiver like Ruby.**
  A bare `hash.merge` now reads as a zero-argument call that returns a copy of
  the receiver instead of leaking the unbound method value. This matches Ruby,
  where the call has no parentheses distinction, so `{ a: 1 }.merge` and
  `{ a: 1 }.merge()` both return `{ a: 1 }`.

### Numbers, ranges, and Math

- **Added: Ruby-style range query and conversion helpers.** Ranges now answer
  `cover?`, `include?`, and `member?` membership predicates (numeric arguments
  are tested against the range bounds, exclusivity, and direction; any other
  type is never a member and returns `false` rather than raising), the metadata
  helpers `first`, `last`, `size`, and `exclude_end?`, and the `to_a`
  materialization. Because Vibescript iterates descending ranges such as `5..1`,
  `size`, `to_a`, `first(n)`, and `last(n)` report that descending sequence
  rather than the empty result Ruby produces; the remaining helpers match Ruby.
  `to_a` and the counted `first(n)`/`last(n)` forms build their arrays under the
  sandbox step and memory quotas so large ranges fail safely instead of
  exhausting memory.
- **Added: Ruby-style range and numeric enumeration helpers.** Integers now
  answer `upto(limit)`, `downto(limit)`, and `step(limit, by = 1)`, each yielding
  the relevant integers to the block and returning the receiver. Ranges gained
  Enumerable-style iteration helpers `each`, `step(n)`, `map`, `select`,
  `reject`, `find`, `reduce`, `count`, `sum`, `min`, and `max`. The iteration
  follows Vibescript's range direction (ascending for `1..5`, descending for
  `5..1`) and charges one sandbox step per element so a wide span fails on the
  step quota rather than running unbounded; the array-building helpers (`map`,
  `select`, `reject`) honor the memory quota as the result grows. `step` advances
  by its stride directly, so a sparse step over a wide span only charges the step
  quota for the values it yields. `step` arguments must be nonzero (integers) and
  positive (ranges), `sum` errors on 64-bit overflow, and the terminal value is
  detected before each increment so a span reaching the 64-bit bounds stops
  cleanly instead of wrapping.
- **Added: Ruby-style float special values and division-by-zero behavior.** Float
  division by zero with the `/` operator (and `Float#fdiv`/`Integer#fdiv`) now
  follows IEEE 754 like Ruby instead of raising: a finite nonzero numerator
  yields `Infinity` or `-Infinity` and a zero numerator yields `NaN`. Integer
  division by zero (`1 / 0`) still raises, matching Ruby's `ZeroDivisionError`.
  Floats gained `nan?` (true only for `NaN`), `infinite?` (`1` for `Infinity`,
  `-1` for `-Infinity`, `nil` otherwise), and `finite?` (true when neither
  infinite nor `NaN`). Special values print as `Infinity`, `-Infinity`, and
  `NaN`, and `JSON.stringify` continues to reject non-finite floats because JSON
  has no representation for them. `div`, `divmod`, `modulo`, and `remainder` keep
  raising on a zero divisor, matching Ruby. Comparisons follow IEEE 754:
  comparisons against `NaN` are unordered, so `<`, `<=`, `>`, and `>=` return
  `false` and the spaceship operator `<=>` returns `nil`. Coercing a non-finite
  float to an integer now raises rather than silently yielding a garbage value,
  so a `NaN`/`Infinity` range endpoint, `money_cents` amount, or duration operand
  reports a clear error.
- **Added: Ruby-style numeric rounding precision.** `Float#round`, `Float#floor`,
  and `Float#ceil` now accept an optional Integer precision: positive `ndigits`
  keep the value a float rounded to that many fractional digits, while zero or
  negative `ndigits` return an integer bucketed to a power of ten. `Integer#round`,
  `Integer#floor`, and `Integer#ceil` gained the same precision argument, leaving
  the value unchanged for non-negative precision and bucketing for negative
  precision. Float rounding matches Ruby's half-away-from-zero correction, and
  any conversion back to an integer keeps the existing 64-bit overflow checks
  instead of widening like Ruby's bignums.
- **Added: Ruby-style numeric division helpers.** Integers and floats now expose
  `div` (floored division returning an integer), `divmod` (the floored quotient
  paired with the divisor-signed modulo), `fdiv` (floating division), and
  `remainder` (truncated-division remainder whose sign follows the receiver, so
  it differs from `%` for mixed-sign operands). A zero divisor errors for all
  four, and quotients outside the 64-bit range error rather than wrapping. Ruby's
  `fdiv` infinity result is intentionally an error instead, matching the `/`
  operator, and `quo` is intentionally omitted because Vibescript has no rational
  number type.
- **Added: Ruby-style numeric predicate and successor helpers.** Integers and
  floats gain `zero?`, `positive?`, `negative?`, and `nonzero?` (returning the
  receiver or `nil`), and integers gain `next`/`succ` and `pred`.
- **Added: Ruby-style `Math` module.** The new `Math` namespace exposes the
  constants `Math::PI` and `Math::E` (readable with `::` or `.`) and the pure
  numeric helpers `sqrt`, `cbrt`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`,
  `atan2`, `exp`, `log`, `log2`, `log10`, and `hypot`. Integer arguments are
  promoted to floats and every helper returns a `float`, matching Ruby.
  Arguments outside a function's domain (e.g. `Math.sqrt(-1)`, `Math.asin(2)`,
  or `Math.sin`'s well-defined relatives applied beyond their range) raise a
  domain error like Ruby's `Math::DomainError`. In-domain special values follow
  Ruby and IEEE 754: `Math.log(0)` returns `-Infinity`, `Math.sin`/`cos`/`tan`
  of `Infinity` return `NaN`, and a `NaN` argument propagates unchanged.
- **Changed: the spaceship operator `<=>` now returns `nil` for incomparable
  operands** instead of raising, matching Ruby. Mixed-kind pairs such as
  `1 <=> "a"`, money values in different currencies, and `Time#<=>` against a
  non-`Time` now yield `nil`, while comparable pairs still return `-1`/`0`/`1`.
  The relational operators `<`, `<=`, `>`, `>=` keep raising on incomparable
  operands, matching Ruby's `ArgumentError`.

### Time

- **Added: Ruby-style `Time` HTTP/XML/RFC date helpers.** `Time#xmlschema` is an
  alias for `Time#iso8601` (including its optional `ndigits` precision argument).
  `Time#httpdate` renders the HTTP-date / IMF-fixdate form (RFC 7231), always in
  GMT, e.g. `"Tue, 02 Jan 2024 03:04:05 GMT"`. `Time#rfc2822` and its alias
  `Time#rfc822` render the RFC 2822 mail date preserving the receiver's zone
  offset; a genuine UTC receiver uses the `-0000` zone Ruby reserves for
  timestamps without real zone information while an explicit zero offset uses
  `+0000`. `httpdate`, `rfc2822`, and `rfc822` drop sub-second precision and take
  no arguments, raising on any positional or keyword argument.
- **Added: Ruby-style `Time#iso8601(ndigits)` precision.** `Time#iso8601` and its
  `Time#rfc3339` alias now accept an optional non-negative `ndigits` argument that
  appends fractional-second digits, truncated toward zero like Ruby. No argument
  keeps whole-second RFC3339 output, the timezone offset is preserved, and digits
  beyond nanosecond resolution are zero-padded (capped at 100 digits to bound
  allocations). Negative, non-integer, out-of-range, or extra arguments raise a
  clear runtime error.
- **Added: Ruby-style subsecond parts for `Time.local`, `mktime`, `utc`, and
  `gm`.** These calendar constructors now read their seventh positional argument
  as microseconds-with-fraction instead of routing it through timezone parsing.
  Integer microseconds are exact and floats carry sub-microsecond precision down
  to the nanosecond, while a non-numeric microsecond argument raises a runtime
  error. `Time.new` keeps its Ruby distinction of accepting a zone/offset in the
  seventh position. Unlike Ruby, a string microsecond argument is rejected rather
  than coerced via leading-digit parsing.
- **Added: Ruby-style subsecond arguments for `Time.at`.** `Time.at` now accepts
  an optional second positional subsecond value and an optional third positional
  unit symbol in addition to int/float epoch seconds. The subsecond value
  defaults to microseconds, and the unit may be `:microsecond`/`:usec`,
  `:millisecond`, or `:nanosecond`/`:nsec` (e.g. `Time.at(0, 123456)` and
  `Time.at(0, 123456789, :nsec)`). The `in:` zone keyword composes with every
  form. A unit symbol without a subsecond value, an unknown unit, or a
  non-numeric subsecond value raises a runtime error. Unlike the calendar
  constructors (`Time.utc`/`Time.local`), `Time.at` does not treat an explicit
  `nil` subsecond as omitted: `Time.at(0, nil)` raises just as Ruby does.
  Subsecond values are floored toward negative infinity at nanosecond
  resolution rather than retaining Ruby's arbitrary-precision rationals, so a
  negative fractional offset rounds the way Ruby exposes it
  (`Time.at(0, -1.9, :nsec).nsec == 999999998`). A subsecond magnitude too large
  to express within that nanosecond range is rejected with `Time.at subsecond
  value out of range` instead of silently wrapping into a bogus instant.
- **Added: Ruby-style default date parts for `Time` calendar constructors.**
  `Time.new`, `Time.utc`, `Time.gm`, `Time.local`, and `Time.mktime` now require
  only a year. As in Ruby, an omitted month or day defaults to `1` and omitted
  time fields default to midnight, so forms such as `Time.new(2024)`,
  `Time.utc(2024)`, and `Time.utc(2024, 2)` build January 1 (or the first of the
  given month) at the start of the day instead of raising an arity error. An
  explicit `nil` in an optional position is treated the same as omitting it, so
  `Time.utc(2024, nil)` builds January 1 rather than normalizing month `0` into
  the prior year. The year itself remains required: a `nil` (or any other
  non-numeric) year raises instead of coercing to year `0`.
- **Added: Ruby-style offset arguments for `Time#getlocal` and
  `Time#localtime`.** Both now accept an optional timezone offset (for example
  `"+05:30"`, `"-04:00"`, a named zone, or `"UTC"`) and return the same instant
  in that zone, falling back to the host's local zone when the argument is
  omitted or `nil`. The offset uses the shared zone-parsing rules, and the
  receiver is never mutated, so `localtime` fits Vibescript's immutable value
  model while matching Ruby's non-mutating `getlocal(offset)` result.
- **Added: Ruby-style `Time#to_a` tuple conversion.** `Time#to_a` returns the
  positional field tuple `[sec, min, hour, mday, month, year, wday, yday, isdst,
  zone]`, matching Ruby for compatibility with positional field processing. Field
  values reuse the existing `Time` accessors, so UTC, local, and offset receivers
  stay consistent across both forms.
- **Added: `Time#round` precision argument.** `Time#round` now accepts an
  optional Ruby-style `ndigits` (defaulting to `0`) so `round(3)` and `round(6)`
  produce millisecond and microsecond precision, with non-negative `Integer`
  validation and clear errors on misuse.
- **Added: Ruby-style `Time#strftime` formatting.** `Time#strftime` accepts a
  Ruby percent format string so Ruby date-formatting code runs unchanged, sitting
  alongside the existing Go-layout `Time#format`. The supported directive subset
  covers year/month/day, 12- and 24-hour time, minute/second, sub-second
  (`%L`, `%N`, and widths like `%6N`), weekday and month names, weekday numbers
  (`%w`/`%u`), epoch seconds (`%s`), UTC offset (`%z`, `%:z`, `%::z`, and the
  `%:::z` compact form), zone name
  (`%Z`), the `%n`/`%t`/`%%` escapes, and the compound shortcuts
  `%F`/`%T`/`%X`/`%R`/`%D`/`%x`/`%r`/`%c`. Directives honor Ruby's flags and
  width between the `%` and the letter: `-` (no padding), `_` (space padding),
  `0` (zero padding), `^` (uppercase), and `#` (toggle case), with an optional
  width applied to every numeric and name directive (so `%-d` is `2`, `%6Y` is
  `002024`, and `%^B` is `JANUARY`). Unknown directives pass through verbatim
  like Ruby (`%Q` stays `%Q`), `%Z` mirrors `Time#zone` (so fixed-offset
  receivers render their offset name rather than Ruby's empty string), and a
  trailing `%` (or a modifier with no directive) raises a clear runtime error. A
  script-controlled width is bounded by the sandbox memory quota, so a format like
  `%1000000000N` is rejected before allocating its padding rather than risking an
  out-of-memory crash.
- **Added: Ruby-style numeric-second `Time` arithmetic.** `time + number` and
  `time - number` now treat the number as seconds, matching Ruby. Integers shift
  by whole seconds, floats carry sub-second precision down to the nanosecond, and
  negative values shift backward. Numeric addition commutes (`number + time`),
  and an out-of-range or non-finite offset raises a runtime error.
- **Changed: `time - time` now returns a `Float` number of seconds** instead of a
  whole-second `Duration`, matching Ruby's `Time#-` and preserving sub-second
  precision.
- **Fixed: `Time#eql?` and `Duration#eql?` now behave like Ruby predicates for
  wrong-type operands.** Both methods return `false` when given an operand of the
  wrong kind (for example `time.eql?(1)` or `duration.eql?(Time.utc(2024, 1,
  1))`) instead of raising a type error, matching Ruby's `Time#eql?`. Equal
  same-kind operands still return `true`, unequal ones `false`, and only the
  wrong number of arguments raises an argument-count error.

### Checker, CLI, and embedding

- **Added: check mode flags unresolved value and function names.** `vibes run
  -check` and the `CheckWarnings*` APIs now report `undefined variable NAME`
  for bare value/function references that cannot resolve to any binding the
  checker can see, matching the error the reference raises at runtime. The
  resolution model deliberately over-approximates the defined-name set —
  locals any branch can bind, exports of any statically resolvable `require`
  anywhere in the script, builtins, host globals and capabilities supplied
  through `CallOptions`, and implicit-self scopes in methods and class bodies
  — so a warning means the reference is guaranteed to fail. A `require` whose
  module name is not a literal suppresses the check for the whole script.
  `-e` snippets additionally run an order-independent whole-snippet pass so
  functions the snippet never calls are covered. Hosts that inject globals or
  capabilities must check with the same `CallOptions` they later pass to
  `Call`; those names then resolve and are never reported. `vibes analyze`
  deliberately does not surface these warnings: it runs over documentation
  fragments and host-embedded scripts whose free names are legal at runtime.
- **Added: check mode rejects typed block parameters contradicted by literal
  receivers.** When a literal array of scalar literals flows through a builtin
  element iterator (`each`, `map`, `select`, `reject`, `find`, and
  `each_with_index` with its integer index parameter), a block parameter type
  annotation that any yielded element misses is now a check warning —
  `argument NAME expected TYPE, got KIND` at the annotation, exactly the error
  the first mismatching yield raises at runtime. The check stays silent for
  non-literal or empty receivers, hash receivers, destructured or extra block
  parameters, untyped parameters, user-defined iterators, and iterators
  outside the covered set, leaving those shapes to runtime enforcement.
- **Hardened CLI source-size enforcement.** `vibes run`, `vibes analyze`, and
  `vibes test` now read each script through a single size-checked descriptor,
  bounded at the engine's configured source-size limit, so an oversized file
  (even one swapped or grown after the check) is rejected before it is loaded
  fully into memory.
- **Fixed: bounded result rendering for large composite return values.**
  `Value.String` now streams array and hash rendering directly into one growing
  buffer instead of building a `[]string` per element and a `fmt.Sprintf` per
  hash entry before joining, so formatting no longer allocates O(n) intermediate
  strings. The new `Value.StringBounded(limit)` renders a value while stopping
  at a byte budget and reporting `ErrStringRenderTruncated`, and the `vibes run`
  CLI uses it with a 1 MiB cap so a script that returns a huge nested array or
  hash fails with `result rendering exceeds …` instead of allocating the whole
  formatted string in host memory after the runtime quotas have already
  released. Cycle detection is unchanged.
- **Hardened the public jobqueue option parser.** `jobqueue.ParseEnqueueOptions`
  now rejects extra enqueue keywords that are not data-only or that contain
  cyclic references instead of cloning them through to the host, closing a
  contract gap for embedders that call it directly. A new
  `jobqueue.ParseEnqueueOptionsValidated` fast path lets the runtime adapter skip
  the redundant walk when it has already enforced the contract, and the carved
  package gained direct unit tests for constructor validation, retry detection,
  option parsing, cloning, and invalid/cyclic values.
- **Fixed: static check no longer takes exponential time on deeply nested
  conditionals.** `vibes run -check` and the `CheckWarnings*` API used to grow
  ~4x per two levels of nested `if`/`elsif` and hung near depth 300; deep
  nesting now checks in milliseconds. As a backstop, the checker also rejects
  control flow nested beyond 512 levels with a deterministic
  `check exceeded maximum nesting depth of 512` diagnostic instead of
  stalling. The cap applies only to check mode: the parser and runtime accept
  and execute such scripts unchanged.
- **Fixed: check-mode false positives from incomplete AST walkers.** `vibes
  run -check` now resolves locals bound inside destructuring index selectors
  and no longer flags later literal elements when a block body contains
  `retry`, which ends the iteration early just like `break`.
- **Improved: `vibes analyze` unreachable-statement coverage.** Statements
  following an unconditional `break`, `next`, or `retry` are now reported as
  unreachable, and nested definition bodies are linted.
- **Changed: the embedding API is tiered and documented for 1.0.** Every
  exported symbol in `vibes`, `vibes/value`, `vibes/source`, and the
  capability packages now carries documentation; the internal-plumbing
  exports the runtime needs from `vibes/value` are explicitly marked as
  carrying no compatibility promise; and two orphaned helpers
  (`value.TimeFromEpoch`, `value.InspectByteLen`) were removed. The supported
  surface, host ownership model, and concurrency hazards are declared in
  `docs/embedding-api-stability.md`.

### Tooling: LSP and watch mode

- **Improved: LSP large-document response cost.** Diagnostics publishes now
  skip the recompile when the buffer text is byte-identical to the last
  compile, diagnostics payloads use typed structs instead of nested maps, and
  document-symbol outlines are cached per document and invalidated on every
  edit and fresh compile, keeping large buffers responsive on each keystroke.
- **Improved: watch-mode overhead on large module roots.** Full rescans reuse
  the previous snapshot's map storage and walk directory entries without
  per-file `FileInfo` allocations, and the polling loop's per-tick known-file
  check stats through a platform shim that avoids `os.Stat` allocations on
  macOS and Linux; symlink handling, added/deleted `.vibe` detection, and
  periodic full scans are unchanged.
- **Performance: reduced large-document LSP diagnostics and symbol allocation
  churn.** `textDocument/documentSymbol` now builds the outline from typed
  structs instead of nested `map[string]any` values, and the script-local
  completion index is built lazily on the first completion request rather than
  on every diagnostics publish. Repeated edits in large files no longer pay to
  clone every compiled function or to allocate per-symbol range maps when no
  completion is requested. The wire output for diagnostics, document symbols,
  and completions is unchanged.
- **Fixed: `vibes lsp` releases per-document state on `textDocument/didClose`.**
  Closing a file now evicts its document text, compiled-script, navigation,
  completion, diagnostics, and outline caches and publishes an empty
  diagnostics set so editors clear stale squiggles. Previously the server kept
  every document's state for the life of the process, so long editor sessions
  touching many files grew memory unboundedly.

### Performance and sandbox accounting

- **Fixed: typed hash boundaries no longer copy conforming payloads.** Passing
  a hash through a `hash<K, V>` parameter or return annotation used to build a
  full output copy before discovering that no value needed coercion, roughly
  doubling the allocation cost of a no-op boundary crossing (a conforming
  10,000-entry `hash<string, int>` argument paid for two hashes instead of
  one). Normalization now validates in place and returns the original
  container when nothing changes; small symbol-keyed hashes also skip the
  temporary entry materialization during validation. When a value does need
  coercion (for example a symbol against an enum-valued hash), the copy still
  carries every already-validated entry in insertion order, normalizes the
  Ruby-style default value, and preserves the hash-versus-object receiver
  kind, exactly as before.
- **Improved runtime allocation behavior for string splitting and array
  aggregation.** `String#split` now builds its result values directly instead
  of staging an intermediate `[]string`, and `Array#group_by`,
  `Array#group_by_stable`, and `Array#tally` avoid parallel key-value maps while
  preserving first-encounter result ordering.
- **Improved: call-boundary copying for large host data payloads.** Data-only
  host argument graphs (scalars plus alias-free arrays, hashes, and objects)
  now deep-copy through a tight copier instead of the full rebind walk at
  `Script.Call` entry, composite `CallOptions.Globals` bind lazily and are
  cloned only when the script (or an inheriting task) actually reads them, and
  the contracted capability boundary clones scalar-only row maps wholesale.
  Isolation is unchanged: every host value a script can reach is still a
  per-call deep copy, capability grants captured by escaped closures are still
  revoked on re-entry, and `StrictEffects` still validates globals eagerly at
  bind time. An unused large global no longer pays its full clone (~24x less
  wall time in `BenchmarkTasksMapUnusedLargeGlobal`); large payload calls
  spend less on rebind bookkeeping and capability-boundary cloning. (#422,
  #366, #353)
- **Changed: memory-quota timing for unread composite globals.** A
  `Script.Call` carrying a composite global that would exceed the memory
  quota no longer fails at bind time when the script never reads it: the
  quota is charged when (and only when) the global is materialized on first
  read, at which point the call fails exactly as before. Hosts using the
  memory quota as inbound-payload admission control should size-check
  payloads before the call instead of relying on the bind-time rejection.
  (#366)
- **Performance: memory-quota checks memoize the estimator's base walk.** The
  estimator's reachable-graph walk is now memoized per execution and reused by
  consecutive checks while a process-wide mutation epoch (bumped by every value
  wrapper mutator, environment write, and builtin dispatch) and the execution's
  root-set topology prove the graph unchanged, so per-statement, argument, and
  call-boundary checks around a large stable payload no longer each re-pay a
  full graph walk. Estimates are byte-identical to the unmemoized walk — the
  memoized check resumes the exact same deduplicated computation — so quota
  pass/fail thresholds do not move; large capability payload calls
  (`BenchmarkCapabilityContractLargeArgs/rows_10000`) run about 2x faster.
- **Hardened `for`-loop sandbox accounting.** Each `for` iteration over an array
  or range now charges a step before evaluating the body, matching `while` and
  `until`. A large `for` loop therefore still respects the step quota and still
  surfaces `context.Canceled` once a host callback cancels the context, even when
  the loop body is empty.
- **Fixed: `Array#flatten` and `Hash#flatten` now participate in sandbox
  accounting while their results are being built.** Flatten's output length
  cannot be cheaply bounded up front (each receiver slot can expand into
  arbitrarily many leaves), so both builds now charge one step per element
  examined — running the periodic memory and context checks — and charge the
  output backing's growth before each doubling is allocated. `Hash#flatten`
  additionally meters its `[key, value]` pair pre-build the way `Hash#to_a`
  does. Both methods previously charged ~0 steps, so large flattens now
  consume step quota proportional to the elements they examine. An oversized
  flatten is rejected before the over-quota backing exists, instead of after
  the full result was allocated natively, and a canceled context stops the walk
  mid-build. The recursion also builds into a single shared output slice,
  replacing the per-nesting-level slices the old merge-upward implementation
  allocated. Results are unchanged for in-quota calls. Focused quota coverage
  now pins `Array#join`, fixed-size `Array#window`, `Array#flatten`, and
  `Hash#flatten` in both directions: an in-quota call succeeds byte-identically
  and an oversized call fails with the quota error before the result
  materializes.
- **Hardened hash transform sandbox accounting.** `Hash#merge`, `Hash#update`,
  `Hash#merge!`, `Hash#replace`, `Hash#store`, `Hash#except`, `Hash#slice`,
  `Hash#compact`, `Hash#remap_keys`, `Hash#select`, `Hash#reject`,
  `Hash#transform_keys`, and `Hash#transform_values` now project the size of the
  derived map against the memory quota before reserving it, so a transform over a
  large hash is rejected up front instead of after a full output map has already
  been allocated. The blockless transforms charge the step quota per entry and
  honor context cancellation while walking their entries, and `Hash#replace`
  charges a step per copied entry as it adopts the replacement's contents.
  Block-driven transforms (`Hash#transform_keys`, `Hash#transform_values`, and
  the `Hash#merge` conflict block) additionally charge each block result against
  the quota as it is produced. That block-result charge is deliberately
  conservative: each result is counted at its full current size, deduplicated
  only against other block results and never against the receiver or other call
  roots, because deduplicating against the baseline would let a block that
  mutates a receiver-owned container in place escape the quota; a block that
  returns a receiver-shared value unchanged is therefore over-counted rather
  than under-counted, keeping the bound sound (the array-side equivalent of
  this accounting is tracked in #787). The sorted key scratch these transforms
  iterate with, the preallocated output backing, and `Hash#except`'s exclusion
  set are all reserved against the quota for as long as they are live, so the
  combined output-plus-scratch peak stays bounded; `Hash#merge` with a conflict
  block reserves the exact distinct key union rather than a loose sum, so an
  overlapping merge that genuinely fits is not falsely rejected. `Hash#each`,
  `Hash#each_key`, and `Hash#each_value` build no derived map and no longer
  reserve one, they charge the step quota per entry directly so an empty block
  still observes cancellation, and a bare `Hash#merge { ... }` with no argument
  hashes returns a copy of the receiver without running the block or charging
  scratch it never allocates. The charge is cycle-safe: a cyclic block result is
  charged a finite amount once. `Hash#store` sizes its projection by the
  existing-key case so replacing a key no longer over-reports the result size.
  `Hash#deep_transform_keys` is intentionally left unbounded for now; bounding
  its recursive materialization is tracked separately in #786.
- **Hardened interpolated string materialization under sandbox limits.**
  Double-quoted interpolation now builds its result incrementally, charging a
  step and checking the projected byte length against the memory quota before
  appending each segment. The projection for an interpolated expression is
  computed without rendering the value, so an aggregate whose representation
  expands far beyond its own footprint (for example an array or hash holding
  many references to one large string) is rejected before the oversized join is
  materialized rather than after. A script that grows a string through repeated
  or large interpolation (for example `"#{text}#{text}"` in a loop) now fails
  with a memory quota error before the oversized result is materialized, and a
  canceled context stops construction promptly. Small interpolations are
  unchanged.
- **Fixed: `Array#reduce` charges its accumulator against the memory quota
  without double counting.** A fold whose block destructures the accumulator with
  a rest target, such as `reduce(big) do |(head, *tail), item| ... end`, copies
  part of the live accumulator into a fresh backing. The accumulator lives only on
  the runtime's Go stack and evolves every call, so it was missing from the
  per-call memory accounting; the fold now charges the current accumulator on each
  call, closing a path that could allocate past the sandbox quota. A reduce with no
  initial value makes the accumulator the receiver's first element, which is already
  counted in the receiver, so the accumulator charge now deduplicates against the
  receiver: a large first element is charged once, not twice, so a quota that fits
  the real peak is no longer wrongly rejected.
- **Fixed: rest-collecting block parameters reject an over-quota tail before
  allocating it.** A block such as `[[huge...]].each { |(head, *tail)| }` copies the
  collected tail into a fresh backing slice when it binds `tail`. The bind charge now
  preflights that window against the memory quota before the copy, so a quota smaller
  than a single copied tail rejects the walk before the backing is materialized
  instead of allocating the whole tail first and only then reporting the overflow. The
  charge applies only to a named rest: a bare anonymous rest such as
  `|(head, *)|` discards its window without allocating a backing, so it keeps the
  no-allocation fast path instead of seeding the estimator with the whole yielded value
  on every iteration.
- **Fixed: block-driven hash transforms count their output map in the rest-bind
  charge.** `Hash#select`, `#reject`, `#transform_keys`, `#transform_values`, and a
  block-conflict `#merge` hold their preallocated output map and sorted-key scratch
  while the block binds a rest-collecting destructure parameter such as
  `|k, (head, *tail)|`. Those buffers are now reserved against the memory quota before
  the block runs, so the rest-bind charge measures the fresh tail copy on top of them
  rather than against a baseline that omitted them, closing a path where the combined
  peak could exceed the quota.
- **Fixed: rest destructuring now charges its captured window and live
  right-hand side against the memory quota.** When a destructuring assignment
  builds a named rest array (for example `values[1], *rest = values` or
  `a, *rest = build_large_array()`), the captured window is now metered before
  it is allocated, alongside both the right-hand-side snapshot it may coexist
  with and the evaluated right-hand side itself when that value is held only on
  the call stack (a function or capability return, or an array literal). A
  sandboxed script can no longer exceed `MemoryQuotaBytes` by roughly the size
  of the right-hand side by routing a large off-stack array through a rest
  target.
- **Fixed: block bodies now charge an ephemeral receiver against the memory
  quota alongside what they retain.** Iterating a receiver held only by the
  builtin's call frame (for example `make_hash().each do |(k, (head, *tail))|`)
  with a rest-collecting destructure used to count the receiver only in the
  one-time bind-charge snapshot while the body's checks counted only the
  environment, so a loop copying tails into a retained accumulator could
  transiently hold roughly twice `MemoryQuotaBytes` without either view
  noticing. The bind charge now measures those Go-frame-only roots once and
  reserves them into the live baseline for each block call, so per-statement
  and mutator-growth checks bound the combined peak and reject it mid-loop at
  the quota. Receivers reachable from the environment deduplicate to a ~zero
  reservation, and the reservation itself is O(1) per call, so existing
  workloads are unaffected.
- **Fixed: large blockless materializations no longer re-walk the whole heap
  every 16 steps.** Blockless `Array#flatten`, `chunk(n)`, `window`, `join`,
  and `reverse`, plus `Hash#to_a` and `Hash#flatten`, already charge every
  allocation against the memory quota before performing it, yet their
  per-element step accounting also re-ran the full reachable-graph memory walk
  each period, making big builds quadratic — a 1M-leaf `flatten` under raised
  quotas took minutes of CPU for a sub-second build. These loops now run as
  accumulator-metered sections that skip the redundant periodic walk while
  keeping the step-quota and context-cancellation checks on the same schedule.
  Quota acceptance thresholds are unchanged, and any script re-entry or nested
  builtin dispatch suspends the section so full checks always apply outside
  the metered loop.

### Fixes

- **Fixed: a host-crashing panic in destructuring assignment.** A destructuring
  target with more fixed targets than the value provides plus a rest target (for
  example a block parameter `|(a, b, c, *rest)|` applied to a two-element value)
  sliced out of range and panicked the interpreter, which a sandboxed script
  could trigger as a denial of service. The missing fixed targets now bind to
  `nil` and the rest is empty, matching Ruby.
- **Fixed: AST cloner dropped mixin and visibility class members.** The
  definition cloner in `internal/ast` silently omitted `include`/`extend`
  and visibility declarations from cloned class member lists and shared
  nested module declarations with the original. The parser fuzz target's
  completeness invariant also lagged the last several syntax additions,
  rejecting valid parses of endless/beginless ranges, splat and keyword-splat
  arguments, block-pass arguments, mixin and visibility members, empty quoted
  symbols, and bare rest-discard assignments; each form is now accepted,
  seeded into the fuzz corpus, and pinned by a coverage test that fails
  deterministically when a new AST shape is missed. (#902)
- **Internal: AST walker completeness gates.** New tests enumerate every AST
  node type from source and fail when a hand-maintained walker (cloner,
  checker collectors, escape analysis, symbol interning, evaluator dispatch,
  analyzer) misses a type, and a reflection gate proves the AST cloner copies
  every field of every node without sharing state.

## v0.50.0 - 2026-06-11

- **Added: stronger CLI workflows.** `vibes run` now supports inline `-e`
  evaluation and recursive `-watch` mode, and the new `vibes test` command runs
  `.vibe` test files with module-aware fixture coverage.
- **Added: broader LSP support.** The language server now exposes document
  formatting through `vibes fmt`, context-aware completion, signature help,
  go-to-definition, and document symbols, with live-buffer re-anchoring for
  completion and navigation targets.
- **Improved host-facing diagnostics.** Structured parse-error positions are
  available through the public API and LSP, inline snippets remap diagnostics to
  user source positions, lookup failures include did-you-mean suggestions, and
  runtime error wording now follows the documented error-message conventions.
- **Fixed parser and runtime edge cases.** Newline statement boundaries no longer
  accidentally chain line-start expressions, line-ending minus continuations are
  preserved, trailing brace blocks parse correctly, `case` ranges match by
  membership, enum value kinds render distinctly, and hash method names win over
  colliding hash keys in member dispatch.
- **Hardened runtime accounting and arithmetic.** Sandbox limit terminations are
  classified distinctly, capability argument validation runs once while still
  counting validated arguments against memory quota, and integer, duration, and
  time arithmetic now reject `int64` overflow instead of silently wrapping.
- **Improved runtime performance.** Per-call environment and builtin churn,
  memory-estimation work, regex compilation, step context polling, builtin member
  dispatch allocation, and scalar-key array set operations were all tightened
  with regression coverage and benchmark-focused checks.
- **Expanded quality gates and documentation.** CI now enforces coverage, the
  stdlib and LSP documentation were expanded, benchmark artifacts and hotspot
  profiles were refreshed, input-guard limits were centralized, module
  containment edge cases were pinned, and public facade/value package tests were
  added.

## v0.40.0 - 2026-06-06

- **Added: `Tasks` structured concurrency for bounded in-script fanout.**
  Scripts can now use `Tasks.run` to create an automatically awaited task scope,
  `tasks.spawn` to start named function calls, `task.value` to wait for a single
  result, and `Tasks.map` to collect ordered concurrent results.
- **Added host-controlled task concurrency settings.**
  `Config.DefaultTaskConcurrency` defaults task fanout to `4` unless the host
  cap is lower, and `Config.MaxTaskConcurrency` caps script-provided `max:`
  overrides. Requests above the host cap raise an error instead of being
  silently clamped.
- **Hardened task isolation, cancellation, and quota accounting.** Task
  arguments, keyword arguments, results, and inherited mutable globals are cloned
  across task boundaries; task failures propagate through handles or scope exit;
  retained task results count against the parent memory quota while the task
  scope is alive.
- Added a Tasks ADR, README and host-cookbook coverage, a runnable Tasks example,
  `# vibe: 0.4` example headers, deterministic `testing/synctest` coverage for
  concurrency behavior, and a Go 1.26 goroutine leak profile CI gate.

## v0.31.0 - 2026-05-30

- **Fixed: `Money` arithmetic now rejects `int64` overflow instead of silently
  wrapping.** `Add`, `Sub`, `MulInt`, and `DivInt` detect overflow (including
  the `-MinInt64` and `MinInt64 / -1` edges) and return an error, matching the
  range check `ParseMoneyLiteral` already enforces. **Breaking (embedders):
  `value.Money.MulInt` now returns `(Money, error)` instead of `Money`; update
  call sites to handle the error.** Plain integer arithmetic in scripts still
  wraps — money is deliberately stricter.
- **Fixed: deeply nested type annotations can no longer crash the host.** The
  parser bounds type-annotation recursion at depth 64 and emits a normal parse
  error (`type annotation nesting too deep`) instead of overflowing the
  goroutine stack on attacker-supplied source reached through `Engine.Compile`.
- **Fixed: capability contracts now follow builtins captured in closures and
  blocks.** The contract scanner descends into script-function and block
  environments — with a cycle guard and an ambient-global stop — so a contracted
  builtin captured in a closure no longer escapes its `ValidateArgs` /
  `ValidateReturn` enforcement, and an unrelated same-named global is never bound
  to a capability scope through a script-supplied closure. Defense-in-depth: no
  bundled capability returns a closure-wrapped builtin, so default embedders were
  not exposed.

## v0.30.0 - 2026-05-30

- **Fixed: `||` and `&&` now return the surviving operand, not a coerced
  boolean.** `a || b` is `a ? a : b` and `a && b` is `a ? b : a` (Ruby
  semantics), so the documented `value = optional || default` idiom works.
  Previously both collapsed to `true`/`false`. Truthiness rules are unchanged.

## v0.29.0 - 2026-05-17

- Completed the embedder-facing package refactor: value-system types now live
  in `github.com/mgomes/vibescript/vibes/value`, capability adapter contracts
  live under `vibes/capability/{contextcap,db,events,jobqueue}`, and source
  positions live in `vibes/source`.
- Hid the AST, parser, runtime engine, module loader, builtins, and runtime
  capability adapters under `internal/` packages so the public API is centered
  on `vibes.Engine`, `vibes.Script`, capability construction, runtime errors,
  and value/capability subpackages.
- **Breaking (embedders): removed the v0.28 deprecation alias bridge.**
  Update imports from root `vibes` to the new subpackages:
  - `vibes.Value`, `Money`, `Duration`, `Range`, `KindXxx`, and `NewXxx` move
    to `github.com/mgomes/vibescript/vibes/value`.
  - `vibes.Database`, `DatabaseReader`, `DatabaseWriter`, `DBFindRequest`, and
    related database request/result types move to
    `github.com/mgomes/vibescript/vibes/capability/db`.
  - `vibes.EventPublisher` and `EventPublishRequest` move to
    `github.com/mgomes/vibescript/vibes/capability/events` as
    `events.Publisher` and `events.PublishRequest`.
  - `vibes.JobQueue`, `vibes.JobQueueWithRetry`, `vibes.JobQueueJob`,
    `vibes.JobQueueEnqueueOptions`, and `vibes.JobQueueRetryRequest` move to
    `github.com/mgomes/vibescript/vibes/capability/jobqueue` as
    `jobqueue.JobQueue`, `jobqueue.JobQueueWithRetry`,
    `jobqueue.JobQueueJob`, `jobqueue.JobQueueEnqueueOptions`, and
    `jobqueue.JobQueueRetryRequest`.
  - `vibes.ContextCapabilityResolver` moves to
    `github.com/mgomes/vibescript/vibes/capability/contextcap` as
    `contextcap.Resolver`.
  - Previously deprecated AST/parser types under `vibes` are removed; drive
    scripts through `vibes.Engine` and `vibes.Script` instead.
- **Breaking (embedders): root `vibes` no longer exports direct runtime payload
  constructors or accessors** for blocks, classes, instances, enums, or script
  functions. Use the documented engine/script APIs and typed payload markers on
  `value.Value`.
- Updated `Value` runtime-bound accessors so builtin, class, instance, function,
  block, enum, and enum-value accessors return `value.*Payload` marker
  interfaces instead of concrete runtime types. Data-only accessors such as
  `Bool`, `Int`, `Float`, `String`, `Array`, `Hash`, `Money`, `Duration`,
  `Time`, and `Range` are unchanged.
- Extracted `cmd/vibes analyze` into `internal/tools/analyze`, added CLI package
  documentation, added public API Godoc examples, and documented `vibes/value`
  as the home for `Money`, `Duration`, and `Range`.
- Added a `golangci-lint` baseline, opt-in pre-commit hook, contribution guide,
  and stronger lint/benchmark automation.
- Modernized the test suite with focused internal runtime/parser/AST packages,
  table-driven cases, `cmp.Diff` snapshots, shared CLI helpers, Godoc examples,
  and broader safe `t.Parallel` coverage.

## v0.28.2 - 2026-05-16

- Fixed a quadratic `combineErrors` path where invalid-UTF-8 input drove CPU usage that scaled with the square of the number of parse errors, closing a cheap server-side DoS vector.
- Closed a module-policy bypass where require arguments that normalized to empty (e.g. `.vibe`, `..vibe`, `0.vibe.vibe`) silently skipped allow/deny enforcement.
- Aligned module-policy normalization with the loader by stripping at most one implicit `.vibe` so allow-lists no longer widen to sibling files like `helper.vibe.vibe` or `pkg/..vibe`.
- Added the fuzz-minimized regression inputs to the committed corpus and expanded the policy invariant tests so the bypass classes are pinned for future runs.

## v0.28.1 - 2026-05-15

- Fixed module policy normalization so whitespace-only path segments cannot produce non-idempotent policy patterns or module names.
- Added the fuzz-minimized module policy case to the committed corpus so the regression is replayed by normal tests and future fuzz runs.

## v0.28.0 - 2026-05-15

- Added broad fuzz coverage across command input paths, formatting, lexing, parsing, compilation, runtime execution, JSON/value conversion, module handling, capability validation, and scalar input helpers.
- Added a `just fuzz` sweep with a 10-second default and a nightly GitHub Actions fuzz workflow for heavier automated coverage.
- Raised the LSP JSON-RPC payload cap so valid near-1 MiB source files are not rejected solely because of protocol framing overhead.
- Restored dot access for keyword-named hash/object members loaded from JSON or remapped data, such as `payload.raise` and `payload.begin`.

## v0.27.0 - 2026-05-04

- Hardened engine API snapshot boundaries so caller-mutated snapshots cannot corrupt later executions, including deep-cloned object-valued builtin tables.
- Tightened module containment by freezing configured module roots at engine creation, preventing cwd/symlink drift, and rejecting non-regular module files before reading.
- Aligned regex-based string helpers with the guarded `Regex` builtins for pattern, input, replacement, and output size limits.
- Added containment coverage for cyclic host arrays, mutable API snapshots, module root drift, regex guard bypasses, and related breakout paths while preserving benchmark smoke gates.
- Cleaned up Go API boundary and test hygiene with stronger error matching, interface checks, documentation, and focused performance follow-ups.

## v0.26.2 - 2026-03-08

- Fixed newline-sensitive parsing in control-flow headers and statement expressions so next-line literals no longer get consumed accidentally while explicit multiline continuations still work.
- Made `&&` and `||` short-circuit lazily and aligned integer division/modulo with Ruby-style floor semantics for signed integer algorithm ports.
- Added Ruby-friendly array query aliases and helpers with `length`, `empty?`, and `fetch`, plus stricter `array.fetch` index validation.
- Expanded regression coverage for Rosetta-style examples with multiline header parsing, short-circuit guards, signed integer arithmetic, and array helper behavior.

## v0.21.0 - 2026-03-08

- Added nominal enums with `::` member access, reflective member helpers, and typed symbol coercion across function and block boundaries.
- Hardened enum and type normalization with case-insensitive resolution, stricter enum-name validation, shadowed-scope lookup fixes, union/hash-key fast-path fixes, and recursive normalization guards.
- Added runnable enum examples and integration coverage, upgraded the REPL to Bubble Tea v2, and added a `just install` recipe for the CLI.
- Strengthened release and quality automation with a race-detector lane, fuzz and benchmark gate tuning, editor support docs, and idempotent release tag reruns.

## v0.20.0 - 2026-02-23

- Runtime call-path performance and benchmark-gating improvements ahead of 1.0.

## v0.1.0 - 2026-02-19

- Initial pre-1.0 project baseline and public documentation set.
