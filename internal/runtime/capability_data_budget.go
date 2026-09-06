package runtime

import "github.com/mgomes/vibescript/internal/capabilitydata"

func newCapabilityDataBudget(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (*capabilitydata.Budget, *loopScratchReservation, error) {
	reservation := &loopScratchReservation{exec: exec}
	budget := capabilitydata.NewBudget(exec.Context(), exec.chargeScanSteps, reservation.reserve)
	budget.SetSnapshotRefresh(func() error {
		// The previous request or row has crossed its boundary. Release its
		// temporary charge before measuring the next snapshot; copies retained
		// by script code now belong to the live roots. The operation's Budget
		// still counts cumulative work and allocation reservations.
		reservation.release()
		if exec.memoryQuota <= 0 {
			return nil
		}
		used, walked := exec.hashCallRootUsage(receiver, args, kwargs, block)
		reservation.baseline = used
		if exec.memoryExceeded(used) {
			return exec.memoryQuotaExceededError()
		}
		return budget.Work(walked)
	})
	if err := budget.Refresh(); err != nil {
		return nil, nil, err
	}
	return budget, reservation, nil
}

type capabilityDataExecution struct {
	*Execution
	budget *capabilitydata.Budget
}

// CapabilityDataBudget supplies a call-scoped budget without changing db's
// public ExecutionContext interface or wrapping the context seen by hosts.
func (e *capabilityDataExecution) CapabilityDataBudget() *capabilitydata.Budget {
	return e.budget
}

func cloneCapabilityResult(budget *capabilitydata.Budget, method string, result Value) (Value, error) {
	if err := budget.Refresh(); err != nil {
		return NewNil(), err
	}
	label := method + " return value"
	if err := capabilitydata.NewValidator(budget).Validate(label, result); err != nil {
		return NewNil(), err
	}
	return capabilitydata.NewCloner(budget, capabilitydata.Options{PreserveObjectTags: true}).Clone(label, result)
}
