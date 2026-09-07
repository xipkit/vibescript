//go:build !darwin && !linux && !windows

package runtime

func moduleStoredBaseNative(string) (string, error) {
	return "", errModuleNameUnavailable
}
