package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

const (
	cliContractHelperEnv = "VIBES_CLI_CONTRACT_HELPER"
	cliContractArgsEnv   = "VIBES_CLI_CONTRACT_ARGS"
)

const cliContractRootHelp = `NAME:
   vibes - run Vibescript programs and development tools

USAGE:
   vibes [global options] [command [command options]]

COMMANDS:
   run      execute a script file or inline snippet
   check    statically check a script without executing it
   fmt      canonically format Vibescript source files
   analyze  analyze a script for lint issues
   test     discover and run Vibescript tests
   lsp      start the language server over stdio
   repl     start the interactive Vibescript REPL
   help, h  Shows a list of commands or help for one command

GLOBAL OPTIONS:
   --help, -h  show help
`

const cliContractRunHelp = `NAME:
   vibes run - execute a script file or inline snippet

USAGE:
   vibes run [options] <script> [args...]
   vibes run [options] -e SNIPPET

OPTIONS:
   --function string                              function to invoke; without it, top-level statements run when present, otherwise run
   --check                                        compile and validate static contracts without executing
   -e string                                      evaluate an inline snippet instead of a script file
   --watch                                        re-run whenever the script or its modules change
   --module-path string [ --module-path string ]  add a module search directory (repeatable)
   --profile string                               execution quota profile: low, medium, high, xhigh (default: "xhigh")
   --step-quota int                               override the profile's step quota (-1 = unlimited)
   --memory-quota int                             override the profile's memory quota in bytes (-1 = unlimited)
   --recursion-limit int                          override the profile's recursion limit (-1 = unlimited, which can crash on infinite recursion)
   --help, -h                                     show help
`

type cliContractResult struct {
	ExitCode int
	Stdout   string
	Stderr   string
}

func TestCLIContractHelperProcess(t *testing.T) {
	if os.Getenv(cliContractHelperEnv) != "1" {
		return
	}

	var args []string
	if err := json.Unmarshal([]byte(os.Getenv(cliContractArgsEnv)), &args); err != nil {
		fmt.Fprintf(os.Stderr, "decode CLI contract arguments: %v\n", err)
		os.Exit(2)
	}
	os.Args = append([]string{"vibes"}, args...)
	main()
	os.Exit(0)
}

func TestCLIContractRootDispatch(t *testing.T) {
	tests := []struct {
		name string
		args []string
		want cliContractResult
	}{
		{
			name: "help",
			args: []string{"--help"},
			want: cliContractResult{Stdout: cliContractRootHelp},
		},
		{
			name: "missing command",
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "command required\n",
			},
		},
		{
			name: "unknown command",
			args: []string{"unknown"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"unknown\"\n",
			},
		},
		{
			name: "unknown command before help flag",
			args: []string{"unknown", "--help"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"unknown\"\n",
			},
		},
		{
			name: "root flag terminator is not a command escape",
			args: []string{"--", "run"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"--\"\n",
			},
		},
		{
			name: "unsupported single-dash help spelling",
			args: []string{"-help"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"-help\"\n",
			},
		},
		{
			name: "unsupported abbreviated help spelling",
			args: []string{"--h"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"--h\"\n",
			},
		},
		{
			name: "unknown command tail cannot resume root parsing",
			args: []string{"unknown", "", "--help"},
			want: cliContractResult{
				ExitCode: 1,
				Stderr:   cliContractRootHelp + "unknown command \"unknown\"\n",
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := runCLIContractProcess(t, "", tc.args...)
			assertCLIContractResult(t, got, tc.want)
		})
	}
}

func TestCLIContractRunHelp(t *testing.T) {
	for _, args := range [][]string{{"run", "-h"}, {"run", "--help"}, {"help", "run"}} {
		got := runCLIContractProcess(t, "", args...)
		assertCLIContractResult(t, got, cliContractResult{Stdout: cliContractRunHelp})
	}
}

func TestCLIContractHelpCommandHasHelp(t *testing.T) {
	got := runCLIContractProcess(t, "", "help", "--help")
	if got.ExitCode != 0 {
		t.Errorf("help --help exit code = %d, want 0", got.ExitCode)
	}
	if got.Stderr != "" {
		t.Errorf("help --help stderr = %q, want empty", got.Stderr)
	}
	if want := "NAME:\n   vibes help"; !strings.Contains(got.Stdout, want) {
		t.Errorf("help --help stdout = %q, want substring %q", got.Stdout, want)
	}
}

