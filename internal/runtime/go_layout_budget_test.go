package runtime

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestGoLayoutProjection(t *testing.T) {
	layouts := []string{
		time.RFC3339Nano, time.RFC1123, time.RubyDate, time.ANSIC,
		"January Jan Janitor Monday Mon Mondayx MST _2006 __2006 _2 __2 002 01 02 03 04 05 06 15 1 2 3 4 5 PM pm",
		"-07:00:00 -070000 -07:00 -0700 -07 Z07:00:00 Z070000 Z07:00 Z0700 Z07",
		".999 .000 .0002 ,9999x .9999 .00000000000000000000 2006",
		strings.Repeat("2006", 8193), "2006" + strings.Repeat("MST", 512),
	}
	for _, tm := range []time.Time{
		time.Date(2006, 1, 2, 15, 4, 5, 0, time.UTC),
		time.Date(-10000, 9, 30, 23, 59, 59, 123456789, time.FixedZone("long zone name", 123456)),
		time.Unix(0, 0).In(time.FixedZone("", -int(^uint(0)>>1))),
	} {
		for _, layout := range layouts {
			projected, err := goLayoutOutputBytes(tm, layout, &strftimeBudget{})
			actual := len(tm.Format(layout))
			if err != nil || projected < actual {
				t.Fatalf("layout prefix %q: projection %d, actual %d, error %v", layout[:min(len(layout), 40)], projected, actual, err)
			}
		}
	}
}

func TestGoLayoutDiagnosticLimits(t *testing.T) {
	tm := time.Unix(0, 0).In(time.FixedZone(strings.Repeat("Z", 32768), 0))
	layout := "2006" + strings.Repeat("MST", 1024)
	var err error
	allocated := allocBytes(func() {
		_, err = callTimeStrftime(&Execution{quota: 1 << 30, memoryQuota: 256 << 10}, tm, []Value{NewString(layout)}, nil, NewNil())
	})
	if !errors.Is(err, errMemoryQuotaExceeded) || allocated > 256<<10 {
		t.Fatalf("diagnostic expansion: allocated %d, error %v", allocated, err)
	}
	layout = "2006" + strings.Repeat("\x00", 32768)
	_, err = callTimeStrftime(&Execution{quota: 1 << 30, memoryQuota: 256 << 10}, time.Unix(0, 0).UTC(), []Value{NewString(layout)}, nil, NewNil())
	if err == nil || !strings.Contains(err.Error(), "Go layout") || len(err.Error()) > 2048 {
		t.Fatalf("bounded diagnostic: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = goLayoutOutputBytes(time.Unix(0, 0), "2006", &strftimeBudget{exec: &Execution{ctx: ctx}})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("projection cancellation: %v", err)
	}
}

func FuzzGoLayoutProjection(f *testing.F) {
	for _, layout := range []string{time.RFC3339Nano, "January 2006 MST", "__2006 002 .0002", "." + strings.Repeat("0", 4096), "Janitor 2006", "Z070000-07:00:00"} {
		f.Add(layout, int64(0))
	}
	f.Fuzz(func(t *testing.T, layout string, seconds int64) {
		if len(layout) > 8192 {
			t.Skip()
		}
		tm := time.Unix(seconds, 123456789).In(time.FixedZone("a long zone name", 123456))
		projected, err := goLayoutOutputBytes(tm, layout, &strftimeBudget{})
		if actual := len(tm.Format(layout)); err != nil || projected < actual {
			t.Fatalf("projection %d, actual %d, error %v", projected, actual, err)
		}
	})
}
