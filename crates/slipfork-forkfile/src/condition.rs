//! Boolean feature conditions.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::diag::Span;
use crate::syntax::Spanned;

/// A boolean feature condition (`feat`, `!feat`, `a & b`, `a | b`, `(...)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    /// A bare feature reference.
    Feature(Spanned<String>),
    /// `!inner`
    Not {
        /// The negated condition.
        inner: Box<Condition>,
        /// The whole condition range.
        span: Span,
    },
    /// `lhs & rhs`
    And {
        /// Left operand.
        lhs: Box<Condition>,
        /// Right operand.
        rhs: Box<Condition>,
        /// The whole condition range.
        span: Span,
    },
    /// `lhs | rhs`
    Or {
        /// Left operand.
        lhs: Box<Condition>,
        /// Right operand.
        rhs: Box<Condition>,
        /// The whole condition range.
        span: Span,
    },
}

impl Condition {
    /// The span of this condition.
    pub fn span(&self) -> Span {
        match self {
            Self::Feature(feature) => feature.span,
            Self::Not { span, .. } | Self::And { span, .. } | Self::Or { span, .. } => *span,
        }
    }

    /// Evaluate the condition against the resolved active feature set.
    pub fn eval(&self, active: &BTreeSet<String>) -> bool {
        match self {
            Self::Feature(feature) => active.contains(&feature.value),
            Self::Not { inner, .. } => !inner.eval(active),
            Self::And { lhs, rhs, .. } => lhs.eval(active) && rhs.eval(active),
            Self::Or { lhs, rhs, .. } => lhs.eval(active) || rhs.eval(active),
        }
    }

    /// Collect every feature reference (including inactive branches).
    pub fn collect_features<'a>(&'a self, out: &mut Vec<&'a Spanned<String>>) {
        match self {
            Self::Feature(feature) => out.push(feature),
            Self::Not { inner, .. } => inner.collect_features(out),
            Self::And { lhs, rhs, .. } | Self::Or { lhs, rhs, .. } => {
                lhs.collect_features(out);
                rhs.collect_features(out);
            }
        }
    }
}
