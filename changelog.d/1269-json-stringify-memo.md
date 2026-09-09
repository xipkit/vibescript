### Performance

- Reuse memory-estimator graph walks during `JSON.stringify` so escape-heavy strings avoid repeatedly traversing unchanged values while enforcing the same quotas.
