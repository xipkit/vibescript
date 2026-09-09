//go:build go1.27 && goexperiment.simd && (arm64 || amd64)

package runtime

// Restrict SIMD to sizes with repeatable gains over word processing.
const (
	asciiCaseSIMDMinBytes = 64
	asciiCaseSIMDMaxBytes = 16 * 1024
)
