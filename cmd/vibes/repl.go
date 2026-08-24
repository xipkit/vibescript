package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/mgomes/vibescript/internal/ast"
	vibesruntime "github.com/mgomes/vibescript/internal/runtime"
	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
	"github.com/urfave/cli/v3"
)

var (
	accentColor    = lipgloss.Color("#3B82F6")
	successColor   = lipgloss.Color("#10B981")
	errorColor     = lipgloss.Color("#EF4444")
	mutedColor     = lipgloss.Color("#6B7280")
	highlightColor = lipgloss.Color("#F59E0B")

	promptStyle = lipgloss.NewStyle().
			Foreground(accentColor).
			Bold(true)

	resultStyle = lipgloss.NewStyle().
			Foreground(successColor)

	errorStyle = lipgloss.NewStyle().
			Foreground(errorColor)

	mutedStyle = lipgloss.NewStyle().
			Foreground(mutedColor)

	headerStyle = lipgloss.NewStyle().
			Foreground(accentColor).
			Bold(true).
			Padding(0, 1)

	helpKeyStyle = lipgloss.NewStyle().
			Foreground(highlightColor)

	helpDescStyle = lipgloss.NewStyle().
			Foreground(mutedColor)

	borderStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(accentColor).
			Padding(0, 1)
)

type historyEntry struct {
	input  string
	output string
	isErr  bool
}

type replModel struct {
	ctx         context.Context
	textInput   textinput.Model
	engine      *vibes.Engine
	builtins    builtinCatalog
	env         map[string]value.Value
	stdout      *bytes.Buffer
	stderr      *bytes.Buffer
	history     []historyEntry
	cmdHistory  []string
	historyIdx  int
	lastError   string
	width       int
	height      int
	showHelp    bool
	showVars    bool
	quitting    bool
	initialized bool
}

type keyMap struct {
	Up        key.Binding
	Down      key.Binding
	Enter     key.Binding
	CtrlC     key.Binding
	CtrlD     key.Binding
	CtrlL     key.Binding
	Tab       key.Binding
	CtrlV     key.Binding
	CtrlH     key.Binding
	ShiftUp   key.Binding
	ShiftDown key.Binding
}

var keys = keyMap{
	Up: key.NewBinding(
		key.WithKeys("up"),
		key.WithHelp("↑", "previous command"),
	),
	Down: key.NewBinding(
		key.WithKeys("down"),
		key.WithHelp("↓", "next command"),
	),
	Enter: key.NewBinding(
		key.WithKeys("enter"),
		key.WithHelp("enter", "execute"),
	),
	CtrlC: key.NewBinding(
		key.WithKeys("ctrl+c"),
		key.WithHelp("ctrl+c", "quit"),
	),
	CtrlD: key.NewBinding(
		key.WithKeys("ctrl+d"),
		key.WithHelp("ctrl+d", "quit"),
	),
	CtrlL: key.NewBinding(
		key.WithKeys("ctrl+l"),
		key.WithHelp("ctrl+l", "clear"),
	),
	Tab: key.NewBinding(
		key.WithKeys("tab"),
		key.WithHelp("tab", "autocomplete"),
	),
	CtrlV: key.NewBinding(
		key.WithKeys("ctrl+v"),
		key.WithHelp("ctrl+v", "toggle vars"),
	),
	CtrlH: key.NewBinding(
		key.WithKeys("ctrl+k"),
		key.WithHelp("ctrl+k", "toggle help"),
	),
	ShiftUp: key.NewBinding(
		key.WithKeys("shift+up"),
	),
	ShiftDown: key.NewBinding(
		key.WithKeys("shift+down"),
	),
}

var replCommandCompletions = []string{
	":help",
	":h",
	":vars",
	":v",
	":globals",
	":g",
	":functions",
	":f",
	":types",
	":t",
	":clear",
	":c",
	":reset",
	":r",
	":last_error",
	":le",
	":quit",
	":q",
}

func newREPLModel(quota quotaConfig) (replModel, error) {
	return newREPLModelContext(context.Background(), quota)
}