func TestCLIContractEverySubcommandHasHelp(t *testing.T) {
	for _, name := range []string{"run", "check", "fmt", "analyze", "test", "lsp", "repl"} {
		t.Run(name, func(t *testing.T) {
			got := runCLIContractProcess(t, "", name, "--help")
			if got.ExitCode != 0 {
				t.Errorf("%s --help exit code = %d, want 0", name, got.ExitCode)
			}
			if got.Stderr != "" {
				t.Errorf("%s --help stderr = %q, want empty", name, got.Stderr)
			}
			if want := "NAME:\n   vibes " + name; !strings.Contains(got.Stdout, want) {
				t.Errorf("%s --help stdout = %q, want substring %q", name, got.Stdout, want)
			}
			if name == "lsp" || name == "repl" {
				if want := "USAGE:\n   vibes " + name + " [options]\n"; !strings.Contains(got.Stdout, want) {
					t.Errorf("%s --help stdout = %q, want substring %q", name, got.Stdout, want)
				}
				if strings.Contains(got.Stdout, "[argument") {
					t.Errorf("%s --help stdout advertises rejected positional arguments: %q", name, got.Stdout)
				}
			}
		})
	}
}

func TestCLIContractSubcommandFlagErrors(t *testing.T) {
	tests := []struct {
		name string
		args []string
		want string
	}{
		{
			name: "unknown flag",
			args: []string{"run", "-unknown"},
			want: "flag provided but not defined: -unknown\n",
		},
		{
			name: "unknown flag with inline value",
			args: []string{"fmt", "--unknown=value"},
			want: "flag provided but not defined: -unknown\n",
		},
		{
			name: "unknown help flag",
			args: []string{"help", "-unknown"},
			want: "flag provided but not defined: -unknown\n",
		},
		{
			name: "missing double-dash flag value",
			args: []string{"run", "--e"},
			want: "flag needs an argument: -e\n",
		},
		{
			name: "too many flag dashes",
			args: []string{"fmt", "---w"},
			want: "bad flag syntax: ---w\n",
		},
		{
			name: "empty flag name",
			args: []string{"run", "--=value"},
			want: "bad flag syntax: --=value\n",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := runCLIContractProcess(t, "", tc.args...)
			assertCLIContractResult(t, got, cliContractResult{
				ExitCode: 1,
				Stderr:   tc.want,
			})
		})
	}
}

func TestCLIContractLeafCommandsTreatHelpAsAnArgument(t *testing.T) {
	dir := t.TempDir()
	writeCLIContractFile(t, dir, "help", `"script"`+"\n")

	for _, args := range [][]string{{"run", "help"}, {"run", "--", "help"}} {
		got := runCLIContractProcessInDir(t, dir, "", args...)
		assertCLIContractResult(t, got, cliContractResult{Stdout: "script\n"})
	}

	for _, args := range [][]string{{"lsp", "help"}, {"repl", "h"}} {
		got := runCLIContractProcess(t, "", args...)
		assertCLIContractResult(t, got, cliContractResult{
			ExitCode: 1,
			Stderr:   "vibes " + args[0] + ": does not accept positional arguments\n",
		})
	}
}

func TestCLIContractVersionRemainsUnavailable(t *testing.T) {
	got := runCLIContractProcess(t, "", "--version")
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "flag provided but not defined: -version\n",
	})
}

func TestCLIContractRejectsUndocumentedExtraArguments(t *testing.T) {
	scriptPath := writeVibeScript(t, "1\n")
	tests := []struct {
		name string
		args []string
		want string
	}{
		{
			name: "analyze",
			args: []string{"analyze", scriptPath, "extra.vibe"},
			want: "vibes analyze: expected a single script path\n",
		},
		{
			name: "lsp",
			args: []string{"lsp", "extra"},
			want: "vibes lsp: does not accept positional arguments\n",
		},
		{
			name: "repl",
			args: []string{"repl", "extra"},
			want: "vibes repl: does not accept positional arguments\n",
		},
		{
			name: "help",
			args: []string{"help", "run", "extra"},
			want: "vibes help: expected at most one command\n",
		},
		{
			name: "help with terminator after topic",
			args: []string{"help", "run", "--"},
			want: "vibes help: expected at most one command\n",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := runCLIContractProcess(t, "", tc.args...)
			assertCLIContractResult(t, got, cliContractResult{ExitCode: 1, Stderr: tc.want})
		})
	}
}

func TestCLIContractRunStopsParsingAtFirstPositional(t *testing.T) {
	scriptPath := writeVibeScript(t, `def run(value)
  value
end
`)

	got := runCLIContractProcess(t, "", "run", scriptPath, "-check=")
	assertCLIContractResult(t, got, cliContractResult{Stdout: "-check=\n"})
}

