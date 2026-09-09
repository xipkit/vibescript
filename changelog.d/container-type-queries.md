Static checking now reuses container classification for union types, avoiding repeated union traversal and temporary allocations when many statements use the same annotation.
