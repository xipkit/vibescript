package runtime

import "fmt"

func arrayProjectionCopy(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, elements []Value) (Value, error) {
	if err := exec.checkStepBudgetFor(len(elements)); err != nil {
		return NewNil(), err
	}
	// The copied values alias the call roots; only the new backing and wrapper
	// add memory. No callback runs while this reservation is in force.
	defer exec.beginAccumulatorMeteredSection()()
	if err := checkArrayProjection(exec, receiver, args, kwargs, block, len(elements), 0); err != nil {
		return NewNil(), err
	}
	if err := exec.checkStepBudgetFor(len(elements)); err != nil {
		return NewNil(), err
	}
	out := make([]Value, len(elements))
	for i, item := range elements {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		out[i] = item
	}
	return NewArray(out), nil
}

func arrayTranspose(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) > 0 || len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("array.transpose does not take arguments")
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	// Validation allocates no result, then the complete build is reserved below.
	// Neither phase invokes script code or changes the baseline roots.
	defer exec.beginAccumulatorMeteredSection()()
	rows := receiver.Array()
	columnCount := 0
	for i, row := range rows {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		if row.Kind() != KindArray {
			return NewNil(), fmt.Errorf("array.transpose requires arrays as elements, but element at index %d is a %s", i, row.Kind())
		}
		got := len(row.Array())
		if i == 0 {
			columnCount = got
			continue
		}
		if got != columnCount {
			return NewNil(), fmt.Errorf("array.transpose requires equal-length rows, but element at index %d has length %d (expected %d)", i, got, columnCount)
		}
	}
	work, err := arrayCombinatoricsWork("array.transpose", columnCount, len(rows))
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkStepBudgetFor(work); err != nil {
		return NewNil(), err
	}
	if err := checkArrayProjection(exec, receiver, args, kwargs, block, columnCount, arrayTupleRowBackingBytes(columnCount, len(rows))); err != nil {
		return NewNil(), err
	}
	if err := exec.checkStepBudgetFor(work); err != nil {
		return NewNil(), err
	}
	columns := make([]Value, columnCount)
	for col := range columnCount {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		transposed := make([]Value, len(rows))
		for rowIndex, row := range rows {
			if err := exec.step(); err != nil {
				return NewNil(), err
			}
			transposed[rowIndex] = row.Array()[col]
		}
		columns[col] = NewArray(transposed)
	}
	return NewArray(columns), nil
}

func checkArrayProjection(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, slots, payloadBytes int) error {
	acc := newArrayBuildAccumulator(exec, receiver, args, kwargs, block)
	outputBytes := saturatingAdd(arraySlotBackingBytes(slots), payloadBytes)
	budget := exec.memoryBudgetBytes()
	// Subtract before comparing: a saturated base-plus-output sum could
	// otherwise equal and pass a maximum-int quota on 32-bit builds.
	if exec.memoryQuota > 0 && (outputBytes > budget || acc.base > budget-outputBytes) {
		return exec.memoryQuotaExceededError()
	}
	if acc.est != nil {
		return exec.chargeEstimatorWalk(acc.est.walked)
	}
	return nil
}
