use super::*;

#[test]
fn availability_uses_visibility_and_metadata_without_invoking_methods() {
    run(
        "class C;property p;private def hidden;raise 'invoked';end;protected def guarded;raise 'invoked';end;public;def check;[respond_to?(:hidden),self.respond_to?(:hidden),self.respond_to?(:guarded),respond_to?(:guarded,false)];end;end;def run;c=C.new;c.data=JSON::parse;c.tap=3;[c.respond_to?(:p),c.respond_to?('p='),c.respond_to?(:data),c.respond_to?(:hidden),c.respond_to?(:hidden,true),c.respond_to?(:guarded,true),c.respond_to?(:tap),c.check];end",
        "[true, true, false, false, true, true, false, [true, false, false, true]]",
    );
    run(
        "class C;end;module M;end;def run;c=C.new;[C.respond_to?(:new),M.respond_to?(:new),c.respond_to?(:class),c.respond_to?(:new),C.respond_to?(:class),c.respond_to?(:to_s),c.respond_to?(:tap),c.respond_to?(:is_type?)];end",
        "[true, false, true, false, false, false, true, true]",
    );
    run(
        "class C;end;def run;c=C.new;c.tap=JSON::parse;C.tap=JSON::parse;[c.respond_to?(:tap),C.respond_to?(:tap)];end",
        "[true, true]",
    );
}

