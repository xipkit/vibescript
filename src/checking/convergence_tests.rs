use crate::{CallOptions, CheckReport, Engine, Limits, Value};

/// A modest bound for these small programs. The heaviest, a nested union grown through
/// mutual recursion across a method on new objects, needs a little over half of it.
const BUDGET: u64 = 2_000_000;

fn budget() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(BUDGET),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

/// Checks a whole file within the budget and requires complete analysis.
fn converged(source: &str) -> CheckReport {
    let script = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let report = script
        .check(&budget())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

/// Requires the checker's decision to agree with executing `run(3)`.
fn witnessed(source: &str, clean: bool) {
    let report = converged(source);
    assert_eq!(report.is_clean(), clean, "{source}: {report:?}");
    let script = Engine::new().compile(source).unwrap();
    let executed = script.call("run", &[Value::int(3)], CallOptions::default());
    assert_eq!(executed.is_ok(), clean, "{source}: {executed:?}");
}

fn looped(init: &str, step: &str) -> String {
    format!(
        r##"
        def run(n)
          x = {init}
          i = 0
          while i < n
            {step}
            i = i + 1
          end
          x
        end
        "##
    )
}

#[test]
fn computed_literals_widen_in_loops_blocks_and_recursion() {
    let mut sources = Vec::new();
    for (init, step) in [
        (r##""ab""##, r##"x = "#{x}#{x}""##),
        (r##""ab""##, r##"x = "#{x}!""##),
        (r##""a""##, r##"x = x.gsub("a", "aa")"##),
        (r##""a""##, r##"x = x.sub("a", "ba")"##),
        (r##"[""]"##, r##"x = ["#{x[0]}z"]"##),
        (r##"{"a": ""}"##, r##"x = {"a": "#{x["a"]}z"}"##),
        (r##"[""]"##, r##"x << "#{x[0]}z""##),
        (r##""a""##, r##"x = "#{x}b".to_sym.to_s"##),
    ] {
        sources.push(looped(init, step));
    }
    sources.extend(
        [
            r##"
            def run(n)
              x = "a"
              n.times do |i|
                x = "#{x}#{x}"
              end
              x
            end
            "##,
            r##"
            def run(n)
              x = "a"
              [n, n].each do |i|
                x = "#{x}#{i}"
              end
              x
            end
            "##,
            r##"
            def grow(s, n)
              if n > 0
                grow("#{s}#{s}", n - 1)
              else
                s
              end
            end
            def run(n)
              grow("a", n)
            end
            "##,
            r##"
            def grow(n)
              if n > 0
                "#{grow(n - 1)}x"
              else
                ""
              end
            end
            def run(n)
              grow(n)
            end
            "##,
            r##"
            module Log
              @@text = ""
              def self.add(n)
                @@text = "#{@@text}x"
                if n > 0
                  add(n - 1)
                end
                @@text
              end
            end
            def run(n)
              Log.add(n)
            end
            "##,
        ]
        .map(String::from),
    );
    for source in &sources {
        witnessed(source, true);
    }
}

#[test]
fn stable_literal_alternatives_keep_their_precision() {
    // Constant states and stable computed keys still select exact shape fields.
    witnessed(
        r##"
        def run(n)
          h = {"a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": 6}
          key = "a"
          total = 0
          i = 0
          while i < n
            total = total + h[key]
            if key == "a"
              key = "b"
            elsif key == "b"
              key = "c"
            elsif key == "c"
              key = "d"
            elsif key == "d"
              key = "e"
            elsif key == "e"
              key = "f"
            else
              key = "a"
            end
            i = i + 1
          end
          total
        end
        "##,
        true,
    );
    witnessed(
        r##"
        def run(n)
          p = "k"
          h = {"ka": 1, "kb": 2}
          key = "#{p}a"
          total = 0
          i = 0
          while i < n
            total = total + h[key]
            if i > 1
              key = "#{p}a"
            else
              key = "#{p}b"
            end
            i = i + 1
          end
          total
        end
        "##,
        true,
    );
    // A key that keeps growing becomes a general string, and execution does miss the shape.
    witnessed(
        r##"
        def run(n)
          h = {"a": 1, "aa": 2}
          key = "a"
          total = 0
          i = 0
          while i < n
            total = total + h[key]
            key = "#{key}a"
            i = i + 1
          end
          total
        end
        "##,
        false,
    );
}

#[test]
fn allocation_after_branches_that_allocated_differently_keeps_object_positions() {
    witnessed(
        r##"
        class Row
          def initialize(id)
            @id = id
          end
          def id
            @id
          end
          def set(id)
            @id = id
          end
        end
        def run(n)
          first = Row.new(1)
          if n > 2
            first.set(2)
            Row.new(9)
          end
          second = Row.new(3)
          first.id + second.id
        end
        "##,
        true,
    );
}

#[test]
fn allocations_in_loops_and_callbacks_keep_fresh_objects_exact() {
    for source in [
        // Objects created on every pass fold into one summary entry, while each fresh object
        // keeps the fields its constructor wrote.
        r##"
        class Row
          def initialize(id)
            @id = id
          end
          def id
            @id
          end
        end
        def run(n)
          rows = []
          total = 0
          i = 0
          while i < n
            row = Row.new(i)
            rows << row
            total = total + row.id
            i = i + 1
          end
          total + rows.length
        end
        "##,
        r##"
        class Link
          def initialize(value, rest)
            @value = value
            @rest = rest
          end
          def value
            @value
          end
        end
        def run(n)
          head = Link.new(0, nil)
          n.times do |i|
            head = Link.new(head.value + i, head)
          end
          head.value
        end
        "##,
        r##"
        class Row
          def initialize(id)
            @id = id
          end
          def id
            @id
          end
        end
        def run(n)
          rows = (0...n).map do |i|
            Row.new(i)
          end
          rows.length
        end
        "##,
    ] {
        witnessed(source, true);
    }
}

#[test]
fn recursion_through_new_objects_reaches_a_summary() {
    for source in [
        r##"
        class Counter
          def value(n)
            if n > 0
              Counter.new.value(n - 1)
            else
              0
            end
          end
        end
        def run(n)
          Counter.new.value(n) + 1
        end
        "##,
        r##"
        class Node
          def initialize(depth)
            @depth = depth
          end
          def build(n)
            if n > 0
              Node.new(@depth + 1).build(n - 1)
            else
              @depth
            end
          end
        end
        def run(n)
          Node.new(0).build(n) + 1
        end
        "##,
        r##"
        class Tree
          def initialize(n)
            if n > 0
              @child = Tree.new(n - 1)
            else
              @child = nil
            end
          end
          def size
            child = @child
            if child.nil?
              1
            else
              1 + child.size
            end
          end
        end
        def run(n)
          Tree.new(n).size
        end
        "##,
        r##"
        class Node
          def initialize(value, rest)
            @value = value
            @rest = rest
          end
          def value
            @value
          end
        end
        def build(n)
          if n == 0
            nil
          else
            Node.new(n, build(n - 1))
          end
        end
        def run(n)
          head = build(n)
          if head.nil?
            0
          else
            head.value + 1
          end
        end
        "##,
        r##"
        class Tree
          def grow(n, kids: array)
            if n > 0
              kids << Tree.new
              grow(n - 1, kids)
            end
            kids.length
          end
        end
        def run(n)
          Tree.new.grow(n, [])
        end
        "##,
        r##"
        class Walker
          def step(n)
            if n > 0
              hop(n - 1)
            else
              0
            end
          end
        end
        class Hopper
          def self.go(n)
            Walker.new.step(n)
          end
        end
        def hop(n)
          Hopper.go(n)
        end
        def run(n)
          hop(n) + 1
        end
        "##,
    ] {
        witnessed(source, true);
    }
}

/// Statements that replace `x` with a larger fact on every pass, starting from `x`'s first
/// value: computed strings and symbols, grown and nested arrays and hashes, literal numbers,
/// unions that keep gaining arms and newly allocated objects.
const GROWTH: [(&str, &str); 16] = [
    (r##""ab""##, r##"x = "#{x}#{x}""##),
    (r##""a""##, r##"x = x + "b""##),
    (r##""a""##, r##"x = x.gsub("a", "aa")"##),
    (r##""a""##, r##"x = "#{x}b".to_sym.to_s"##),
    ("[]", "x << x.length"),
    ("[]", "x.push([x.length])"),
    ("[]", "x = x + [x]"),
    ("[0]", "x[0] = [x]"),
    ("{}", r##"x["k#{x.length}"] = x"##),
    (r##"{"a": 0}"##, r##"x = {"a": x["a"] + 1, "b": x}"##),
    ("1", "x = x * 3"),
    ("1.5", "x = x * 1.5"),
    ("1", r##"x = [x, "#{x}"]"##),
    ("nil", "x = if x.nil? then 1 else [x] end"),
    ("[]", "x << row(x.length)"),
    ("nil", "x = row(x)"),
];

/// Places a growth step in a loop, in blocks, and in the arguments and results of recursion
/// through functions, methods on new objects, class methods and mutual calls. Class-body
/// methods find their class through a function: a step on a gradual value has unknown
/// effects that leave later reads of class constants explicitly incomplete.
const CONTEXTS: [&str; 8] = [
    "def run(n)\n  x = INIT\n  i = 0\n  while i < n\n    STEP\n    i = i + 1\n  end\n  x\nend\n",
    "def run(n)\n  x = INIT\n  n.times do |i|\n    STEP\n  end\n  x\nend\n",
    "def run(n)\n  x = INIT\n  (0...n).each do |i|\n    STEP\n  end\n  x\nend\n",
    "def grow(x, n)\n  if n > 0\n    STEP\n    grow(x, n - 1)\n  else\n    x\n  end\nend\ndef run(n)\n  grow(INIT, n)\nend\n",
    "def grow(n)\n  if n > 0\n    x = grow(n - 1)\n    STEP\n    x\n  else\n    INIT\n  end\nend\ndef run(n)\n  grow(n)\nend\n",
    "class Grower\n  def grow(x, n)\n    if n > 0\n      STEP\n      grower.new.grow(x, n - 1)\n    else\n      x\n    end\n  end\nend\ndef grower\n  Grower\nend\ndef run(n)\n  Grower.new.grow(INIT, n)\nend\n",
    "class Grower\n  def self.grow(x, n)\n    if n > 0\n      STEP\n      grower.grow(x, n - 1)\n    else\n      x\n    end\n  end\nend\ndef grower\n  Grower\nend\ndef run(n)\n  Grower.grow(INIT, n)\nend\n",
    "def grow(x, n)\n  if n > 0\n    STEP\n    grower.new.step(x, n - 1)\n  else\n    x\n  end\nend\nclass Grower\n  def step(x, n)\n    grow(x, n)\n  end\nend\ndef grower\n  Grower\nend\ndef run(n)\n  grow(INIT, n)\nend\n",
];

/// Gives the generated programs a class to allocate outside the recursing class body.
const PRELUDE: &str = "class Row\n  def initialize(value)\n    @value = value\n  end\nend\ndef row(value)\n  Row.new(value)\nend\n";

#[test]
fn growing_facts_converge_in_loops_blocks_and_recursion() {
    for context in CONTEXTS {
        for (init, step) in GROWTH {
            let source = format!(
                "{PRELUDE}{}",
                context.replace("INIT", init).replace("STEP", step)
            );
            converged(&source);
            let script = Engine::new().compile(&source).unwrap();
            let executed = script.call("run", &[Value::int(3)], CallOptions::default());
            assert!(executed.is_ok(), "{source}: {executed:?}");
        }
    }
}
