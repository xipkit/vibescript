# Empty hash construction

`Hash.new` and `Hash.new()` create fresh empty hashes. Scoped access, namespace aliases, indexed lookup, `send` and `public_send` use the same constructor. Missing keys read as nil; `fetch(key, fallback)` or its block form supplies a fallback for an individual lookup.

The constructor accepts no defaults, keywords or blocks. Keyword validation precedes the default/block rejection. Arguments retain normal evaluation order, and rejected blocks do not run. A returned hash has the same logical value semantics as a hash literal: changing one binding leaves an earlier copy unchanged.

Each empty hash's storage is accounted for before allocation. Keeping many hashes consumes the memory budget; repeatedly discarding hashes keeps peak storage bounded. Cancellation and exhausted work or memory remain latched, and later calls start with fresh state.

Five native tests cover construction, aliases, argument order, exact work limits, sampled memory boundaries, retention and cancellation. The shared corpus adds 178 reference-checked evaluations and eleven uncaught rejections. One `tap` case follows the selected collection-value policy, and seventeen cases retain differing diagnostic wording. Those observations remain explicit in [the difference record](hash-new-differences.json).
