package runtime

import (
	"context"
	"strings"
	"testing"
)

type mixedHashMutationDB struct{ dbCapabilityStub }

func (d *mixedHashMutationDB) Update(_ context.Context, req DBUpdateRequest) (Value, error) {
	alias := req.Options["alias"]
	delete(req.Attributes, "a")
	if err := alias.HashSet(NewString("a"), NewInt(3)); err != nil {
		return NewNil(), err
	}
	req.Attributes["c"] = NewInt(4)
	return alias, nil
}

func capabilityDataCall(t *testing.T, adapter CapabilityAdapter, name, method string, exec *Execution, args []Value, kwargs map[string]Value) (Value, error) {
	t.Helper()
	bound, err := adapter.Bind(CapabilityBinding{Context: exec.Context()})
	if err != nil {
		t.Fatal(err)
	}
	receiver := bound[name]
	return BuiltinOf(receiver.HashEntryMap()[method]).Fn(exec, receiver, args, kwargs, NewNil())
}

func TestCapabilityRequestSharesClonesAcrossRoots(t *testing.T) {
	t.Parallel()
	for _, method := range []string{"db.find", "db.update", "db.query", "db.sum", "events.publish", "jobs.enqueue", "jobs.retry"} {
		t.Run(method, func(t *testing.T) {
			t.Parallel()
			ctx, cancel := context.WithCancel(context.Background())
			t.Cleanup(cancel)
			exec := &Execution{ctx: ctx, quota: 1 << 20, memoryQuota: 1 << 20}
			child := NewArray([]Value{NewInt(7)})
			payload := NewHash(map[string]Value{"child": child})
			kwargs := map[string]Value{"first": child, "second": child}
			database, publisher, queue := &dbCapabilityStub{}, &eventsCapabilityStub{}, &jobQueueStub{}
			var adapter CapabilityAdapter
			var args []Value
			switch method {
			case "db.find":
				adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), child}
			case "db.update":
				adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), child, payload}
			case "db.query":
				adapter, args = MustNewDBCapability("db", database), []Value{NewString("items")}
			case "db.sum":
				adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), NewString("total")}
			case "events.publish":
				adapter, args = MustNewEventsCapability("events", publisher), []Value{NewString("items"), payload}
			case "jobs.enqueue":
				adapter, args = MustNewJobQueueCapability("jobs", queue), []Value{NewString("items"), payload}
			case "jobs.retry":
				adapter, args = MustNewJobQueueCapability("jobs", queue), []Value{NewString("id"), payload}
			}
			name, operation, _ := strings.Cut(method, ".")
			if _, err := capabilityDataCall(t, adapter, name, operation, exec, args, kwargs); err != nil {
				t.Fatal(err)
			}
			var copies []Value
			var hostContext context.Context
			switch method {
			case "db.find":
				r := database.findCalls[0]
				copies, hostContext = []Value{r.ID, r.Options["first"], r.Options["second"]}, database.findCtx[0]
			case "db.update":
				r := database.updateCalls[0]
				copies, hostContext = []Value{r.ID, r.Attributes["child"], r.Options["first"], r.Options["second"]}, database.updateCtx[0]
			case "db.query":
				r := database.queryCalls[0]
				copies, hostContext = []Value{r.Options["first"], r.Options["second"]}, database.queryCtx[0]
			case "db.sum":
				r := database.sumCalls[0]
				copies, hostContext = []Value{r.Options["first"], r.Options["second"]}, database.sumCtx[0]
			case "events.publish":
				r := publisher.publishCalls[0]
				copies, hostContext = []Value{r.Payload["child"], r.Options["first"], r.Options["second"]}, publisher.publishCtx[0]
			case "jobs.enqueue":
				r := queue.enqueueCalls[0]
				copies, hostContext = []Value{r.Payload["child"], r.Options.Kwargs["first"], r.Options.Kwargs["second"]}, queue.enqueueCtx[0]
			case "jobs.retry":
				r := queue.retryCalls[0]
				copies, hostContext = []Value{r.Options["child"], r.Options["first"], r.Options["second"]}, queue.retryCtx[0]
			}
			for _, copy := range copies {
				if arrayIdentity(copy) != arrayIdentity(copies[0]) || arrayIdentity(copy) == arrayIdentity(child) {
					t.Fatal("request roots must share one isolated child")
				}
			}
			copies[0].Array()[0] = NewInt(9)
			if child.Array()[0].Int() != 7 || copies[1].Array()[0].Int() != 9 {
				t.Fatal("host mutation did not preserve request aliases and source isolation")
			}
			if hostContext != ctx {
				t.Fatal("host retained a context other than the caller's original context")
			}
			if exec.reservedScratchBytes != 0 {
				t.Fatalf("call retained %d scratch bytes", exec.reservedScratchBytes)
			}
		})
	}
}

