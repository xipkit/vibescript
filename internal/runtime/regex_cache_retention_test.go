package runtime

import (
	"fmt"
	"regexp"
	goruntime "runtime"
	"strings"
	"sync"
	"testing"
)

func TestRegexCacheReleasesPatternBackings(t *testing.T) {
	for _, capacity := range []int{2, 64} {
		t.Run(fmt.Sprintf("capacity=%d", capacity), func(t *testing.T) {
			cache := newRegexCache(capacity, compiledRegexCacheInstructionBudget)
			var before, after goruntime.MemStats
			goruntime.GC()
			goruntime.ReadMemStats(&before)
			kept := make([]*regexp.Regexp, 12)
			for i := range kept {
				suffix := fmt.Sprintf("pattern_%02d$", i)
				backing := strings.Repeat("z", 1<<20) + suffix
				re, err := cache.compile(backing[len(backing)-len(suffix):])
				if err != nil {
					t.Fatalf("compile(%q): %v", suffix, err)
				}
				if re.String() != suffix || !re.MatchString(suffix[:len(suffix)-1]) {
					t.Fatalf("compiled pattern %q does not match its suffix", re.String())
				}
				// Keep evicted regexps too: their expression must own its bytes.
				kept[i] = re
			}
			goruntime.GC()
			goruntime.ReadMemStats(&after)
			held := int64(after.HeapAlloc) - int64(before.HeapAlloc)
			t.Logf("12 borrowed patterns retain %d bytes", held)
			if limit := int64(2 << 20); held > limit {
				t.Errorf("12 borrowed patterns retain %d bytes, want less than %d", held, limit)
			}
			if got, want := len(cache.entries), min(capacity, len(kept)); got != want {
				t.Errorf("cache entries = %d, want %d", got, want)
			}
			goruntime.KeepAlive(kept)
			goruntime.KeepAlive(cache)
		})
	}
}

func TestRegexCacheHitDoesNotAllocate(t *testing.T) {
	cache := newRegexCache(2, compiledRegexCacheInstructionBudget)
	pattern := strings.Repeat("unused", 1000) + "x$"
	pattern = pattern[len(pattern)-2:]
	first, err := cache.compile(pattern)
	if err != nil {
		t.Fatal(err)
	}
	allocs := testing.AllocsPerRun(100, func() {
		got, err := cache.compile(pattern)
		if err != nil || got != first {
			t.Fatalf("cached compile = %p, %v, want %p, nil", got, err, first)
		}
	})
	if allocs != 0 {
		t.Errorf("cache hit allocates %g times, want 0", allocs)
	}
}

func TestRegexCacheConcurrentMissesShareProgram(t *testing.T) {
	t.Parallel()
	cache := newRegexCache(2, compiledRegexCacheInstructionBudget)
	pattern := strings.Repeat("unused", 1000) + strings.Repeat("a?", 100)
	pattern = pattern[len(pattern)-200:]
	var results [16]*regexp.Regexp
	var errs [16]error
	var wg sync.WaitGroup
	start := make(chan struct{})
	for i := range results {
		wg.Go(func() {
			<-start
			results[i], errs[i] = cache.compile(pattern)
		})
	}
	close(start)
	wg.Wait()
	for i := range results {
		if errs[i] != nil || results[i] == nil || results[i] != results[0] {
			t.Errorf("concurrent compile %d = %p, %v, want shared %p, nil", i, results[i], errs[i], results[0])
		}
	}
	cost, err := compiledRegexCost(pattern)
	if err != nil {
		t.Fatal(err)
	}
	if len(cache.entries) != 1 || cache.lru.Len() != 1 || cache.cost != cost {
		t.Errorf("cache entries=%d, LRU=%d, cost=%d, want 1, 1, %d", len(cache.entries), cache.lru.Len(), cache.cost, cost)
	}
}
