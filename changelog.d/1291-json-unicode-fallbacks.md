### Performance

- Reduce escaped JSON decoding and Unicode `index`/`rindex` costs by filling pre-sized output buffers and reusing prior UTF-8 validation while preserving quota checks.
