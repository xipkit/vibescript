package db_test

import (
	"context"
	"errors"
	"testing"

	"github.com/mgomes/vibescript/internal/capabilitydata"
	"github.com/mgomes/vibescript/vibes/capability/db"
	"github.com/mgomes/vibescript/vibes/value"
)

type cloneExecution struct {
	budget *capabilitydata.Budget
	step   func() error
	yield  func(value.Value) error
}

func (e *cloneExecution) Context() context.Context                     { return context.Background() }
func (e *cloneExecution) Step() error                                  { return e.step() }
func (e *cloneExecution) CapabilityDataBudget() *capabilitydata.Budget { return e.budget }
func (e *cloneExecution) CallBlock(_ value.Value, args []value.Value) (value.Value, error) {
	return value.NewNil(), e.yield(args[0])
}

func TestEachClonesIndependentSnapshots(t *testing.T) {
	t.Parallel()
	child := value.NewArray([]value.Value{value.NewInt(1)})
	row := value.NewHash(map[string]value.Value{"a": child, "b": child})
	stub := &dbCapabilityStub{eachRows: []value.Value{row, row}}
	capability := db.MustNewCapability("db", stub)
	budget := capabilitydata.NewBudget(context.Background(), nil, nil)
	refreshes := 0
	budget.SetSnapshotRefresh(func() error { refreshes++; return nil })
	var snapshots []value.Value
	exec := &cloneExecution{budget: budget, step: func() error { return nil }}
	exec.yield = func(snapshot value.Value) error {
		snapshots = append(snapshots, snapshot)
		a, b := snapshot.HashEntryMap()["a"], snapshot.HashEntryMap()["b"]
		if value.ArrayIdentity(a) != value.ArrayIdentity(b) || value.ArrayIdentity(a) == value.ArrayIdentity(child) {
			t.Fatal("row lost its shared child or retained host storage")
		}
		if a.Array()[0].Int() != int64(len(snapshots)) {
			t.Fatal("row reused a stale snapshot after host data changed")
		}
		a.Array()[0] = value.NewInt(99)
		child.Array()[0] = value.NewInt(2)
		return nil
	}
	_, err := capability.CallEach(exec, []value.Value{value.NewString("items")}, nil, value.NewValue(value.KindBlock, nil))
	if err != nil {
		t.Fatal(err)
	}
	if len(snapshots) != 2 || refreshes != 2 {
		t.Fatalf("yielded %d snapshots and refreshed %d times, want 2 of each", len(snapshots), refreshes)
	}
	if value.HashIdentity(snapshots[0]) == value.HashIdentity(snapshots[1]) {
		t.Fatal("separate rows share one mutable snapshot")
	}
}

func TestEachChargesCopiesAcrossSnapshots(t *testing.T) {
	t.Parallel()
	row := value.NewArray(make([]value.Value, 64))
	rows := make([]value.Value, 128)
	for i := range rows {
		rows[i] = row
	}
	stub := &dbCapabilityStub{eachRows: rows}
	capability := db.MustNewCapability("db", stub)
	steps, yields := 1000, 0
	exhausted := errors.New("step quota exceeded")
	step := func() error {
		steps--
		if steps < 0 {
			return exhausted
		}
		return nil
	}
	budget := capabilitydata.NewBudget(context.Background(), func(n int) error {
		for range n {
			if err := step(); err != nil {
				return err
			}
		}
		return nil
	}, nil)
	exec := &cloneExecution{budget: budget, step: step, yield: func(value.Value) error { yields++; return nil }}
	_, err := capability.CallEach(exec, []value.Value{value.NewString("items")}, nil, value.NewValue(value.KindBlock, nil))
	if !errors.Is(err, exhausted) {
		t.Fatalf("Each error = %v, want step exhaustion from cumulative copy work", err)
	}
	if yields == 0 || yields >= len(rows) {
		t.Fatalf("yielded %d rows, want a nonempty prefix", yields)
	}
}