func newREPLModelContext(ctx context.Context, quota quotaConfig) (replModel, error) {
	ti := textinput.New()
	ti.Placeholder = "type an expression..."
	ti.Focus()
	ti.CharLimit = 500
	ti.SetWidth(60)
	ti.Prompt = "vibes> "
	styles := textinput.DefaultDarkStyles()
	styles.Focused.Prompt = promptStyle
	styles.Blurred.Prompt = promptStyle
	ti.SetStyles(styles)

	stdout := new(bytes.Buffer)
	stderr := new(bytes.Buffer)
	// The REPL evaluates the developer's own expressions interactively, so it
	// defaults to the same generous xhigh profile as `vibes run` rather than the
	// embedding sandbox floor. The quota is configurable via flags. An embedding
	// caller can cancel an in-flight expression through the command
	// context. Quotas remain a defense against runaway work in an interactive
	// session; users can select a finite budget with `vibes repl -profile low`
	// or `-step-quota`.
	cfg := vibes.Config{
		OutputWriter: stdout,
		ErrorWriter:  stderr,
	}
	quota.applyTo(&cfg)
	engine, err := vibes.NewEngine(cfg)
	if err != nil {
		return replModel{}, fmt.Errorf("init engine: %w", err)
	}

	return replModel{
		ctx:        ctx,
		textInput:  ti,
		engine:     engine,
		builtins:   newBuiltinCatalog(engine.Builtins()),
		env:        make(map[string]value.Value),
		stdout:     stdout,
		stderr:     stderr,
		historyIdx: -1,
		showHelp:   false,
		showVars:   false,
	}, nil
}

func (m replModel) Init() tea.Cmd {
	return textinput.Blink
}

func (m replModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmd tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		m.textInput.SetWidth(max(msg.Width-10, 0))
		m.initialized = true
		return m, nil

	case tea.KeyPressMsg:
		switch {
		case key.Matches(msg, keys.CtrlC), key.Matches(msg, keys.CtrlD):
			m.quitting = true
			return m, tea.Quit

		case key.Matches(msg, keys.CtrlL):
			m.history = nil
			return m, nil

		case key.Matches(msg, keys.CtrlV):
			m.showVars = !m.showVars
			return m, nil

		case key.Matches(msg, keys.CtrlH):
			m.showHelp = !m.showHelp
			return m, nil

		case key.Matches(msg, keys.Up):
			if len(m.cmdHistory) > 0 {
				if m.historyIdx == -1 {
					m.historyIdx = len(m.cmdHistory) - 1
				} else if m.historyIdx > 0 {
					m.historyIdx--
				}
				m.textInput.SetValue(m.cmdHistory[m.historyIdx])
				m.textInput.CursorEnd()
			}
			return m, nil

		case key.Matches(msg, keys.Down):
			if m.historyIdx != -1 {
				if m.historyIdx < len(m.cmdHistory)-1 {
					m.historyIdx++
					m.textInput.SetValue(m.cmdHistory[m.historyIdx])
				} else {
					m.historyIdx = -1
					m.textInput.SetValue("")
				}
				m.textInput.CursorEnd()
			}
			return m, nil

		case key.Matches(msg, keys.Tab):
			m = m.handleAutocomplete()
			return m, nil

		case key.Matches(msg, keys.Enter):
			input := strings.TrimSpace(m.textInput.Value())
			if input == "" {
				return m, nil
			}

			if strings.HasPrefix(input, ":") {
				var cmd tea.Cmd
				m, cmd = m.handleCommand(input)
				m.textInput.SetValue("")
				m.historyIdx = -1
				return m, cmd
			}

			output, isErr := m.evaluate(input)
			m.history = append(m.history, historyEntry{
				input:  input,
				output: output,
				isErr:  isErr,
			})
			m.cmdHistory = append(m.cmdHistory, input)
			m.textInput.SetValue("")
			m.historyIdx = -1
			return m, nil
		}
	}

	m.textInput, cmd = m.textInput.Update(msg)
	return m, cmd
}

