use crate::{Value, value::Kind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receiver {
    Bytes,
    Array,
    Hash,
    Int,
    Big,
    Float,
    Bool,
    Nil,
    Symbol,
    Regex,
    Range,
    Money,
    Duration,
    Time,
    Zoned,
    Enum,
    EnumMember,
    Other,
}

impl Receiver {
    pub(crate) fn of(value: &Value) -> Self {
        match value.0 {
            Kind::Bytes(_) => Self::Bytes,
            Kind::Array(_) => Self::Array,
            Kind::Hash(_) => Self::Hash,
            Kind::Int(_) => Self::Int,
            Kind::Big(_) => Self::Big,
            Kind::Float(_) => Self::Float,
            Kind::Bool(_) => Self::Bool,
            Kind::Nil => Self::Nil,
            Kind::Symbol(_) => Self::Symbol,
            Kind::Regex(_) => Self::Regex,
            Kind::Range(_) => Self::Range,
            Kind::Money(_) => Self::Money,
            Kind::Duration(_) => Self::Duration,
            Kind::Time(_) => Self::Time,
            Kind::Zoned(_) => Self::Zoned,
            Kind::Enum(_) => Self::Enum,
            Kind::EnumMember(_) => Self::EnumMember,
            _ => Self::Other,
        }
    }

    /// Returns the message refusing keywords passed to `name`, or `None` when
    /// the member accepts them. Array, hash and range members the reference
    /// checks positional arguments for first defer to that check while
    /// `arguments` is wrong.
    pub(crate) fn keyword_refusal(
        self,
        method: Option<crate::bytecode::Method>,
        name: &str,
        arguments: usize,
    ) -> Option<String> {
        use crate::bytecode::Method::*;
        if self == Self::Array {
            let refused = match method? {
                Transpose => return Some("array.transpose does not take arguments".to_owned()),
                Reverse | Compact | Clear | ToString | Uniq | ToHash => arguments == 0,
                Shift => arguments <= 1,
                First | Last | ValuesAt | Push | Prepend | Pop | Delete | Insert | Fill | Sum => {
                    true
                }
                _ => {
                    return self
                        .rejects_keywords(method)
                        .then(|| format!("{name} does not accept keyword arguments"));
                }
            };
            return refused.then(|| format!("array.{name} does not take keyword arguments"));
        }
        if self == Self::Hash {
            let (refused, wording) = match method? {
                ToArray | Clear => (arguments == 0, "does not take keyword arguments"),
                Flatten | Delete | Replace | ValuesAt => {
                    (true, "does not accept keyword arguments")
                }
                _ => {
                    return self
                        .rejects_keywords(method)
                        .then(|| format!("{name} does not accept keyword arguments"));
                }
            };
            return refused.then(|| format!("hash.{name} {wording}"));
        }
        if self == Self::Range {
            let refused = match method? {
                Include => arguments == 1,
                Length | ExcludeEnd | ToArray => arguments == 0,
                First | Last => true,
                _ => {
                    return self
                        .rejects_keywords(method)
                        .then(|| format!("{name} does not accept keyword arguments"));
                }
            };
            return refused.then(|| format!("range.{name} does not take keyword arguments"));
        }
        if self == Self::Bytes {
            match method {
                Some(ByteSlice | GetByte) => {
                    return Some(format!("string.{name} does not accept keyword arguments"));
                }
                Some(Bytes | Chars | Lines | Codepoints) => {
                    return Some(format!("string.{name} does not take arguments"));
                }
                _ => {}
            }
        }
        self.rejects_keywords(method)
            .then(|| format!("{name} does not accept keyword arguments"))
    }

    pub(crate) fn rejects_keywords(self, method: Option<crate::bytecode::Method>) -> bool {
        use crate::bytecode::Method::*;
        match method {
            Some(Dup | ToString | ToInt | ToFloat) => true,
            Some(method) => match self {
                Self::Array => matches!(
                    method,
                    First
                        | Last
                        | ValuesAt
                        | Reverse
                        | Compact
                        | Uniq
                        | Transpose
                        | ToHash
                        | Push
                        | Prepend
                        | Pop
                        | Shift
                        | Delete
                        | Insert
                        | Clear
                        | Fill
                        | Sum
                ),
                Self::Hash => matches!(
                    method,
                    ToArray | Flatten | Delete | Replace | Clear | ValuesAt
                ),
                Self::Bytes => matches!(
                    method,
                    ByteSlice | GetByte | Bytes | Chars | Lines | Codepoints
                ),
                Self::Range => true,
                _ => false,
            },
            None => false,
        }
    }

    /// Returns the block rejection after leaving ordinary arity errors to
    /// the caller. A block selects chunk's grouping form, which takes no size.
    pub(crate) fn rejects_block(
        self,
        method: Option<crate::bytecode::Method>,
        arguments: bool,
    ) -> Option<&'static str> {
        use crate::bytecode::Method::*;
        match (self, method?) {
            (Self::Array, Chunk) if arguments => {
                Some("array.chunk does not take arguments when a block is supplied")
            }
            (Self::Array, Clear) if !arguments => Some("array.clear does not accept a block"),
            (Self::Array, Compact) if !arguments => Some("array.compact does not accept a block"),
            (Self::Array, Reverse) if !arguments => Some("array.reverse does not accept a block"),
            (Self::Array, ToString) if !arguments => Some("array.to_s does not take a block"),
            (Self::Hash, Clear) if !arguments => Some("hash.clear does not accept a block"),
            _ => None,
        }
    }

    pub(crate) fn typed(self, name: &str) -> Option<&'static str> {
        match self {
            Self::Bytes
                if matches!(
                    name,
                    "length"
                        | "bytesize"
                        | "ord"
                        | "chr"
                        | "getbyte"
                        | "byteslice"
                        | "hex"
                        | "oct"
                        | "empty?"
                        | "concat"
                        | "prepend"
                        | "insert"
                        | "start_with?"
                        | "end_with?"
                        | "include?"
                        | "count"
                        | "casecmp"
                        | "casecmp?"
                        | "between?"
                        | "match"
                        | "match?"
                        | "scan"
                        | "index"
                        | "rindex"
                        | "slice"
                        | "strip"
                        | "strip!"
                        | "squish"
                        | "squish!"
                        | "lstrip"
                        | "lstrip!"
                        | "rstrip"
                        | "rstrip!"
                        | "chomp"
                        | "chomp!"
                        | "chop"
                        | "chop!"
                        | "delete"
                        | "delete!"
                        | "delete_prefix"
                        | "delete_prefix!"
                        | "delete_suffix"
                        | "delete_suffix!"
                        | "tr"
                        | "tr!"
                        | "squeeze"
                        | "squeeze!"
                        | "upcase"
                        | "upcase!"
                        | "downcase"
                        | "downcase!"
                        | "capitalize"
                        | "capitalize!"
                        | "swapcase"
                        | "swapcase!"
                        | "reverse"
                        | "reverse!"
                        | "sub"
                        | "sub!"
                        | "gsub"
                        | "gsub!"
                        | "split"
                        | "partition"
                        | "rpartition"
                        | "chars"
                        | "lines"
                        | "bytes"
                        | "codepoints"
                        | "each_char"
                        | "each_line"
                        | "each_byte"
                        | "each_codepoint"
                        | "template"
                        | "center"
                        | "ljust"
                        | "rjust"
                        | "clamp"
                        | "inspect"
                        | "to_sym"
                        | "to_s"
                        | "to_i"
                        | "to_f"
                ) =>
            {
                Some("string")
            }
            Self::Array
                if matches!(
                    name,
                    "length"
                        | "empty?"
                        | "each"
                        | "each_with_index"
                        | "each_slice"
                        | "each_cons"
                        | "reverse_each"
                        | "cycle"
                        | "map"
                        | "map_with_index"
                        | "flat_map"
                        | "filter_map"
                        | "select"
                        | "reject"
                        | "find"
                        | "reduce"
                        | "include?"
                        | "index"
                        | "rindex"
                        | "fetch"
                        | "values_at"
                        | "dig"
                        | "count"
                        | "any?"
                        | "all?"
                        | "none?"
                        | "one?"
                        | "take_while"
                        | "drop_while"
                        | "grep"
                        | "grep_v"
                        | "slice_when"
                        | "chunk_while"
                        | "push"
                        | "prepend"
                        | "pop"
                        | "shift"
                        | "delete"
                        | "insert"
                        | "clear"
                        | "delete_if"
                        | "keep_if"
                        | "uniq"
                        | "first"
                        | "last"
                        | "sum"
                        | "compact"
                        | "flatten"
                        | "fill"
                        | "chunk"
                        | "window"
                        | "join"
                        | "reverse"
                        | "to_h"
                        | "drop"
                        | "zip"
                        | "transpose"
                        | "union"
                        | "difference"
                        | "sample"
                        | "shuffle"
                        | "rotate"
                        | "product"
                        | "combination"
                        | "permutation"
                        | "repeated_combination"
                        | "repeated_permutation"
                        | "sort"
                        | "sort_by"
                        | "partition"
                        | "group_by"
                        | "group_by_stable"
                        | "tally"
                        | "min"
                        | "max"
                        | "minmax"
                        | "min_by"
                        | "max_by"
                        | "inspect"
                        | "to_s"
                ) =>
            {
                Some("array")
            }
            Self::Hash
                if matches!(
                    name,
                    "length"
                        | "empty?"
                        | "key?"
                        | "value?"
                        | "keys"
                        | "values"
                        | "values_at"
                        | "fetch"
                        | "fetch_values"
                        | "dig"
                        | "each"
                        | "each_with_index"
                        | "each_key"
                        | "each_value"
                        | "to_a"
                        | "merge"
                        | "replace"
                        | "delete"
                        | "clear"
                        | "delete_if"
                        | "keep_if"
                        | "slice"
                        | "except"
                        | "flatten"
                        | "select"
                        | "reject"
                        | "map"
                        | "map_with_index"
                        | "transform_keys"
                        | "deep_transform_keys"
                        | "remap_keys"
                        | "transform_values"
                        | "compact"
                        | "inspect"
                ) =>
            {
                Some("hash")
            }
            Self::Int | Self::Big
                if matches!(
                    name,
                    "abs"
                        | "clamp"
                        | "between?"
                        | "even?"
                        | "odd?"
                        | "times"
                        | "upto"
                        | "downto"
                        | "step"
                        | "zero?"
                        | "positive?"
                        | "negative?"
                        | "nonzero?"
                        | "succ"
                        | "pred"
                        | "round"
                        | "floor"
                        | "ceil"
                        | "div"
                        | "divmod"
                        | "fdiv"
                        | "remainder"
                        | "to_s"
                        | "to_i"
                        | "to_f"
                        | "inspect"
                ) =>
            {
                Some("int")
            }
            Self::Float
                if matches!(
                    name,
                    "abs"
                        | "clamp"
                        | "between?"
                        | "round"
                        | "floor"
                        | "ceil"
                        | "zero?"
                        | "positive?"
                        | "negative?"
                        | "nonzero?"
                        | "nan?"
                        | "infinite?"
                        | "finite?"
                        | "div"
                        | "divmod"
                        | "fdiv"
                        | "remainder"
                        | "to_s"
                        | "to_i"
                        | "to_f"
                        | "inspect"
                ) =>
            {
                Some("float")
            }
            Self::Bool if matches!(name, "inspect" | "to_s") => Some("bool"),
            Self::Nil if matches!(name, "inspect" | "to_s") => Some("nil"),
            Self::Symbol if matches!(name, "inspect" | "to_s" | "to_sym") => Some("symbol"),
            Self::Regex if matches!(name, "match" | "match?" | "source" | "flags" | "inspect") => {
                Some("regex")
            }
            Self::Range
                if matches!(
                    name,
                    "include?"
                        | "first"
                        | "last"
                        | "length"
                        | "exclude_end?"
                        | "to_a"
                        | "each"
                        | "step"
                        | "map"
                        | "select"
                        | "reject"
                        | "find"
                        | "reduce"
                        | "count"
                        | "sum"
                        | "min"
                        | "max"
                        | "to_s"
                        | "inspect"
                ) =>
            {
                Some("range")
            }
            Self::Money if name == "between?" => Some("money"),
            _ => None,
        }
    }

    /// The reference lookup-failure wording and suggestion candidates for a
    /// receiver kind with a fixed member table.
    pub(crate) fn unknown(self) -> Option<(&'static str, &'static [&'static str])> {
        use candidates::*;
        Some(match self {
            Self::Bytes => ("unknown string method", STRING),
            Self::Array => ("unknown array method", ARRAY),
            Self::Int | Self::Big => ("unknown int method", INT),
            Self::Float => ("unknown float method", FLOAT),
            Self::Bool => ("unknown bool method", BOOL),
            Self::Nil => ("unknown nil method", NIL),
            Self::Symbol => ("unknown symbol method", SYMBOL),
            Self::Regex => ("unknown regex method", REGEX),
            Self::Range => ("unknown range method", RANGE),
            Self::Money => ("unknown money member", MONEY),
            Self::Duration => ("unknown duration method", DURATION),
            Self::Time | Self::Zoned => ("unknown time method", TIME),
            Self::Enum => ("unknown enum property", ENUM),
            Self::EnumMember => ("unknown enum member property", ENUM_MEMBER),
            Self::Hash | Self::Other => return None,
        })
    }

    pub(crate) fn available(self, name: &str) -> bool {
        if self.typed(name).is_some() || self.temporal_method(name) {
            return true;
        }
        match self {
            Self::Int => matches!(name, "seconds") || duration_part(name),
            Self::Money => matches!(name, "currency" | "cents"),
            Self::Duration => {
                duration_part(name)
                    || matches!(
                        name,
                        "in_seconds"
                            | "in_minutes"
                            | "in_hours"
                            | "in_days"
                            | "in_weeks"
                            | "in_months"
                            | "in_years"
                            | "to_i"
                            | "iso8601"
                            | "parts"
                    )
            }
            Self::Time | Self::Zoned => matches!(
                name,
                "utc"
                    | "nsec"
                    | "usec"
                    | "subsec"
                    | "to_i"
                    | "to_f"
                    | "year"
                    | "month"
                    | "day"
                    | "hour"
                    | "min"
                    | "sec"
                    | "wday"
                    | "yday"
                    | "utc_offset"
                    | "zone"
                    | "utc?"
                    | "dst?"
                    | "sunday?"
                    | "monday?"
                    | "tuesday?"
                    | "wednesday?"
                    | "thursday?"
                    | "friday?"
                    | "saturday?"
                    | "to_a"
            ),
            Self::Enum => matches!(name, "name" | "to_s" | "inspect"),
            Self::EnumMember => matches!(name, "name" | "symbol" | "enum" | "to_s" | "inspect"),
            _ => false,
        }
    }

    pub(crate) fn temporal_method(self, name: &str) -> bool {
        match self {
            Self::Money => matches!(name, "to_s" | "inspect"),
            Self::Duration => matches!(
                name,
                "to_s" | "inspect" | "between?" | "after" | "from_now" | "ago" | "before"
            ),
            Self::Time | Self::Zoned => matches!(
                name,
                "between?"
                    | "to_s"
                    | "inspect"
                    | "iso8601"
                    | "httpdate"
                    | "rfc2822"
                    | "format"
                    | "strftime"
                    | "localtime"
                    | "round"
                    | "ceil"
                    | "floor"
            ),
            _ => false,
        }
    }
}

