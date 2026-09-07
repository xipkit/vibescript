//go:build !darwin && !windows

package runtime

func moduleStoredBaseNative(string) (string, error) {
	// Linux procfs descriptor names can preserve the first lookup's casing
	// rather than the stored directory entry, so they are not a proof here.
	return "", errModuleNameUnavailable
}