func (m replModel) handleCommand(input string) (replModel, tea.Cmd) {
	parts := strings.Fields(input)
	cmd := parts[0]

	switch cmd {
	case ":help", ":h":
		m.showHelp = !m.showHelp
	case ":clear", ":c":
		m.history = nil
	case ":vars", ":v":
		m.showVars = !m.showVars
	case ":globals", ":g":
		m.history = append(m.history, historyEntry{
			input:  input,
			output: globalsSnapshot(m.env),
			isErr:  false,
		})
	case ":functions", ":f":
		m.history = append(m.history, historyEntry{
			input:  input,
			output: functionsSnapshot(m.builtins, m.env),
			isErr:  false,
		})
	case ":types", ":t":
		m.history = append(m.history, historyEntry{
			input:  input,
			output: typesSnapshot(m.env),
			isErr:  false,
		})
	case ":reset", ":r":
		m.env = make(map[string]value.Value)
		m.history = append(m.history, historyEntry{
			input:  input,
			output: "Environment reset",
			isErr:  false,
		})
	case ":last_error", ":le":
		output := "No previous error"
		isErr := false
		if m.lastError != "" {
			output = m.lastError
			isErr = true
		}
		m.history = append(m.history, historyEntry{
			input:  input,
			output: output,
			isErr:  isErr,
		})
	case ":quit", ":q":
		m.quitting = true
		return m, tea.Quit
	default:
		m.history = append(m.history, historyEntry{
			input:  input,
			output: fmt.Sprintf("Unknown command: %s", cmd),
			isErr:  true,
		})
	}
	return m, nil
}

func (m replModel) handleAutocomplete() replModel {
	input := m.textInput.Value()
	if input == "" {
		return m
	}

	// Get the last word for completion
	words := strings.Fields(input)
	if len(words) == 0 {
		return m
	}
	lastWord := words[len(words)-1]

	matches := make(map[string]bool)
	addMatches := func(names []string) {
		for _, name := range names {
			if strings.HasPrefix(name, lastWord) {
				matches[name] = true
			}
		}
	}

	if strings.HasPrefix(lastWord, ":") {
		addMatches(replCommandCompletions)
	} else {
		if strings.Contains(lastWord, ".") {
			addMatches(m.builtins.documentedNames)
		} else {
			addMatches(m.builtins.topLevelNames)
		}
		addMatches(ast.Keywords())
	}

	// Environment variables
	for name := range m.env {
		if strings.HasPrefix(name, lastWord) {
			matches[name] = true
		}
	}
	completions := make([]string, 0, len(matches))
	for name := range matches {
		completions = append(completions, name)
	}
	slices.Sort(completions)

	if len(completions) == 1 {
		// Single match - complete it
		prefix := strings.TrimSuffix(input, lastWord)
		m.textInput.SetValue(prefix + completions[0])
		m.textInput.CursorEnd()
	} else if len(completions) > 1 {
		// Multiple matches - show them in history
		m.history = append(m.history, historyEntry{
			input:  "",
			output: "Completions: " + strings.Join(completions, ", "),
			isErr:  false,
		})
	}

	return m
}

func (m *replModel) evaluate(input string) (string, bool) {
	m.resetCapturedOutput()

	wrapped := fmt.Sprintf("def __repl__()\n  %s\nend", input)
	sourceMap := snippetSourceMap{
		syntheticFunction:     "__repl__",
		displayFunction:       "<repl>",
		lineOffset:            1,
		firstLineColumnOffset: 2,
	}

	script, err := m.engine.Compile(wrapped)
	if err != nil {
		m.lastError = "compile error: " + remapSnippetCompileError(err, input, sourceMap).Error()
		return m.lastError, true
	}

	opts := vibes.CallOptions{
		Globals: m.env,
	}

	result, err := script.Call(m.ctx, "__repl__", nil, opts)
	if err != nil {
		m.lastError = "runtime error: " + remapSnippetRuntimeError(err, input, sourceMap).Error()
		return m.lastError, true
	}

	m.extractAssignments(script, result)

	m.env["_"] = result

	return m.formatEvaluationOutput(result), false
}

func (m *replModel) resetCapturedOutput() {
	if m.stdout != nil {
		m.stdout.Reset()
	}
	if m.stderr != nil {
		m.stderr.Reset()
	}
}

