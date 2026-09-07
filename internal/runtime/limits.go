package runtime

// Input-guard limits for stdlib helpers that operate on script-provided
// data. These caps are fixed (not configurable through Config) so the
// JSON and Regex builtins stay bounded regardless of how an embedder
// tunes the engine. They are documented for embedders in
// docs/stdlib_core_utilities.md and the README's "Runtime Sandbox &
// Limits" section; keep those in sync when changing a value.
const (
	// maxJSONPayloadBytes caps JSON.parse input and JSON.stringify
	// output at 1 MiB. JSON decoding allocates proportionally to the
	// payload, so the cap keeps a hostile document from ballooning
	// host memory before interpreter quotas can account for it.
	maxJSONPayloadBytes = 1 << 20

	// maxJSONNestingDepth caps JSON.parse and JSON.stringify container
	// nesting. It matches the standard library JSON scanner's default
	// guard and prevents recursive descent from exhausting the Go stack.
	maxJSONNestingDepth = 10000

	// maxRegexInputBytes caps the subject text, replacement strings,
	// and produced output of every regex entry point (Regex.match,
	// Regex.replace, Regex.replace_all, and the string match/scan/
	// sub/gsub members including the ! variants) at 1 MiB, bounding
	// both scan time and the size of replacement results.
	maxRegexInputBytes = 1 << 20

	// maxFormatOutputBytes caps format/sprintf/String#%/Time#strftime output at 1 MiB.
	// Go's fmt.Sprintf materializes width- and precision-padded output before
	// interpreter quotas can observe the result, so the formatter projects the
	// result size and rejects hostile formats before calling fmt.
	maxFormatOutputBytes = 1 << 20

	// maxOutputHelperBytes caps a single rendered argument passed to
	// puts/print/warn/p. The helpers project the rendering under the execution
	// quotas before materializing it, then this fixed cap prevents unbounded
	// host-side writes when embedders configure very large memory quotas.
	maxOutputHelperBytes = 1 << 20

	// maxRegexPatternSize caps regex patterns at 16 KiB. Patterns are
	// compiled before any quota accounting happens, and pathological
	// patterns are far smaller than pathological inputs, so the
	// pattern cap is deliberately much tighter than the text cap.
	maxRegexPatternSize = 16 << 10

	// maxRegexScanIndexBytes caps the worst-case [][]int index table
	// String#scan's FindAllStringSubmatchIndex call could materialize at
	// 256 MiB. That call allocates 2 + 2*groups ints per match in one
	// contiguous table before any interpreter quota can account for it, so
	// a pattern of thousands of empty capture groups over a large subject
	// would request tens of gigabytes and OOM the host inside the call.
	// scan projects the worst-case table from the subject's rune count, the
	// pattern's minimum match length, and its group count (see
	// regexScanMaxMatches) and rejects before calling the engine when that
	// worst case exceeds this cap. The bound is independent of the
	// configurable memory quota: that quota bounds the script-visible result
	// and is enforced incrementally as the result accumulates, while this
	// fixed cap exists solely to keep the transient host-side index table
	// from exhausting host memory. 256 MiB comfortably admits every
	// no-capture and modest-capture scan over the 1 MiB subject cap (their
	// worst-case table is tens of megabytes) while rejecting the
	// many-empty-group explosion.
	maxRegexScanIndexBytes = 256 << 20
)
