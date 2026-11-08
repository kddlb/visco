//! Generic codec parameters, so the UI can build a parameter panel for any codec.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum ParamSpec {
    Int {
        key: &'static str,
        label: &'static str,
        min: i64,
        max: i64,
        default: i64,
    },
    Choice {
        key: &'static str,
        label: &'static str,
        options: Vec<&'static str>,
        default: usize,
    },
    Bool {
        key: &'static str,
        label: &'static str,
        default: bool,
    },
}

impl ParamSpec {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Int { key, .. } | Self::Choice { key, .. } | Self::Bool { key, .. } => key,
        }
    }

    pub fn default_value(&self) -> ParamValue {
        match self {
            Self::Int { default, .. } => ParamValue::Int(*default),
            Self::Choice { default, .. } => ParamValue::Choice(*default),
            Self::Bool { default, .. } => ParamValue::Bool(*default),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamValue {
    Int(i64),
    Choice(usize),
    Bool(bool),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params(pub BTreeMap<&'static str, ParamValue>);

impl Params {
    pub fn defaults(schema: &[ParamSpec]) -> Self {
        Self(schema.iter().map(|s| (s.key(), s.default_value())).collect())
    }

    pub fn int(&self, key: &str, fallback: i64) -> i64 {
        match self.0.get(key) {
            Some(ParamValue::Int(v)) => *v,
            _ => fallback,
        }
    }

    pub fn choice(&self, key: &str, fallback: usize) -> usize {
        match self.0.get(key) {
            Some(ParamValue::Choice(v)) => *v,
            _ => fallback,
        }
    }

    pub fn bool(&self, key: &str, fallback: bool) -> bool {
        match self.0.get(key) {
            Some(ParamValue::Bool(v)) => *v,
            _ => fallback,
        }
    }

    pub fn set(&mut self, key: &'static str, v: ParamValue) {
        self.0.insert(key, v);
    }
}
