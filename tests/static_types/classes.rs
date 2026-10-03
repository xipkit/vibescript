//! Classes, instance variables, properties, enums and exhaustive `case`.

use super::support::{clean, codes, error, fixed, spanned};

const POINT: &str = "class Point\n  property x: int\n  @label: string = \"p\"\n  @y: int\n  def initialize(x: int, y: int)\n    @x = x\n    @y = y\n  end\n  def sum -> int\n    @x + @y\n  end\n  def label -> string\n    @label\n  end\nend\n";

#[test]
fn classes_type_their_constructors_properties_and_methods() {
    clean(&format!(
        "{POINT}point = Point.new(1, 2)\ntotal: int = point.sum + point.x\npoint.x = 3\n"
    ));
    codes(&format!("{POINT}Point.new(1)\n"), &["V0301"]);
    codes(&format!("{POINT}Point.new(1, \"2\")\n"), &["V0101"]);
    codes(
        &format!("{POINT}point = Point.new(1, 2)\npoint.x = \"a\"\n"),
        &["V0101"],
    );
    let source = format!("{POINT}Point.new(1, 2).missing\n");
    let diagnostic = error(&source, "V0203", "Point has no member `missing`");
    assert_eq!(spanned(&source, &diagnostic), "missing");
}

#[test]
fn instance_variables_must_be_declared() {
    let source = "class C\n  def initialize\n    @count = 1\n  end\nend\n";
    let diagnostic = error(source, "V0204", "`@count` is not declared in `C`");
    assert_eq!(spanned(source, &diagnostic), "@count");
    codes(
        "class C\n  @count: int = 0\n  def bump\n    @count = \"x\"\n  end\nend\n",
        &["V0101"],
    );
}

#[test]
fn initialize_assigns_every_instance_variable_without_a_default() {
    clean(
        "class C\n  @a: int\n  def initialize(flag: bool)\n    if flag\n      @a = 1\n    else\n      @a = 2\n    end\n  end\nend\n",
    );
    error(
        "class C\n  @a: int\n  def initialize(flag: bool)\n    if flag\n      @a = 1\n    end\n  end\nend\n",
        "V0205",
        "does not assign @a on every path",
    );
    error(
        "class C\n  @a: int\n  def initialize(flag: bool)\n    return if flag\n    @a = 1\n  end\nend\n",
        "V0205",
        "@a",
    );
    clean("class C\n  @a: int?\n  def initialize\n  end\nend\n");
    clean("class C\n  property a: int\n  def initialize(@a: int)\n  end\nend\n");
    // An ensure runs on every path through its `begin`, so what it assigns
    // is assigned after it, as master's checker found.
    clean(
        "class C\n  @a: int\n  def initialize\n    begin\n      nil\n    ensure\n      @a = 1\n    end\n  end\n  def value -> int\n    @a\n  end\nend\np(C.new.value)\n",
    );
    error(
        "class C\n  @a: int\n  def initialize(flag: bool)\n    begin\n      nil\n    ensure\n      @a = 1 if flag\n    end\n  end\nend\n",
        "V0205",
        "@a",
    );
}

#[test]
fn operators_a_class_defines_are_typed_by_their_methods() {
    clean(
        "class Money2\n  @cents: int = 0\n  def +(other: int) -> Money2\n    self\n  end\nend\nm = Money2.new + 1\nn: Money2 = m\n",
    );
    codes("class C\nend\nC.new + 1\n", &["V0108"]);
}

#[test]
fn enum_members_and_symbols_naming_them() {
    let status = "enum Status\n  Draft\n  InReview\nend\n";
    clean(&format!(
        "{status}def f(s: Status) -> string\n  s.name\nend\nf(Status::Draft)\nf(:in_review)\n"
    ));
    let source = format!("{status}def f(s: Status) -> string\n  s.name\nend\nf(:archived)\n");
    let diagnostic = error(&source, "V0206", "`:archived` is not a member of `Status`");
    assert_eq!(spanned(&source, &diagnostic), ":archived");
    codes(&format!("{status}Status::Archived\n"), &["V0206"]);
    clean(&format!("{status}s: Status = :draft\n"));
}

#[test]
fn case_over_an_enum_or_bool_is_exhaustive() {
    let status = "enum Status\n  Draft\n  Live\nend\n";
    clean(&format!(
        "{status}def f(s: Status) -> int\n  case s\n  when Status::Draft then 1\n  when Status::Live then 2\n  end\nend\n"
    ));
    clean(&format!(
        "{status}def f(s: Status) -> int\n  case s\n  when Status::Draft then 1\n  else 2\n  end\nend\n"
    ));
    let source = format!(
        "{status}def f(s: Status) -> int?\n  case s\n  when Status::Draft then 1\n  end\nend\n"
    );
    error(&source, "V0114", "does not handle `Status::Live`");
    clean("def f(b: bool) -> int\n  case b\n  when true then 1\n  when false then 0\n  end\nend\n");
    codes(
        "def f(b: bool) -> int?\n  case b\n  when true then 1\n  end\nend\n",
        &["V0114"],
    );
    clean("def f(n: int) -> int?\n  case n\n  when 1 then 1\n  end\nend\n");
}

