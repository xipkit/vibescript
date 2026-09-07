- **Fixed: JSON parsing resource limits.** Parsing charges collection elements
  and all partially built values without repeatedly walking growing prefixes.
  Escaped strings allocate only for their decoded token, and parsing checks
  cancellation while building collections. Duplicate keys still keep the last
  value and their original insertion order.