func TestCapabilityCopyStopsBeforeHostCall(t *testing.T) {
	t.Parallel()
	for _, method := range []string{"db.update", "events.publish", "jobs.enqueue", "jobs.retry"} {
		for _, limit := range []string{"steps", "memory"} {
			t.Run(method+"/"+limit, func(t *testing.T) {
				t.Parallel()
				exec := &Execution{ctx: context.Background(), quota: 1 << 20, memoryQuota: 1 << 20}
				database, publisher, queue := &dbCapabilityStub{}, &eventsCapabilityStub{}, &jobQueueStub{}
				payload := NewHash(map[string]Value{"data": NewArray(make([]Value, 4096))})
				var adapter CapabilityAdapter
				var args []Value
				switch method {
				case "db.update":
					adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), NewInt(1), payload}
				case "events.publish":
					adapter, args = MustNewEventsCapability("events", publisher), []Value{NewString("topic"), payload}
				default:
					adapter, args = MustNewJobQueueCapability("jobs", queue), []Value{NewString("job"), payload}
				}
				if limit == "steps" {
					exec.quota, exec.memoryQuota = 32, 0
				} else {
					// The source fits; a second backing array does not.
					exec.memoryQuota = exec.hashCallRootBytes(NewNil(), args, nil, NewNil()) + 8192
				}
				name, operation, _ := strings.Cut(method, ".")
				_, err := capabilityDataCall(t, adapter, name, operation, exec, args, nil)
				want := "step quota"
				if limit == "memory" {
					want = "memory quota"
				}
				requireErrorContains(t, err, want)
				if len(database.updateCalls)+len(publisher.publishCalls)+len(queue.enqueueCalls)+len(queue.retryCalls) != 0 {
					t.Fatal("host was called after the input copy exceeded its budget")
				}
				if exec.reservedScratchBytes != 0 {
					t.Fatalf("failed copy retained %d scratch bytes", exec.reservedScratchBytes)
				}
			})
		}
	}
}

func TestCapabilityRuntimeMetersValidation(t *testing.T) {
	t.Parallel()
	for _, method := range []string{"events.publish", "jobs.enqueue", "jobs.retry"} {
		t.Run(method, func(t *testing.T) {
			t.Parallel()
			publisher, queue := &eventsCapabilityStub{}, &jobQueueStub{}
			var adapter CapabilityAdapter
			if strings.HasPrefix(method, "events") {
				adapter = MustNewEventsCapability("events", publisher)
			} else {
				adapter = MustNewJobQueueCapability("jobs", queue)
			}
			script := compileScriptWithConfig(t, Config{StepQuota: 100, MemoryQuotaBytes: Unlimited}, "def run(payload)\n  "+method+"(\"item\", payload)\nend")
			payload := NewHash(map[string]Value{"data": NewArray(make([]Value, 16384))})
			_, err := script.Call(context.Background(), "run", []Value{payload}, callOptionsWithCapabilities(adapter))
			requireErrorContains(t, err, "step quota")
			if len(publisher.publishCalls)+len(queue.enqueueCalls)+len(queue.retryCalls) != 0 {
				t.Fatal("runtime called host after validation exceeded the step quota")
			}
		})
	}
}

func TestCapabilityRetryDoesNotIntroduceCycle(t *testing.T) {
	t.Parallel()
	queue := &jobQueueStub{}
	payload := NewHash(map[string]Value{"child": NewArray([]Value{NewInt(1)})})
	exec := &Execution{ctx: context.Background(), quota: 1 << 20}
	_, err := capabilityDataCall(t, MustNewJobQueueCapability("jobs", queue), "jobs", "retry", exec,
		[]Value{NewString("id"), payload}, map[string]Value{"original": payload})
	if err != nil {
		t.Fatal(err)
	}
	options := queue.retryCalls[0].Options
	if _, ok := options["original"].HashEntryMap()["original"]; ok {
		t.Fatal("merging keyword options created a cycle in the positional snapshot")
	}
	if arrayIdentity(options["child"]) != arrayIdentity(options["original"].HashEntryMap()["child"]) {
		t.Fatal("retry lost a shared child across positional and keyword options")
	}
}

