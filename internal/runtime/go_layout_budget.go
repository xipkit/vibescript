package runtime

import (
	"strings"
	"time"
)

// goLayoutOutputBytes bounds Go's diagnostic rendering without allocating it.
// Keep input bytes when a field shrinks; fractional runs only shrink, and
// unrecognized text remains literal. Longer reference fields take precedence.
func goLayoutOutputBytes(t time.Time, layout string, budget *strftimeBudget) (int, error) {
	fields := [...]string{
		"January", "Monday", "Jan", "Mon", "MST", "_2006", "2006", "__2", "_2", "002",
		"01", "02", "03", "04", "05", "06", "15", "1", "2", "3", "4", "5", "PM", "pm",
		"-07:00:00", "-070000", "-07:00", "-0700", "-07",
		"Z07:00:00", "Z070000", "Z07:00", "Z0700", "Z07",
	}
	var sizes [len(fields)]int
	zone, _ := t.Zone()
	size := len(layout)
	for i := 0; i < len(layout); {
		consumed := 1
		for index, field := range fields {
			if layout[i] != field[0] || !strings.HasPrefix(layout[i:], field) {
				continue
			}
			if sizes[index] == 0 {
				if field == "MST" && zone != "" {
					sizes[index] = len(zone)
				} else {
					sizes[index] = len(t.Format(field))
				}
			}
			size = saturatingAdd(size, max(0, sizes[index]-len(field)))
			consumed = len(field)
			break
		}
		if err := budget.charge(consumed); err != nil {
			return 0, err
		}
		i += consumed
	}
	return size, nil
}
