package runtime

import "github.com/mgomes/vibescript/vibes/value"

func (c *baseWalkCache) captureMutationEpochs(epoch uint64) {
	c.epoch = epoch
	c.opaqueEpoch = value.OpaqueMutationEpoch()
	c.wrapperEpoch = value.WrapperMutationEpoch()
}

func (c *baseWalkCache) mutationsCurrent(est *memoryEstimator, epoch uint64) bool {
	if c.epoch == epoch {
		return true
	}
	return c.validateMutations(est, epoch)
}

// validateMutations validates only writes since the committed walk. Env versions
// isolate lexical binding traffic without a journal limit; wrapper identities
// use the estimator's existing seen sets, so no collection gains ownership or
// subscriber metadata. Unknown writes and journal overflow still discard the
// whole memo conservatively.
func (c *baseWalkCache) validateMutations(est *memoryEstimator, epoch uint64) bool {
	if c.opaqueEpoch != value.OpaqueMutationEpoch() || !est.envVersionsCurrent() {
		return false
	}
	through := value.WrapperMutationEpoch()
	if value.WrapperMutationsAffect(c.wrapperEpoch, through, est.mutationAffectsGraph) {
		return false
	}
	c.epoch = epoch
	c.wrapperEpoch = through
	return true
}

func (est *memoryEstimator) envVersionsCurrent() bool {
	for i := range est.seenEnvInlineLen {
		if est.seenEnvInline[i].mutationVersion != est.seenEnvVersions[i] {
			return false
		}
	}
	for env, version := range est.seenEnvs {
		if env.mutationVersion != version {
			return false
		}
	}
	return true
}

func (est *memoryEstimator) mutationAffectsGraph(kind value.MutationKind, identity, backing uintptr) bool {
	switch kind {
	case value.MutationSlice:
		_, seen := est.seenSlices[identity]
		return seen
	case value.MutationHash:
		_, wrapperSeen := est.seenHashData[identity]
		_, mapSeen := est.seenMaps[backing]
		return wrapperSeen || mapSeen
	case value.MutationObject:
		_, wrapperSeen := est.seenObjectData[identity]
		_, mapSeen := est.seenMaps[backing]
		return wrapperSeen || mapSeen
	default:
		return true
	}
}
