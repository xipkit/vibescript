- **Fixed: module policy filename matching.** Whitespace within path components
  and additional filename extensions no longer let distinct modules share an
  allow-list or deny-list match. Module loading requires the stored spelling of
  each path component, so filesystem aliases cannot bypass policy checks or
  initialize a second copy of the same named module.
