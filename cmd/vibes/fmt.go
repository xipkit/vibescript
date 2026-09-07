package main

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/urfave/cli/v3"
)

type fmtCommandConfig struct {
	write     bool
	check     bool
	arguments []string
}

func newFmtCommand() *cli.Command {
	config := new(fmtCommandConfig)
	stopAfterPath := 1
	return configureCLICommand(&cli.Command{
		Name:         "fmt",
		Usage:        "canonically format Vibescript source files",
		ArgsUsage:    "<path>...",
		StopOnNthArg: &stopAfterPath,
		Flags: []cli.Flag{
			&cli.BoolFlag{
				Name:        "w",
				Usage:       "write results to source files instead of stdout",
				Destination: &config.write,
			},
			&cli.BoolFlag{
				Name:        "check",
				Usage:       "fail if any source file needs formatting",
				Destination: &config.check,
			},
		},
		Arguments: stringArguments("path", &config.arguments),
		Action: func(ctx context.Context, command *cli.Command) error {
			return fmtAction(ctx, command, config)
		},
	})
}

func fmtAction(_ context.Context, command *cli.Command, config *fmtCommandConfig) error {
	if len(config.arguments) == 0 {
		return errors.New("vibes fmt: path required")
	}

	inputs, err := collectVibeFiles(config.arguments)
	if err != nil {
		return fmt.Errorf("collect files: %w", err)
	}
	defer inputs.close()
	if len(inputs.files) == 0 {
		return nil
	}

	changedCount := 0
	for _, source := range inputs.files {
		path := source.path
		originalBytes, info, err := source.read()
		if err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}
		original := string(originalBytes)
		formatted := formatVibeSource(original)
		changed := formatted != original
		if changed {
			changedCount++
		}

		switch {
		case config.write && changed:
			if err := source.write(info, []byte(formatted)); err != nil {
				return fmt.Errorf("write %s: %w", path, err)
			}
		case !config.write && !config.check:
			if _, err := fmt.Fprint(command.Writer, formatted); err != nil {
				return fmt.Errorf("write formatted output: %w", err)
			}
		}
	}

	if config.check && changedCount > 0 {
		return fmt.Errorf("vibes fmt: %d file(s) need formatting", changedCount)
	}

	return nil
}

func formatVibeSource(source string) string {
	var out strings.Builder
	out.Grow(len(source) + 1)

	lineStart := 0
	pendingBlankLines := 0
	wrote := false
	for i := 0; i < len(source); {
		if source[i] != '\n' && source[i] != '\r' {
			i++
			continue
		}
		lineEnd := trimLineEnd(source, lineStart, i)
		wrote = appendFormattedLine(&out, source[lineStart:lineEnd], &pendingBlankLines, wrote)
		if source[i] == '\r' && i+1 < len(source) && source[i+1] == '\n' {
			i += 2
		} else {
			i++
		}
		lineStart = i
	}
	if lineStart < len(source) {
		lineEnd := trimLineEnd(source, lineStart, len(source))
		wrote = appendFormattedLine(&out, source[lineStart:lineEnd], &pendingBlankLines, wrote)
	}
	if !wrote {
		return "\n"
	}
	return out.String()
}

func appendFormattedLine(out *strings.Builder, line string, pendingBlankLines *int, wrote bool) bool {
	if line == "" {
		*pendingBlankLines = *pendingBlankLines + 1
		return wrote
	}
	for range *pendingBlankLines {
		out.WriteByte('\n')
	}
	*pendingBlankLines = 0
	out.WriteString(line)
	out.WriteByte('\n')
	return true
}

func trimLineEnd(source string, start, end int) int {
	for end > start && (source[end-1] == ' ' || source[end-1] == '\t') {
		end--
	}
	return end
}
