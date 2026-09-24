// Command generate-lsp-data prints the reference's builtin member contracts
// as JSON for scripts/generate-lsp-data.py.
package main

import (
	"encoding/json"
	"log/slog"
	"os"

	"github.com/mgomes/vibescript/vibes"
)

func main() {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", " ")
	if err := encoder.Encode(vibes.MemberContracts()); err != nil {
		slog.Error("encode contracts", "error", err)
		os.Exit(1)
	}
}
