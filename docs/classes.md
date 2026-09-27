# Classes

Classes group state and methods. Instances have shared identity: assigning an instance to another variable, or calling `dup`, refers to the same object. Arrays and hashes stored in its fields still follow collection value semantics. Classes are nominal and have no inheritance, so an instance's type is exactly its class.

```vibe
class Counter
  property count: int

  def initialize(@count: int = 0)
  end

  def increment(n: int = 1)
    @count += n
  end

  alias bump increment
end

def run -> [int, bool]
  counter = Counter.new(10)
  copy = counter.dup
  copy.bump(3)
  [counter.count, copy == counter]
end
```

This returns `[13, true]`. Constructors forward positional arguments, keywords and an attached block to `initialize`, which declares its parameter types like any method. The constructor returns the new instance regardless of the initializer's return value. Without an initializer, `new` takes no arguments. A written `initialize` method is private by default.

## Fields and accessors

`@name` reads or writes an instance variable, and every instance variable is declared: in the class body as `@count: int = 0`, which gives each instance that default before `initialize` runs; as `@name: string` without a default, which `initialize` must assign on every path (V0205); or by a `property`, `getter` or `setter`. Reading or assigning an undeclared instance variable is a compile error (V0204). The `@name: T` parameter shorthand assigns the bound argument to that declared field.

The `@name: T` initializer parameter shorthand assigns a field; it does not replace the class-body declaration or accessor declaration.

`property` generates a getter and setter; `getter` and `setter` generate one half. A member assignment such as `counter.count = 3` calls the setter, and one without a setter is a compile error. Arrays or hashes returned by a generated getter are collection values: updating that result does not write through the getter. Methods update the backing field directly.

```vibe
class Basket
  getter items: array<int>

  def initialize
    @items = [1]
  end

  def add(value: int)
    @items.push(value)
  end
end

def run -> array<array<int>>
  basket = Basket.new
  snapshot = basket.items
  basket.add(2)
  [snapshot, basket.items]
end
```

This returns `[[1], [1, 2]]`. Declared field types also guard the shorthand parameters, nested updates and values imported from the host; a rejected nested update preserves the previous field value. A nullable field, such as `@next: Node? = nil`, can hold `nil`.

## Class state and visibility

Class methods use `def self.name`. Class variables are declared with a value, `@@name: T = value`, and are shared within one invocation. Every call starts with independent class state.

```vibe
class Counter
  @@instances: int = 0

  def initialize
    @@instances += 1
  end

  def self.instances -> int
    @@instances
  end
end

def run -> int
  Counter.new
  Counter.new
  Counter.instances
end
```

This returns `2` on every call. Uppercase assignments in a class body define class constants, read as `LIMIT` inside the class and `Counter::LIMIT` outside it. `LIMIT: int = 3` declares a constant's type, which every assignment to it keeps. Nested classes are named through their scope, `Outer::Inner`, in types as in values.

Methods and accessors support public, private and protected sections, inline modifiers such as `private def helper`, and symbol directives. Ordinary private calls require an implicit receiver. Protected instance methods allow callers from the same class's instances; protected class methods allow callers from that class's class methods. Aliases preserve the target definition and its visibility at the alias declaration. With static types, a call its visibility forbids is a compile error (V0208).

An instance's class is tested with `value.is_type?(:Counter)` and asserted with the checked cast `value.as(Counter)`.

## Operators and indexed access

Instances can define `+`, `-`, `*`, `/`, `%`, `**`, `<<`, `&`, `==`, `!=`, `<`, `<=`, `>`, `>=` and `<=>`, each with typed parameters and a declared result. Operator syntax calls the left instance's method, and an operator the class does not define is a compile error (V0108); `<` is not derived from `<=>`. Compound assignments use the corresponding operator and store its result. An explicit `!=` takes precedence; otherwise `!=` negates the result of `==`.

```vibe
class Counter
  getter value: int

  def initialize(@value: int)
  end

  def +(amount: int) -> Counter
    Counter.new(@value + amount)
  end

  def to_s -> string
    "count=#{@value}"
  end
end

def run -> [int, int, string]
  before = Counter.new(2)
  after = before + 3
  [before.value, after.value, "#{after}"]
end
```

This returns `[2, 5, "count=5"]`. Interpolation, output helpers and `format` call a `to_s` that accepts zero arguments, including private methods and methods with optional parameters. A required parameter or a non-string result preserves the default instance rendering. Containers keep their own element rendering, as does the string `%` operator. Errors and exhausted limits propagate through the conversion.

`[]` receives the index selectors; `[]=` receives those selectors followed by the assigned value. Indexed assignment returns the assigned value. Plain assignment evaluates the right-hand side before its target; compound assignment evaluates its receiver and selectors once before reading and updating the value.

```vibe
class Grid
  @cells: hash<string, int> = {}

  def [](row: int, column: int) -> int
    @cells.fetch("#{row}:#{column}", 0)
  end

  def []=(row: int, column: int, value: int)
    @cells["#{row}:#{column}"] = value
  end
end

def run -> [int, int]
  grid = Grid.new
  grid[1, 2] = 4
  grid[1, 2] += 5
  [grid[1, 2], grid[3, 4]]
end
```

This returns `[9, 0]`. Arrays and hashes returned by an index getter remain collection values. Updating the returned temporary does not write into stored collections or earlier snapshots; returned instances retain their shared identity. Operator and index syntax enforce method visibility and normal call boundaries.

## Limits and retained values

Object fields, identity storage, imports and graph traversal are accounted. Cycles are supported and unreachable objects are reclaimed. Cancellation, deadlines and exhausted limits stay latched through constructors, methods and cleanup. A host may retain an instance after a successful or failed call.

Instances returned to Rust can be passed back to the same compiled script. Imports preserve shared references and cycles within the new call while isolating updates from the source value. Concurrent calls also get independent imported objects and class state.

Inheritance, singleton classes, `super`, and module mixins are outside the language.