/// Member names per receiver kind in reference order, the candidates for a
/// lookup failure's suggestion. Every name resolves on its receiver.
pub(crate) mod candidates {
    pub(crate) const ARRAY: &[&str] = &[
        "length",
        "empty?",
        "each",
        "each_with_index",
        "each_slice",
        "each_cons",
        "reverse_each",
        "cycle",
        "map",
        "map_with_index",
        "flat_map",
        "filter_map",
        "select",
        "reject",
        "find",
        "reduce",
        "include?",
        "index",
        "rindex",
        "fetch",
        "values_at",
        "dig",
        "count",
        "any?",
        "all?",
        "none?",
        "one?",
        "take_while",
        "drop_while",
        "grep",
        "grep_v",
        "slice_when",
        "chunk_while",
        "push",
        "prepend",
        "pop",
        "shift",
        "delete",
        "insert",
        "clear",
        "delete_if",
        "keep_if",
        "uniq",
        "first",
        "last",
        "sum",
        "compact",
        "flatten",
        "fill",
        "chunk",
        "window",
        "join",
        "reverse",
        "to_h",
        "drop",
        "zip",
        "transpose",
        "union",
        "difference",
        "sample",
        "shuffle",
        "rotate",
        "product",
        "combination",
        "permutation",
        "repeated_combination",
        "repeated_permutation",
        "sort",
        "sort_by",
        "partition",
        "group_by",
        "group_by_stable",
        "tally",
        "min",
        "max",
        "minmax",
        "min_by",
        "max_by",
        "inspect",
        "to_s",
    ];
    pub(crate) const HASH: &[&str] = &[
        "length",
        "empty?",
        "key?",
        "value?",
        "keys",
        "values",
        "values_at",
        "fetch",
        "fetch_values",
        "dig",
        "each",
        "each_with_index",
        "each_key",
        "each_value",
        "to_a",
        "merge",
        "replace",
        "delete",
        "clear",
        "delete_if",
        "keep_if",
        "slice",
        "except",
        "flatten",
        "select",
        "reject",
        "map",
        "map_with_index",
        "transform_keys",
        "deep_transform_keys",
        "remap_keys",
        "transform_values",
        "compact",
        "inspect",
    ];
    pub(crate) const STRING: &[&str] = &[
        "length",
        "bytesize",
        "ord",
        "chr",
        "getbyte",
        "byteslice",
        "hex",
        "oct",
        "empty?",
        "concat",
        "prepend",
        "insert",
        "start_with?",
        "end_with?",
        "include?",
        "count",
        "casecmp",
        "casecmp?",
        "between?",
        "match",
        "match?",
        "scan",
        "index",
        "rindex",
        "slice",
        "strip",
        "strip!",
        "squish",
        "squish!",
        "lstrip",
        "lstrip!",
        "rstrip",
        "rstrip!",
        "chomp",
        "chomp!",
        "chop",
        "chop!",
        "delete",
        "delete!",
        "delete_prefix",
        "delete_prefix!",
        "delete_suffix",
        "delete_suffix!",
        "tr",
        "tr!",
        "squeeze",
        "squeeze!",
        "upcase",
        "upcase!",
        "downcase",
        "downcase!",
        "capitalize",
        "capitalize!",
        "swapcase",
        "swapcase!",
        "reverse",
        "reverse!",
        "sub",
        "sub!",
        "gsub",
        "gsub!",
        "split",
        "partition",
        "rpartition",
        "chars",
        "lines",
        "bytes",
        "codepoints",
        "each_char",
        "each_line",
        "each_byte",
        "each_codepoint",
        "template",
        "center",
        "ljust",
        "rjust",
        "clamp",
        "inspect",
        "to_sym",
        "to_s",
        "to_i",
        "to_f",
    ];
    pub(crate) const INT: &[&str] = &[
        "seconds",
        "minutes",
        "hours",
        "days",
        "weeks",
        "abs",
        "clamp",
        "between?",
        "even?",
        "odd?",
        "times",
        "upto",
        "downto",
        "step",
        "zero?",
        "positive?",
        "negative?",
        "nonzero?",
        "succ",
        "pred",
        "round",
        "floor",
        "ceil",
        "div",
        "divmod",
        "fdiv",
        "remainder",
        "to_s",
        "to_i",
        "to_f",
        "inspect",
    ];
    pub(crate) const FLOAT: &[&str] = &[
        "abs",
        "clamp",
        "between?",
        "round",
        "floor",
        "ceil",
        "zero?",
        "positive?",
        "negative?",
        "nonzero?",
        "nan?",
        "infinite?",
        "finite?",
        "div",
        "divmod",
        "fdiv",
        "remainder",
        "to_s",
        "to_i",
        "to_f",
        "inspect",
    ];
    pub(crate) const MONEY: &[&str] = &["currency", "cents", "between?", "to_s", "inspect"];
    pub(crate) const DURATION: &[&str] = &[
        "minutes",
        "hours",
        "days",
        "weeks",
        "in_seconds",
        "in_minutes",
        "in_hours",
        "in_days",
        "in_weeks",
        "in_months",
        "in_years",
        "iso8601",
        "parts",
        "to_i",
        "to_s",
        "inspect",
        "between?",
        "after",
        "from_now",
        "ago",
        "before",
    ];
    pub(crate) const TIME: &[&str] = &[
        "year",
        "month",
        "day",
        "hour",
        "min",
        "sec",
        "usec",
        "nsec",
        "subsec",
        "wday",
        "yday",
        "utc_offset",
        "to_f",
        "to_i",
        "zone",
        "utc?",
        "dst?",
        "sunday?",
        "monday?",
        "tuesday?",
        "wednesday?",
        "thursday?",
        "friday?",
        "saturday?",
        "between?",
        "to_s",
        "inspect",
        "to_a",
        "iso8601",
        "httpdate",
        "rfc2822",
        "format",
        "strftime",
        "utc",
        "localtime",
        "round",
        "ceil",
        "floor",
    ];
    pub(crate) const RANGE: &[&str] = &[
        "include?",
        "first",
        "last",
        "exclude_end?",
        "to_a",
        "each",
        "step",
        "map",
        "select",
        "reject",
        "find",
        "reduce",
        "count",
        "sum",
        "min",
        "max",
        "to_s",
        "inspect",
        "length",
    ];
    pub(crate) const REGEX: &[&str] = &["match", "match?", "source", "flags", "inspect"];
    pub(crate) const SYMBOL: &[&str] = &["inspect", "to_s", "to_sym"];
    pub(crate) const NIL: &[&str] = &["inspect", "to_s"];
    pub(crate) const BOOL: &[&str] = &["inspect", "to_s"];
    pub(crate) const ENUM: &[&str] = &["name", "to_s", "inspect"];
    pub(crate) const ENUM_MEMBER: &[&str] = &["name", "symbol", "enum", "to_s", "inspect"];
    pub(crate) const UNIVERSAL: &[&str] = &["dup", "is_type?"];
}