func TestCLIContractRunDoubleDashTerminatesFlags(t *testing.T) {
	scriptPath := writeVibeScript(t, `def run(value)
  value
end
`)

	got := runCLIContractProcess(t, "", "run", "--", scriptPath, "-check=")
	assertCLIContractResult(t, got, cliContractResult{Stdout: "-check=\n"})
}

func TestCLIContractRunPreservesDoubleDashAfterScript(t *testing.T) {
	scriptPath := writeVibeScript(t, `def run(first, second)
  first + ":" + second
end
`)

	got := runCLIContractProcess(t, "", "run", scriptPath, "--", "-check")
	assertCLIContractResult(t, got, cliContractResult{Stdout: "--:-check\n"})
}

func TestCLIContractRunTreatsEmptyArgumentsAsOpaque(t *testing.T) {
	scriptPath := writeVibeScript(t, `def run(first, second)
  first + ":" + second
end
`)

	for _, first := range []string{"", " "} {
		got := runCLIContractProcess(t, "", "run", scriptPath, first, "-check")
		assertCLIContractResult(t, got, cliContractResult{Stdout: first + ":-check\n"})
	}
}

func TestCLIContractSingleDashScriptPreservesFollowingArguments(t *testing.T) {
	dir := t.TempDir()
	writeCLIContractFile(t, dir, "-", `def run(value)
  value
end
`)

	got := runCLIContractProcessInDir(t, dir, "", "run", "-", "argument")
	assertCLIContractResult(t, got, cliContractResult{Stdout: "argument\n"})

	got = runCLIContractProcessInDir(t, dir, "", "check", "-", "extra")
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "vibes check: expected a single script path\n",
	})
}

func TestCLIContractRejectsNonLetterFlagWithoutModifyingFile(t *testing.T) {
	const source = "def run()\n1\nend\n"
	dir := t.TempDir()
	path := writeCLIContractFile(t, dir, "-1", source)

	got := runCLIContractProcessInDir(t, dir, "", "fmt", "-w", "-1")
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "flag provided but not defined: -1\n",
	})
	assertCLIContractFileUnchanged(t, path, source)
}

func TestCLIContractRejectsUnicodeSingleDashFlagWithoutModifyingFile(t *testing.T) {
	const source = "def run()\n1\nend\n"
	dir := t.TempDir()
	path := writeCLIContractFile(t, dir, "-א.vibe", source)

	got := runCLIContractProcessInDir(t, dir, "", "fmt", "-w", "-א.vibe")
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "flag provided but not defined: -א.vibe\n",
	})
	assertCLIContractFileUnchanged(t, path, source)
}

func TestCLIContractWhitespaceWrappedFlagsDoNotChangeMeaning(t *testing.T) {
	const source = "def run()\n1\nend\n"
	path := writeVibeScript(t, source)

	got := runCLIContractProcess(t, "", "fmt", "-w ", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "flag provided but not defined: -w \n",
	})
	assertCLIContractFileUnchanged(t, path, source)

	got = runCLIContractProcess(t, "", "fmt", "-w=true ", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "invalid boolean value \"true \" for -w: parse error\n",
	})
	assertCLIContractFileUnchanged(t, path, source)

	got = runCLIContractProcess(t, "", "fmt", " -w", path)
	if got.ExitCode != 1 {
		t.Errorf("leading-space path exit code = %d, want 1", got.ExitCode)
	}
	assertCLIContractFileUnchanged(t, path, source)
}

func TestCLIContractValuedHelpCannotEnableMutatingFlags(t *testing.T) {
	const source = "def run()\n1\nend\n"
	path := writeVibeScript(t, source)

	got := runCLIContractProcess(t, "", "fmt", "-h=false", "-w", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "help flag does not accept a value\n",
	})
	assertCLIContractFileUnchanged(t, path, source)

	got = runCLIContractProcess(t, "", "--help=false", "fmt", "-w", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   cliContractRootHelp + "unknown command \"--help=false\"\n",
	})
	assertCLIContractFileUnchanged(t, path, source)
}

func TestCLIContractReportsFirstFlagErrorWithoutModifyingFile(t *testing.T) {
	const source = "def run()\n1\nend\n"
	path := writeVibeScript(t, source)

	got := runCLIContractProcess(t, "", "fmt", "-w=bogus", "-check=", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "invalid boolean value \"bogus\" for -w: parse error\n",
	})
	assertCLIContractFileUnchanged(t, path, source)
}

