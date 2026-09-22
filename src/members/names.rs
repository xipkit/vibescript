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

    pub(crate) fn rejects_keywords(self, method: Option<crate::bytecode::Method>) -> bool {
        use crate::bytecode::Method::*;
        match method {
            Some(IsNil | Itself | Dup | ToString | ToInt | ToFloat) => true,
            Some(method) => match self {
                Self::Array => matches!(
                    method,
                    First
                        | Last
                        | At
                        | Slice
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
                    ToArray | Flatten | Store | Delete | Replace | Clear | ValuesAt
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
        name: &str,
    ) -> Option<&'static str> {
        use crate::bytecode::Method::*;
        match (self, method?) {
            (Self::Array, Chunk) if arguments => {
                Some("array.chunk does not take arguments when a block is supplied")
            }
            (Self::Array, Clear) if !arguments => Some("array.clear does not accept a block"),
            (Self::Array, Compact) if !arguments => Some("array.compact does not accept a block"),
            (Self::Array, Reverse) if !arguments => Some("array.reverse does not accept a block"),
            (Self::Array, ToString) if !arguments => Some(if name == "string" {
                "array.string does not take a block"
            } else {
                "array.to_s does not take a block"
            }),
            (Self::Hash, Clear) if !arguments => Some("hash.clear does not accept a block"),
            _ => None,
        }
    }

    pub(crate) fn typed(self, name: &str) -> Option<&'static str> {
        match self {
            Self::Bytes
                if matches!(
                    name,
                    "size"
                        | "length"
                        | "bytesize"
                        | "ord"
                        | "chr"
                        | "getbyte"
                        | "byteslice"
                        | "hex"
                        | "oct"
                        | "empty?"
                        | "clear"
                        | "concat"
                        | "prepend"
                        | "insert"
                        | "replace"
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
                        | "intern"
                        | "to_s"
                        | "string"
                        | "to_i"
                        | "to_f"
                ) =>
            {
                Some("string")
            }
            Self::Array
                if matches!(
                    name,
                    "size"
                        | "length"
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
                        | "collect_concat"
                        | "filter_map"
                        | "select"
                        | "reject"
                        | "find"
                        | "find_index"
                        | "reduce"
                        | "include?"
                        | "index"
                        | "rindex"
                        | "at"
                        | "slice"
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
                        | "append"
                        | "prepend"
                        | "unshift"
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
                        | "take"
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
                        | "string"
                ) =>
            {
                Some("array")
            }
            Self::Hash
                if matches!(
                    name,
                    "size"
                        | "length"
                        | "empty?"
                        | "key?"
                        | "has_key?"
                        | "member?"
                        | "include?"
                        | "value?"
                        | "has_value?"
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
                        | "store"
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
                        | "next"
                        | "succ"
                        | "pred"
                        | "round"
                        | "floor"
                        | "ceil"
                        | "div"
                        | "divmod"
                        | "fdiv"
                        | "remainder"
                        | "modulo"
                        | "to_s"
                        | "string"
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
                        | "modulo"
                        | "to_s"
                        | "string"
                        | "to_i"
                        | "to_f"
                        | "inspect"
                ) =>
            {
                Some("float")
            }
            Self::Bool if matches!(name, "inspect" | "to_s" | "string") => Some("bool"),
            Self::Nil if matches!(name, "inspect" | "to_s" | "string") => Some("nil"),
            Self::Symbol
                if matches!(name, "inspect" | "id2name" | "to_s" | "string" | "to_sym") =>
            {
                Some("symbol")
            }
            Self::Regex if matches!(name, "match" | "match?" | "source" | "flags" | "inspect") => {
                Some("regex")
            }
            Self::Range
                if matches!(
                    name,
                    "cover?"
                        | "include?"
                        | "member?"
                        | "first"
                        | "last"
                        | "size"
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
                        | "string"
                        | "inspect"
                ) =>
            {
                Some("range")
            }
            Self::Money if matches!(name, "format" | "between?") => Some("money"),
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
        let unit = duration_unit(name);
        match self {
            Self::Int => unit,
            Self::Money => matches!(name, "currency" | "cents" | "amount"),
            Self::Duration => {
                unit || matches!(
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
                        | "format"
                )
            }
            Self::Time | Self::Zoned => matches!(
                name,
                "getutc"
                    | "getgm"
                    | "utc"
                    | "gmtime"
                    | "nsec"
                    | "tv_nsec"
                    | "usec"
                    | "tv_usec"
                    | "subsec"
                    | "hash"
                    | "to_i"
                    | "tv_sec"
                    | "to_f"
                    | "to_r"
                    | "year"
                    | "month"
                    | "mon"
                    | "day"
                    | "mday"
                    | "hour"
                    | "min"
                    | "sec"
                    | "wday"
                    | "yday"
                    | "utc_offset"
                    | "gmt_offset"
                    | "gmtoff"
                    | "zone"
                    | "utc?"
                    | "gmt?"
                    | "dst?"
                    | "isdst"
                    | "sunday?"
                    | "monday?"
                    | "tuesday?"
                    | "wednesday?"
                    | "thursday?"
                    | "friday?"
                    | "saturday?"
                    | "to_a"
            ),
            Self::Enum => matches!(name, "name" | "to_s" | "string" | "inspect"),
            Self::EnumMember => matches!(
                name,
                "name" | "symbol" | "enum" | "to_s" | "string" | "inspect"
            ),
            _ => false,
        }
    }

    pub(crate) fn property(self, name: &str) -> bool {
        match self {
            Self::Int | Self::Big => duration_unit(name),
            Self::Money | Self::Duration | Self::Time | Self::Zoned => {
                self.available(name) && self.typed(name).is_none() && !self.temporal_method(name)
            }
            Self::Enum => name == "name",
            Self::EnumMember => matches!(name, "name" | "symbol" | "enum"),
            _ => false,
        }
    }

    pub(crate) fn temporal_method(self, name: &str) -> bool {
        match self {
            Self::Money => matches!(name, "to_s" | "string" | "inspect"),
            Self::Duration => matches!(
                name,
                "to_s"
                    | "string"
                    | "inspect"
                    | "eql?"
                    | "between?"
                    | "after"
                    | "since"
                    | "from_now"
                    | "ago"
                    | "before"
                    | "until"
            ),
            Self::Time | Self::Zoned => matches!(
                name,
                "<=>"
                    | "eql?"
                    | "between?"
                    | "to_s"
                    | "string"
                    | "inspect"
                    | "iso8601"
                    | "xmlschema"
                    | "rfc3339"
                    | "httpdate"
                    | "rfc2822"
                    | "rfc822"
                    | "format"
                    | "strftime"
                    | "getlocal"
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
        "size",
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
        "collect_concat",
        "filter_map",
        "select",
        "reject",
        "find",
        "find_index",
        "reduce",
        "include?",
        "index",
        "rindex",
        "at",
        "slice",
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
        "append",
        "prepend",
        "unshift",
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
        "take",
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
        "string",
    ];
    pub(crate) const HASH: &[&str] = &[
        "size",
        "length",
        "empty?",
        "key?",
        "has_key?",
        "member?",
        "include?",
        "value?",
        "has_value?",
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
        "store",
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
        "size",
        "length",
        "bytesize",
        "ord",
        "chr",
        "getbyte",
        "byteslice",
        "hex",
        "oct",
        "empty?",
        "clear",
        "concat",
        "prepend",
        "insert",
        "replace",
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
        "intern",
        "to_s",
        "string",
        "to_i",
        "to_f",
    ];
    pub(crate) const INT: &[&str] = &[
        "seconds",
        "second",
        "minutes",
        "minute",
        "hours",
        "hour",
        "days",
        "day",
        "weeks",
        "week",
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
        "next",
        "succ",
        "pred",
        "round",
        "floor",
        "ceil",
        "div",
        "divmod",
        "fdiv",
        "remainder",
        "modulo",
        "to_s",
        "string",
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
        "modulo",
        "to_s",
        "string",
        "to_i",
        "to_f",
        "inspect",
    ];
    pub(crate) const MONEY: &[&str] = &[
        "currency", "cents", "amount", "format", "between?", "to_s", "string", "inspect",
    ];
    pub(crate) const DURATION: &[&str] = &[
        "seconds",
        "second",
        "minutes",
        "minute",
        "hours",
        "hour",
        "days",
        "day",
        "weeks",
        "week",
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
        "string",
        "inspect",
        "format",
        "eql?",
        "between?",
        "after",
        "since",
        "from_now",
        "ago",
        "before",
        "until",
    ];
    pub(crate) const TIME: &[&str] = &[
        "year",
        "month",
        "mon",
        "mday",
        "day",
        "hour",
        "min",
        "sec",
        "usec",
        "tv_usec",
        "nsec",
        "tv_nsec",
        "subsec",
        "wday",
        "yday",
        "hash",
        "utc_offset",
        "gmt_offset",
        "gmtoff",
        "to_f",
        "to_i",
        "tv_sec",
        "to_r",
        "zone",
        "utc?",
        "gmt?",
        "dst?",
        "isdst",
        "sunday?",
        "monday?",
        "tuesday?",
        "wednesday?",
        "thursday?",
        "friday?",
        "saturday?",
        "<=>",
        "eql?",
        "between?",
        "to_s",
        "string",
        "inspect",
        "to_a",
        "iso8601",
        "xmlschema",
        "rfc3339",
        "httpdate",
        "rfc2822",
        "rfc822",
        "format",
        "strftime",
        "getutc",
        "getgm",
        "getlocal",
        "utc",
        "gmtime",
        "localtime",
        "round",
        "ceil",
        "floor",
    ];
    pub(crate) const RANGE: &[&str] = &[
        "cover?",
        "include?",
        "member?",
        "first",
        "last",
        "size",
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
        "string",
        "inspect",
    ];
    pub(crate) const REGEX: &[&str] = &["match", "match?", "source", "flags", "inspect"];
    pub(crate) const SYMBOL: &[&str] = &["inspect", "id2name", "to_s", "string", "to_sym"];
    pub(crate) const NIL: &[&str] = &["inspect", "to_s", "string"];
    pub(crate) const BOOL: &[&str] = &["inspect", "to_s", "string"];
    pub(crate) const ENUM: &[&str] = &["name", "to_s", "inspect"];
    pub(crate) const ENUM_MEMBER: &[&str] =
        &["name", "symbol", "enum", "to_s", "string", "inspect"];
    pub(crate) const UNIVERSAL: &[&str] = &[
        "itself",
        "dup",
        "clone",
        "freeze",
        "frozen?",
        "nil?",
        "eql?",
        "equal?",
        "send",
        "public_send",
        "tap",
        "yield_self",
        "respond_to?",
        "is_a?",
        "kind_of?",
        "instance_of?",
        "is_type?",
    ];
}

pub(crate) fn typed(value: &Value, name: &str) -> Option<&'static str> {
    Receiver::of(value).typed(name)
}

pub(crate) fn available(value: &Value, name: &str) -> bool {
    Receiver::of(value).available(name)
}

pub(crate) fn property(value: &Value, name: &str) -> bool {
    Receiver::of(value).property(name)
}

pub(crate) fn temporal_method(value: &Value, name: &str) -> bool {
    Receiver::of(value).temporal_method(name)
}

pub(crate) fn universal(name: &str) -> bool {
    matches!(
        name,
        "itself"
            | "dup"
            | "clone"
            | "freeze"
            | "frozen?"
            | "nil?"
            | "eql?"
            | "equal?"
            | "send"
            | "public_send"
            | "tap"
            | "yield_self"
            | "respond_to?"
            | "is_a?"
            | "kind_of?"
            | "instance_of?"
            | "is_type?"
    )
}

fn duration_unit(name: &str) -> bool {
    matches!(
        name,
        "second"
            | "seconds"
            | "minute"
            | "minutes"
            | "hour"
            | "hours"
            | "day"
            | "days"
            | "week"
            | "weeks"
    )
}

#[cfg(test)]
mod tests {
    use super::{Receiver, candidates::*, universal};

    #[test]
    fn suggestion_candidates_resolve_on_their_receiver() {
        for (receiver, names) in [
            (Receiver::Array, ARRAY),
            (Receiver::Bytes, STRING),
            (Receiver::Int, INT),
            (Receiver::Float, FLOAT),
            (Receiver::Money, MONEY),
            (Receiver::Duration, DURATION),
            (Receiver::Time, TIME),
            (Receiver::Range, RANGE),
            (Receiver::Regex, REGEX),
            (Receiver::Symbol, SYMBOL),
            (Receiver::Nil, NIL),
            (Receiver::Bool, BOOL),
            (Receiver::Enum, ENUM),
            (Receiver::EnumMember, ENUM_MEMBER),
        ] {
            for name in names {
                assert!(receiver.available(name), "{name}");
            }
            assert!(receiver.unknown().is_some());
        }
        for name in HASH {
            assert!(Receiver::Hash.typed(name) == Some("hash"), "{name}");
        }
        for name in UNIVERSAL {
            assert!(universal(name), "{name}");
        }
    }
}