pub(crate) fn typed(value: &Value, name: &str) -> Option<&'static str> {
    Receiver::of(value).typed(name)
}

pub(crate) fn available(value: &Value, name: &str) -> bool {
    Receiver::of(value).available(name)
}

pub(crate) fn temporal_method(value: &Value, name: &str) -> bool {
    Receiver::of(value).temporal_method(name)
}

pub(crate) fn universal(name: &str) -> bool {
    matches!(name, "dup" | "is_type?")
}

/// A duration's whole minutes, hours, days or weeks, which an int also
/// converts to a duration.
fn duration_part(name: &str) -> bool {
    matches!(name, "minutes" | "hours" | "days" | "weeks")
}

#[cfg(test)]
mod tests {
    use super::{Receiver, candidates::*, universal};

    /// The receiver kind of a class in the signature table.
    fn receiver(class: &str) -> Option<Receiver> {
        Some(match class {
            "string" => Receiver::Bytes,
            "symbol" => Receiver::Symbol,
            "array" => Receiver::Array,
            "hash" => Receiver::Hash,
            "int" => Receiver::Int,
            "float" => Receiver::Float,
            "money" => Receiver::Money,
            "duration" => Receiver::Duration,
            "time" => Receiver::Time,
            "range" => Receiver::Range,
            "regex" => Receiver::Regex,
            "nil" => Receiver::Nil,
            "bool" => Receiver::Bool,
            "enum_type" => Receiver::Enum,
            "enum_value" => Receiver::EnumMember,
            _ => return None,
        })
    }

