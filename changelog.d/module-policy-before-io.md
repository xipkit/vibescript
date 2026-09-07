- **Fixed: denied module requests stop before filesystem access.** Module
  allow-list and deny-list checks now precede filename inspection and source
  reads, so denied requests consistently return policy errors regardless of
  whether the target exists, is readable, or is a valid module file.
