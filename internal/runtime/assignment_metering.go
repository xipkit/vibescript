package runtime

import (
	"reflect"

	"github.com/mgomes/vibescript/vibes/value"
)

func (exec *Execution) assignBinding(env *Env, name string, val Value) {
	assignment := env.assignValueWithAppendBufferHandling(name, val, true)
	exec.noteBindingAssignment(assignment)
}

func (exec *Execution) noteBindingAssignment(assignment bindingAssignment) {
	if assignment.scope.mutationVersion != assignment.version {
		exec.noteAssignmentMutation(assignment.scope)
	}
}

// noteAssignmentMutation records a committed script write independently of
// cache invalidations, which may instead come from unrelated executions.
func (exec *Execution) noteAssignmentMutation(scope *Env) {
	if exec.memoryQuota <= 0 {
		return
	}
	if scope != nil && scope.epochNeutral {
		exec.assignmentSuffixDirty = true
		return
	}
	exec.assignmentBaseDirty = true
}

func (exec *Execution) assignmentPrefixKnown() bool {
	c := exec.baseWalkCache
	return !exec.baseWalkOpen && exec.blockRegionActive && c != nil && (c.valid || c.unmemoizedPrefix) &&
		c.regionBoundary == exec.blockRegionBoundary && c.topo == exec.baseTopoVersion
}

func (exec *Execution) noteAssignmentValueMutation(val Value) {
	if exec.memoryQuota <= 0 {
		return
	}
	if !exec.assignmentPrefixKnown() {
		exec.assignmentBaseDirty = true
		return
	}
	var inPrefix bool
	switch val.Kind() {
	case KindArray:
		inPrefix = exec.memoryEst.mutationAffectsGraph(value.MutationSlice, sliceBackingIdentity(val.Array()), 0)
	case KindHash:
		inPrefix = exec.memoryEst.mutationAffectsGraph(value.MutationHash, hashIdentity(val), reflect.ValueOf(val.HashEntryMap()).Pointer())
	case KindObject:
		inPrefix = exec.memoryEst.mutationAffectsGraph(value.MutationObject, value.ObjectIdentity(val), reflect.ValueOf(val.HashEntryMap()).Pointer())
	default:
		inPrefix = true
	}
	if inPrefix {
		exec.assignmentBaseDirty = true
	} else {
		exec.assignmentSuffixDirty = true
	}
}

func (exec *Execution) noteAssignmentMapMutation(entries map[string]Value) {
	if exec.memoryQuota <= 0 {
		return
	}
	if !exec.assignmentPrefixKnown() {
		exec.assignmentBaseDirty = true
		return
	}
	if _, seen := exec.memoryEst.seenMaps[reflect.ValueOf(entries).Pointer()]; seen {
		exec.assignmentBaseDirty = true
	} else {
		exec.assignmentSuffixDirty = true
	}
}

func (s *baseWalkSession) collectAssignmentWork() {
	exec := s.exec
	nodes := 0
	if !s.walkBilled {
		if exec.assignmentBaseDirty {
			nodes = s.nodes()
		} else if exec.assignmentSuffixDirty && s.region {
			// A suffix write cannot own a prefix rebuild caused by unrelated
			// opaque traffic or an overflowing wrapper-mutation journal.
			nodes = s.nodes() - s.prefixNodes
		}
	}
	exec.assignmentBaseDirty = false
	exec.assignmentSuffixDirty = false
	exec.assignmentWalkNodes = saturatingAdd(exec.assignmentWalkNodes, nodes)
}

func (exec *Execution) chargeAssignmentWalk() error {
	nodes := exec.assignmentWalkNodes
	if nodes == 0 {
		return nil
	}
	// Clear before stepN: its periodic check can open another memory session.
	exec.assignmentWalkNodes = 0
	return exec.chargeEstimatorWalk(nodes)
}
