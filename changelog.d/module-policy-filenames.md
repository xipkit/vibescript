- **Fixed: module policy filename matching.** Whitespace within path components
  and additional filename extensions no longer let distinct modules share an
  allow-list or deny-list match. Module loading requires the stored spelling of
  each path component, so filesystem aliases cannot bypass policy checks or
  initialize a second copy of the same named module.
  Policy patterns with ambiguous edge whitespace now fail engine construction;
  use explicit dot components for literal whitespace. Linux module directories
  must permit listing for filename verification.