func TestCapabilityReturnCopyUsesRuntimeBudget(t *testing.T) {
	t.Parallel()
	for _, method := range []string{"db.find", "db.query", "db.update", "db.sum", "events.publish", "jobs.enqueue", "jobs.retry"} {
		for _, limit := range []string{"steps", "memory", "control"} {
			t.Run(method+"/"+limit, func(t *testing.T) {
				t.Parallel()
				exec := &Execution{ctx: context.Background(), quota: 1 << 20, memoryQuota: 1 << 20}
				child := NewArray(make([]Value, 4096))
				source := NewHash(map[string]Value{"a": child, "b": child})
				database := &dbCapabilityStub{findResult: source, queryResult: source, updateResult: source, sumResult: source}
				publisher := &eventsCapabilityStub{publishResult: source}
				queue := &sharedReturnQueue{enqueueResult: source, retryResult: source}
				var adapter CapabilityAdapter
				var args []Value
				switch method {
				case "db.find":
					adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), NewInt(1)}
				case "db.query":
					adapter, args = MustNewDBCapability("db", database), []Value{NewString("items")}
				case "db.update":
					adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), NewInt(1), NewHash(nil)}
				case "db.sum":
					adapter, args = MustNewDBCapability("db", database), []Value{NewString("items"), NewString("total")}
				case "events.publish":
					adapter, args = MustNewEventsCapability("events", publisher), []Value{NewString("topic"), NewHash(nil)}
				default:
					adapter, args = MustNewJobQueueCapability("jobs", queue), []Value{NewString("job"), NewHash(nil)}
				}
				switch limit {
				case "steps":
					exec.quota, exec.memoryQuota = 256, 0
				case "memory":
					exec.memoryQuota = 32 << 10
				}
				name, operation, _ := strings.Cut(method, ".")
				got, err := capabilityDataCall(t, adapter, name, operation, exec, args, nil)
				switch limit {
				case "steps":
					requireErrorContains(t, err, "step quota")
					requireErrorContains(t, err, "return value")
				case "memory":
					requireErrorContains(t, err, "memory quota")
					requireErrorContains(t, err, "return value")
				case "control":
					if err != nil {
						t.Fatal(err)
					}
					a, b := got.HashEntryMap()["a"], got.HashEntryMap()["b"]
					if arrayIdentity(a) != arrayIdentity(b) || arrayIdentity(a) == arrayIdentity(child) {
						t.Fatal("returned graph lost its aliases or host isolation")
					}
				}
				if exec.reservedScratchBytes != 0 {
					t.Fatalf("return copy retained %d scratch bytes", exec.reservedScratchBytes)
				}
			})
		}
	}
}

func TestContextCapabilityCopyUsesRuntimeBudget(t *testing.T) {
	t.Parallel()
	for _, limit := range []string{"steps", "memory", "control", "standalone"} {
		t.Run(limit, func(t *testing.T) {
			t.Parallel()
			ctx, cancel := context.WithCancel(context.Background())
			t.Cleanup(cancel)
			exec := &Execution{ctx: ctx, quota: 1 << 20, memoryQuota: 1 << 20}
			child := NewArray(make([]Value, 4096))
			source := NewHash(map[string]Value{"a": child, "b": child})
			var hostContext context.Context
			adapter := MustNewContextCapability("ctx", func(ctx context.Context) (Value, error) {
				hostContext = ctx
				return source, nil
			}).(*contextCapabilityAdapter)
			switch limit {
			case "steps":
				exec.quota, exec.memoryQuota = 32, 0
			case "memory":
				exec.memoryQuota = 32 << 10
			}
			binding := CapabilityBinding{Context: ctx}
			var bound map[string]Value
			var err error
			if limit == "standalone" {
				bound, err = adapter.Bind(binding)
			} else {
				bound, err = adapter.bindWithExecution(exec, binding)
			}
			switch limit {
			case "steps":
				requireErrorContains(t, err, "step quota")
			case "memory":
				requireErrorContains(t, err, "memory quota")
			default:
				if err != nil {
					t.Fatal(err)
				}
				a, b := bound["ctx"].HashEntryMap()["a"], bound["ctx"].HashEntryMap()["b"]
				if arrayIdentity(a) != arrayIdentity(b) || arrayIdentity(a) == arrayIdentity(child) {
					t.Fatal("context graph lost its aliases or host isolation")
				}
			}
			if hostContext != ctx {
				t.Fatal("resolver retained the runtime budget carrier instead of the original context")
			}
			if exec.reservedScratchBytes != 0 {
				t.Fatalf("context copy retained %d scratch bytes", exec.reservedScratchBytes)
			}
		})
	}
}

