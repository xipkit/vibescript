# Classes in Rust

Classes group state and methods. Instances have shared identity: assigning an instance to another variable, or calling `dup`, refers to the same object. Arrays and hashes stored in its fields still follow collection value semantics.

```vibe
class Counter
  property count: int

  def initialize(@count = 0)
  end

  def increment(n: int = 1)
    @count += n
  end

  alias bump increment
end

def run(input)
  counter = Counter.new(10)
  copy = counter.dup
  copy.bump(3)
  [counter.count, copy == counter]
end
```

This returns `[13, true]`. Constructors forward positional arguments, keywords and an attached block to `initialize`. The constructor returns the new instance regardless of the initializer's return value or return annotation. With no initializer, arguments are evaluated and ignored; an attached block is ignored. A written `initialize` method is private by default.

## Fields and accessors

`@name` accesses an instance variable. Missing instance variables read as nil. The `@name` parameter shorthand assigns the bound value to both the parameter and its backing field.

`property` generates a getter and setter; `getter` and `setter` generate one half. A member assignment calls its setter when present, rejects a getter-only property, and otherwise writes a raw field. Arrays or hashes returned by a generated getter are collection values: mutating that result does not write through the getter. Methods can update the backing field directly.

```vibe
class Basket
  getter items: array<int>

  def initialize
    @items = [1]
  end

  def append(value: int)
    @items.push(value)
  end
end

def run(input)
  basket = Basket.new
  snapshot = basket.items
  basket.append(2)
  [snapshot, basket.items]
end
```

This returns `[[1], [1, 2]]`. Typed generated accessors also guard direct backing-field writes, shorthand parameters and nested mutations. Rejected nested mutations preserve the previous field value. A handwritten setter takes over the write contract, so backing-field writes stay dynamic beside that setter. Nominal class types accept instances of the exact declared class; nullable class fields can hold nil.

## Class state and visibility

Class methods use `def self.name`. Class variables use `@@name` and are shared within one invocation. Every call starts with independent class state.

```vibe
class Counter
  @@instances = 0

  def initialize
    @@instances += 1
  end

  def self.instances
    @@instances
  end
end

def run(input)
  Counter.new
  Counter.new
  Counter.instances
end
```

This returns `2` on every call. Uppercase assignments in a class body or class method write class constants. In instance methods, assignments normally create method locals; explicitly referring to a class member accesses the class state.

Methods and accessors support public, private and protected sections, inline modifiers and symbol directives. Ordinary private calls require an implicit receiver. Protected instance methods allow callers from the same class's instances; protected class methods allow callers from that class's class methods. [Dynamic `send`](forwarding.md) can reach private and protected methods; `public_send` retains normal explicit-receiver visibility. Aliases preserve the target definition and its visibility at the alias declaration.

Instances expose `class`, `respond_to?`, `is_a?`, `kind_of?` and `instance_of?`. The class predicates compare exact class identity. Private and protected methods are reported by `respond_to?` when called implicitly on the current receiver or with a true second argument.

## Operators and indexed access

Instances can define `+`, `-`, `*`, `/`, `%`, `**`, `<<`, `&`, `==`, `!=`, `<`, `<=`, `>`, `>=` and `<=>`. Operator syntax calls the left instance's method. Compound assignments use the corresponding operator and store its result. An explicit `!=` takes precedence; otherwise `!=` negates the truthiness of the result from `==`.

```vibe
class Counter
  getter value: int

  def initialize(@value)
  end

  def +(amount: int)
    Counter.new(@value + amount)
  end

  def to_s
    "count=#{@value}"
  end
end

def run(input)
  before = Counter.new(2)
  after = before + 3
  [before.value, after.value, "#{after}"]
end
```

This returns `[2, 5, "count=5"]`. Direct interpolation calls a `to_s` that accepts zero arguments, including private methods and methods with optional parameters. A required parameter or a non-string result preserves the default instance rendering. Containers keep their own element rendering. Errors and exhausted limits propagate through the conversion.

`[]` receives the index selectors; `[]=` receives those selectors followed by the assigned value. Indexed assignment returns the assigned value, while still enforcing the setter's return annotation. Plain assignment evaluates the RHS before its target; compound assignment evaluates its receiver and selectors once before reading and updating the value.

```vibe
class Grid
  def initialize
    @cells = {}
  end

  def [](row, column)
    @cells.fetch("#{row}:#{column}", nil)
  end

  def []=(row, column, value)
    @cells["#{row}:#{column}"] = value
  end
end

def run(input)
  grid = Grid.new
  grid[1, 2] = 4
  grid[1, 2] += 5
  grid[3, 4] ||= 7
  [grid[1, 2], grid[3, 4]]
end
```

This returns `[9, 7]`. Arrays and hashes returned by an index getter remain collection values. Mutating the returned temporary does not write into stored collections or earlier snapshots; returned instances retain their shared identity. Operator and index syntax enforce method visibility and normal call boundaries.

## Limits and retained values

Object fields, identity storage, imports and graph traversal are accounted. Cycles are supported and unreachable objects are reclaimed. Cancellation, deadlines and exhausted limits stay latched through constructors, methods and cleanup. A host may retain an instance after a successful or failed call.

Instances returned to Rust can be passed back to the same compiled script. Imports preserve shared references and cycles within the new call while isolating mutations from the source value. Concurrent calls also get independent imported objects and class state.

The class port is incomplete. `puts`/`print` output and transfer of live namespace state between compiled scripts remain pending. Inheritance, singleton classes, `super`, and module mixins are outside the Vibescript language.
