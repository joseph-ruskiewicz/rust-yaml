//! Parse- and emit-time configuration.

/// Selects scalar-resolution rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Schema {
    /// YAML 1.2 core schema (default).
    #[default]
    Core1_2,
    /// YAML 1.2 JSON schema (strict).
    Json1_2,
    /// YAML 1.1 resolution (the "Norway problem", leading-zero octal).
    Yaml1_1,
    /// Everything resolves to a string.
    Failsafe,
}

/// Whether `<<` merge keys are honored. Independent of schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeKeys {
    /// On for the 1.1 schema, off otherwise.
    #[default]
    Auto,
    On,
    Off,
}

impl MergeKeys {
    /// Resolves whether merge keys are active under the given schema.
    pub fn enabled_for(self, schema: Schema) -> bool {
        match self {
            MergeKeys::On => true,
            MergeKeys::Off => false,
            MergeKeys::Auto => matches!(schema, Schema::Yaml1_1),
        }
    }
}

/// Security guards, enabled by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Max number of alias expansions (billion-laughs guard).
    pub max_aliases: usize,
    /// Max nesting depth of collections.
    pub max_depth: usize,
    /// Max accepted input size in bytes.
    pub max_input_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_aliases: 10_000,
            max_depth: 128,
            max_input_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Options controlling parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptions {
    pub schema: Schema,
    pub merge_keys: MergeKeys,
    pub preserve_formatting: bool,
    pub limits: Limits,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            schema: Schema::Core1_2,
            merge_keys: MergeKeys::Auto,
            preserve_formatting: false,
            limits: Limits::default(),
        }
    }
}

impl ParseOptions {
    /// Default options with formatting preservation enabled (round-trip).
    pub fn preserve_formatting() -> Self {
        Self { preserve_formatting: true, ..Self::default() }
    }

    pub fn with_schema(mut self, schema: Schema) -> Self {
        self.schema = schema;
        self
    }

    pub fn with_merge_keys(mut self, merge_keys: MergeKeys) -> Self {
        self.merge_keys = merge_keys;
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// Options controlling emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitOptions {
    pub round_trip: bool,
    pub indent: usize,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self { round_trip: false, indent: 2 }
    }
}

impl EmitOptions {
    /// Default options with round-trip emission enabled.
    pub fn round_trip() -> Self {
        Self { round_trip: true, ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_options_defaults() {
        let o = ParseOptions::default();
        assert_eq!(o.schema, Schema::Core1_2);
        assert_eq!(o.merge_keys, MergeKeys::Auto);
        assert!(!o.preserve_formatting);
        assert_eq!(o.limits, Limits::default());
    }

    #[test]
    fn preserve_formatting_constructor() {
        let o = ParseOptions::preserve_formatting();
        assert!(o.preserve_formatting);
        assert_eq!(o.schema, Schema::Core1_2);
    }

    #[test]
    fn builder_methods() {
        let o = ParseOptions::default()
            .with_schema(Schema::Yaml1_1)
            .with_merge_keys(MergeKeys::On);
        assert_eq!(o.schema, Schema::Yaml1_1);
        assert_eq!(o.merge_keys, MergeKeys::On);
    }

    #[test]
    fn merge_keys_auto_follows_schema() {
        assert!(!MergeKeys::Auto.enabled_for(Schema::Core1_2));
        assert!(MergeKeys::Auto.enabled_for(Schema::Yaml1_1));
        assert!(MergeKeys::On.enabled_for(Schema::Core1_2));
        assert!(!MergeKeys::Off.enabled_for(Schema::Yaml1_1));
    }

    #[test]
    fn emit_options_round_trip_constructor() {
        assert!(!EmitOptions::default().round_trip);
        assert!(EmitOptions::round_trip().round_trip);
        assert_eq!(EmitOptions::default().indent, 2);
    }
}
