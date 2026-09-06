- **Fixed: enforce regex replacement output limits before expansion.**
  `Regex.replace` and `Regex.replace_all` bound each captured substring before
  appending it, including calls through the direct dispatch path.
