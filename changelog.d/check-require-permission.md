- **Fixed: static checks respect require permission.** Under strict effects,
  checking a script no longer reads modules or exposes their diagnostics unless
  the call options allow `require`. Authorized checks still resolve imports.