    fn served(kind: Receiver, name: &str) -> bool {
        if kind == Receiver::Hash {
            kind.typed(name) == Some("hash")
        } else {
            kind.available(name)
        }
    }

    #[test]
    fn canonical_members_resolve_on_their_receiver() {
        for item in &crate::signatures::table().items {
            let crate::signatures::Item::Class(class) = item else {
                continue;
            };
            let Some(kind) = receiver(class.base()) else {
                continue;
            };
            for member in &class.members {
                let name = member.name();
                assert!(served(kind, name), "{}.{name}", class.base());
            }
        }
    }

    /// The suggestion lists keep the spellings ADR-008 removed, which the
    /// surface rules read; every other name resolves on its receiver.
    #[test]
    fn suggestion_candidates_are_members_or_removed_spellings() {
        let removed = |class: &str, name: &str| {
            crate::signatures::renames().iter().any(|rename| {
                (rename.receiver == class || rename.receiver == "T") && rename.name == name
            })
        };
        for (class, names) in [
            ("array", ARRAY),
            ("string", STRING),
            ("hash", HASH),
            ("int", INT),
            ("float", FLOAT),
            ("money", MONEY),
            ("duration", DURATION),
            ("time", TIME),
            ("range", RANGE),
            ("regex", REGEX),
            ("symbol", SYMBOL),
            ("nil", NIL),
            ("bool", BOOL),
            ("enum_type", ENUM),
            ("enum_value", ENUM_MEMBER),
        ] {
            let kind = receiver(class).unwrap();
            for name in names {
                assert!(served(kind, name) || removed(class, name), "{class}.{name}");
            }
            assert!(kind == Receiver::Hash || kind.unknown().is_some());
        }
        for name in UNIVERSAL {
            assert!(universal(name), "{name}");
        }
    }
}