func (m *replModel) formatEvaluationOutput(result value.Value) string {
	captured := m.capturedOutput()
	if captured == "" {
		if result.IsNil() {
			return "nil"
		}
		return formatValue(result)
	}
	captured = strings.TrimSuffix(captured, "\n")
	if result.IsNil() {
		return captured
	}
	if captured == "" {
		return formatValue(result)
	}
	return captured + "\n" + formatValue(result)
}

func (m *replModel) capturedOutput() string {
	var b strings.Builder
	if m.stdout != nil {
		b.WriteString(m.stdout.String())
	}
	if m.stderr != nil {
		b.WriteString(m.stderr.String())
	}
	return b.String()
}

func (m *replModel) extractAssignments(script *vibes.Script, result value.Value) {
	if script == nil {
		return
	}

	fn, ok := script.Function("__repl__")
	if !ok || len(fn.Body) != 1 {
		return
	}

	assign, ok := fn.Body[0].(*ast.AssignStmt)
	if !ok {
		return
	}

	m.extractTargetAssignment(assign.Target, result)
}

func (m *replModel) extractTargetAssignment(target ast.Expression, result value.Value) {
	switch t := target.(type) {
	case *ast.Identifier:
		m.env[t.Name] = result
	case *ast.DestructureTarget:
		_ = vibesruntime.AssignDestructure(t, result, func(target ast.Expression, result value.Value) error {
			if ident, ok := target.(*ast.Identifier); ok {
				m.env[ident.Name] = result
			}
			return nil
		})
	}
}

func formatValue(v value.Value) string {
	return v.String()
}

