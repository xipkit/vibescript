package main

import (
	"encoding/json"
	"log/slog"
	"os"
	"runtime"
	"runtime/debug"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/cases"
	"golang.org/x/text/language"
)

type mapping struct {
	Rune   rune      `json:"rune"`
	Values [5]string `json:"values"`
	Fold   rune      `json:"fold"`
}

func main() {
	upper := cases.Upper(language.Und)
	lower := cases.Lower(language.Und, cases.HandleFinalSigma(false))
	title := cases.Title(language.Und, cases.NoLower)
	fold := cases.Fold()
	var rows []mapping
	for point := range unicode.MaxRune + 1 {
		r := rune(point)
		if !utf8.ValidRune(r) {
			continue
		}
		text := string(r)
		values := [5]string{upper.String(text), lower.String(text), title.String(text), fold.String(text), text}
		switch {
		case unicode.IsUpper(r) || unicode.IsTitle(r) || unicode.ToLower(r) != r:
			values[4] = values[1]
		case unicode.IsLower(r) || unicode.ToUpper(r) != r:
			values[4] = values[0]
		}
		canonical := r
		for next := unicode.SimpleFold(r); next != r; next = unicode.SimpleFold(next) {
			canonical = min(canonical, next)
		}
		if values != [5]string{text, text, text, text, text} || canonical != r {
			rows = append(rows, mapping{Rune: r, Values: values, Fold: canonical})
		}
	}
	var textVersion string
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, dep := range info.Deps {
			if dep.Path == "golang.org/x/text" {
				textVersion = dep.Version
			}
		}
	}
	output := struct {
		Go      string    `json:"go"`
		Text    string    `json:"text"`
		Unicode string    `json:"unicode"`
		Cases   string    `json:"cases"`
		Rows    []mapping `json:"rows"`
	}{
		Go: runtime.Version(), Text: textVersion, Unicode: unicode.Version,
		Cases: cases.UnicodeVersion, Rows: rows,
	}
	if err := json.NewEncoder(os.Stdout).Encode(output); err != nil {
		slog.Error("write casing mappings", "error", err)
		os.Exit(1)
	}
}