func TestCLIContractReportsIntegerErrorsBeforeLaterFlags(t *testing.T) {
	for _, args := range [][]string{
		{"-step-quota=nope", "-unknown"},
		{"-step-quota", "nope", "-unknown"},
	} {
		got := runCLIContractProcess(t, "", append([]string{"run"}, args...)...)
		assertCLIContractResult(t, got, cliContractResult{
			ExitCode: 1,
			Stderr:   "invalid value \"nope\" for flag -step-quota: parse error\n",
		})
	}
}

func TestCLIContractBareHelpShortCircuitsMutatingFlags(t *testing.T) {
	const source = "def run()\n1\nend\n"
	for _, args := range [][]string{{"-h", "-w"}, {"-w", "--help"}} {
		t.Run(strings.Join(args, "_"), func(t *testing.T) {
			path := writeVibeScript(t, source)
			got := runCLIContractProcess(t, "", append([]string{"fmt"}, append(args, path)...)...)
			if got.ExitCode != 0 {
				t.Errorf("vibes fmt %v exit code = %d, want 0", args, got.ExitCode)
			}
			if got.Stderr != "" {
				t.Errorf("vibes fmt %v stderr = %q, want empty", args, got.Stderr)
			}
			if want := "NAME:\n   vibes fmt"; !strings.Contains(got.Stdout, want) {
				t.Errorf("vibes fmt %v stdout = %q, want substring %q", args, got.Stdout, want)
			}
			assertCLIContractFileUnchanged(t, path, source)
		})
	}
}

func TestCLIContractRunExplicitEmptySnippet(t *testing.T) {
	got := runCLIContractProcess(t, "", "run", "-e=")
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "vibes run: -e requires a non-empty snippet\n",
	})
}

func TestCLIContractRejectsExplicitEmptyBoolean(t *testing.T) {
	scriptPath := writeVibeScript(t, "1\n")
	got := runCLIContractProcess(t, "", "run", "-check=", scriptPath)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "invalid boolean value \"\" for -check: parse error\n",
	})
}

func TestCLIContractEmptyWriteBooleanDoesNotModifyFile(t *testing.T) {
	const source = "def run()\n1\nend\n"
	path := writeVibeScript(t, source)

	got := runCLIContractProcess(t, "", "fmt", "-w=", path)
	assertCLIContractResult(t, got, cliContractResult{
		ExitCode: 1,
		Stderr:   "invalid boolean value \"\" for -w: parse error\n",
	})
	assertCLIContractFileUnchanged(t, path, source)
}

func assertCLIContractFileUnchanged(t *testing.T, path, source string) {
	t.Helper()
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read source after rejected command: %v", err)
	}
	if string(after) != source {
		t.Fatalf("source after rejected command = %q, want unchanged %q", after, source)
	}
}

func TestCLIContractRunExplicitDefaultFunction(t *testing.T) {
	scriptPath := writeVibeScript(t, `def run
  "function"
end

"top-level"
`)

	got := runCLIContractProcess(t, "", "run", scriptPath)
	assertCLIContractResult(t, got, cliContractResult{Stdout: "top-level\n"})

	got = runCLIContractProcess(t, "", "run", "-function=run", scriptPath)
	assertCLIContractResult(t, got, cliContractResult{Stdout: "function\n"})
}

func TestCLIContractRunExplicitZeroQuota(t *testing.T) {
	scriptPath := writeVibeScript(t, `def count(n)
  i = 0
  while i < n
    i = i + 1
  end
  i
end

puts count(200000)
`)

	got := runCLIContractProcess(t, "", "run", scriptPath)
	assertCLIContractResult(t, got, cliContractResult{Stdout: "200000\n"})

	got = runCLIContractProcess(t, "", "run", "-step-quota=0", scriptPath)
	if got.ExitCode != 1 {
		t.Errorf("explicit zero quota exit code = %d, want 1", got.ExitCode)
	}
	if got.Stdout != "" {
		t.Errorf("explicit zero quota stdout = %q, want empty", got.Stdout)
	}
	if !strings.Contains(got.Stderr, "step quota exceeded (1000000)") {
		t.Errorf("explicit zero quota stderr = %q, want low-profile step quota failure", got.Stderr)
	}
}