func TestJobQueueRequestPreservesDistinctTagPolicies(t *testing.T) {
	t.Parallel()
	queue := &jobQueueStub{}
	child := NewArray([]Value{NewInt(1)})
	tagged := NewTaggedObject(map[string]Value{"child": child}, ObjectTagRescuedError, "original")
	parent := NewArray([]Value{tagged, child})
	payload := NewHash(map[string]Value{"parent": parent})
	exec := &Execution{ctx: context.Background(), quota: 1 << 20}
	_, err := capabilityDataCall(t, MustNewJobQueueCapability("jobs", queue), "jobs", "enqueue", exec,
		[]Value{NewString("job"), payload}, map[string]Value{"parent": parent})
	if err != nil {
		t.Fatal(err)
	}
	job := queue.enqueueCalls[0]
	preserved, stripped := job.Payload["parent"], job.Options.Kwargs["parent"]
	if arrayIdentity(preserved) == arrayIdentity(stripped) {
		t.Fatal("payload and options shared an ancestor containing policy-sensitive data")
	}
	if preserved.Array()[0].ObjectTag() != ObjectTagRescuedError || stripped.Array()[0].ObjectTag() != ObjectTagNone {
		t.Fatal("enqueue changed payload or option provenance")
	}
	if arrayIdentity(preserved.Array()[1]) != arrayIdentity(stripped.Array()[1]) {
		t.Fatal("enqueue duplicated a tag-free shared descendant")
	}
}

func TestContextCapabilityRefreshesMemoryAfterResolver(t *testing.T) {
	t.Parallel()
	exec := &Execution{ctx: context.Background(), root: newEnv(nil), quota: 1 << 20, memoryQuota: 32 << 10}
	adapter := MustNewContextCapability("ctx", func(context.Context) (Value, error) {
		exec.root.Define("retained", NewString(strings.Repeat("x", 64<<10)))
		return NewHash(map[string]Value{"id": NewInt(1)}), nil
	}).(*contextCapabilityAdapter)
	_, err := adapter.bindWithExecution(exec, CapabilityBinding{Context: exec.Context()})
	requireErrorContains(t, err, "memory quota")
	if exec.reservedScratchBytes != 0 {
		t.Fatalf("failed refresh retained %d scratch bytes", exec.reservedScratchBytes)
	}
}

func TestDBEachMemoryCountsLiveSnapshots(t *testing.T) {
	t.Parallel()
	for _, retain := range []bool{false, true} {
		name := "discard"
		if retain {
			name = "retain"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			row := NewArray(make([]Value, 64))
			rows := make([]Value, 500)
			for i := range rows {
				rows[i] = row
			}
			stub := &dbCapabilityStub{eachRows: rows}
			body := "total = total + 1"
			if retain {
				body = "kept.push(row)"
			}
			script := compileScriptWithConfig(t, Config{StepQuota: 1 << 20, MemoryQuotaBytes: 128 << 10},
				"def run()\n total = 0\n kept = []\n db.each(\"items\") do |row|\n "+body+"\n end\n total\nend")
			got, err := script.Call(context.Background(), "run", nil, callOptionsWithCapabilities(MustNewDBCapability("db", stub)))
			if retain {
				requireErrorContains(t, err, "memory quota")
			} else if err != nil || got.Int() != 500 {
				t.Fatalf("discarding rows = %v, %v; want 500 within live-memory quota", got, err)
			}
		})
	}
}

func TestCapabilityExposedHashKeepsCompleteIteration(t *testing.T) {
	t.Parallel()
	source := NewHash(map[string]Value{"a": NewInt(1), "b": NewInt(2)})
	exec := &Execution{ctx: context.Background(), quota: 1 << 20}
	result, err := capabilityDataCall(t, MustNewDBCapability("db", &mixedHashMutationDB{}), "db", "update", exec,
		[]Value{NewString("items"), NewInt(1), source}, map[string]Value{"alias": source})
	if err != nil {
		t.Fatal(err)
	}
	keys := result.HashKeyOrder()
	if len(keys) != 3 || keys[0].String() != "a" || keys[1].String() != "b" || keys[2].String() != "c" {
		t.Fatalf("iteration after mixed map/wrapper writes = %v, want [a b c]", keys)
	}
	if source.HashLen() != 2 || source.HashEntryMap()["a"].Int() != 1 {
		t.Fatal("host writes changed the original request")
	}
}