func sortedEnvKeys(env map[string]value.Value) []string {
	keys := make([]string, 0, len(env))
	for key := range env {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	return keys
}

func globalsSnapshot(env map[string]value.Value) string {
	if len(env) == 0 {
		return "No globals defined"
	}
	lines := make([]string, 0, len(env))
	for _, name := range sortedEnvKeys(env) {
		lines = append(lines, fmt.Sprintf("%s = %s", name, formatValue(env[name])))
	}
	return strings.Join(lines, "\n")
}

func functionsSnapshot(builtins builtinCatalog, env map[string]value.Value) string {
	names := make([]string, 0, len(builtins.functionNames)+len(env))
	names = append(names, builtins.functionNames...)
	for _, name := range sortedEnvKeys(env) {
		if isCallableValue(env[name]) {
			names = append(names, name)
		}
	}
	slices.Sort(names)
	return strings.Join(names, "\n")
}

func typesSnapshot(env map[string]value.Value) string {
	if len(env) == 0 {
		return "No globals defined"
	}
	lines := make([]string, 0, len(env))
	for _, name := range sortedEnvKeys(env) {
		lines = append(lines, fmt.Sprintf("%s: %s", name, env[name].Kind()))
	}
	return strings.Join(lines, "\n")
}

func (m replModel) View() tea.View {
	if !m.initialized {
		return altScreenView("Loading...")
	}

	if m.quitting {
		return altScreenView(mutedStyle.Render("Goodbye!\n"))
	}

	var b strings.Builder

	header := headerStyle.Render("Vibescript REPL")
	version := mutedStyle.Render("v0.60.0")
	b.WriteString(header + " " + version + "\n")
	b.WriteString(mutedStyle.Render(strings.Repeat("─", max(min(m.width-2, 60), 0))) + "\n\n")

	reservedLines := 8 // header, input, help hint, etc.
	if m.showHelp {
		reservedLines += 10
	}
	if m.showVars {
		reservedLines += len(m.env) + 3
	}
	availableHeight := m.height - reservedLines

	historyStart := 0
	if len(m.history) > availableHeight {
		historyStart = len(m.history) - availableHeight
	}

	for i := historyStart; i < len(m.history); i++ {
		entry := m.history[i]
		if entry.input != "" {
			b.WriteString(mutedStyle.Render("  › ") + entry.input + "\n")
		}
		if entry.isErr {
			b.WriteString("  " + errorStyle.Render("✗ "+entry.output) + "\n")
		} else {
			b.WriteString("  " + resultStyle.Render("→ "+entry.output) + "\n")
		}
		b.WriteString("\n")
	}

	if m.showVars {
		b.WriteString(renderVarsPanel(m.env, m.width))
		b.WriteString("\n")
	}

	if m.showHelp {
		b.WriteString(renderHelpPanel(m.width))
		b.WriteString("\n")
	}

	b.WriteString(m.textInput.View() + "\n\n")

	footer := helpKeyStyle.Render("ctrl+k") + helpDescStyle.Render(" help  ") +
		helpKeyStyle.Render("ctrl+v") + helpDescStyle.Render(" vars  ") +
		helpKeyStyle.Render("ctrl+l") + helpDescStyle.Render(" clear  ") +
		helpKeyStyle.Render("ctrl+c") + helpDescStyle.Render(" quit")
	b.WriteString(footer)

	return altScreenView(b.String())
}

func altScreenView(content string) tea.View {
	view := tea.NewView(content)
	view.AltScreen = true
	return view
}

func renderVarsPanel(env map[string]value.Value, width int) string {
	if len(env) == 0 {
		return borderStyle.Render(mutedStyle.Render("No variables defined"))
	}

	keys := sortedEnvKeys(env)
	lines := make([]string, 0, 1+len(keys))
	lines = append(lines, lipgloss.NewStyle().Bold(true).Foreground(accentColor).Render("Variables"))
	varNameStyle := lipgloss.NewStyle().Foreground(highlightColor)
	for _, name := range keys {
		val := env[name]
		line := fmt.Sprintf("  %s = %s", varNameStyle.Render(name), val.String())
		lines = append(lines, line)
	}
	return borderStyle.Render(strings.Join(lines, "\n"))
}

func renderHelpPanel(width int) string {
	help := []struct {
		key  string
		desc string
	}{
		{"↑/↓", "Navigate command history"},
		{"Tab", "Autocomplete"},
		{"Enter", "Execute expression"},
		{":help", "Toggle this help"},
		{":vars", "Toggle variables panel"},
		{":globals", "Print current globals"},
		{":functions", "List callable functions"},
		{":types", "Show global value types"},
		{":clear", "Clear history"},
		{":reset", "Reset environment"},
		{":last_error", "Show previous error"},
		{":quit", "Exit REPL"},
	}

	lines := make([]string, 0, 1+len(help))
	lines = append(lines, lipgloss.NewStyle().Bold(true).Foreground(accentColor).Render("Help"))
	for _, h := range help {
		line := fmt.Sprintf("  %s  %s",
			helpKeyStyle.Render(fmt.Sprintf("%-8s", h.key)),
			helpDescStyle.Render(h.desc))
		lines = append(lines, line)
	}

	return borderStyle.Render(strings.Join(lines, "\n"))
}

type replCommandConfig struct {
	quota quotaFlagValues
}

func newREPLCommand() *cli.Command {
	config := &replCommandConfig{quota: newQuotaFlagValues()}
	stopAfterArgument := 1
	return configureCLICommand(&cli.Command{
		Name:         "repl",
		Usage:        "start the interactive Vibescript REPL",
		StopOnNthArg: &stopAfterArgument,
		Flags:        newQuotaFlags(&config.quota),
		Action: func(ctx context.Context, command *cli.Command) error {
			return replAction(ctx, command, config)
		},
	})
}

func replAction(ctx context.Context, command *cli.Command, config *replCommandConfig) error {
	if command.NArg() > 0 {
		return errors.New("vibes repl: does not accept positional arguments")
	}
	quota, err := resolveCommandQuota(command, &config.quota)
	if err != nil {
		return fmt.Errorf("vibes repl: %w", err)
	}

	model, err := newREPLModelContext(ctx, quota)
	if err != nil {
		return fmt.Errorf("init repl: %w", err)
	}
	return runREPLProgram(ctx, model, tea.WithInput(command.Reader), tea.WithOutput(command.Writer))
}

func runREPLProgram(ctx context.Context, model replModel, options ...tea.ProgramOption) error {
	options = append([]tea.ProgramOption{tea.WithContext(ctx)}, options...)
	p := tea.NewProgram(model, options...)
	if _, err := p.Run(); err != nil {
		return fmt.Errorf("repl: %w", err)
	}
	return nil
}
