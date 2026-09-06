- **Fixed: bounded module request cache text.** Repeated `require` calls now
  retain at most 8 MiB of request, search, and suggestion text per engine.
  Requests beyond the cache limit still resolve normally.