#[test]
fn predicate_overrides_and_lifecycle_helpers_keep_namespace_identity() {
    run(
        "class C;def respond_to?(name);7;end;def is_a?(other);8;end;def is_type?(other);9;end;end;def run;c=C.new;[c.respond_to?(:missing),c.is_a?(C),c.is_type?(:int),C.itself.equal?(C),C.dup==C,C.nil?,C.frozen?];end",
        "[7, 8, 9, true, true, false, true]",
    );
    run(
        "module M;def self.respond_to?(name);7;end;end;def run;[M.respond_to?(:missing),M.dup==M];end",
        "[7, true]",
    );
    let script=Engine::legacy_unchecked().compile("class C;private def respond_to?(name,all=false);7;end;end;def run;begin;C.new.respond_to?(:hidden,true);rescue;99;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
}

#[test]
fn membership_predicates_compare_class_identity_and_validate_arguments() {
    for method in ["is_a?", "kind_of?", "instance_of?"] {
        run(
            &format!(
                "class C;end;class D;end;module M;end;def run;c=C.new;[c.{method}(C),c.{method}(D),c.{method}(M),C.{method}(C),1.{method}(C),nil.{method}(C)];end"
            ),
            "[true, false, false, false, false, false]",
        );
        for arg in ["1", "C.new", "JSON", ":C"] {
            let script = Engine::legacy_unchecked()
                .compile(&format!(
                    "class C;end;def run;begin;C.new.{method}({arg});rescue RuntimeError;99;end;end"
                ))
                .unwrap();
            witness(&script, &[], &CallOptions::default(), "99", true);
        }
    }
}

#[test]
fn namespace_predicates_reject_call_shapes_before_running_blocks() {
    for receiver in ["C", "C.new"] {
        for method in [
            "respond_to?",
            "is_a?",
            "kind_of?",
            "instance_of?",
            "is_type?",
        ] {
            for args in ["", "1,2,3", "extra:1"] {
                let script=Engine::legacy_unchecked().compile(&format!("class C;end;def run;seen=[];begin;{receiver}.{method}({args}){{seen.push(1);return 999}};rescue RuntimeError;[99,seen];end;end")).unwrap();
                witness(&script, &[], &CallOptions::default(), "[99, []]", true);
            }
        }
    }
    for expression in [
        "C.new.respond_to?(1)",
        "C.new.respond_to?(:class,1)",
        "C.is_type?(1)",
        "C.is_type?('int[]')",
    ] {
        let script = Engine::legacy_unchecked()
            .compile(&format!(
                "class C;end;def run;begin;{expression};rescue;99;end;end"
            ))
            .unwrap();
        witness(&script, &[], &CallOptions::default(), "99", true);
    }
}

#[test]
fn named_type_predicates_use_exact_definition_names_and_live_bindings() {
    run(
        "class C;end;class D;end;enum E;A;end;def run;c=C.new;C=D;[c.is_type?(:C),nil.is_type?('C?'),C.is_type?(:C),c.is_type?(:D),E::A.is_type?(:E),E.is_type?(:E),:a.is_type?(:E),c.is_type?(:object),c.is_type?(:int)];end",
        "[true, false, false, false, true, false, false, false, false]",
    );
    run(
        "class User;end;def run;c=User.new;Alias=User;[c.is_type?(:User),c.is_type?(:USER),c.is_type?(:Alias),nil.is_type?('User?'),nil.is_type?('USER?'),nil.is_type?('Missing?')];end",
        "[true, false, false, true, false, false]",
    );
    run(
        "def run;[1.is_type?(:Unknown),nil.is_type?('Unknown?')];end",
        "[false, false]",
    );
    run(
        "class C;end;class D;end;def run;C=D;c=D.new;[c.is_type?(:C),c.is_type?(:D)];end",
        "[false, true]",
    );
}

#[test]
fn type_queries_capture_parent_bindings_through_nested_and_forwarded_blocks() {
    run(
        "class C;end;class D;end;def once;yield;end;def run;C=D;once{once{[nil.is_type?('C?'),nil.is_type?('D?')]}};end",
        "[false, true]",
    );
    run(
        "class C;end;class D;end;def once;yield;end;def run;C=D;once{once{nil.send(:is_type?,'C?')}};end",
        "false",
    );
    run(
        "class C;end;class D;end;def once;yield;end;def run;C=D;once{[nil,'C?'].reduce(:is_type?)};end",
        "false",
    );
    run(
        "class C;end;class D;end;def once;yield;end;def run;C=D;a=once{C=1;nil.is_type?('C?')};[a,nil.is_type?('C?')];end",
        "[false, false]",
    );
    for query in [
        "nil.is_type?('C?')",
        "nil.send(:is_type?,'C?')",
        "[nil,'C?'].reduce(:is_type?)",
    ] {
        run(
            &format!(
                "class C;end;class D;end;def once;yield;end;def query(C);once{{once{{{query}}}}};end;def run;query(D);end"
            ),
            "false",
        );
    }
    run(
        "class C;end;def once;yield;end;def query(C);once{nil.is_type?('C?')};end;def run;query(1);end",
        "true",
    );
}

#[test]
fn declaration_rebinding_crosses_calls_blocks_and_error_cleanup() {
    run(
        "class C;def self.n;1;end;end;class D;def self.n;2;end;end;def replace;C=D;end;def run;original=C;replace;[C.n,original.n];end",
        "[2, 1]",
    );
    run(
        "class C;end;class D;end;def accept(x:C);x;end;def run;C=D;accept(D.new).is_a?(D);end",
        "true",
    );
    run(
        "class C;end;class D;end;def replace;C=D;raise 'changed';ensure;C=D;end;def run;begin;replace;rescue;nil;end;[C.new.is_type?(:D),nil.is_type?('C?')];end",
        "[true, false]",
    );
    run(
        "enum E;A;end;enum F;B;end;def once;yield;end;def run;E=F;once{[E::B.is_type?(:F),nil.is_type?('E?')]};end",
        "[true, false]",
    );
    run(
        "class C;end;def run;C=JSON::parse;(begin;C;end)('[7]')[0];end",
        "7",
    );
    let script = Engine::legacy_unchecked()
        .compile("class C;end;def run;C=JSON::parse;begin;C('[7]');rescue;99;end;end")
        .unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
    for choose in [false, true] {
        let mut engine = Engine::legacy_unchecked();
        engine.register("choose", move |_, _| Ok(Value::boolean(choose)));
        let script = engine.compile("class C;def self.n;1;end;end;class D;def self.n;2;end;end;def run;if choose();C=D;end;C.n;end").unwrap();
        witness(
            &script,
            &[],
            &CallOptions::default(),
            if choose { "2" } else { "1" },
            false,
        );
    }
}

#[test]
fn nested_forwarding_uses_bounded_stack_and_retains_class_identity() {
    for depth in [1, 16, 128] {
        run(
            &format!(
                "class C;end;def run;C.new.send({}:is_a?,C);end",
                ":send,".repeat(depth)
            ),
            "true",
        );
    }
}

#[test]
fn qualified_type_predicates_resolve_enum_exports_without_calling_them() {
    run(
        "enum E;A;end;def run;JSON[:E]=E;[E::A.is_type?('JSON.E'),nil.is_type?('JSON.E?'),E.is_type?('JSON.E')];end",
        "[true, true, false]",
    );
    for atom in ["JSON.Missing", "JSON.Missing?"] {
        let script = Engine::legacy_unchecked()
            .compile(&format!(
                "def run;begin;nil.is_type?('{atom}');rescue;99;end;end"
            ))
            .unwrap();
        witness(&script, &[], &CallOptions::default(), "99", true);
    }
    let script = Engine::legacy_unchecked()
        .compile("class C;end;def run;JSON[:C]=C;begin;C.new.is_type?('JSON.C');rescue;99;end;end")
        .unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
}

#[test]
fn forwarding_preserves_nested_visibility_and_script_overrides() {
    run(
        "class C;private def hidden(x);x+1;end;end;def run;c=C.new;[c.send(:hidden,2),c.public_send(:send,:hidden,3),c.send(:respond_to?,:hidden),c.public_send(:respond_to?,:hidden)];end",
        "[3, 4, true, false]",
    );
    for call in ["c.public_send(:hidden,2)", "c.send(:public_send,:hidden,2)"] {
        let script=Engine::legacy_unchecked().compile(&format!("class C;private def hidden(x);x;end;end;def run;c=C.new;begin;{call};rescue;99;end;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "99", true);
    }
    run(
        "class C;def use(other);other.public_send(:guarded,2);end;protected def guarded(x);x+1;end;end;def run;C.new.use(C.new);end",
        "3",
    );
    run(
        "class C;def send(name,value);value+10;end;end;def run;C.new.public_send(:send,:anything,3);end",
        "13",
    );
    run(
        "module M;private def self.hidden(x);x+1;end;end;def run;M.send(:hidden,2);end",
        "3",
    );
}

#[test]
fn forwarded_calls_and_constructors_use_options_hashes_while_members_are_strict() {
    run(
        "class C;property n;def initialize(options);@n=options[:n];end;def configure(options);@n=options[:n];end;end;def run;c=C.send(:new,n:3);c.send(:configure,n:7);c.n;end",
        "7",
    );
    run(
        "module M;def self.configure(options);options[:n];end;end;def run;M.public_send(:configure,n:7);end",
        "7",
    );
    for call in ["c.configure(n:7)", "(c.configure)(n:7)"] {
        let script=Engine::legacy_unchecked().compile(&format!("class C;def configure(options);options[:n];end;end;def run;c=C.new;begin;{call};rescue;99;end;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "99", true);
    }
    let script=Engine::legacy_unchecked().compile("module M;def self.configure(options);options[:n];end;end;def run;begin;M.configure(n:7);rescue;99;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "99", true);
    for (kind, prefix, receiver) in [
        ("class", "", "C.new"),
        ("class", "self.", "C"),
        ("module", "self.", "C"),
    ] {
        for call in [
            "configure(n:7)",
            "(configure)(n:7)",
            "(missing rescue configure)(n:7)",
            "[1].map{configure(n:7)}",
        ] {
            let script=Engine::legacy_unchecked().compile(&format!("{kind} C;def {prefix}configure(options);options[:n];end;def {prefix}check;{call};end;end;def run;begin;{receiver}.check;rescue;99;end;end")).unwrap();
            witness(&script, &[], &CallOptions::default(), "99", true);
        }
        for call in ["configure n:7", "send(:configure,n:7)"] {
            run(
                &format!(
                    "{kind} C;def {prefix}configure(options);options[:n];end;def {prefix}check;{call};end;end;def run;{receiver}.check;end"
                ),
                "7",
            );
        }
    }
    for method in ["call", "send", "public_send"] {
        let script=Engine::legacy_unchecked().compile(&format!("class C;def {method}(options);options[:n];end;end;def run;begin;C.new.{method}(n:7);rescue;99;end;end")).unwrap();
        witness(&script, &[], &CallOptions::default(), "99", true);
        let forward = if method == "send" {
            "public_send"
        } else {
            "send"
        };
        run(
            &format!(
                "class C;def {method}(options);options[:n];end;end;def run;C.new.{forward}(:{method},n:7);end"
            ),
            "7",
        );
    }
}

#[test]
fn forwarded_methods_keep_property_guards_copies_and_assignment_results() {
    run(
        "class C;getter xs:array<int>;def initialize;@xs=[1];end;def add(x);@xs.push(x);end;end;def run;c=C.new;c.send(:add,2);c.send(:xs).push(3);c.xs;end",
        "[1, 2]",
    );
    let script=Engine::legacy_unchecked().compile("class C;getter xs:array<int>;def initialize;@xs=[1];end;def add(x);@xs.push(x);end;end;def run;c=C.new;begin;c.send(:add,false);rescue;nil;end;c.xs;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "[1]", true);
    run(
        "class C;property n;end;def run;c=C.new;c.send(:'n=',7);c.n;end",
        "7",
    );
    let script=Engine::legacy_unchecked().compile("class C;def value(x);x;end;end;def run;c=C.new;n=0;begin;c.send(:missing,begin;n+=1;end);rescue;n;end;end").unwrap();
    witness(&script, &[], &CallOptions::default(), "1", true);
}

#[test]
fn universal_blocks_and_symbolic_reductions_keep_instance_state() {
    run(
        "class C;property n;def initialize;@n=1;end;def add(x);@n+=x;self;end;end;def run;c=C.new;a=c.tap{|x|x.n=3};b=c.yield_self{|x|x.n+2};d=[c,4,5].reduce(:add);[a==c,b,d==c,c.n];end",
        "[true, 5, true, 12]",
    );
    run(
        "class C;end;def run;c=C.new;[[c,C].reduce(:is_a?),[c,:class].reduce(:respond_to?),[c,:C].reduce(:is_type?)];end",
        "[true, true, true]",
    );
    run(
        "module M;end;def run;M.tap{|m|m.n=3};M.yield_self{|m|m.n+2};end",
        "5",
    );
}

#[test]
fn introspection_preserves_unknown_visibility_flags_and_method_name_alternatives() {
    for flag in [false, true] {
        let mut engine = Engine::legacy_unchecked();
        engine.register("choose", move |_, _| Ok(Value::boolean(flag)));
        let script=engine.compile("class C;private def hidden;raise 'invoked';end;end;def run;c=C.new;[c.respond_to?(:hidden,choose()),c.respond_to?(if choose();:class;else;:missing;end)];end").unwrap();
        witness(
            &script,
            &[],
            &CallOptions::default(),
            if flag {
                "[true, true]"
            } else {
                "[false, false]"
            },
            false,
        );
    }
}

#[test]
fn forwarded_calls_preserve_ignored_host_control_transfers_and_cleanup() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        let _ = call.call_block(&[]);
        let _ = call.call_block(&[]);
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: true,
    })
    .unwrap();
    let driver = Value::object(vec![(b"visit".to_vec(), method.value())]);
    for (transfer, expected) in [("break 7", "[7, 3]"), ("return 9", "[9, 3]")] {
        let script=Engine::legacy_unchecked().compile(&format!("class C;property n;def initialize;@n=0;end;def go(driver);driver.visit{{@n+=1;{transfer}}};ensure;@n+=2;end;end;def run(driver);c=C.new;[c.send(:go,driver),c.n];end")).unwrap();
        witness(
            &script,
            std::slice::from_ref(&driver),
            &CallOptions::default(),
            expected,
            false,
        );
    }
}

#[test]
fn namespace_queries_and_forwarding_obey_quotas_and_release_storage() {
    for source in [
        "class C;private def hidden;7;end;end;def run;c=C.new;[c.respond_to?(:hidden,true),c.send(:hidden)];end",
        "class C;end;class D;end;def once;yield;end;def run;C=D;once{nil.is_type?('C?')};end",
        "class C;property n;def initialize;@n=1;end;def add(x);@n+=x;end;end;def run;c=C.new;c.public_send(:send,:add,2);c.n;end",
    ] {
        metered(source);
    }
}