#[test]
fn class_variables_are_declared_in_the_body() {
    clean(
        "class C\n  @@calls: int = 0\n  def self.bump -> int\n    @@calls += 1\n    @@calls\n  end\nend\n",
    );
    // A declaration types the value, so an empty literal needs no context.
    clean(
        "module Registry\n  @@names: array<string> = []\n  def self.add(name: string) -> array<string>\n    @@names << name\n  end\nend\n",
    );
    // A method may read a declaration that comes after it in the body.
    clean("class C\n  def self.read -> int\n    @@calls\n  end\n  @@calls: int = 0\nend\n");
    codes(
        "class C\n  @@calls: int = 0\n  def self.bump\n    @@calls = \"x\"\n  end\nend\n",
        &["V0101"],
    );
    error(
        "class C\n  @@calls: int = \"x\"\nend\n",
        "V0101",
        "`@@calls` is int, found string",
    );
    error(
        "class C\n  def self.bump -> int\n    @@calls\n  end\nend\n",
        "V0204",
        "declare it in the class body",
    );
}

#[test]
fn an_undeclared_class_variable_is_declared_by_its_fix() {
    let source = "class C\n  @@calls = 0\n  def self.bump -> int\n    @@calls += 1\n    @@calls\n  end\nend\n";
    let diagnostic = error(
        source,
        "V0204",
        "class variable `@@calls` is not declared in `C`",
    );
    assert_eq!(spanned(source, &diagnostic), "@@calls");
    let repaired = fixed(source, &diagnostic);
    assert!(
        repaired.starts_with("class C\n  @@calls: int = 0\n"),
        "{repaired}"
    );
    clean(&repaired);
    // A later assignment is checked against the first value's type.
    codes(
        "class C\n  @@calls = 0\n  def self.bump\n    @@calls = \"x\"\n  end\nend\n",
        &["V0204", "V0101"],
    );
    // An empty literal or nil does not say which type to declare, and an
    // assignment nested in the body cannot become a declaration.
    for source in [
        "class C\n  @@names = []\nend\n",
        "class C\n  @@label = nil\nend\n",
        "class C\n  if true\n    @@calls = 0\n  end\nend\n",
    ] {
        let diagnostic = error(source, "V0204", "is not declared");
        assert!(diagnostic.fixes.is_empty(), "{source}: {diagnostic:?}");
    }
}

#[test]
fn modules_are_namespaces_of_functions_and_constants() {
    clean(
        "module Scoring\n  BONUS = 10\n  def self.with_bonus(score: int) -> int\n    score + BONUS\n  end\nend\ndef run -> int\n  Scoring.with_bonus(80) + Scoring::BONUS\nend\n",
    );
    codes(
        "module Scoring\n  def self.f(score: int) -> int\n    score\n  end\nend\nScoring.f(\"x\")\n",
        &["V0101"],
    );
}

#[test]
fn symbols_are_never_equal_to_enum_members() {
    // Typed parameters turn a symbol naming a member into the member, but
    // `case` and `==` compare values as they are.
    let status = "enum Status\n  Draft\n  Live\nend\n";
    let source = format!(
        "{status}def f(s: Status) -> int\n  case s\n  when :draft then 1\n  when Status::Live then 2\n  end\nend\n"
    );
    let diagnostic = error(&source, "V0101", "a `when` over it never matches a symbol");
    assert_eq!(spanned(&source, &diagnostic), ":draft");
    assert!(super::support::fixed(&source, &diagnostic).contains("when Status::Draft then 1"));
    let source = format!("{status}def f(s: Status) -> bool\n  s == :live\nend\n");
    let diagnostic = error(&source, "V0101", "they never compare equal");
    assert!(super::support::fixed(&source, &diagnostic).contains("s == Status::Live"));
    codes(
        &format!("{status}def f(s: Status) -> bool\n  s == :gone\nend\n"),
        &["V0206"],
    );
}

#[test]
fn properties_declare_their_types() {
    let source = "class C\n  property name\nend\n";
    let diagnostic = error(source, "V0118", "property `name` has no type");
    assert_eq!(spanned(source, &diagnostic), "name");
    clean("class C\n  property name: string\n  def initialize\n    @name = \"a\"\n  end\nend\n");
}

#[test]
fn nested_classes_are_refused() {
    // The runtime never binds a class declared below the top level, and
    // fails as soon as the declaration runs.
    for source in [
        "class Outer\n  class Inner\n  end\nend\n",
        "def make\n  class Inner\n  end\nend\n",
    ] {
        let diagnostic = error(source, "V0001", "only supported at the top level");
        assert_eq!(spanned(source, &diagnostic), "class", "{source}");
    }
}

#[test]
fn class_variable_initializers_keep_their_declared_types() {
    clean("class C; @@n: int? = nil; def self.n -> int?; @@n; end; end; C.n");
    codes("class C; @@n: int = nil; end", &["V0101"]);
}