func TestCLIContractRunRepeatableModulePaths(t *testing.T) {
	root := t.TempDir()
	firstDir := filepath.Join(root, "first,modules")
	secondDir := filepath.Join(root, "second-modules")
	mainDir := filepath.Join(root, "main")
	writeCLIContractFile(t, firstDir, "first.vibe", `def value
  "first"
end
`)
	writeCLIContractFile(t, secondDir, "second.vibe", `def value
  "second"
end
`)
	mainPath := writeCLIContractFile(t, mainDir, "main.vibe", `first = require("first")
second = require("second")
first.value + ":" + second.value
`)

	got := runCLIContractProcess(t, "",
		"run",
		"-module-path", firstDir,
		"-module-path", secondDir,
		mainPath,
	)
	assertCLIContractResult(t, got, cliContractResult{Stdout: "first:second\n"})
}

func TestCLIContractLSPPreservesStdoutFraming(t *testing.T) {
	initialize := `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}`
	exit := `{"jsonrpc":"2.0","method":"exit"}`
	input := frameCLIContractLSPPayload(initialize) + frameCLIContractLSPPayload(exit)

	got := runCLIContractProcess(t, input, "lsp")
	if got.ExitCode != 0 {
		t.Errorf("lsp exit code = %d, want 0", got.ExitCode)
	}
	if got.Stderr != "" {
		t.Errorf("lsp stderr = %q, want empty", got.Stderr)
	}

	payload := decodeCLIContractLSPFrame(t, got.Stdout)
	var response struct {
		JSONRPC string `json:"jsonrpc"`
		ID      int    `json:"id"`
		Result  struct {
			Capabilities map[string]any `json:"capabilities"`
		} `json:"result"`
	}
	if err := json.Unmarshal(payload, &response); err != nil {
		t.Fatalf("decode LSP response: %v", err)
	}
	if response.JSONRPC != "2.0" {
		t.Errorf("LSP jsonrpc = %q, want 2.0", response.JSONRPC)
	}
	if response.ID != 1 {
		t.Errorf("LSP response id = %d, want 1", response.ID)
	}
	if len(response.Result.Capabilities) == 0 {
		t.Error("LSP initialize capabilities are empty")
	}
}

func runCLIContractProcess(t *testing.T, stdin string, args ...string) cliContractResult {
	t.Helper()
	return runCLIContractProcessInDir(t, "", stdin, args...)
}

func runCLIContractProcessInDir(t *testing.T, dir, stdin string, args ...string) cliContractResult {
	t.Helper()

	encodedArgs, err := json.Marshal(args)
	if err != nil {
		t.Fatalf("encode CLI arguments: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, os.Args[0], "-test.run=^TestCLIContractHelperProcess$")
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		cliContractHelperEnv+"=1",
		cliContractArgsEnv+"="+string(encodedArgs),
	)
	cmd.Stdin = strings.NewReader(stdin)
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	runErr := cmd.Run()
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		t.Fatalf("CLI process timed out for arguments %q", args)
	}
	exitCode := 0
	if runErr != nil {
		var exitErr *exec.ExitError
		if !errors.As(runErr, &exitErr) {
			t.Fatalf("run CLI process with arguments %q: %v", args, runErr)
		}
		exitCode = exitErr.ExitCode()
	}

	return cliContractResult{
		ExitCode: exitCode,
		Stdout:   stdout.String(),
		Stderr:   stderr.String(),
	}
}

func assertCLIContractResult(t *testing.T, got, want cliContractResult) {
	t.Helper()
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("CLI result mismatch (-want +got):\n%s", diff)
	}
}

func writeCLIContractFile(t *testing.T, dir, name, source string) string {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("create directory %s: %v", dir, err)
	}
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(source), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
	return path
}

func frameCLIContractLSPPayload(payload string) string {
	return fmt.Sprintf("Content-Length: %d\r\n\r\n%s", len(payload), payload)
}

func decodeCLIContractLSPFrame(t *testing.T, output string) []byte {
	t.Helper()
	header, body, ok := strings.Cut(output, "\r\n\r\n")
	if !ok {
		t.Fatalf("LSP stdout = %q, want a framed payload", output)
	}
	const prefix = "Content-Length: "
	if !strings.HasPrefix(header, prefix) {
		t.Fatalf("LSP header = %q, want Content-Length", header)
	}
	contentLength, err := strconv.Atoi(strings.TrimPrefix(header, prefix))
	if err != nil {
		t.Fatalf("parse LSP Content-Length from %q: %v", header, err)
	}
	if len(body) != contentLength {
		t.Fatalf("LSP body length = %d, want %d; stdout = %q", len(body), contentLength, output)
	}
	return []byte(body)
}
