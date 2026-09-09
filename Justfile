set shell := ["bash", "-lc"]

test:
	go test ./...

test-race:
	go test -timeout 30m -race ./...

leakcheck:
	GOEXPERIMENT=goroutineleakprofile go test ./internal/runtime

# The default budget is iteration-based (Nx) rather than time-based: a duration
# makes Go's fuzz coordinator set a context deadline whose teardown races,
# intermittently failing with a bare "context deadline exceeded" and no crasher
# file (golang/go#48591). Duration budgets (used by the nightly for real
# coverage) are still safe because run_target retries a target once when a
# failure matches that exact signature — deadline error, no new corpus file. A
# real finding always writes a file under testdata/fuzz/<target>/, so it is
# never retried away; a double flake stays red.
#
# scope selects which targets run: 'all' (default) fuzzes both the ./cmd/vibes
# tooling targets and the ./internal/runtime targets; 'runtime' fuzzes only the
# runtime targets. Use 'runtime' when running under VIBES_ESTIMATOR_VERIFY /
# VIBES_ENV_RECYCLE_VERIFY: those toggles are read only by the runtime package's
# test TestMain, so the ./cmd/vibes targets (a separate test binary) would run
# with the oracle off — duplicate coverage rather than verification.
fuzz fuzztime='25000x' scope='all':
	#!/usr/bin/env bash
	set -uo pipefail

	fuzztime="{{fuzztime}}"
	scope="{{scope}}"
	failed=()

	shutdown_flake() {
		local log="$1" corpus="$2"
		grep -q "context deadline exceeded" "$log" \
			&& [ -z "$(git status --porcelain -- "$corpus")" ]
	}

	run_target() {
		local pkg="$1" target="$2"
		local corpus="${pkg#./}/testdata/fuzz/$target"
		local log
		log=$(mktemp)
		if go test "$pkg" -run=^$ -fuzz="$target" -fuzztime="$fuzztime" 2>&1 | tee "$log"; then
			rm -f "$log"
			return
		fi
		if shutdown_flake "$log" "$corpus"; then
			echo "retrying $target after shutdown flake (golang/go#48591)" >&2
			if go test "$pkg" -run=^$ -fuzz="$target" -fuzztime="$fuzztime"; then
				rm -f "$log"
				return
			fi
		fi
		rm -f "$log"
		failed+=("$target")
	}

	if [ "$scope" = "all" ]; then
		for target in \
			FuzzFormatVibeSource \
			FuzzCLIArgumentAndPathInputs \
			FuzzREPLInputFlow \
			FuzzLSPPayloadAndMessageHandling
		do
			run_target ./cmd/vibes "$target"
		done
	fi

	for target in \
		FuzzLexerTokenStreamTerminates \
		FuzzParserSuccessfulProgramsHaveCompleteAST \
		FuzzCompileScriptDoesNotPanic \
		FuzzGeneratedScriptSemantics \
		FuzzRuntimeEdgeCasesDoNotPanic \
		FuzzStringIsASCII \
		FuzzJSONValueRoundTripPreservesStructure \
		FuzzJSONSpans \
		FuzzValueOperationsPreserveInvariants \
		FuzzModuleRequestNormalization \
		FuzzModuleAliasValidation \
		FuzzScalarInputParsersAndConversions \
		FuzzModulePolicyValidation \
		FuzzCapabilityInputValidation
	do
		run_target ./internal/runtime "$target"
	done

	if [ "${#failed[@]}" -gt 0 ]; then
		echo "fuzz targets failed: ${failed[*]}" >&2
		exit 1
	fi

bench:
	scripts/bench_runtime.sh

bench-profile pattern='^BenchmarkExecutionArrayPipeline$':
	scripts/bench_profile.sh --pattern "{{pattern}}"

lint:
	golangci-lint fmt --diff
	golangci-lint run --timeout=10m

lint-fix:
	golangci-lint fmt
	golangci-lint run --timeout=10m --fix

# deadcode roots its reachability analysis at main functions, so the choice of
# roots decides the answer. ./... supplies the only non-test main (./cmd/vibes)
# and -test adds every package's test binary; together they cover the surface
# that actually runs. ./cmd/vibes alone is not enough: the CLI reaches only the
# part of the library it happens to need, so the rest of the embedder-facing
# API under vibes/ would be condemned for having no in-repo caller but its own
# tests, which is exactly what an embedding library looks like.
#
# The price of -test is that code only a test reaches counts as live. What is
# still reported is API that neither a command nor a test touches, which is a
# coverage gap as often as it is dead code, so judge each hit before deleting.
# The recipe reports and exits 0; it is not a gate.
#
# Known, intentionally-kept hits (identical on darwin, linux, and windows as
# of the ADR-006 removals): the vibes facade's DeclareNonMutating,
# DeclareNonRetaining, and NewTypedBuiltin. They are embedder API -- the
# boundary consults the declarations (#1210) and typed builtins are host
# surface -- while in-repo callers use the runtime-internal forms, so a
# main-rooted walk cannot see their callers.
#
# Analysis is valid for one GOOS/GOARCH at a time, so prefix the recipe
# (GOOS=linux just deadcode) to cover build-tagged files for another platform,
# and follow up on a single function with
# `deadcode -whylive=<pkg>.<func> -test ./...`.
[doc("Report functions no command and no test can reach")]
deadcode:
	deadcode -test ./...

precommit-install:
	#!/usr/bin/env bash
	set -euo pipefail

	repo_root="$(git rev-parse --show-toplevel)"
	common_dir="$(git rev-parse --git-common-dir)"
	hook_path="$common_dir/hooks/pre-commit"
	source_path="$repo_root/scripts/pre-commit.sh"

	mkdir -p "$(dirname "$hook_path")"
	cp "$source_path" "$hook_path"
	chmod +x "$hook_path"

	echo "Installed pre-commit hook at $hook_path"

repl:
	go build -o vibes-cli ./cmd/vibes && ./vibes-cli repl

install dest='':
	#!/usr/bin/env bash
	set -euo pipefail

	dest="{{dest}}"
	if [[ -z "$dest" ]]; then
		dest="$(go env GOBIN)"
	fi
	if [[ -z "$dest" ]]; then
		dest="$(go env GOPATH)/bin"
	fi

	mkdir -p "$dest"
	GOBIN="$dest" go install ./cmd/vibes

	echo "Installed vibes to $dest/vibes"
	if [[ ":$PATH:" != *":$dest:"* ]]; then
		echo "PATH does not include $dest"
		echo "Add it with: export PATH=\"$dest:\$PATH\""
	fi
